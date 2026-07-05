//! IPC wire types + length-prefixed framing, shared by the daemon and any
//! client (the TUI, and later the GUI).
//!
//! Transport is a local socket (Unix domain socket / Windows named pipe) owned
//! by the daemon. The model is **lockstep request/response with polling** — the
//! client polls `Snapshot`/`Activity` at ~10 Hz. Everything is local, so polling
//! latency is imperceptible and the protocol stays trivial (no push framing).

use std::io;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A client request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Monotonic id, echoed back in the response.
    pub id: u64,
    /// Method name (one of the `METHOD_*` constants).
    pub method: String,
    /// Method-specific params (or `null`).
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A server response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, result: impl Serialize) -> Self {
        Response {
            id,
            result: Some(serde_json::to_value(result).unwrap_or(serde_json::Value::Null)),
            error: None,
        }
    }
    pub fn err(id: u64, msg: impl Into<String>) -> Self {
        Response {
            id,
            result: None,
            error: Some(msg.into()),
        }
    }
}

/// How to resolve a both-sides conflict from the UI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    KeepLocal,
    KeepRemote,
    KeepBoth,
}

// Method name constants ----------------------------------------------------

pub const METHOD_SNAPSHOT: &str = "snapshot";
pub const METHOD_STATUS: &str = "status";
pub const METHOD_ACTIVITY: &str = "activity";
pub const METHOD_SYNC_NOW: &str = "sync_now";
pub const METHOD_PAUSE: &str = "pause";
pub const METHOD_RESUME: &str = "resume";
pub const METHOD_RESOLVE_CONFLICT: &str = "resolve_conflict";
pub const METHOD_SEARCH: &str = "search";
pub const METHOD_INDEX_NOW: &str = "index_now";

// Framing ------------------------------------------------------------------

/// Write one length-prefixed JSON message.
pub async fn write_msg<W, M>(w: &mut W, msg: &M) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    M: Serialize,
{
    let bytes = serde_json::to_vec(msg)?;
    let len = bytes.len() as u32;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed JSON message. Returns `Ok(None)` on clean EOF.
pub async fn read_msg<R, M>(r: &mut R) -> io::Result<Option<M>>
where
    R: AsyncRead + Unpin,
    M: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    if r.read_exact(&mut len_buf).await.is_err() {
        return Ok(None);
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    // ponytail: a 64 MiB cap guards against a malformed length prefix; raise if
    // real snapshots ever exceed it.
    if len > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let msg =
        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(msg))
}
