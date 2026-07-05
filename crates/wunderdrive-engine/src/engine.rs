//! The [`Engine`] — frontend-agnostic facade over the mirror. Owns the sync
//! loop and exposes the local API the daemon serves over IPC.
//!
//! Sync triggers: local watcher (debounced) · periodic remote poll · periodic
//! full local rescan (watchers drop events) · explicit `sync_now`. All S3 I/O
//! happens inside the loop; [`Engine::snapshot`] is a pure local-disk read so
//! browsing the UI never touches S3 (spec §2).

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use redb::Database;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex as TokioMutex};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep};
use tracing::{debug, error, info};

use crate::config::{default_index_path, default_journal_path, Config};
use crate::creds;
use crate::error::{Error, Result};
use crate::index::Indexer;
use crate::journal::{self, key_for_local};
use crate::mirror::{Mirror, SyncEvent};
use crate::store;
use crate::watch::LocalWatcher;

const ACTIVITY_CAP: usize = 2000;

/// Per-file status as seen by a (S3-free) local snapshot.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// In journal and matching local mtime/size.
    Synced,
    /// In journal but local differs → will upload on next sync.
    PendingUpload,
    /// Local-only, never uploaded.
    #[default]
    NewLocal,
    /// Was synced, now missing locally → delete pending.
    DeletedPending,
    /// Known conflict (both sides changed).
    Conflict,
}

/// One file in a [`Snapshot`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStat {
    pub key: String,
    pub size: u64,
    pub mtime_millis: u64,
    pub status: FileStatus,
}

/// A pure-local view of the mirror — never touches S3.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Snapshot {
    pub paused: bool,
    pub last_sync_millis: Option<u64>,
    pub files: Vec<FileStat>,
}

/// Daemon status summary.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Status {
    pub paused: bool,
    pub last_sync_millis: Option<u64>,
    pub endpoint: Option<String>,
    pub bucket: String,
    pub prefix: String,
    pub local_root: String,
}

/// One activity log line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub seq: u64,
    pub kind: String,
    pub key: String,
    pub ts_millis: u64,
}

#[derive(Debug)]
enum Cmd {
    SyncNow,
    Pause,
    Resume,
    Shutdown,
}

/// A nudge to the indexer that something changed. Coalesces; the indexer just
/// re-walks the journal and picks up cache misses.
#[derive(Debug)]
enum IndexCmd {
    Trigger,
    Shutdown,
}

/// Shared, lock-protected engine state (mutated by the loop, read by IPC).
#[derive(Default)]
struct State {
    paused: AtomicBool,
    last_sync_millis: Mutex<Option<u64>>,
    conflicts: Mutex<HashSet<String>>,
    activity: Mutex<VecDeque<ActivityEntry>>,
    seq: AtomicU64,
}

/// The engine handle. Drop to stop the sync loop.
pub struct Engine {
    state: Arc<State>,
    cmd_tx: mpsc::Sender<Cmd>,
    event_tx: broadcast::Sender<SyncEvent>,
    cfg: Config,
    db: Arc<Database>,
    mirror: Arc<Mirror>,
    indexer: Option<Arc<Indexer>>,
    _loop: TokioMutex<Option<JoinHandle<()>>>,
}

impl Engine {
    /// Load config from the default path, open creds/store/journal, spawn the
    /// sync loop, and return a handle.
    pub fn start() -> Result<Self> {
        Self::start_with(Config::load_default()?, default_journal_path())
    }
    /// Fully explicit constructor (used by tests + `--config`).
    pub fn start_with(cfg: Config, journal_path: PathBuf) -> Result<Self> {
        Self::start_with_creds(cfg, journal_path, None)
    }

    /// Like [`Engine::start_with`] but with explicit credentials overriding
    /// keychain/env resolution. `None` means "resolve from keychain then env".
    pub fn start_with_creds(
        cfg: Config,
        journal_path: PathBuf,
        creds_override: Option<creds::Credentials>,
    ) -> Result<Self> {
        let mut cfg = cfg;
        cfg.expand()?;
        std::fs::create_dir_all(&cfg.local_root)?;

        let creds = match creds_override {
            Some(c) => Some(c),
            None => creds::resolve(None, None, &cfg.bucket, cfg.endpoint.as_deref())?,
        }
        .ok_or_else(|| {
            Error::Config(
                "no S3 credentials found. Set one of: \
                 `wunderdrive-daemon --access-key-id ID --secret-access-key KEY`, \
                 the WUNDERDRIVE_ACCESS_KEY_ID/WUNDERDRIVE_SECRET_ACCESS_KEY env vars, \
                 or store them in the OS keychain."
                    .into(),
            )
        })?;
        let obj_store = store::build(&cfg, &creds)?;
        let db = Arc::new(journal::open(&journal_path)?);

        let (event_tx, _) = broadcast::channel::<SyncEvent>(512);
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(64);
        let state = Arc::new(State::default());

        let mirror = Arc::new(Mirror::new(cfg.clone(), obj_store, db.clone()));
        let (index_tx, index_rx) = mpsc::channel::<IndexCmd>(64);

        // Indexer is optional: a failure to open the Tantivy dir must not stop
        // sync. The daemon still serves files; search just returns [].
        let indexer = match Indexer::open(db.clone(), cfg.local_root.clone(), &default_index_path())
        {
            Ok(i) => Some(Arc::new(i)),
            Err(e) => {
                tracing::warn!(error = %e, "failed to open search index; search disabled");
                None
            }
        };
        if let Some(ix) = indexer.clone() {
            tokio::spawn(run_index_loop(ix, index_rx));
        }

        let loop_state = state.clone();
        let loop_event_tx = event_tx.clone();
        let loop_cfg = cfg.clone();

        let handle = tokio::spawn(run_loop(
            mirror.clone(),
            loop_cfg,
            cmd_rx,
            loop_event_tx,
            loop_state,
            index_tx.clone(),
        ));

        Ok(Engine {
            state,
            cmd_tx,
            event_tx,
            cfg,
            db,
            mirror,
            indexer,
            _loop: TokioMutex::new(Some(handle)),
        })
    }

    /// Subscribe to live sync events (broadcast).
    pub fn events(&self) -> broadcast::Receiver<SyncEvent> {
        self.event_tx.subscribe()
    }

    pub fn cfg(&self) -> &Config {
        &self.cfg
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        let journal_map = journal::snapshot(&self.db)?;
        let local = gather_local_stat(&self.cfg.local_root)?;
        let conflicts = self.state.conflicts.lock().unwrap().clone();
        let mut files = Vec::with_capacity(journal_map.len() + local.len());

        let mut keys: HashSet<&String> = HashSet::new();
        for k in journal_map.keys() {
            keys.insert(k);
        }
        for k in local.keys() {
            keys.insert(k);
        }
        let mut keys: Vec<&String> = keys.into_iter().collect();
        keys.sort();

        for key in keys {
            let j = journal_map.get(key);
            let l = local.get(key);
            let status = if conflicts.contains(key) {
                FileStatus::Conflict
            } else {
                match (j, l) {
                    (Some(j), Some(l)) => {
                        if j.size == l.size && j.mtime_millis == l.mtime_millis {
                            FileStatus::Synced
                        } else {
                            FileStatus::PendingUpload
                        }
                    }
                    (None, Some(_)) => FileStatus::NewLocal,
                    (Some(_), None) => FileStatus::DeletedPending,
                    (None, None) => continue,
                }
            };
            let (size, mtime) = match l {
                Some(l) => (l.size, l.mtime_millis),
                None => (j.unwrap().size, j.unwrap().mtime_millis),
            };
            files.push(FileStat {
                key: key.clone(),
                size,
                mtime_millis: mtime,
                status,
            });
        }

        Ok(Snapshot {
            paused: self.state.paused.load(Ordering::Relaxed),
            last_sync_millis: *self.state.last_sync_millis.lock().unwrap(),
            files,
        })
    }

    pub fn status(&self) -> Status {
        Status {
            paused: self.state.paused.load(Ordering::Relaxed),
            last_sync_millis: *self.state.last_sync_millis.lock().unwrap(),
            endpoint: self.cfg.endpoint.clone(),
            bucket: self.cfg.bucket.clone(),
            prefix: self.cfg.prefix.clone(),
            local_root: self.cfg.local_root.to_string_lossy().into_owned(),
        }
    }

    pub fn activity(&self, since: u64) -> Vec<ActivityEntry> {
        self.state
            .activity
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.seq > since)
            .cloned()
            .collect()
    }

    pub async fn sync_now(&self) -> Result<()> {
        self.cmd_tx
            .send(Cmd::SyncNow)
            .await
            .map_err(|_| Error::other("engine stopped"))
    }

    pub async fn pause(&self) -> Result<()> {
        self.cmd_tx
            .send(Cmd::Pause)
            .await
            .map_err(|_| Error::other("engine stopped"))
    }

    pub async fn resume(&self) -> Result<()> {
        self.cmd_tx
            .send(Cmd::Resume)
            .await
            .map_err(|_| Error::other("engine stopped"))
    }

    /// Resolve a tracked conflict. The mirror already kept both bytes
    /// (conflict-copy); this dismisses the flag and (for keep-local/remote)
    /// deletes the losing copy locally and remotely, then triggers a sync.
    pub async fn resolve_conflict(
        &self,
        key: &str,
        resolution: crate::protocol::Resolution,
    ) -> Result<()> {
        self.mirror.resolve_conflict(key, resolution).await?;
        self.state.conflicts.lock().unwrap().remove(key);
        self.sync_now().await?;
        Ok(())
    }

    /// Stop the sync loop and wait for it.
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(Cmd::Shutdown).await;
        if let Some(h) = self._loop.lock().await.take() {
            let _ = h.await;
        }
    }

    /// Full-text search across the indexed corpus. Returns ranked hits with
    /// snippets. Empty when the index is disabled or the query yields nothing.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<crate::SearchHit>> {
        let Some(ix) = self.indexer.as_ref() else {
            return Ok(Vec::new());
        };
        ix.search(query, limit)
    }

    /// Force an indexing sweep now (otherwise it runs after each sync pass).
    /// Returns the number of newly-indexed keys, or 0 if indexing is disabled.
    pub async fn index_now(&self) -> std::result::Result<usize, &'static str> {
        let Some(ix) = self.indexer.as_ref() else {
            return Ok(0);
        };
        ix.sweep().await.map_err(|_| "index sweep failed")
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Best-effort: nudge the loop if still running.
        let _ = self.cmd_tx.try_send(Cmd::Shutdown);
    }
}

/// The sync loop: local watch + remote poll + rescan + debounce, gated by pause.
async fn run_loop(
    mirror: Arc<Mirror>,
    cfg: Config,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    event_tx: broadcast::Sender<SyncEvent>,
    state: Arc<State>,
    index_tx: mpsc::Sender<IndexCmd>,
) {
    // Watcher channel. We keep a sender (`watch_keepalive`) alive for the whole
    // loop so `recv()` blocks forever rather than returning `None` if the watcher
    // ever drops its sender — that keeps the `select!` branch panic-free without
    // any `Option`/`unwrap`.
    let (watch_tx, mut watch_rx) = mpsc::channel::<()>(256);
    let _watch_keepalive = watch_tx.clone();
    match LocalWatcher::start_with_sender(&cfg.local_root, watch_tx) {
        Ok(_) => debug!("file watcher started on {}", cfg.local_root.display()),
        Err(e) => error!(
            error = %e,
            "file watcher failed to start; relying on periodic rescan"
        ),
    }

    let mut remote_poll = interval(Duration::from_secs(cfg.remote_poll_secs));
    let mut rescan = interval(Duration::from_secs(cfg.local_rescan_secs));
    // First ticks fire immediately — do an initial sync on startup.
    remote_poll.tick().await;
    // `Sleep` is `!Unpin`; `Pin<Box<Sleep>>` is a `Future` that's also `Unpin`,
    // so the `select!` branch type-checks without extra ceremony.
    let mut debounce: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;

    loop {
        if debounce.is_some() {
            // Armed: a sync is pending its debounce window.
            tokio::select! {
                biased;

                Some(cmd) = cmd_rx.recv() => match cmd {
                    Cmd::Shutdown => {
                        info!("engine loop shutting down");
                        let _ = index_tx.send(IndexCmd::Shutdown).await;
                        break;
                    }
                    Cmd::SyncNow => { run_sync(&mirror, &event_tx, &state, &index_tx).await; }
                    Cmd::Pause => { state.paused.store(true, Ordering::Relaxed); info!("sync paused"); }
                    Cmd::Resume => { state.paused.store(false, Ordering::Relaxed); info!("sync resumed"); }
                },

                // Re-arm (refresh) on new watcher activity.
                _ = watch_rx.recv() => {
                    debounce = Some(Box::pin(sleep(Duration::from_millis(400))));
                }

                // Debounce window elapsed → run one sync now.
                _ = debounce.as_mut().unwrap() => {
                    debounce = None;
                    if !state.paused.load(Ordering::Relaxed) {
                        run_sync(&mirror, &event_tx, &state, &index_tx).await;
                    }
                }

                _ = remote_poll.tick() => {
                    if !state.paused.load(Ordering::Relaxed) {
                        run_sync(&mirror, &event_tx, &state, &index_tx).await;
                    }
                }

                _ = rescan.tick() => {
                    if !state.paused.load(Ordering::Relaxed) {
                        run_sync(&mirror, &event_tx, &state, &index_tx).await;
                    }
                }
            }
        } else {
            // Idle: no pending debounce.
            tokio::select! {
                biased;

                Some(cmd) = cmd_rx.recv() => match cmd {
                    Cmd::Shutdown => {
                        info!("engine loop shutting down");
                        let _ = index_tx.send(IndexCmd::Shutdown).await;
                        break;
                    }
                    Cmd::SyncNow => { run_sync(&mirror, &event_tx, &state, &index_tx).await; }
                    Cmd::Pause => { state.paused.store(true, Ordering::Relaxed); info!("sync paused"); }
                    Cmd::Resume => { state.paused.store(false, Ordering::Relaxed); info!("sync resumed"); }
                },

                // Arm the debounce on the first watcher event.
                _ = watch_rx.recv() => {
                    debounce = Some(Box::pin(sleep(Duration::from_millis(400))));
                }

                _ = remote_poll.tick() => {
                    if !state.paused.load(Ordering::Relaxed) {
                        run_sync(&mirror, &event_tx, &state, &index_tx).await;
                    }
                }

                _ = rescan.tick() => {
                    if !state.paused.load(Ordering::Relaxed) {
                        run_sync(&mirror, &event_tx, &state, &index_tx).await;
                    }
                }
            }
        }
    }
}

/// Indexer task: waits for triggers, runs a sweep. Triggers coalesce — if a
/// sweep is in flight, the next trigger queues behind it. Avoids touching the
/// writer from the sync loop (which would block S3 I/O).
async fn run_index_loop(indexer: Arc<Indexer>, mut rx: mpsc::Receiver<IndexCmd>) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            IndexCmd::Shutdown => break,
            IndexCmd::Trigger => {
                if let Err(e) = indexer.sweep().await {
                    tracing::warn!(error = %e, "index sweep failed");
                }
            }
        }
    }
}

/// Run one sync pass, forward events, and nudge the indexer.
async fn run_sync(
    mirror: &Arc<Mirror>,
    event_tx: &broadcast::Sender<SyncEvent>,
    state: &Arc<State>,
    index_tx: &mpsc::Sender<IndexCmd>,
) {
    let (tx, mut rx) = mpsc::channel::<SyncEvent>(256);
    let forward_state = state.clone();
    let forward_tx = event_tx.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            record_activity(&forward_state, &ev);
            let _ = forward_tx.send(ev);
        }
    });

    if let Err(e) = mirror.sync_once(&tx).await {
        error!(error = %e, "sync pass failed");
    }
    drop(tx);
    let _ = forwarder.await;
    *state.last_sync_millis.lock().unwrap() = Some(now_millis());
    // Index sweep runs in its own task — fire-and-forget; coalesces if busy.
    let _ = index_tx.try_send(IndexCmd::Trigger);
}

fn record_activity(state: &State, ev: &SyncEvent) {
    let (kind, key) = match ev {
        SyncEvent::Started => return,
        SyncEvent::Finished(_) => return,
        SyncEvent::Uploaded(k) => ("uploaded", k.as_str()),
        SyncEvent::Downloaded(k) => ("downloaded", k.as_str()),
        SyncEvent::Conflict(k) => {
            state.conflicts.lock().unwrap().insert(k.clone());
            ("conflict", k.as_str())
        }
        SyncEvent::DeletedRemote(k) => ("deleted_remote", k.as_str()),
        SyncEvent::DeletedLocal(k) => ("deleted_local", k.as_str()),
        SyncEvent::Error(k, _) => ("error", k.as_str()),
    };
    let seq = state.seq.fetch_add(1, Ordering::Relaxed) + 1;
    let mut act = state.activity.lock().unwrap();
    act.push_back(ActivityEntry {
        seq,
        kind: kind.to_string(),
        key: key.to_string(),
        ts_millis: now_millis(),
    });
    while act.len() > ACTIVITY_CAP {
        act.pop_front();
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Lightweight local scan (size + mtime only) for the snapshot — shared shape
/// with the mirror's gather but kept allocation-light.
fn gather_local_stat(root: &Path) -> Result<std::collections::HashMap<String, LocalStat>> {
    use std::collections::HashMap;
    let mut map: HashMap<String, LocalStat> = HashMap::new();
    if !root.exists() {
        return Ok(map);
    }
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = entry.metadata()?;
        let key = match key_for_local(root, entry.path()) {
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
            LocalStat {
                size: meta.len(),
                mtime_millis: mtime,
            },
        );
    }
    Ok(map)
}

#[derive(Clone, Copy)]
struct LocalStat {
    size: u64,
    mtime_millis: u64,
}
