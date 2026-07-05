use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use interprocess::local_socket::tokio::{prelude::*, Stream};
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use tokio::io::BufStream;
use wunderdrive_engine::protocol::{read_msg, write_msg, Request, Response, METHOD_STATUS};
use wunderdrive_engine::Status;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Connect to the daemon and fetch its status. Spawns the daemon binary on
/// first failure, then retries for up to ~3s while it boots.
pub async fn fetch_status(socket_name: String) -> Result<Status> {
    for attempt in 0..30 {
        if let Some(mut stream) = try_connect(&socket_name).await {
            return request_status(&mut stream).await;
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

async fn try_connect(socket_name: &str) -> Option<BufStream<Stream>> {
    let name = socket_name.to_ns_name::<GenericNamespaced>().ok()?;
    Stream::connect(name).await.ok().map(BufStream::new)
}

async fn request_status(stream: &mut BufStream<Stream>) -> Result<Status> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let req = Request {
        id,
        method: METHOD_STATUS.into(),
        params: serde_json::Value::Null,
    };
    write_msg(stream, &req).await?;
    let resp: Response = read_msg(stream)
        .await?
        .context("daemon closed connection")?;
    if let Some(e) = resp.error {
        return Err(anyhow!("daemon: {e}"));
    }
    let val = resp.result.unwrap_or(serde_json::Value::Null);
    serde_json::from_value(val).context("decode status")
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
