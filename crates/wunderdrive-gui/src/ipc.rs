use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use interprocess::local_socket::tokio::{prelude::*, Stream};
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use serde::de::DeserializeOwned;
use tokio::io::BufStream;
use wunderdrive_engine::protocol::{
    read_msg, write_msg, Request, Resolution, Response, METHOD_MATERIALIZE, METHOD_PAUSE,
    METHOD_RESOLVE_CONFLICT, METHOD_RESUME, METHOD_SEARCH, METHOD_SNAPSHOT, METHOD_STATUS,
    METHOD_SYNC_NOW,
};
use wunderdrive_engine::{SearchHit, Snapshot, Status};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Connect to the daemon and fetch its status. Spawns the daemon binary on
/// first failure, then retries for up to ~3s while it boots.
pub async fn fetch_status(socket_name: String) -> Result<Status> {
    for attempt in 0..30 {
        if let Some(mut stream) = try_connect(&socket_name).await {
            return request_with_params(&mut stream, METHOD_STATUS, serde_json::Value::Null).await;
        }
        if attempt == 0 {
            spawn_daemon();
        }
        if attempt < 29 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Err(anyhow!("could not connect to daemon at '{socket_name}'"))
}

/// Fetch the current snapshot. Assumes the daemon is already running.
pub async fn fetch_snapshot(socket_name: String) -> Result<Snapshot> {
    let mut stream = try_connect(&socket_name)
        .await
        .context("daemon not running")?;
    request_with_params(&mut stream, METHOD_SNAPSHOT, serde_json::Value::Null).await
}

/// Full-text search the index.
pub async fn search(socket_name: String, query: String, limit: usize) -> Result<Vec<SearchHit>> {
    let mut stream = try_connect(&socket_name)
        .await
        .context("daemon not running")?;
    let params = serde_json::json!({ "query": query, "limit": limit });
    request_with_params(&mut stream, METHOD_SEARCH, params).await
}

/// Trigger an immediate sync cycle.
pub async fn sync_now(socket_name: String) -> Result<()> {
    let mut stream = try_connect(&socket_name)
        .await
        .context("daemon not running")?;
    request_with_params(&mut stream, METHOD_SYNC_NOW, serde_json::Value::Null).await
}

/// Pause the sync loop.
pub async fn pause(socket_name: String) -> Result<()> {
    let mut stream = try_connect(&socket_name)
        .await
        .context("daemon not running")?;
    request_with_params(&mut stream, METHOD_PAUSE, serde_json::Value::Null).await
}

/// Resume the sync loop.
pub async fn resume(socket_name: String) -> Result<()> {
    let mut stream = try_connect(&socket_name)
        .await
        .context("daemon not running")?;
    request_with_params(&mut stream, METHOD_RESUME, serde_json::Value::Null).await
}

/// Download a remote-only object into the local mirror.
pub async fn materialize(socket_name: String, key: String) -> Result<()> {
    let mut stream = try_connect(&socket_name)
        .await
        .context("daemon not running")?;
    let params = serde_json::json!({ "key": key });
    request_with_params(&mut stream, METHOD_MATERIALIZE, params).await
}

/// Resolve a both-sides conflict.
pub async fn resolve_conflict(
    socket_name: String,
    key: String,
    resolution: Resolution,
) -> Result<()> {
    let mut stream = try_connect(&socket_name)
        .await
        .context("daemon not running")?;
    let params = serde_json::json!({ "key": key, "resolution": resolution });
    request_with_params(&mut stream, METHOD_RESOLVE_CONFLICT, params).await
}

async fn try_connect(socket_name: &str) -> Option<BufStream<Stream>> {
    let name = socket_name.to_ns_name::<GenericNamespaced>().ok()?;
    Stream::connect(name).await.ok().map(BufStream::new)
}

async fn request_with_params<T: DeserializeOwned>(
    stream: &mut BufStream<Stream>,
    method: &str,
    params: serde_json::Value,
) -> Result<T> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let req = Request {
        id,
        method: method.into(),
        params,
    };
    write_msg(stream, &req).await?;
    let resp: Response = read_msg(stream)
        .await?
        .context("daemon closed connection")?;
    if let Some(e) = resp.error {
        return Err(anyhow!("daemon: {e}"));
    }
    let val = resp.result.unwrap_or(serde_json::Value::Null);
    serde_json::from_value(val).context("decode")
}

fn spawn_daemon() {
    let Some(daemon_exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wunderdrive-daemon")))
        .filter(|p| p.exists())
    else {
        return;
    };
    let _ = std::process::Command::new(daemon_exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
