//! Mirror integration tests against `object_store::memory::InMemory`.
//!
//! These run with plain `cargo test` (no Docker) and exercise the real
//! upload/download/delete/conflict-copy paths including the blake3-in-metadata
//! round trip. Real S3 semantics (versioning) are covered by `tests/minio.rs`,
//! which is `#[ignore]` and needs the MinIO compose.

use std::sync::Arc;

use object_store::{ObjectStore, ObjectStoreExt};
use tokio::sync::mpsc;
use wunderdrive_engine::config::Config;
use wunderdrive_engine::index::Indexer;
use wunderdrive_engine::journal;
use wunderdrive_engine::mirror::{Mirror, SyncEvent};

fn cfg(root: &std::path::Path) -> Config {
    Config {
        endpoint: None,
        region: "us-east-1".into(),
        bucket: "test".into(),
        prefix: "".into(),
        local_root: root.to_path_buf(),
        remote_poll_secs: 3600,
        local_rescan_secs: 3600,
        lazy: false,
    }
}

fn make_mirror(dir: &std::path::Path) -> Mirror {
    let store = Arc::new(object_store::memory::InMemory::new()) as Arc<dyn ObjectStore>;
    let db = journal::open(&dir.join("j.redb")).unwrap();
    Mirror::new(cfg(&dir.join("root")), store.clone(), Arc::new(db))
}

/// Drain the sync event channel so sync_once can make progress.
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
    // small delay so the receiver is live
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let out = mirror.sync_once(&tx).await.unwrap();
    drop(tx);
    let _ = h.await;
    out
}

#[tokio::test]
async fn upload_new_file_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mirror = make_mirror(dir.path());
    let root = mirror.cfg().local_root.clone();

    // Local file appears.
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.txt"), b"hello world").unwrap();

    let out = run_sync(&mirror).await;
    assert_eq!(out.uploaded, 1);

    // Journal now has an entry with a blake3.
    let snap = journal::snapshot(mirror.db()).unwrap();
    let entry = snap.get("hello.txt").expect("journal has entry");
    assert_eq!(entry.size, 11);

    // Remote object exists and carries the content-hash attribute.
    let store = mirror.store_handle();
    let path = object_store::path::Path::parse("hello.txt").unwrap();
    let res = store.get(&path).await.unwrap();
    assert!(res
        .attributes
        .get(&object_store::Attribute::Metadata(
            std::borrow::Cow::Borrowed("content-hash")
        ))
        .is_some());
}

#[tokio::test]
async fn download_remote_new() {
    let dir = tempfile::tempdir().unwrap();
    let mirror = make_mirror(dir.path());
    let root = mirror.cfg().local_root.clone();
    std::fs::create_dir_all(&root).unwrap();

    // Pre-seed the store with an object.
    let store = mirror_store(&mirror);
    let path = object_store::path::Path::parse("notes.md").unwrap();
    store
        .put(
            &path,
            object_store::PutPayload::from_bytes(b"remote body".to_vec().into()),
        )
        .await
        .unwrap();

    let out = run_sync(&mirror).await;
    assert_eq!(out.downloaded, 1);
    assert_eq!(
        std::fs::read(root.join("notes.md")).unwrap(),
        b"remote body"
    );
}

#[tokio::test]
async fn delete_propagates_remote() {
    let dir = tempfile::tempdir().unwrap();
    let mirror = make_mirror(dir.path());
    let root = mirror.cfg().local_root.clone();
    std::fs::create_dir_all(&root).unwrap();

    // Sync once to upload.
    std::fs::write(root.join("gone.txt"), b"x").unwrap();
    run_sync(&mirror).await;

    // Delete locally, then sync.
    std::fs::remove_file(root.join("gone.txt")).unwrap();
    let out = run_sync(&mirror).await;
    assert_eq!(out.deleted_remote, 1);
    assert!(journal::snapshot(mirror.db()).unwrap().is_empty());
}

#[tokio::test]
async fn conflict_keeps_both() {
    let dir = tempfile::tempdir().unwrap();
    let mirror = make_mirror(dir.path());
    let root = mirror.cfg().local_root.clone();
    std::fs::create_dir_all(&root).unwrap();

    // Establish baseline: upload a file.
    std::fs::write(root.join("doc.txt"), b"baseline").unwrap();
    run_sync(&mirror).await;

    // Mutate BOTH sides so the next sync sees a conflict.
    std::fs::write(root.join("doc.txt"), b"local edit").unwrap();
    let store = mirror_store(&mirror);
    let path = object_store::path::Path::parse("doc.txt").unwrap();
    store
        .put(
            &path,
            object_store::PutPayload::from_bytes(b"remote edit".to_vec().into()),
        )
        .await
        // bump the "version" so reconcile sees remote_dirty; InMemory sets a new
        // generation on every put, which our gather_remote surfaces as version.
        .unwrap();

    let out = run_sync(&mirror).await;
    assert_eq!(out.conflicts, 1);

    // Both bytes are present locally: canonical (remote) + conflict copy (local).
    let canonical = std::fs::read(root.join("doc.txt")).unwrap();
    assert!(canonical == b"remote edit" || canonical == b"local edit");
    // A sibling conflict file exists.
    let mut found_sibling = false;
    for e in std::fs::read_dir(&root).unwrap() {
        let name = e.unwrap().file_name();
        if name.to_string_lossy().starts_with("doc (conflict ") {
            found_sibling = true;
        }
    }
    assert!(found_sibling, "conflict-copy should exist");
}

// ---- helpers ---------------------------------------------------------------

fn mirror_store(mirror: &Mirror) -> Arc<dyn ObjectStore> {
    mirror.store_handle()
}

#[tokio::test]
async fn search_works_after_sync() {
    let dir = tempfile::tempdir().unwrap();
    let mirror = make_mirror(dir.path());
    let root = mirror.cfg().local_root.clone();
    std::fs::create_dir_all(&root).unwrap();

    // Write a file and sync to upload it.
    std::fs::write(root.join("doc.txt"), b"hello world from test").unwrap();
    run_sync(&mirror).await;

    // Create an indexer, sweep, and search.
    let indexer = Indexer::open(
        mirror.db().clone(),
        root.clone(),
        &dir.path().join("idx"),
        None,
    )
    .unwrap();
    indexer.sweep().await.unwrap();

    let hits = indexer.search("hello", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key, "doc.txt");
    assert!(hits[0].snippet.as_deref().unwrap().contains("hello"));
}
