//! wunderdrive engine — headless mirror/sync over any S3-compatible bucket.
//!
//! The engine owns the journal, the object store, the reconcile loop, and the
//! local filesystem mirror. It exposes a frontend-agnostic API that the daemon
//! (and any future client) drives.

#![forbid(unsafe_code)]

pub mod config;
pub mod creds;
pub mod engine;
pub mod error;
pub mod extract;
pub mod hash;
pub mod index;
pub mod journal;
pub mod mirror;
pub mod protocol;
pub mod reconcile;
pub mod store;
pub mod watch;

pub use engine::{ActivityEntry, Engine, FileStat, FileStatus, Snapshot, Status};
pub use error::{Error, Result};
pub use index::SearchHit;
