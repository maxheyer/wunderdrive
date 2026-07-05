//! The local journal — a [`redb`] table keyed by relative key, holding the
//! last-synced snapshot per file. This snapshot is what enables real
//! three-way reconciliation instead of naive mirroring (spec §5).
//!
//! Key space: the "relative key" = the object's path relative to the local
//! mirror root, using `/` separators (and with the configured bucket prefix
//! already stripped by [`PrefixStore`](object_store::prefix::PrefixStore)).
//! `local_path` and `s3_key` are derived deterministically from this key.

use std::collections::HashMap;
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
    // Ensure both tables exist.
    let txn = db.begin_write()?;
    {
        let _ = txn.open_table(TABLE)?;
        let _ = txn.open_table(EXTRACT_TABLE)?;
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
}
