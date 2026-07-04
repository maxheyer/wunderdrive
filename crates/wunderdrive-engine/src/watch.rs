//! Local filesystem watching (spec §5).
//!
//! `notify` gives realtime events; we coalesce them in the engine loop. This
//! module just turns "something under the mirror root changed" into an async
//! signal. Watchers drop events under load, so the engine *also* runs a
//! periodic full rescan — that backup lives in [`crate::engine`].

use std::path::Path;

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// A handle to the background watcher. Dropping it stops watching.
pub struct LocalWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl LocalWatcher {
    /// Start watching `root` recursively, pushing `()` onto `sender` on any
    /// create/modify/remove event. The caller is expected to keep the matching
    /// receiver (and, for a headless fallback, a sender clone) alive.
    pub fn start_with_sender(root: &Path, sender: mpsc::Sender<()>) -> Result<Self, notify::Error> {
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    if matches!(
                        ev.kind,
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                    ) {
                        let _ = sender.try_send(());
                    }
                }
            })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }
}
