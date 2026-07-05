//! The mirror — applies reconcile [`Action`](crate::reconcile::Action)s against
//! the local filesystem and the object store, and keeps the journal in sync.
//!
//! All S3 I/O in the engine happens here (and in [`crate::watch`]). The local
//! mirror is a real folder on disk, so browsing/opening never touches S3
//! (spec §2 invariant: "Fast = never touch S3 on the interactive path").

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use object_store::{ObjectStore, ObjectStoreExt, PutPayload, WriteMultipart};
use redb::Database;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::hash::{hash_file, to_hex};
use crate::journal::{self, key_for_local, local_for_key, mtime_millis, JournalEntry};
use crate::reconcile::{plan, Action, Inputs, Local, Remote};
use crate::store::HASH_ATTR;

/// Per-sync aggregate counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    pub uploaded: usize,
    pub downloaded: usize,
    pub conflicts: usize,
    pub deleted_remote: usize,
    pub deleted_local: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Progress events emitted during a sync (forwarded to clients by the engine).
#[derive(Debug, Clone)]
pub enum SyncEvent {
    Started,
    Uploaded(String),
    Downloaded(String),
    Conflict(String),
    DeletedRemote(String),
    DeletedLocal(String),
    Error(String, String),
    Finished(SyncOutcome),
}

/// Size threshold above which uploads use multipart.
const MULTIPLE_PART_THRESHOLD: u64 = 8 * 1024 * 1024;

/// The mirror: owns the config, the object store, and the journal handle.
pub struct Mirror {
    cfg: Config,
    store: Arc<dyn ObjectStore>,
    db: Arc<Database>,
}

impl Mirror {
    pub fn new(cfg: Config, store: Arc<dyn ObjectStore>, db: Arc<Database>) -> Self {
        Mirror { cfg, store, db }
    }

    pub fn cfg(&self) -> &Config {
        &self.cfg
    }

    /// Test/utility accessor for the journal database.
    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// Test/utility accessor for the object store.
    pub fn store_handle(&self) -> Arc<dyn ObjectStore> {
        self.store.clone()
    }

    /// Run one full reconcile pass: gather local + remote + journal, plan, apply.
    pub async fn sync_once(&self, tx: &mpsc::Sender<SyncEvent>) -> Result<SyncOutcome> {
        let _ = tx.try_send(SyncEvent::Started);
        let journal_map = journal::snapshot(&self.db)?;
        let stub_map = journal::stub_list(&self.db)?;

        let local = self.gather_local()?;
        let remote = self.gather_remote().await?;

        let actions = plan(Inputs {
            journal: &journal_map,
            local: &local,
            remote: &remote,
            stub: &stub_map,
            lazy: self.cfg.lazy,
        });

        let mut out = SyncOutcome::default();
        for (key, action) in actions {
            if let Err(e) = self
                .apply(&key, &action, &journal_map, &local, &remote, tx)
                .await
            {
                warn!(key = %key, error = %e, "sync action failed");
                out.errors += 1;
                let _ = tx.try_send(SyncEvent::Error(key.clone(), e.to_string()));
            } else {
                self.tally(&action, &mut out);
            }
        }

        info!(?out, "sync finished");
        let _ = tx.try_send(SyncEvent::Finished(out.clone()));
        Ok(out)
    }

    fn tally(&self, action: &Action, out: &mut SyncOutcome) {
        tally_action(action, out);
    }

    /// Walk the mirror root and build the `key → Local` map.
    fn gather_local(&self) -> Result<HashMap<String, Local>> {
        let mut map = HashMap::new();
        let root = &self.cfg.local_root;
        if !root.exists() {
            std::fs::create_dir_all(root)?;
            return Ok(map);
        }
        for entry in walkdir::WalkDir::new(root).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let meta = entry.metadata()?;
            let key = match key_for_local(root, path) {
                Ok(k) => k,
                Err(_) => continue,
            };
            let mtime = meta
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            map.insert(
                key,
                Local {
                    size: meta.len(),
                    mtime_millis: mtime,
                },
            );
        }
        Ok(map)
    }

    /// List the (prefixed) bucket and build the `key → Remote` map.
    ///
    /// Uses only the cheap listing signal — no `get`. Content hashes are read
    /// lazily later, only for keys that actually changed (spec §5).
    async fn gather_remote(&self) -> Result<HashMap<String, Remote>> {
        let mut map = HashMap::new();
        let mut stream = self.store.list(None);
        while let Some(meta) = futures::StreamExt::next(&mut stream).await {
            let meta = meta?;
            let key = meta.location.to_string();
            map.insert(
                key,
                Remote {
                    size: meta.size,
                    version: meta.version.clone().or(meta.e_tag.clone()),
                },
            );
        }
        Ok(map)
    }

    /// Apply a single planned action.
    async fn apply(
        &self,
        key: &str,
        action: &Action,
        journal_map: &HashMap<String, JournalEntry>,
        _local: &HashMap<String, Local>,
        remote: &HashMap<String, Remote>,
        tx: &mpsc::Sender<SyncEvent>,
    ) -> Result<()> {
        match action {
            Action::Skip | Action::DropJournal => {
                if matches!(action, Action::DropJournal) {
                    journal::remove(&self.db, key)?;
                }
                Ok(())
            }

            Action::Upload => {
                let local_path = local_for_key(&self.cfg.local_root, key);
                let hash = hash_file(&local_path)?;
                let baseline = journal_map.get(key);
                // mtime-only change (touch): content identical → just fix the journal.
                if let Some(j) = baseline {
                    if j.blake3 == hash {
                        let updated = JournalEntry {
                            blake3: hash,
                            size: std::fs::metadata(&local_path)?.len(),
                            mtime_millis: mtime_millis(&local_path)?,
                            remote_version: j.remote_version.clone(),
                        };
                        journal::upsert(&self.db, key, &updated)?;
                        debug!(key = %key, "upload skipped: unchanged content");
                        return Ok(());
                    }
                }
                let version = self.do_upload(key, &local_path, &hash).await?;
                self.record(key, &hash, &local_path, version)?;
                let _ = tx.try_send(SyncEvent::Uploaded(key.to_string()));
                Ok(())
            }

            Action::UploadNew => {
                let local_path = local_for_key(&self.cfg.local_root, key);
                let hash = hash_file(&local_path)?;
                let version = self.do_upload(key, &local_path, &hash).await?;
                self.record(key, &hash, &local_path, version)?;
                let _ = tx.try_send(SyncEvent::Uploaded(key.to_string()));
                Ok(())
            }

            Action::Download => {
                self.do_download(key, None).await?;
                let _ = tx.try_send(SyncEvent::Downloaded(key.to_string()));
                Ok(())
            }

            Action::DownloadNew => {
                self.do_download(key, None).await?;
                let _ = tx.try_send(SyncEvent::Downloaded(key.to_string()));
                Ok(())
            }

            Action::DeleteRemote => {
                let path = self.parse_path(key);
                self.store.delete(&path).await?;
                journal::remove(&self.db, key)?;
                let _ = tx.try_send(SyncEvent::DeletedRemote(key.to_string()));
                Ok(())
            }

            Action::DeleteLocal => {
                let local_path = local_for_key(&self.cfg.local_root, key);
                if local_path.exists() {
                    std::fs::remove_file(&local_path)?;
                }
                journal::remove(&self.db, key)?;
                let _ = tx.try_send(SyncEvent::DeletedLocal(key.to_string()));
                Ok(())
            }

            Action::Conflict => {
                self.do_conflict(key).await?;
                let _ = tx.try_send(SyncEvent::Conflict(key.to_string()));
                Ok(())
            }

            Action::Compare => {
                // No baseline, both sides present: compare content hashes to decide.
                let local_path = local_for_key(&self.cfg.local_root, key);
                let lhash = if local_path.exists() {
                    hash_file(&local_path)?
                } else {
                    return Err(Error::other("compare: local gone"));
                };
                let (bytes, rhash, version) = self.fetch_with_hash(key).await?;
                match rhash {
                    Some(rh) if rh == lhash => {
                        // Identical content — just record the journal entry.
                        self.write_file(&local_path, &bytes).await?;
                        let entry = JournalEntry {
                            blake3: lhash,
                            size: bytes.len() as u64,
                            mtime_millis: mtime_millis(&local_path)?,
                            remote_version: version,
                        };
                        journal::upsert(&self.db, key, &entry)?;
                        Ok(())
                    }
                    _ => {
                        // Differ (or remote has no hash attr) → keep both.
                        self.do_conflict(key).await?;
                        let _ = tx.try_send(SyncEvent::Conflict(key.to_string()));
                        Ok(())
                    }
                }
            }

            Action::RecordStub => {
                // Lazy mode: remote-only object → record metadata, no download.
                let r = remote.get(key).ok_or_else(|| {
                    Error::other("RecordStub: remote entry vanished before apply")
                })?;
                let stub = journal::StubEntry {
                    size: r.size,
                    version: r.version.clone(),
                };
                journal::stub_put(&self.db, key, &stub)?;
                debug!(key = %key, "recorded stub (lazy)");
                Ok(())
            }

            Action::Dematerialize => {
                // Lazy mode: materialized file deleted locally → move to stub,
                // keep the remote object. Never auto-delete remote in lazy mode.
                let j = journal_map.get(key).ok_or_else(|| {
                    Error::other("Dematerialize: journal entry vanished before apply")
                })?;
                let stub = journal::StubEntry {
                    size: j.size,
                    version: j.remote_version.clone(),
                };
                journal::stub_put(&self.db, key, &stub)?;
                journal::remove(&self.db, key)?;
                debug!(key = %key, "dematerialized (lazy local delete)");
                Ok(())
            }
        }
    }

    /// Upload `local_path` to `key` with the blake3 hash in metadata.
    /// Returns the new remote version id (or ETag).
    async fn do_upload(
        &self,
        key: &str,
        local_path: &Path,
        hash: &[u8; 32],
    ) -> Result<Option<String>> {
        let path = self.parse_path(key);
        let size = std::fs::metadata(local_path)?.len();

        let mut attrs = object_store::Attributes::new();
        attrs.insert(
            object_store::Attribute::Metadata(std::borrow::Cow::Borrowed(HASH_ATTR)),
            object_store::AttributeValue::from(to_hex(hash)),
        );

        if size > MULTIPLE_PART_THRESHOLD {
            let opts = object_store::PutMultipartOptions {
                attributes: attrs,
                ..Default::default()
            };
            let upload = self.store.put_multipart_opts(&path, opts).await?;
            let mut writer = WriteMultipart::new(upload);
            let mut f = tokio::fs::File::open(local_path).await?;
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = f.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                writer.write(&buf[..n]);
            }
            writer.finish().await?;
            // No version id returned from multipart; re-head for it.
            let meta = self.store.head(&path).await?;
            Ok(meta.version.or(meta.e_tag))
        } else {
            let bytes = tokio::fs::read(local_path).await?;
            let opts: object_store::PutOptions = attrs.into();
            let res = self
                .store
                .put_opts(
                    &path,
                    PutPayload::from_bytes(bytes::Bytes::from(bytes)),
                    opts,
                )
                .await?;
            Ok(res.version.or(res.e_tag))
        }
    }

    /// Download `key` to its local path, optionally forcing a fresh mtime.
    async fn do_download(&self, key: &str, _force_mtime: Option<u64>) -> Result<()> {
        let local_path = local_for_key(&self.cfg.local_root, key);
        let (bytes, _hash, _version) = self.fetch_with_hash(key).await?;
        self.write_file(&local_path, &bytes).await?;
        let hash = crate::hash::hash_bytes(&bytes);
        let entry = JournalEntry {
            blake3: hash,
            size: bytes.len() as u64,
            mtime_millis: mtime_millis(&local_path)?,
            remote_version: _version,
        };
        journal::upsert(&self.db, key, &entry)?;
        Ok(())
    }

    /// Fetch `key` plus its content-hash attribute. Returns (bytes, attr_hash, version).
    async fn fetch_with_hash(
        &self,
        key: &str,
    ) -> Result<(bytes::Bytes, Option<[u8; 32]>, Option<String>)> {
        let path = self.parse_path(key);
        let result = self
            .store
            .get_opts(&path, object_store::GetOptions::default())
            .await?;
        let version = result.meta.version.clone().or(result.meta.e_tag.clone());
        let attr_hash = result
            .attributes
            .get(&object_store::Attribute::Metadata(
                std::borrow::Cow::Borrowed(HASH_ATTR),
            ))
            .and_then(|v| crate::hash::from_hex(v));
        let bytes = result.bytes().await?;
        Ok((bytes, attr_hash, version))
    }

    /// Keep-both conflict resolution (spec §5: never lose data).
    ///
    /// The remote object becomes canonical at `key`; the local file is preserved
    /// as `name (conflict <ts>).ext` and uploaded as a new key. Bucket versioning
    /// is the backstop. Both sides end up with both bytes.
    async fn do_conflict(&self, key: &str) -> Result<()> {
        let local_path = local_for_key(&self.cfg.local_root, key);

        // 1. Save the local version under a conflict name + upload it.
        if local_path.exists() {
            let conflict_key = conflict_key(key);
            let conflict_path = local_for_key(&self.cfg.local_root, &conflict_key);
            if let Some(parent) = conflict_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::rename(&local_path, &conflict_path)
                .await
                .or_else(|_| {
                    // rename across dirs can fail; fall back to copy+remove
                    std::fs::copy(&local_path, &conflict_path).map(|_| ())?;
                    std::fs::remove_file(&local_path).map_err(crate::Error::from)
                })?;
            let chash = hash_file(&conflict_path)?;
            let version = self
                .do_upload(&conflict_key, &conflict_path, &chash)
                .await?;
            self.record(&conflict_key, &chash, &conflict_path, version)?;
        }

        // 2. Download the remote (now canonical) to the original local name + record.
        self.do_download(key, None).await?;
        Ok(())
    }

    /// Record a journal entry for `key` after a successful upload.
    fn record(
        &self,
        key: &str,
        hash: &[u8; 32],
        local_path: &Path,
        version: Option<String>,
    ) -> Result<()> {
        let entry = JournalEntry {
            blake3: *hash,
            size: std::fs::metadata(local_path)?.len(),
            mtime_millis: mtime_millis(local_path)?,
            remote_version: version,
        };
        journal::upsert(&self.db, key, &entry)?;
        Ok(())
    }

    /// Write `bytes` to `path`, creating parent dirs, and clamp mtime so the
    /// next scan sees the file as in-sync.
    async fn write_file(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, bytes).await?;
        Ok(())
    }

    fn parse_path(&self, key: &str) -> object_store::path::Path {
        object_store::path::Path::parse(key).unwrap_or_else(|_| object_store::path::Path::default())
    }

    /// Resolve a tracked conflict after the user picks a winner.
    ///
    /// After [`do_conflict`](Self::do_conflict), the canonical `key` holds the
    /// remote version and a sibling `name (conflict …)` holds the local version
    /// (both on disk and in the bucket). This removes the losing copy from disk,
    /// the bucket, and the journal.
    pub async fn resolve_conflict(
        &self,
        key: &str,
        resolution: crate::protocol::Resolution,
    ) -> Result<()> {
        use crate::protocol::Resolution;
        let sibling = find_conflict_sibling(&self.db, key)?;

        match resolution {
            Resolution::KeepBoth => {
                // Both already coexist; nothing to delete.
                Ok(())
            }
            Resolution::KeepLocal => {
                // Delete the remote/canonical copy; promote the sibling next sync.
                if let Some(sibling) = sibling {
                    // Move sibling file back onto the canonical local path so the
                    // next sync re-uploads it as the canonical key.
                    let sib_path = local_for_key(&self.cfg.local_root, &sibling);
                    let canon_path = local_for_key(&self.cfg.local_root, key);
                    if sib_path.exists() {
                        if let Some(parent) = canon_path.parent() {
                            tokio::fs::create_dir_all(parent).await?;
                        }
                        let _ = tokio::fs::remove_file(&canon_path).await;
                        tokio::fs::rename(&sib_path, &canon_path)
                            .await
                            .or_else(|_| {
                                std::fs::copy(&sib_path, &canon_path)?;
                                std::fs::remove_file(&sib_path)?;
                                Ok::<_, crate::Error>(())
                            })?;
                    }
                    // Drop both journal rows so reconcile re-evaluates cleanly.
                    journal::remove(&self.db, key)?;
                    journal::remove(&self.db, &sibling)?;
                    let path = self.parse_path(&sibling);
                    let _ = self.store.delete(&path).await;
                }
                Ok(())
            }
            Resolution::KeepRemote => {
                // Canonical already = remote. Delete the sibling everywhere.
                if let Some(sibling) = sibling {
                    let sib_path = local_for_key(&self.cfg.local_root, &sibling);
                    if sib_path.exists() {
                        let _ = tokio::fs::remove_file(&sib_path).await;
                    }
                    journal::remove(&self.db, &sibling)?;
                    let path = self.parse_path(&sibling);
                    let _ = self.store.delete(&path).await;
                }
                Ok(())
            }
        }
    }

    /// Materialize a lazy-download stub: fetch the bytes, write the local file,
    /// promote the stub to the main journal. The next index sweep will pick it
    /// up for search. No-op (returns Ok) if the key isn't a stub.
    pub async fn materialize(&self, key: &str) -> Result<()> {
        let stub = match journal::stub_list(&self.db)?.remove(key) {
            Some(s) => s,
            None => return Ok(()),
        };
        // Fetch + hash + write — same path as do_download but we already have
        // the stub metadata to fall back on if the remote version changed.
        let (bytes, _rhash, version) = self.fetch_with_hash(key).await?;
        let local_path = local_for_key(&self.cfg.local_root, key);
        self.write_file(&local_path, &bytes).await?;
        let hash = crate::hash::hash_bytes(&bytes);
        let entry = JournalEntry {
            blake3: hash,
            size: bytes.len() as u64,
            mtime_millis: mtime_millis(&local_path)?,
            remote_version: version.or(stub.version),
        };
        journal::upsert(&self.db, key, &entry)?;
        journal::stub_remove(&self.db, key)?;
        info!(key = %key, "materialized stub");
        Ok(())
    }
}

/// Find the sibling conflict-copy key for `key` in the journal (the saved
/// local version from a prior `do_conflict`). Matches files whose filename
/// starts with `<stem> (conflict ` under the same directory.
fn find_conflict_sibling(db: &Database, key: &str) -> Result<Option<String>> {
    let snap = journal::snapshot(db)?;
    let p = Path::new(key);
    let parent = p.parent();
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let needle = format!("{stem} (conflict ");
    for k in snap.keys() {
        let kp = Path::new(k);
        if kp.parent() != parent {
            continue;
        }
        if let Some(fname) = kp.file_name().and_then(|s| s.to_str()) {
            if fname.starts_with(&needle) {
                return Ok(Some(k.clone()));
            }
        }
    }
    Ok(None)
}

/// Build the conflict sibling name for `key`: keeps the directory and rewrites
/// the filename to `stem (conflict YYYYmmdd-HHMMSS).ext`.
fn conflict_key(key: &str) -> String {
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let p = Path::new(key);
    let parent = p.parent().map(|x| x.to_path_buf()).unwrap_or_default();
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = p.extension().and_then(|s| s.to_str());
    let name = match ext {
        Some(e) => format!("{stem} (conflict {ts}).{e}"),
        None => format!("{stem} (conflict {ts})"),
    };
    if parent.as_os_str().is_empty() {
        name
    } else {
        let mut s: PathBuf = parent;
        s.push(name);
        s.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_key_keeps_dir_and_ext() {
        let k = conflict_key("docs/report.pdf");
        let p = Path::new(&k);
        assert_eq!(p.parent().unwrap(), Path::new("docs"));
        assert!(p
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("report (conflict "));
        assert_eq!(p.extension().unwrap(), "pdf");
    }

    #[test]
    fn conflict_key_no_ext() {
        let k = conflict_key("README");
        assert!(k.starts_with("README (conflict "));
    }

    #[test]
    fn outcome_tally() {
        let mut out = SyncOutcome::default();
        for a in [
            Action::Skip,
            Action::Upload,
            Action::Download,
            Action::Conflict,
            Action::DeleteRemote,
            Action::DeleteLocal,
        ] {
            tally_action(&a, &mut out);
        }
        assert_eq!(out.skipped, 1);
        assert_eq!(out.uploaded, 1);
        assert_eq!(out.downloaded, 1);
        assert_eq!(out.conflicts, 1);
        assert_eq!(out.deleted_remote, 1);
        assert_eq!(out.deleted_local, 1);
    }
}

/// Free-function form of the per-action counter (also used by [`Mirror::tally`]).
fn tally_action(action: &Action, out: &mut SyncOutcome) {
    match action {
        Action::Skip => out.skipped += 1,
        Action::Upload | Action::UploadNew => out.uploaded += 1,
        Action::Download | Action::DownloadNew => out.downloaded += 1,
        Action::Conflict => out.conflicts += 1,
        Action::DeleteRemote => out.deleted_remote += 1,
        Action::DeleteLocal => out.deleted_local += 1,
        // Lazy-mode bookkeeping; not counted as sync transfer activity.
        Action::RecordStub | Action::Dematerialize => {}
        Action::DropJournal | Action::Compare => {}
    }
}
