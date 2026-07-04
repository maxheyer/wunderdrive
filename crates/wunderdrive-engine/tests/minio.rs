//! Real-S3 integration tests against MinIO (or any S3-compatible endpoint).
//!
//! `#[ignore]` by default so `cargo test` stays green without Docker. Enable:
//!
//! ```sh
//! docker compose -f docker-compose.minio.yml up -d
//! export WD_TEST_ENDPOINT=http://localhost:9000
//! export WD_TEST_ACCESS_KEY=wunderdrive
//! export WD_TEST_SECRET_KEY=wunderdrive-secret
//! export WD_TEST_BUCKET=wunderdrive-test
//! cargo test -p wunderdrive-engine --test minio -- --ignored --nocapture
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use object_store::ObjectStoreExt;
use tokio::sync::mpsc;
use wunderdrive_engine::config::Config;
use wunderdrive_engine::creds::Credentials;
use wunderdrive_engine::journal;
use wunderdrive_engine::mirror::{Mirror, SyncEvent};
use wunderdrive_engine::store;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Per-call unique counter so parallel tests get distinct prefixes (each test
/// otherwise shares the bucket and would see the others' objects).
static SETUP_SEQ: AtomicU64 = AtomicU64::new(0);

struct Env {
    cfg: Config,
    creds: Credentials,
    tmp: tempfile::TempDir,
}

fn setup() -> Env {
    let endpoint = env_or("WD_TEST_ENDPOINT", "http://localhost:9000");
    let creds = Credentials {
        access_key_id: env_or("WD_TEST_ACCESS_KEY", "wunderdrive"),
        secret_access_key: env_or("WD_TEST_SECRET_KEY", "wunderdrive-secret"),
    };
    let tmp = tempfile::tempdir().expect("tmpdir");
    let seq = SETUP_SEQ.fetch_add(1, Ordering::Relaxed);
    let cfg = Config {
        endpoint: Some(endpoint),
        region: "us-east-1".into(),
        bucket: env_or("WD_TEST_BUCKET", "wunderdrive-test"),
        prefix: format!("itest-{}-{}", std::process::id(), seq),
        local_root: tmp.path().join("root"),
        remote_poll_secs: 3600,
        local_rescan_secs: 3600,
    };
    Env { cfg, creds, tmp }
}

async fn run_sync(mirror: &Mirror) -> wunderdrive_engine::mirror::SyncOutcome {
    let (tx, mut rx) = mpsc::channel::<SyncEvent>(256);
    let h = tokio::spawn(async move {
        let mut last = wunderdrive_engine::mirror::SyncOutcome::default();
        while let Some(ev) = rx.recv().await {
            if let SyncEvent::Finished(o) = ev {
                last = o;
            }
        }
        last
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let out = mirror.sync_once(&tx).await.unwrap();
    drop(tx);
    let _ = h.await;
    out
}

#[tokio::test]
#[ignore]
async fn minio_upload_and_download() {
    let env = setup();
    let store = store::build(&env.cfg, &env.creds).expect("store");
    let db = Arc::new(journal::open(&env.tmp.path().join("j.redb")).unwrap());
    let mirror = Mirror::new(env.cfg.clone(), store, db);
    std::fs::create_dir_all(&env.cfg.local_root).unwrap();

    // Upload a local file.
    std::fs::write(env.cfg.local_root.join("a.txt"), b"minio body").unwrap();
    let out = run_sync(&mirror).await;
    assert_eq!(out.uploaded, 1);

    // A second device sees it on download after clearing the local copy + journal.
    std::fs::remove_file(env.cfg.local_root.join("a.txt")).unwrap();
    journal::snapshot(mirror.db())
        .unwrap()
        .keys()
        .cloned()
        .for_each(|k| journal::remove(mirror.db(), &k).unwrap());
    let out = run_sync(&mirror).await;
    assert_eq!(out.downloaded, 1);
    assert_eq!(
        std::fs::read(env.cfg.local_root.join("a.txt")).unwrap(),
        b"minio body"
    );
}

#[tokio::test]
#[ignore]
async fn minio_large_multipart_upload() {
    let env = setup();
    let store = store::build(&env.cfg, &env.creds).expect("store");
    let db = Arc::new(journal::open(&env.tmp.path().join("j.redb")).unwrap());
    let mirror = Mirror::new(env.cfg.clone(), store, db);
    std::fs::create_dir_all(&env.cfg.local_root).unwrap();

    // 12 MiB → exceeds the 8 MiB multipart threshold.
    let big = vec![0xABu8; 12 * 1024 * 1024];
    std::fs::write(env.cfg.local_root.join("big.bin"), &big).unwrap();
    let out = run_sync(&mirror).await;
    assert_eq!(out.uploaded, 1);

    // Verify via the store directly.
    let path = object_store::path::Path::parse("big.bin").unwrap();
    let got = mirror
        .store_handle()
        .get(&path)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(got.len(), 12 * 1024 * 1024);
}

#[tokio::test]
#[ignore]
async fn minio_conflict_keeps_both() {
    let env = setup();
    let store = store::build(&env.cfg, &env.creds).expect("store");
    let db = Arc::new(journal::open(&env.tmp.path().join("j.redb")).unwrap());
    let mirror = Mirror::new(env.cfg.clone(), store.clone(), db);
    std::fs::create_dir_all(&env.cfg.local_root).unwrap();

    std::fs::write(env.cfg.local_root.join("c.txt"), b"base").unwrap();
    run_sync(&mirror).await;

    std::fs::write(env.cfg.local_root.join("c.txt"), b"local").unwrap();
    let path = object_store::path::Path::parse("c.txt").unwrap();
    store
        .put(
            &path,
            object_store::PutPayload::from_bytes(b"remote".to_vec().into()),
        )
        .await
        .unwrap();

    let out = run_sync(&mirror).await;
    assert_eq!(out.conflicts, 1);

    // A conflict-copy sibling exists alongside the canonical file.
    let entries: Vec<_> = std::fs::read_dir(&env.cfg.local_root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(entries.iter().any(|n| n.starts_with("c (conflict ")));
}
