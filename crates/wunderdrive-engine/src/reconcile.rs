//! The reconcile core (spec §5).
//!
//! [`plan`] is a **pure** function over three key→info maps — the journal
//! baseline, the current local filesystem, and the current remote listing —
//! and returns the list of [`Action`]s to take. No I/O, so it is trivially
//! unit-testable. The [`mirror`](crate::mirror) module applies the actions.
//!
//! "Dirty" detection uses the cheap signals (size + mtime locally, version +
//! size remotely); the expensive content hash is recomputed lazily by the
//! mirror only when we actually move bytes or need to disambiguate.

use std::collections::{BTreeMap, HashMap};

use crate::journal::{JournalEntry, StubEntry};

/// A local file's observed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub size: u64,
    pub mtime_millis: u64,
}

/// A remote object's observed state (from a cheap listing — no `get`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub size: u64,
    /// ETag / version id. `None` if the provider returned none.
    pub version: Option<String>,
}

/// What the engine should do for one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// In sync; nothing to do.
    Skip,
    /// Local differs from baseline → recompute hash, push (skip if unchanged).
    Upload,
    /// Remote differs from baseline → pull.
    Download,
    /// Changed on both sides → keep both via conflict-copy (never lose data).
    Conflict,
    /// Local was deleted since baseline → delete remote, drop journal.
    DeleteRemote,
    /// Remote was deleted since baseline → delete local, drop journal.
    DeleteLocal,
    /// New on both sides with no baseline → mirror computes hashes to decide:
    /// equal ⇒ record, differ ⇒ conflict-copy.
    Compare,
    /// New local-only → upload.
    UploadNew,
    /// New remote-only → download (non-lazy) or record stub (lazy).
    DownloadNew,
    /// Lazy mode: new remote-only → record stub, no I/O.
    RecordStub,
    /// Lazy mode: materialized file deleted locally → move back to stub,
    /// keep the remote object. Never auto-delete remote in lazy mode.
    Dematerialize,
    /// Present in journal but gone on both sides → drop the journal row.
    DropJournal,
}

impl Action {
    /// Whether this action needs the local content hash computed.
    pub fn needs_local_hash(&self) -> bool {
        matches!(self, Action::Upload | Action::Compare)
    }
    /// Whether this action needs the remote object fetched (content + attr).
    pub fn needs_remote_get(&self) -> bool {
        matches!(self, Action::Compare)
    }
}

/// Inputs to [`plan`], keyed by relative key.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    pub journal: &'a HashMap<String, JournalEntry>,
    pub local: &'a HashMap<String, Local>,
    pub remote: &'a HashMap<String, Remote>,
    /// Lazy-download stubs (remote-only metadata, no local bytes).
    pub stub: &'a HashMap<String, StubEntry>,
    /// Whether lazy download is enabled. When true, remote-only objects become
    /// stubs instead of being downloaded; local deletes dematerialize instead
    /// of propagating to the remote.
    pub lazy: bool,
}

/// Decide the action list for every key in the union of the four inputs.
///
/// Deterministic: iterates keys in sorted order and returns actions in that
/// order (handy for stable tests and logs).
pub fn plan(inputs: Inputs<'_>) -> Vec<(String, Action)> {
    let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for k in inputs.journal.keys() {
        keys.insert(k.clone());
    }
    for k in inputs.local.keys() {
        keys.insert(k.clone());
    }
    for k in inputs.remote.keys() {
        keys.insert(k.clone());
    }
    for k in inputs.stub.keys() {
        keys.insert(k.clone());
    }

    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let action = decide(
            inputs.journal.get(&key),
            inputs.local.get(&key),
            inputs.remote.get(&key),
            inputs.stub.get(&key),
            inputs.lazy,
        );
        out.push((key, action));
    }
    out
}

/// Decide the action for a single key. Exposed for unit testing.
///
/// `stub` is the prior stub entry for this key (if any); `lazy` selects the
/// lazy-download policy.
pub fn decide(
    j: Option<&JournalEntry>,
    l: Option<&Local>,
    r: Option<&Remote>,
    stub: Option<&StubEntry>,
    lazy: bool,
) -> Action {
    match (j, l, r) {
        // Baseline exists (materialized file) — reconcile against it.
        (Some(j), Some(l), Some(r)) => {
            let l_dirty = l.size != j.size || l.mtime_millis != j.mtime_millis;
            let r_dirty = r.version != j.remote_version || r.size != j.size;
            match (l_dirty, r_dirty) {
                (false, false) => Action::Skip,
                (true, false) => Action::Upload,
                (false, true) => Action::Download,
                (true, true) => Action::Conflict,
            }
        }
        // Materialized file deleted locally, still remote.
        (Some(_), None, Some(_)) => {
            if lazy {
                Action::Dematerialize
            } else {
                Action::DeleteRemote
            }
        }
        // Materialized file deleted locally, remote also gone → full delete.
        (Some(_), Some(_), None) => Action::DeleteLocal,
        // Baseline only, both gone → clean up the journal.
        (Some(_), None, None) => Action::DropJournal,

        // No baseline (first time we see this key on at least one side).
        (None, Some(_), Some(_)) => Action::Compare,
        (None, Some(_), None) => Action::UploadNew,
        (None, None, Some(_)) => {
            if lazy {
                // Remote-only, no local file, no baseline. If we already have a
                // stub for this key, skip; otherwise record one.
                if stub.is_some() {
                    Action::Skip
                } else {
                    Action::RecordStub
                }
            } else {
                Action::DownloadNew
            }
        }

        // (None, None, None) impossible — key came from one of the maps.
        (None, None, None) => Action::Skip,
    }
}

/// Helper to build a small `HashMap` inline (tests + mirror).
pub fn map<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> HashMap<K, V>
where
    K: std::hash::Hash + Eq,
{
    pairs.into_iter().collect()
}

/// Same but sorted (BTreeMap) — handy for deterministic assertions.
#[allow(dead_code)]
pub fn bmap<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> BTreeMap<K, V>
where
    K: Ord,
{
    pairs.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(blake3: u8, size: u64, mtime: u64, ver: Option<&str>) -> JournalEntry {
        JournalEntry {
            blake3: [blake3; 32],
            size,
            mtime_millis: mtime,
            remote_version: ver.map(str::to_string),
        }
    }
    fn l(size: u64, mtime: u64) -> Local {
        Local {
            size,
            mtime_millis: mtime,
        }
    }
    fn r(size: u64, ver: Option<&str>) -> Remote {
        Remote {
            size,
            version: ver.map(str::to_string),
        }
    }

    fn act(j: Option<JournalEntry>, l: Option<Local>, r: Option<Remote>) -> Action {
        decide(j.as_ref(), l.as_ref(), r.as_ref(), None, false)
    }

    fn act_lazy(j: Option<JournalEntry>, l: Option<Local>, r: Option<Remote>) -> Action {
        decide(j.as_ref(), l.as_ref(), r.as_ref(), None, true)
    }

    fn stub(size: u64, ver: Option<&str>) -> StubEntry {
        StubEntry {
            size,
            version: ver.map(str::to_string),
        }
    }

    #[test]
    fn all_in_sync_skip() {
        assert_eq!(
            act(
                Some(j(1, 10, 100, Some("v"))),
                Some(l(10, 100)),
                Some(r(10, Some("v")))
            ),
            Action::Skip
        );
    }

    #[test]
    fn local_only_changed_upload() {
        // size differs
        assert_eq!(
            act(
                Some(j(1, 10, 100, Some("v"))),
                Some(l(11, 100)),
                Some(r(10, Some("v")))
            ),
            Action::Upload
        );
        // mtime differs
        assert_eq!(
            act(
                Some(j(1, 10, 100, Some("v"))),
                Some(l(10, 200)),
                Some(r(10, Some("v")))
            ),
            Action::Upload
        );
    }

    #[test]
    fn remote_only_changed_download() {
        // version differs
        assert_eq!(
            act(
                Some(j(1, 10, 100, Some("v"))),
                Some(l(10, 100)),
                Some(r(10, Some("v2")))
            ),
            Action::Download
        );
        // size differs
        assert_eq!(
            act(
                Some(j(1, 10, 100, Some("v"))),
                Some(l(10, 100)),
                Some(r(11, Some("v")))
            ),
            Action::Download
        );
    }

    #[test]
    fn both_changed_conflict() {
        assert_eq!(
            act(
                Some(j(1, 10, 100, Some("v"))),
                Some(l(12, 200)),
                Some(r(11, Some("v2")))
            ),
            Action::Conflict
        );
    }

    #[test]
    fn local_deleted_delete_remote() {
        assert_eq!(
            act(Some(j(1, 10, 100, Some("v"))), None, Some(r(10, Some("v")))),
            Action::DeleteRemote
        );
    }

    #[test]
    fn remote_deleted_delete_local() {
        assert_eq!(
            act(Some(j(1, 10, 100, Some("v"))), Some(l(10, 100)), None),
            Action::DeleteLocal
        );
    }

    #[test]
    fn both_gone_drop_journal() {
        assert_eq!(
            act(Some(j(1, 10, 100, Some("v"))), None, None),
            Action::DropJournal
        );
    }

    #[test]
    fn first_run_both_sides_compare() {
        assert_eq!(
            act(None, Some(l(10, 100)), Some(r(10, Some("v")))),
            Action::Compare
        );
    }

    #[test]
    fn first_run_local_only_upload_new() {
        assert_eq!(act(None, Some(l(10, 100)), None), Action::UploadNew);
    }

    #[test]
    fn first_run_remote_only_download_new() {
        assert_eq!(act(None, None, Some(r(10, Some("v")))), Action::DownloadNew);
    }

    #[test]
    fn plan_covers_union_and_is_sorted() {
        let journal = map([("a".to_string(), j(1, 1, 1, Some("v")))]);
        let local = map([("c".to_string(), l(1, 1))]);
        let remote = map([("b".to_string(), r(1, Some("v")))]);
        let stubs = HashMap::new();
        let actions = plan(Inputs {
            journal: &journal,
            local: &local,
            remote: &remote,
            stub: &stubs,
            lazy: false,
        });
        let keys: Vec<_> = actions.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
        // a: j only (no l, no r) → DropJournal
        assert_eq!(actions[0].1, Action::DropJournal);
        // b: r only → DownloadNew (non-lazy)
        assert_eq!(actions[1].1, Action::DownloadNew);
        // c: l only → UploadNew
        assert_eq!(actions[2].1, Action::UploadNew);
    }

    #[test]
    fn version_none_never_dirty_against_none() {
        // Baseline with no remote version + remote with no version ⇒ not dirty.
        assert_eq!(
            act(
                Some(j(1, 10, 100, None)),
                Some(l(10, 100)),
                Some(r(10, None))
            ),
            Action::Skip
        );
    }

    // --- lazy mode ---

    #[test]
    fn lazy_remote_only_records_stub() {
        // Remote-only, no local, no baseline, lazy → RecordStub (not DownloadNew).
        assert_eq!(
            act_lazy(None, None, Some(r(10, Some("v")))),
            Action::RecordStub
        );
    }

    #[test]
    fn non_lazy_remote_only_downloads() {
        // Same input, non-lazy → DownloadNew (unchanged behaviour).
        assert_eq!(act(None, None, Some(r(10, Some("v")))), Action::DownloadNew);
    }

    #[test]
    fn lazy_remote_only_with_existing_stub_skips() {
        // Already stubbed → Skip (don't re-record).
        let s = stub(10, Some("v"));
        assert_eq!(
            decide(None, None, Some(&r(10, Some("v"))), Some(&s), true),
            Action::Skip
        );
    }

    #[test]
    fn lazy_local_delete_dematerializes() {
        // Materialized file deleted locally, remote intact, lazy → Dematerialize.
        assert_eq!(
            act_lazy(Some(j(1, 10, 100, Some("v"))), None, Some(r(10, Some("v")))),
            Action::Dematerialize
        );
    }

    #[test]
    fn non_lazy_local_delete_propagates_to_remote() {
        // Same input, non-lazy → DeleteRemote (the original behaviour).
        assert_eq!(
            act(Some(j(1, 10, 100, Some("v"))), None, Some(r(10, Some("v")))),
            Action::DeleteRemote
        );
    }
}
