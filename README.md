# wunderdrive

_A document store on any S3 — with no server component._

wunderdrive is a cross-platform desktop tool that gives you a synced,
searchable, offline-available documents folder backed by **any S3-compatible
bucket** and **nothing else**. No server to run,
no database to babysit. You bring a bucket; wunderdrive does the rest.

The pitch against Nextcloud: it's bad on **both** axes — a heavyweight, clunky
UI _and_ a server you have to run, update, and maintain. wunderdrive fixes both:
a fast native client, and no server at all. Your only backend is the bucket.

**Status:** early. The mirror engine, daemon, and an iced 0.14 desktop client
are working against real S3 endpoints (MinIO + cloud providers). OCR is stubbed.
Open source under Apache-2.0.

---

## Why

- **The bucket is the only backend.** No server component, ever — not for sync,
  not for search, not for OCR, not for sharing. Your documents never leave your
  machine except as your own bytes in your own bucket.
- **Fast = never touch S3 on the interactive path.** S3 is touched only by the
  background sync loop. Browse = local disk. Search = local index. Open = local
  file.
- **Findability is the differentiator.** Text-layer extraction first, OCR only
  as fallback, indexed in Tantivy for instant full-text search. Extraction is
  cached by blake3 content hash — same content is never re-extracted, even after
  a rename, move, or second device.
- **Everything pure Rust, cross-compilable.** Every layer picks the pure-Rust
  option (`redb`, `Tantivy`, `ocrs`/`RTen`, `lopdf`) so the engine builds across
  Linux / macOS / Windows with no C toolchain gymnastics. The GUI is the one
  honest exception — windowing is per-OS, so it ships via a CI build matrix.

## What's inside

```
wunderdrive-engine   Core: S3 mirror, blake3 hashing, redb journal,
                     three-way reconcile, Tantivy search index, extraction
wunderdrive-daemon   Binary: owns the engine, serves IPC over a local socket
wunderdrive-gui      Binary: iced 0.14 desktop client (thin IPC client)
```

The GUI is a **thin client**. It renders state and sends commands — it never
touches the engine, S3, or the filesystem directly. All data flows through IPC.

### Mirror engine

- **Local journal** in `redb` — per path: local path, S3 key, blake3 hash, size,
  mtime, remote version id. This snapshot is what enables real reconciliation
  instead of dumb mirroring.
- **Three-way reconcile** against the snapshot: local-only → upload, remote-only
  → download, both-changed → **conflict-copy** (keep both, never lose data),
  neither → skip.
- **Delete detection via the journal** — in-journal-but-gone-locally = real
  delete; never-in-journal-and-absent = not synced yet. This is what naive S3
  tools get wrong and nuke data.
- **Content hash in metadata** — `blake3` written to `x-amz-meta-content-hash`
  on upload. Identity is our hash, not the provider's ETag (MD5-of-parts on
  multipart, computed differently again on Ceph RGW).
- **Bucket versioning on by default** — nothing truly lost even on a clobber.
- **Credentials in the OS keychain** (`keyring`), never a dotfile.

### Extraction, OCR, search

- **Text-layer first, OCR only as fallback.** Most documents already carry text
  — extract it directly, no ML. Only images and text-layer-less scanned PDFs
  fall through to OCR.
- **Extraction (pure Rust):** `lopdf` / `pdf-extract` for PDF text, `calamine`
  for xlsx, `zip` + `quick-xml` for docx/pptx, trivial for text/md/code.
- **OCR: `ocrs`** — pure-Rust, ML-based, runs on the pure-Rust `RTen` engine.
  Bundles its model, cross-compiles cleanly (even to WASM). Latin-only today;
  the `OcrEngine` trait lets a multilingual backend slot in later.
- **Search index: `Tantivy`** — pure-Rust, Lucene-like, BM25 ranking, fuzzy.
  Local and instant.
- **blake3-keyed extraction cache** — extraction/OCR is expensive, so the
  extracted-text cache is keyed by the blake3 hash already computed for sync.

## Build & run

Requirements: Rust 1.82+, a working S3-compatible endpoint (MinIO works for
local dev — see `docker-compose.minio.yml`).

```bash
# Build the whole workspace
cargo build --release

# Run the daemon (it owns the engine and serves IPC on a local socket)
cargo run --release -p wunderdrive-daemon -- \
  --endpoint http://localhost:9000 \
  --bucket wunderdrive \
  --access-key minioadmin \
  --secret-key minioadmin \
  --local-root ~/wunderdrive

# Run the GUI (separate terminal, or let the GUI auto-spawn the daemon)
cargo run --release -p wunderdrive-gui
```

The GUI needs a real desktop session (Wayland or X11) — it won't boot in a
headless sandbox. Engine and daemon are fully headless.

### Tests

```bash
cargo test -p wunderdrive-engine        # unit + extraction/index tests
cargo test -p wunderdrive-engine --test minio   # integration against a live MinIO
```

## Project layout

```
.
├── spec.md                 # Product + technical spec — read this first
├── DESIGN.md               # GUI design system (tokens, icons, sync-state language)
├── AGENTS.md               # Guide for AI coding agents working on this repo
├── tasks/todo.md           # Completed phases and known debt
├── crates/
│   ├── wunderdrive-engine/ # Core (S3, journal, reconcile, index, extract)
│   ├── wunderdrive-daemon/ # Binary — headless IPC server
│   └── wunderdrive-gui/    # Binary — iced 0.14 desktop client
└── docker-compose.minio.yml  # Local MinIO for dev
```

## Roadmap

Each phase is independently useful. Ship, use, learn, then expand.

1. ✅ **Mirror + TUI** — `object_store` + `redb` + `blake3`, three-way reconcile,
   delete detection, conflict copies, versioning, multipart, keychain creds.
   (TUI since replaced by the GUI.)
2. ✅ **Watch + index** — `notify` + rescan, `ListObjectsV2` poll, extraction +
   Tantivy index + search, blake3-keyed cache, lazy download.
3. 🚧 **GUI polish + multi-device** — iced client, sidecar bucket index so device
   B doesn't re-OCR the corpus, presigned-URL share links.
4. 🔜 **Maybe** — wire the real `ocrs` backend, selective/pinned sync,
   client-side encryption, async shared-bucket access.

## Non-goals (scope walls)

These are decisions, not omissions:

- **No server** of any kind.
- **No realtime multi-user collaboration.** Presence, file locking, and
  share-invite UX are genuinely server-shaped — S3 has no push channel.
- **No VDFS, no realtime multi-device merge.**
- **No five-OS file-manager chrome** (thumbnail pipelines, media previews,
  photo albums) as the foundation.
- **No client-side encryption in v1.**

## License

Apache-2.0. See [LICENSE](LICENSE).
