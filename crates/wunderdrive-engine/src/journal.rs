//! The local journal — a [`redb`] table keyed by relative key, holding the
//! last-synced snapshot per file. This snapshot is what enables real
//! three-way reconciliation instead of naive mirroring (spec §5).
//!
//! Key space: the "relative key" = the object's path relative to the local
//! mirror root, using `/` separators (and with the configured bucket prefix
//! already stripped by [`PrefixStore`](object_store::prefix::PrefixStore)).
//! `local_path` and `s3_key` are derived deterministically from this key.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// The single redb table: relative-key (UTF-8) → JSON [`JournalEntry`].
const TABLE: TableDefinition<&str, &str> = TableDefinition::new("entries");

/// Extraction cache: blake3-hex → extracted text. Keyed by content hash so
/// rename / move / second-device is a free cache hit (spec §6).
const EXTRACT_TABLE: TableDefinition<&str, &str> = TableDefinition::new("extracted");

/// Lazy-download stubs: relative-key → JSON [`StubEntry`]. Records a remote
/// object's existence without downloading bytes. Promoted to `TABLE` on
/// materialize. See spec §10 (selective sync, pulled forward).
const STUB_TABLE: TableDefinition<&str, &str> = TableDefinition::new("stubs");

/// Tantivy index manifest: relative-key → blake3-hex of the indexed text.
/// Tracks what the search index currently holds so the sweep can diff cheaply:
/// orphans (key here but not in the journal) get deleted; renames (new key,
/// same hash) re-insert from the extraction cache without re-parsing.
const INDEXED_TABLE: TableDefinition<&str, &str> = TableDefinition::new("indexed");

/// Object-Lock-aware keys: relative-key → empty marker. Populated when an
/// upload or delete fails with S3 `PermissionDenied` (Object Lock retention).
/// Reconcile skips keys in this set so we don't retry forever. Cleared by
/// [`lock_clear`] when the user explicitly retries.
const LOCK_TABLE: TableDefinition<&str, &str> = TableDefinition::new("locked");

/// Sync metadata: singleton key → JSON value. Stores the last-seen remote key
/// (for incremental listing offset) and the sync counter (to schedule periodic
/// full listings that catch deletes).
const META_TABLE: TableDefinition<&str, &str> = TableDefinition::new("meta");

/// Version history: relative-key → JSON `Vec<VersionEntry>`. Tracks the
/// version IDs seen for each key during remote listings, so the user can
/// restore to a known-good version without S3's ListObjectVersions API
/// (which object_store 0.14 doesn't expose).
const VERSION_TABLE: TableDefinition<&str, &str> = TableDefinition::new("versions");

/// Pinned versions: relative-key → version-id string. When a key is pinned,
/// downloads/materialize fetch the specific version instead of HEAD, and
/// reconcile does not overwrite it.
const PIN_TABLE: TableDefinition<&str, &str> = TableDefinition::new("pinned");

/// Metadata for a remote-only object (lazy-download stub).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StubEntry {
    pub size: u64,
    pub version: Option<String>,
}

/// One row of the last-synced snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalEntry {
    /// blake3 content hash — the identity, source of truth for "same file".
    pub blake3: [u8; 32],
    pub size: u64,
    /// mtime in milliseconds since the Unix epoch.
    pub mtime_millis: u64,
    /// Remote version id / ETag for cheap change detection on the next poll.
    /// `None` for entries not yet uploaded.
    pub remote_version: Option<String>,
}

impl JournalEntry {
    pub fn mtime(&self) -> SystemTime {
        UNIX_EPOCH + std::time::Duration::from_millis(self.mtime_millis)
    }
}

/// Open (or create) the journal database at `path`.
pub fn open(path: &Path) -> Result<Database> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = Database::create(path)?;
    // Ensure all tables exist.
    let txn = db.begin_write()?;
    {
        let _ = txn.open_table(TABLE)?;
        let _ = txn.open_table(EXTRACT_TABLE)?;
        let _ = txn.open_table(STUB_TABLE)?;
        let _ = txn.open_table(INDEXED_TABLE)?;
        let _ = txn.open_table(LOCK_TABLE)?;
        let _ = txn.open_table(META_TABLE)?;
        let _ = txn.open_table(VERSION_TABLE)?;
        let _ = txn.open_table(PIN_TABLE)?;
    }
    txn.commit()?;
    Ok(db)
}

/// Read all entries into a map keyed by relative key.
pub fn snapshot(db: &Database) -> Result<HashMap<String, JournalEntry>> {
    let read = db.begin_read()?;
    let table = read.open_table(TABLE)?;
    let mut out = HashMap::new();
    for row in table.iter()? {
        let (k, v) = row?;
        let key = k.value().to_string();
        let entry: JournalEntry = serde_json::from_str(v.value())
            .map_err(|e| crate::Error::other(format!("journal decode {key}: {e}")))?;
        out.insert(key, entry);
    }
    Ok(out)
}

/// Insert or replace an entry.
pub fn upsert(db: &Database, key: &str, entry: &JournalEntry) -> Result<()> {
    let json =
        serde_json::to_string(entry).map_err(|e| crate::Error::other(format!("encode: {e}")))?;
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(TABLE)?;
        table.insert(key, json.as_str())?;
    }
    txn.commit()?;
    Ok(())
}

/// Batch upsert, in a single transaction.
pub fn upsert_many(db: &Database, entries: &[(String, JournalEntry)]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(TABLE)?;
        for (key, entry) in entries {
            let json = serde_json::to_string(entry)
                .map_err(|e| crate::Error::other(format!("encode: {e}")))?;
            table.insert(key.as_str(), json.as_str())?;
        }
    }
    txn.commit()?;
    Ok(())
}

/// Remove an entry by relative key.
pub fn remove(db: &Database, key: &str) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(TABLE)?;
        table.remove(key)?;
    }
    txn.commit()?;
    Ok(())
}

/// Convert a local filesystem path to its relative key (relative to
/// `local_root`, with `/` separators).
pub fn key_for_local(local_root: &Path, local_path: &Path) -> crate::error::Result<String> {
    let rel = local_path
        .strip_prefix(local_root)
        .map_err(|_| crate::Error::other("path outside mirror root"))?;
    let mut out = String::new();
    for (i, comp) in rel.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(comp.as_os_str().to_str().ok_or(crate::Error::NonUtf8Path)?);
    }
    Ok(out)
}

/// Convert a relative key to a local filesystem path under `local_root`.
pub fn local_for_key(local_root: &Path, key: &str) -> PathBuf {
    let mut p = local_root.to_path_buf();
    for part in key.split('/') {
        p.push(part);
    }
    p
}

/// Current mtime of a file as milliseconds since epoch.
pub fn mtime_millis(path: &Path) -> Result<u64> {
    let meta = std::fs::metadata(path)?;
    let t = meta.modified()?;
    Ok(t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0))
}

// Extraction cache ---------------------------------------------------------

/// Look up extracted text by blake3 hash. `Ok(None)` = cache miss.
pub fn extract_get(db: &Database, hash: &[u8; 32]) -> Result<Option<String>> {
    let key = crate::hash::to_hex(hash);
    let read = db.begin_read()?;
    let table = read.open_table(EXTRACT_TABLE)?;
    Ok(table.get(key.as_str())?.map(|v| v.value().to_string()))
}

/// Some files have no extractable text (images, scans we didn't OCR yet). We
/// record an empty string so the sweep doesn't keep retrying — `extract_get`
/// then surfaces `Some("")`. [`extract_has`] tells the two apart.
pub fn extract_put(db: &Database, hash: &[u8; 32], text: &str) -> Result<()> {
    let key = crate::hash::to_hex(hash);
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(EXTRACT_TABLE)?;
        table.insert(key.as_str(), text)?;
    }
    txn.commit()?;
    Ok(())
}

/// True if a hash is in the cache at all (hit or "no text" sentinel).
pub fn extract_has(db: &Database, hash: &[u8; 32]) -> Result<bool> {
    let key = crate::hash::to_hex(hash);
    let read = db.begin_read()?;
    let table = read.open_table(EXTRACT_TABLE)?;
    Ok(table.get(key.as_str())?.is_some())
}

// Lazy-download stubs ------------------------------------------------------

/// Read all stubs into a map keyed by relative key.
pub fn stub_list(db: &Database) -> Result<HashMap<String, StubEntry>> {
    let read = db.begin_read()?;
    let table = read.open_table(STUB_TABLE)?;
    let mut out = HashMap::new();
    for row in table.iter()? {
        let (k, v) = row?;
        let key = k.value().to_string();
        let entry: StubEntry = serde_json::from_str(v.value())
            .map_err(|e| crate::Error::other(format!("stub decode {key}: {e}")))?;
        out.insert(key, entry);
    }
    Ok(out)
}

/// Record or replace a stub.
pub fn stub_put(db: &Database, key: &str, entry: &StubEntry) -> Result<()> {
    let json = serde_json::to_string(entry)
        .map_err(|e| crate::Error::other(format!("stub encode: {e}")))?;
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(STUB_TABLE)?;
        table.insert(key, json.as_str())?;
    }
    txn.commit()?;
    Ok(())
}

/// Remove a stub (called after materialize promotes it to the main journal).
pub fn stub_remove(db: &Database, key: &str) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(STUB_TABLE)?;
        table.remove(key)?;
    }
    txn.commit()?;
    Ok(())
}

// Tantivy index manifest ---------------------------------------------------

/// Read all indexed keys → blake3-hex. Used by the sweep to diff against the
/// journal and find orphans (deletes) and renames (new key, same hash).
pub fn indexed_list(db: &Database) -> Result<HashMap<String, [u8; 32]>> {
    let read = db.begin_read()?;
    let table = read.open_table(INDEXED_TABLE)?;
    let mut out = HashMap::new();
    for row in table.iter()? {
        let (k, v) = row?;
        let key = k.value().to_string();
        let hash = crate::hash::from_hex(v.value())
            .ok_or_else(|| crate::Error::other(format!("indexed decode {key}: bad hash")))?;
        out.insert(key, hash);
    }
    Ok(out)
}

/// Record that `key` is in the Tantivy index with content `hash`.
pub fn indexed_put(db: &Database, key: &str, hash: &[u8; 32]) -> Result<()> {
    let hex = crate::hash::to_hex(hash);
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(INDEXED_TABLE)?;
        table.insert(key, hex.as_str())?;
    }
    txn.commit()?;
    Ok(())
}

/// Remove an indexed-key record (after the Tantivy doc was deleted).
pub fn indexed_remove(db: &Database, key: &str) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(INDEXED_TABLE)?;
        table.remove(key)?;
    }
    txn.commit()?;
    Ok(())
}

// Object-Lock-aware keys ---------------------------------------------------

/// Read all locked keys into a set. Reconcile skips these so we don't retry
/// forever on objects under S3 Object Lock retention.
pub fn lock_list(db: &Database) -> Result<HashSet<String>> {
    let read = db.begin_read()?;
    let table = read.open_table(LOCK_TABLE)?;
    let mut out = HashSet::new();
    for row in table.iter()? {
        let (k, _) = row?;
        out.insert(k.value().to_string());
    }
    Ok(out)
}

/// Mark a key as locked (e.g. S3 returned PermissionDenied on overwrite/delete).
pub fn lock_add(db: &Database, key: &str) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(LOCK_TABLE)?;
        table.insert(key, "")?;
    }
    txn.commit()?;
    Ok(())
}

/// Remove a key from the locked set (e.g. user explicitly retries).
pub fn lock_clear(db: &Database, key: &str) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(LOCK_TABLE)?;
        table.remove(key)?;
    }
    txn.commit()?;
    Ok(())
}

// Sync metadata (incremental listing state) --------------------------------

/// Incremental-listing metadata key (singleton row in META_TABLE).
const META_KEY: &str = "sync";

/// Incremental-listing state persisted between syncs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncMeta {
    /// The last key seen in the most recent remote listing. Used as the
    /// `start-after` offset for S3's incremental listing.
    pub last_remote_key: Option<String>,
    /// Monotonic sync counter. Used to schedule periodic full listings
    /// (every `full_list_interval` syncs) so deletes are caught.
    pub sync_count: u64,
}

/// Read the sync metadata, or `Default` if not yet set.
pub fn meta_get(db: &Database) -> Result<SyncMeta> {
    let read = db.begin_read()?;
    let table = read.open_table(META_TABLE)?;
    if let Some(v) = table.get(META_KEY)? {
        let meta: SyncMeta = serde_json::from_str(v.value())
            .map_err(|e| crate::Error::other(format!("meta decode: {e}")))?;
        return Ok(meta);
    }
    Ok(SyncMeta::default())
}

/// Write the sync metadata.
pub fn meta_put(db: &Database, meta: &SyncMeta) -> Result<()> {
    let json = serde_json::to_string(meta)
        .map_err(|e| crate::Error::other(format!("meta encode: {e}")))?;
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(META_TABLE)?;
        table.insert(META_KEY, json.as_str())?;
    }
    txn.commit()?;
    Ok(())
}

// Version history ----------------------------------------------------------

/// A version observed for a key during a remote listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionEntry {
    pub version_id: String,
    /// When this version was first observed (epoch millis).
    pub seen_millis: u64,
    /// Object size at that version.
    pub size: u64,
}

const MAX_VERSIONS_PER_KEY: usize = 20;

/// Record that a version was seen for `key`. Maintains a bounded history
/// (most recent `MAX_VERSIONS_PER_KEY` entries). No-op if the version is
/// already recorded.
pub fn version_add(db: &Database, key: &str, entry: &VersionEntry) -> Result<()> {
    let mut history = version_list(db, key)?.unwrap_or_default();
    if history.iter().any(|v| v.version_id == entry.version_id) {
        return Ok(());
    }
    history.push(entry.clone());
    // Keep only the most recent entries (trim oldest).
    if history.len() > MAX_VERSIONS_PER_KEY {
        let start = history.len() - MAX_VERSIONS_PER_KEY;
        history.drain(0..start);
    }
    let json = serde_json::to_string(&history)
        .map_err(|e| crate::Error::other(format!("version encode: {e}")))?;
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(VERSION_TABLE)?;
        table.insert(key, json.as_str())?;
    }
    txn.commit()?;
    Ok(())
}

/// Read the version history for `key`. Returns `None` if no history recorded.
pub fn version_list(db: &Database, key: &str) -> Result<Option<Vec<VersionEntry>>> {
    let read = db.begin_read()?;
    let table = read.open_table(VERSION_TABLE)?;
    if let Some(v) = table.get(key)? {
        let history: Vec<VersionEntry> = serde_json::from_str(v.value())
            .map_err(|e| crate::Error::other(format!("version decode {key}: {e}")))?;
        return Ok(Some(history));
    }
    Ok(None)
}

/// Pinned versions ---------------------------------------------------------

/// Pin a specific version for `key`. Subsequent downloads/materialize fetch
/// this version instead of HEAD.
pub fn pin_set(db: &Database, key: &str, version_id: &str) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(PIN_TABLE)?;
        table.insert(key, version_id)?;
    }
    txn.commit()?;
    Ok(())
}

/// Get the pinned version for `key`, if any.
pub fn pin_get(db: &Database, key: &str) -> Result<Option<String>> {
    let read = db.begin_read()?;
    let table = read.open_table(PIN_TABLE)?;
    Ok(table.get(key)?.map(|v| v.value().to_string()))
}

/// Remove the pin for `key`.
pub fn pin_clear(db: &Database, key: &str) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(PIN_TABLE)?;
        table.remove(key)?;
    }
    txn.commit()?;
    Ok(())
}

/// Clear the entire extraction cache. Used by [`Indexer::rebuild`] to force
/// re-extraction on the next sweep.
pub fn extract_clear(db: &Database) -> Result<()> {
    // ponytail: redb 4.x has no Table::clear; read keys then drop each. Cheap
    // enough for the rare rebuild path. Swap for drain_filter if redb grows it.
    let keys: Vec<String> = {
        let read = db.begin_read()?;
        let table = read.open_table(EXTRACT_TABLE)?;
        table
            .iter()?
            .filter_map(|r| r.ok())
            .map(|(k, _)| k.value().to_string())
            .collect()
    };
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(EXTRACT_TABLE)?;
        for k in &keys {
            table.remove(k.as_str())?;
        }
    }
    txn.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        let e = JournalEntry {
            blake3: [1; 32],
            size: 42,
            mtime_millis: 999,
            remote_version: Some("v1".into()),
        };
        upsert(&db, "a/b.txt", &e).unwrap();
        let snap = snapshot(&db).unwrap();
        assert_eq!(snap.get("a/b.txt"), Some(&e));
        remove(&db, "a/b.txt").unwrap();
        let snap = snapshot(&db).unwrap();
        assert!(snap.is_empty());
    }

    #[test]
    fn key_path_roundtrip() {
        let root = Path::new("/tmp/root");
        let lp = root.join("sub").join("a.txt");
        let key = key_for_local(root, &lp).unwrap();
        assert_eq!(key, "sub/a.txt");
        assert_eq!(local_for_key(root, &key), lp);
    }

    #[test]
    fn extract_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        let h = [7u8; 32];
        assert!(extract_get(&db, &h).unwrap().is_none());
        assert!(!extract_has(&db, &h).unwrap());
        extract_put(&db, &h, "hello").unwrap();
        assert!(extract_has(&db, &h).unwrap());
        assert_eq!(extract_get(&db, &h).unwrap().as_deref(), Some("hello"));
    }

    #[test]
    fn stub_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        assert!(stub_list(&db).unwrap().is_empty());
        let e = StubEntry {
            size: 99,
            version: Some("v3".into()),
        };
        stub_put(&db, "remote/file.pdf", &e).unwrap();
        let map = stub_list(&db).unwrap();
        assert_eq!(map.get("remote/file.pdf"), Some(&e));
        stub_remove(&db, "remote/file.pdf").unwrap();
        assert!(stub_list(&db).unwrap().is_empty());
    }

    #[test]
    fn indexed_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        let h = [42u8; 32];
        indexed_put(&db, "a.txt", &h).unwrap();
        let map = indexed_list(&db).unwrap();
        assert_eq!(map.get("a.txt"), Some(&h));
        indexed_remove(&db, "a.txt").unwrap();
        assert!(indexed_list(&db).unwrap().is_empty());
    }

    #[test]
    fn lock_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        assert!(lock_list(&db).unwrap().is_empty());
        lock_add(&db, "locked-file.pdf").unwrap();
        lock_add(&db, "other.docx").unwrap();
        let set = lock_list(&db).unwrap();
        assert!(set.contains("locked-file.pdf"));
        assert!(set.contains("other.docx"));
        lock_clear(&db, "locked-file.pdf").unwrap();
        let set = lock_list(&db).unwrap();
        assert!(!set.contains("locked-file.pdf"));
        assert!(set.contains("other.docx"));
    }

    #[test]
    fn meta_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        // Default when empty.
        let m = meta_get(&db).unwrap();
        assert_eq!(m.sync_count, 0);
        assert!(m.last_remote_key.is_none());
        // Round-trip.
        let m = SyncMeta {
            last_remote_key: Some("zzzz.txt".into()),
            sync_count: 42,
        };
        meta_put(&db, &m).unwrap();
        let loaded = meta_get(&db).unwrap();
        assert_eq!(loaded.sync_count, 42);
        assert_eq!(loaded.last_remote_key.as_deref(), Some("zzzz.txt"));
    }

    #[test]
    fn version_history_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        assert!(version_list(&db, "a.txt").unwrap().is_none());

        let v1 = VersionEntry {
            version_id: "v1".into(),
            seen_millis: 100,
            size: 42,
        };
        version_add(&db, "a.txt", &v1).unwrap();
        // Adding the same version is a no-op.
        version_add(&db, "a.txt", &v1).unwrap();

        let v2 = VersionEntry {
            version_id: "v2".into(),
            seen_millis: 200,
            size: 50,
        };
        version_add(&db, "a.txt", &v2).unwrap();

        let history = version_list(&db, "a.txt").unwrap().unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().any(|v| v.version_id == "v1"));
        assert!(history.iter().any(|v| v.version_id == "v2"));
    }

    #[test]
    fn version_history_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        for i in 0..(MAX_VERSIONS_PER_KEY + 5) {
            version_add(
                &db,
                "big.txt",
                &VersionEntry {
                    version_id: format!("v{i}"),
                    seen_millis: i as u64,
                    size: i as u64,
                },
            )
            .unwrap();
        }
        let history = version_list(&db, "big.txt").unwrap().unwrap();
        assert_eq!(history.len(), MAX_VERSIONS_PER_KEY);
        // Oldest entries were trimmed.
        assert!(!history.iter().any(|v| v.version_id == "v0"));
        assert!(history.iter().any(|v| v.version_id == "v24"));
    }

    #[test]
    fn pin_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(&dir.path().join("j.redb")).unwrap();
        assert!(pin_get(&db, "a.txt").unwrap().is_none());
        pin_set(&db, "a.txt", "ver-123").unwrap();
        assert_eq!(pin_get(&db, "a.txt").unwrap().as_deref(), Some("ver-123"));
        pin_clear(&db, "a.txt").unwrap();
        assert!(pin_get(&db, "a.txt").unwrap().is_none());
    }
}
