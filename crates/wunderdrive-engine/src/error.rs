//! Error types for the wunderdrive engine.

use thiserror::Error;

/// A specialised `Result` for engine operations.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors produced by the engine.
#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("object store error: {0}")]
    ObjectStore(#[from] object_store::Error),

    #[error("object store path error: {0}")]
    ObjectStorePath(#[from] object_store::path::Error),

    #[error("journal (redb) error: {0}")]
    Journal(#[from] redb::DatabaseError),

    #[error("journal transaction error: {0}")]
    JournalTx(#[from] redb::TransactionError),

    #[error("journal commit error: {0}")]
    JournalCommit(#[from] redb::CommitError),

    #[error("journal table error: {0}")]
    JournalTable(#[from] redb::TableError),

    #[error("journal storage error: {0}")]
    JournalStorage(#[from] redb::StorageError),

    #[error("keyring error: {0}")]
    Keyring(#[from] keyring::Error),

    #[error("invalid path: not valid UTF-8")]
    NonUtf8Path,

    #[error("walkdir error: {0}")]
    Walkdir(#[from] walkdir::Error),

    #[error("notify (watch) error: {0}")]
    Notify(#[from] notify::Error),

    #[error("search index (tantivy) error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("sync is paused")]
    Paused,

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
}
