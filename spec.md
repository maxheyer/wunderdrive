# wunderdrive — Technical Spec

_A Google-Drive-feel document store on any S3, with no server component._

**Status:** Open source (Apache-2.0) · Rust · cross-platform.

---

## 1. What it is

A cross-platform desktop tool that gives you the Google Drive experience — your
documents in a folder, searchable, synced, available offline — backed by **any
S3-compatible bucket** and **nothing else**. No server to run, no database to
babysit. You bring a bucket; the tool does the rest. Open source, so anyone can
point it at their own storage.

The pitch against Nextcloud: it's bad on **both** axes — a heavyweight, clunky UI
_and_ a server you have to run, update, and maintain. wunderdrive fixes both: a
fast native client, and no server at all. Your only backend is the bucket.

## 2. Invariants (non-negotiable)

- **The bucket is the only backend.** No server component, ever. Not for sync, not
  for OCR, not for search, not for sharing.
- **Everything Rust, cross-compilable.** Every layer picks the pure-Rust option so
  the engine builds with `cargo build --target …` across Linux/macOS/Windows with
  no C toolchain gymnastics. (The GUI is the one honest exception — see §8.)
- **Client-side only.** All compute — including OCR — runs on the user's machine.
  Documents never leave it except as the user's own bytes in the user's own bucket.
- **Single-tenant.** Each user runs wunderdrive against their own bucket and their
  own devices — the model implied by "connect any S3." Multi-device is first-class;
  see §3 for where sharing does and doesn't work serverless.
- **Any S3, provider-agnostic.** No provider-specific coupling. Plain S3 API.
- **Fast = never touch S3 on the interactive path.** S3 is touched only by the
  background sync loop. Browse = local disk. Search = local index. Open = local file.

## 3. Non-goals (the scope walls)

These are decisions, not omissions. Spacedrive raised $2M, ran a 12-person team,
and its file-manager vision collapsed twice under exactly this surface area before
v3 threw it all out and kept only the index. Learn from the corpse.

- **No server** of any kind.
- **No realtime multi-user collaboration.** Presence, file locking, and
  share-invite UX are genuinely server-shaped — S3 has no push channel, and the
  moment you add them you rebuild the coordination layer that makes Nextcloud a
  server. _Async shared-bucket_ access by multiple people (each with their own
  S3-scoped credentials, tolerating conflict-copies, no realtime) is achievable
  serverless and is a candidate later phase. Realtime collaboration is not.
- **No VDFS, no realtime multi-device merge.** The two things Spacedrive cut even
  with funding.
- **No five-OS file-manager chrome** (thumbnail pipelines, media previews, photo
  albums, drag-drop parity) as the foundation. This is the bottomless pit.
- **No client-side encryption in v1.** Forces filename encryption via a manifest —
  real complexity, deferred.
- **No AGPL code lifted from Spacedrive.** Copyleft conflicts with Apache-2.0.

## 4. Architecture

Headless engine, swappable frontend. The frontend choice binds nothing because
everything above the API seam is a client.

```
┌─────────────────────────────────────────────┐
│  Frontend   TUI now · GUI later               │  ← swappable
├─────────────────────────────────────────────┤
│  Local API  (the seam — frontend-agnostic)    │
├─────────────────────────────────────────────┤
│  Index      Tantivy (search) + extract + OCR  │
│  Mirror     reconcile · journal · conflict     │
├─────────────────────────────────────────────┤
│  S3 backend  any S3-compatible endpoint        │  ← the only backend
└─────────────────────────────────────────────┘
```

## 5. Mirror engine (the core)

- **Local journal** in `redb` (pure-Rust embedded store). Per path:
  `{ local_path, s3_key, blake3_hash, size, mtime, remote_version_id }`. This
  snapshot is what enables real reconciliation instead of dumb mirroring.
- **Three-way reconcile** against the snapshot: local-only → upload, remote-only →
  download, both-changed → **conflict-copy** (keep both, never lose data), neither
  → skip.
- **Delete detection via the journal** — in-journal-but-gone-locally = real delete;
  never-in-journal-and-absent = not synced yet. This is what naive S3 tools get
  wrong and nuke data.
- **Content hash in metadata** — `blake3` written to `x-amz-meta-content-hash` on
  upload. Do **not** trust ETag: it's MD5-of-parts for multipart and Ceph RGW
  computes it differently again. Identity is our hash, not the provider's.
- **Change detection** — `notify` (local realtime) backed by a periodic full rescan
  (watchers drop events under load); `ListObjectsV2` poll on a 30–60s interval for
  remote, comparing our metadata hash, GET only what changed.
- **Bucket versioning on by default** — nothing truly lost even on a clobber.
- **Multipart** via `object_store`.
- **Credentials in the OS keychain** (`keyring`), never a dotfile.

## 6. Extraction, OCR, search

The differentiator. Spacedrive's whole $2M lesson was that the value is
_findability_, not sync.

- **Text-layer first, OCR only as fallback.** Most documents already carry text —
  extract it directly, no ML. Only images and text-layer-less scanned PDFs fall
  through to OCR. Check for a text layer before ever invoking OCR — this is what
  keeps it fast.
- **Extraction (pure Rust):** `lopdf` / `pdf-extract` for PDF text (chosen over
  `pdfium-render` — Pdfium is a C++ blob that fights cross-compilation),
  `calamine` for xlsx, `zip` + `quick-xml` for docx/pptx, trivial for text/md/code.
- **OCR: `ocrs`** — pure-Rust, ML-based, model exported to ONNX and run on the
  pure-Rust `RTen` engine. No C dependencies, bundles its model, cross-compiles
  cleanly (even to WASM). Rejected Tesseract: mature and multilingual but needs
  libtesseract/libleptonica C libs on every machine → cross-platform packaging
  hell. **Caveat:** ocrs is early-preview and Latin-only today (German/English/
  most-European fine; no CJK/Arabic yet). Put it behind an `OcrEngine` trait so a
  Paddle-ONNX engine can slot in for multilingual later — still client-side.
- **Search index: `Tantivy`** — pure-Rust, Lucene-like, BM25 ranking, fuzzy. Chosen
  over SQLite FTS5 because SQLite's C dependency fights the cross-compile rule.
  Local and instant either way.
- **blake3-keyed extraction cache** — extraction/OCR is expensive, so key the
  extracted-text cache by the blake3 hash we already compute. Same content = never
  re-extract, never re-OCR. Rename/move/second-device = same hash = free.
- **OCR runs in a background worker pool.** Never blocks the sync loop or the UI.
  Low priority, incremental, writes to the index when done.

## 7. Multi-device without a server (sidecar index)

The answer to "how does device B not re-OCR the whole corpus, with no server":
Device A OCRs once and writes the extracted text + index as **sidecar objects into
the bucket** under a metadata prefix (e.g. `.wunderdrive-index/`). Device B pulls
the sidecar instead of re-OCRing. The bucket is already the sync channel — we just
put derived data through it alongside the files. No coordination server, no compute
service. Ship in a later phase; v1 indexes locally per device.

## 8. Cross-compilation

Because "everything Rust, cross-compilable" is a hard requirement, every C
dependency was deliberately designed out:

| Concern       | Chosen (pure Rust)      | Rejected (pulls C)           |
| ------------- | ----------------------- | ---------------------------- |
| Journal store | `redb`                  | `rusqlite` (SQLite)          |
| Search index  | `Tantivy`               | SQLite FTS5                  |
| OCR           | `ocrs` / `RTen`         | Tesseract (libtesseract)     |
| PDF text      | `lopdf` / `pdf-extract` | `pdfium-render` (Pdfium C++) |
| TUI           | `ratatui`               | —                            |

Result: **the engine, OCR, index, and TUI cross-compile cleanly** to
`x86_64`/`aarch64` Linux, macOS (both arches), and Windows — and ocrs/RTen even
target WASM.

**Honest exception:** a _GUI_ does not one-host cross-compile. Windowing and
(if used) system webview are per-OS, so the GUI ships via a **CI build matrix**
that compiles on each target OS. That's a packaging reality, not a design flaw —
and it's a strong reason to keep the GUI a thin, late, swappable layer over an
engine (and TUI) that _do_ cross-compile.

## 9. Frontend

- **Now: a TUI** (`ratatui`, pure Rust) over the engine — browse, search, sync
  status, conflict resolution, in-document search hits, in a terminal-native
  interface that cross-compiles cleanly. The engine runs as a headless daemon; the
  TUI is a client over its local API. And because the mirror produces real files in
  a real folder, any external file browser (yazi, nautilus, lf) works on it too,
  for free.
- **Later: a GUI** — this is where "feels like Google Drive" is won for
  non-technical users, and the expensive layer, so it goes last on a stable engine.
  Options, all Rust: **Dioxus** (React-like ergonomics), **Slint** (native
  declarative, lighter), or **egui** (immediate-mode, simplest). Deferred and
  swappable — the engine doesn't care which.

## 10. Build order

Each phase is independently useful. Ship, use, learn, then expand.

1. **Mirror + TUI** — `object_store` + `redb` + `blake3`, three-way reconcile,
   delete-detection, conflict-copy, versioning-on, multipart, keychain creds; a
   `ratatui` interface to drive and observe it.
2. **Watch + index** — `notify` + rescan, `ListObjectsV2` poll, extraction + `ocrs`
   OCR + `Tantivy` index + search in the TUI, blake3-keyed cache.
3. **GUI + multi-device** — Rust-native GUI (Dioxus/Slint), sidecar bucket index,
   presigned-URL share links.
4. **Maybe** — selective/pinned sync, client-side encryption, additional OCR
   languages, async shared-bucket access.

## 11. Crate summary

| Concern                      | Crate                          |
| ---------------------------- | ------------------------------ |
| S3 (any endpoint, multipart) | `object_store`                 |
| Local journal                | `redb`                         |
| Content hash                 | `blake3`                       |
| Local file watch             | `notify`                       |
| Credentials                  | `keyring`                      |
| PDF text                     | `lopdf` / `pdf-extract`        |
| xlsx / docx / pptx           | `calamine`, `zip`, `quick-xml` |
| OCR                          | `ocrs` (+ `RTen`)              |
| Full-text search             | `tantivy`                      |
| Async runtime                | `tokio`                        |
| TUI                          | `ratatui`                      |
| GUI (later)                  | `dioxus` / `slint` / `egui`    |
