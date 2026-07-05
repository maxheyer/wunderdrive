//! wunderdrive-daemon — owns the engine and serves local IPC.
//!
//! Runs as a foreground process. Clients (the TUI) connect over a local socket
//! and use the lockstep request/response protocol in
//! [`wunderdrive_engine::protocol`].

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use interprocess::local_socket::{
    tokio::{prelude::*, Stream},
    GenericNamespaced, ListenerOptions, ToNsName,
};
use tokio::io::BufStream;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use wunderdrive_engine::protocol::{
    read_msg, write_msg, Request, Resolution, Response, METHOD_ACTIVITY, METHOD_INDEX_NOW,
    METHOD_MATERIALIZE, METHOD_PAUSE, METHOD_PIN_VERSION, METHOD_RESOLVE_CONFLICT,
    METHOD_RESTORE_VERSION, METHOD_RESUME, METHOD_SEARCH, METHOD_SNAPSHOT, METHOD_STATUS,
    METHOD_SYNC_NOW, METHOD_UNLOCK_KEY, METHOD_UNPIN_VERSION, METHOD_VERSION_HISTORY,
};
use wunderdrive_engine::Engine;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wunderdrive=info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let cfg_path = arg_value(&args, "--config");
    let journal = arg_value(&args, "--journal");
    let access_key_id = arg_value(&args, "--access-key-id");
    let secret_access_key = arg_value(&args, "--secret-access-key");
    let socket_name = arg_value(&args, "--socket").unwrap_or_else(|| "wunderdrive".to_string());

    let cfg = match cfg_path {
        Some(p) => wunderdrive_engine::config::Config::load(&PathBuf::from(&p))?,
        None => wunderdrive_engine::config::Config::load_default()?,
    };
    let journal_path = journal
        .map(PathBuf::from)
        .unwrap_or_else(|| wunderdrive_engine::config::default_journal_path());

    // Explicit CLI creds override keychain/env; helpful on headless boxes / pods
    // where there's no secret-service daemon and env wiring is awkward.
    let creds_override = match (access_key_id, secret_access_key) {
        (Some(id), Some(secret)) => Some(wunderdrive_engine::creds::Credentials {
            access_key_id: id,
            secret_access_key: secret,
        }),
        _ => None,
    };

    let engine =
        Engine::start_with_creds(cfg, journal_path, creds_override).context("starting engine")?;
    info!(endpoint = ?engine.cfg().endpoint, bucket = %engine.cfg().bucket, "engine started");

    let name = socket_name
        .clone()
        .to_ns_name::<GenericNamespaced>()
        .context("building socket name")?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .with_context(|| "binding local socket")?;
    info!(name = %socket_name, "listening for clients");

    let engine = Arc::new(engine);
    // Serialize engine-wide mutating commands so sync_now/pause/etc. don't race.
    let cmd_lock = Arc::new(Mutex::new(()));

    loop {
        match listener.accept().await {
            Ok(conn) => {
                let engine = engine.clone();
                let cmd_lock = cmd_lock.clone();
                tokio::spawn(async move {
                    let mut stream = BufStream::new(conn);
                    if let Err(e) = handle_conn(&mut stream, &engine, &cmd_lock).await {
                        if e.kind() != std::io::ErrorKind::UnexpectedEof {
                            warn!(error = %e, "client connection ended");
                        }
                    }
                });
            }
            Err(e) => error!(error = %e, "accept failed"),
        }
    }
}

async fn handle_conn(
    stream: &mut BufStream<Stream>,
    engine: &Engine,
    cmd_lock: &Arc<Mutex<()>>,
) -> std::io::Result<()> {
    loop {
        let req: Request = match read_msg(stream).await? {
            Some(r) => r,
            None => return Ok(()),
        };
        let resp = dispatch(stream, engine, cmd_lock, req).await;
        write_msg(stream, &resp).await?;
    }
}

async fn dispatch(
    _stream: &mut BufStream<Stream>,
    engine: &Engine,
    cmd_lock: &Arc<Mutex<()>>,
    req: Request,
) -> Response {
    let id = req.id;
    let err = |e: String| Response::err(id, e);

    match req.method.as_str() {
        METHOD_SNAPSHOT => match engine.snapshot() {
            Ok(s) => Response::ok(id, serde_json::to_value(s).unwrap()),
            Err(e) => err(e.to_string()),
        },
        METHOD_STATUS => Response::ok(id, serde_json::to_value(engine.status()).unwrap()),
        METHOD_ACTIVITY => {
            let since = req
                .params
                .get("since")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Response::ok(id, serde_json::to_value(engine.activity(since)).unwrap())
        }
        METHOD_SYNC_NOW => {
            let _g = cmd_lock.lock().await;
            match engine.sync_now().await {
                Ok(_) => Response::ok(id, serde_json::Value::Null),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_PAUSE => {
            let _g = cmd_lock.lock().await;
            match engine.pause().await {
                Ok(_) => Response::ok(id, serde_json::Value::Null),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_RESUME => {
            let _g = cmd_lock.lock().await;
            match engine.resume().await {
                Ok(_) => Response::ok(id, serde_json::Value::Null),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_RESOLVE_CONFLICT => {
            let key = match req.params.get("key").and_then(|v| v.as_str()) {
                Some(k) => k.to_string(),
                None => return err("missing 'key'".into()),
            };
            let resolution = match req
                .params
                .get("resolution")
                .and_then(|v| serde_json::from_value::<Resolution>(v.clone()).ok())
            {
                Some(r) => r,
                None => return err("missing/invalid 'resolution'".into()),
            };
            let _g = cmd_lock.lock().await;
            match engine.resolve_conflict(&key, resolution).await {
                Ok(_) => Response::ok(id, serde_json::Value::Null),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_SEARCH => {
            let query = match req.params.get("query").and_then(|v| v.as_str()) {
                Some(q) => q.to_string(),
                None => return err("missing 'query'".into()),
            };
            let limit = req
                .params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as usize;
            // Search is a pure local read — no cmd_lock needed.
            match engine.search(&query, limit) {
                Ok(hits) => Response::ok(id, serde_json::to_value(hits).unwrap()),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_INDEX_NOW => {
            let _g = cmd_lock.lock().await;
            match engine.index_now().await {
                Ok(n) => Response::ok(id, serde_json::json!({ "indexed": n })),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_MATERIALIZE => {
            let key = match req.params.get("key").and_then(|v| v.as_str()) {
                Some(k) => k.to_string(),
                None => return err("missing 'key'".into()),
            };
            let _g = cmd_lock.lock().await;
            match engine.materialize(&key).await {
                Ok(_) => Response::ok(id, serde_json::Value::Null),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_UNLOCK_KEY => {
            let key = match req.params.get("key").and_then(|v| v.as_str()) {
                Some(k) => k.to_string(),
                None => return err("missing 'key'".into()),
            };
            match engine.unlock_key(&key) {
                Ok(was_locked) => Response::ok(id, serde_json::json!({ "was_locked": was_locked })),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_RESTORE_VERSION => {
            let key = match req.params.get("key").and_then(|v| v.as_str()) {
                Some(k) => k.to_string(),
                None => return err("missing 'key'".into()),
            };
            let version_id = match req.params.get("version_id").and_then(|v| v.as_str()) {
                Some(v) => v.to_string(),
                None => return err("missing 'version_id'".into()),
            };
            let _g = cmd_lock.lock().await;
            match engine.restore_version(&key, &version_id).await {
                Ok(_) => Response::ok(id, serde_json::Value::Null),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_PIN_VERSION => {
            let key = match req.params.get("key").and_then(|v| v.as_str()) {
                Some(k) => k.to_string(),
                None => return err("missing 'key'".into()),
            };
            let version_id = match req.params.get("version_id").and_then(|v| v.as_str()) {
                Some(v) => v.to_string(),
                None => return err("missing 'version_id'".into()),
            };
            match engine.pin_version(&key, &version_id) {
                Ok(_) => Response::ok(id, serde_json::Value::Null),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_UNPIN_VERSION => {
            let key = match req.params.get("key").and_then(|v| v.as_str()) {
                Some(k) => k.to_string(),
                None => return err("missing 'key'".into()),
            };
            match engine.unpin_version(&key) {
                Ok(_) => Response::ok(id, serde_json::Value::Null),
                Err(e) => err(e.to_string()),
            }
        }
        METHOD_VERSION_HISTORY => {
            let key = match req.params.get("key").and_then(|v| v.as_str()) {
                Some(k) => k.to_string(),
                None => return err("missing 'key'".into()),
            };
            match engine.version_history(&key) {
                Ok(history) => Response::ok(id, serde_json::to_value(history).unwrap()),
                Err(e) => err(e.to_string()),
            }
        }
        _ => err(format!("unknown method: {}", req.method)),
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_string());
        }
    }
    None
}
