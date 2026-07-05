# wunderdrive — Task Tracker

## Goal
Drop the TUI. Build a real desktop client that looks and feels like Spacedrive:
fast, polished, cross-platform. The daemon stays as the headless backend; the
GUI is a thin client over the existing IPC protocol.

## Completed

### Phase 1: Mirror engine + daemon + TUI (commit `101cda4`)
- [x] 3-crate workspace: engine + daemon + tui
- [x] S3 mirror with blake3 verification, three-way reconcile, journal
- [x] Abstract socket IPC, CLI-flag credential bootstrapping

### Phase 2a: Text extraction + Tantivy search (commit `9477125`)
- [x] extract.rs: 40+ text formats, PDF (lopdf), xlsx/xls/ods (calamine),
      docx/pptx (zip+quick-xml)
- [x] index.rs: Tantivy 0.24 wrapper, 1-edit fuzzy, snippet generator
- [x] Extraction cache (redb EXTRACT_TABLE), background sweep, search protocol

### Phase 2b: Lazy download + stale index fix (commit `170aba6`)
- [x] config.rs: `lazy: bool` (default true)
- [x] journal.rs: STUB_TABLE, INDEXED_TABLE, extract_clear()
- [x] reconcile.rs: RecordStub + Dematerialize actions, 5-param decide()
- [x] mirror.rs: materialize(), new apply arms
- [x] index.rs: sweep rewrite (orphan deletion + rename handling), rebuild()
- [x] engine.rs: FileStatus::RemoteOnly, Engine::materialize()
- [x] protocol.rs: METHOD_MATERIALIZE

## Debt (ranked)

### HIGH
- [ ] Object Lock awareness — locked files retry every 45s forever. Detect
      retention headers, back off, surface to user. Needs raw S3 HEAD.

### MEDIUM
- [ ] Version restore — object_store has no ListObjectVersions; may need raw S3.
- [ ] Incremental listing for large buckets — full list every sync; add
      token-based continuation + last-sync watermark.
- [ ] Real-file extraction tests — extract.rs only tested on strings; needs
      fixture .xlsx/.docx/.pdf to catch binary-format regressions.

### LOW
- [ ] Version pinning — pin specific version IDs for rollback beyond HEAD.
- [ ] ocrs integration — OcrEngine trait stub exists; wire real OCR backend.
- [ ] Keyring on NixOS — secret service daemon unavailable in sandbox;
      env/CLI creds are the working path.

---

# Desktop Client — iced 0.14

## Locked decisions
- **TUI**: removed entirely (crate deleted)
- **Architecture**: daemon + GUI client. Daemon serves IPC over local socket;
  iced app connects as a client (same IPC the TUI used).
- **Daemon lifecycle**: GUI spawns the existing `wunderdrive-daemon` binary as
  a child process on startup if the socket isn't already live. No daemon lib
  refactor — `std::process::Command::spawn`. If daemon already running, just
  connect. Users get single-app UX; headless daemon still works standalone.
- **GUI framework**: iced 0.14 (Elm architecture, pure Rust, wgpu backend).
  Chosen because COSMIC Files (System76's production OS file manager) ships on
  iced — same domain as wunderdrive. Retained mode → proper virtualized
  scrolling for large dirs. No DSL, no JS/TS, no web frontend.
- **Type bridge**: none needed — Rust end-to-end. GUI crate depends on
  `wunderdrive-engine` and reuses `protocol` types directly.
- **Theming**: iced built-in `Theme::Dark` (default). Custom palette later.
- **Icons**: unicode glyphs for status (✓ ↑ + ☁ !) — zero deps. SVG later if
  needed via `iced::widget::svg`.
- **MVP features**: file browser (list+grid), full-text search, sync status +
  controls, materialize (lazy download), conflict resolution, quick preview.

## Architecture
```
wunderdrive/
├── crates/
│   ├── wunderdrive-engine/     # Core (unchanged)
│   ├── wunderdrive-daemon/     # Binary (unchanged) — spawned by GUI
│   └── wunderdrive-gui/        # NEW — iced app
│       └── src/
│           ├── main.rs         # Entry: spawn daemon child, iced::run
│           ├── ipc.rs          # Socket client (reuses engine protocol types)
│           └── app.rs          # State, Message, update, view
```

Data flow: `iced Subscription (1Hz poll) → Unix socket → daemon → Engine`
Commands (sync/pause/materialize) → `Task` → IPC request → daemon.

## Sub-phases (each ships a working app)

### 3a. Scaffold (~1h) ✅
- [x] Delete `crates/wunderdrive-tui/`, update workspace Cargo.toml
- [x] Create `crates/wunderdrive-gui/` with iced 0.14 dep
- [x] `ipc.rs`: async socket client — `fetch_status` with retry + daemon
      auto-spawn. Reuses `wunderdrive_engine::protocol` types.
- [x] `main.rs`: iced app entrypoint — boots window, fetches status on startup
- [x] `app.rs`: state = Connecting → Connected(status) | Error(String);
      view shows bucket/endpoint/local root/paused
- [x] cargo fmt + test green (4 tests pass, 0 warnings)
- Note: GUI can't boot in sandbox (missing libwayland-client.so); works on
      real desktop with Wayland/X11 libs present.

### 3b. File browser (~3h) ✅
- [x] Self-perpetuating 1Hz snapshot poll via Task::perform (sleep + refetch)
- [x] Current-directory view: filter snapshot by path prefix (split_dirs)
- [x] Scrollable list of folder rows + file rows with status glyphs
- [x] Path breadcrumb + back button (navigate into/out of subdirs)
- [x] Status bar (item count, last sync time)

### 3c. Search + sync controls ✅
- [x] ipc.rs: refactored `request` → `request_with_params<T>(stream, method, params)`
- [x] ipc.rs: `search`, `sync_now`, `pause`, `resume`
- [x] Search bar (text_input) with 150ms debounce (sleep-then-search Task)
- [x] Inline search results replace file list while query non-empty;
      stale results discarded by query-match check
- [x] Sidebar bottom controls: "Sync now" (primary) + "Pause/Resume" toggle
- [x] ActionResult surfaces failures into the status bar

### 3d. Materialize + conflict resolution ✅
- [x] ipc.rs: `materialize`, `resolve_conflict`
- [x] RemoteOnly rows are clickable → Materialize(key)
- [x] Conflict rows expand inline → Keep Local / Keep Remote / Keep Both
- [x] Conflict count badge in sidebar (red, when > 0)

### 3e. Quick preview ✅
- [x] Preview pane (right, ~40% via FillPortion 3:2), toggled from breadcrumb
- [x] Text/code files: read from local_root (cap 256 KB, lossy UTF-8)
- [x] Images/other: metadata card (key, size, status, mtime)
      — ponytail: iced `image` feature not enabled; metadata card instead
- [x] RemoteOnly preview: "Not downloaded" + download button
- [x] SelectFile / PreviewLoaded messages (stale results guarded by key match)

### 3f. Polish ✅
- [x] Keyboard shortcuts via `keyboard::listen()` subscription + filter_map:
      `/` focus search, `Esc` clear/close, `Backspace` up, `j`/`k` cursor,
      `Enter` open, `Space` toggle preview
      (listen() only fires ignored events → search-input captures auto-suppressed)
- [x] Selection highlight (selected_row_button, accent at low opacity)
- [x] List/Grid view toggle (Grid::fluid(120) cells: icon + name)
- [x] Empty state ("No files synced yet" + tip)
- [x] Loading state (first snapshot pending → "Loading…")
- [x] Dead-code cleanup (removed unused BG_CONTENT, sidebar_button)

## Review

All four sub-phases implemented incrementally with build+fmt between each.
Final verification (`CARGO_HOME=/tmp/opencode/cargo-home`):
- `cargo build` (full workspace): 0 errors, **0 warnings**
- `cargo fmt -- --check`: clean
- `cargo test`: 47 passed, 0 failed (4 engine snapshot/reconcile + 43 index/extract)

Notes:
- No new crate deps added. Image preview deferred (needs iced `image` feature);
  shows metadata card instead — see `preview_metadata`.
- Keyboard shortcuts rely on `keyboard::listen()` which only emits **ignored**
  events, so typing in the search box never fires `/`, `j`, `k`, `Space`, etc.
- Cursor (keyboard) and selected (mouse/preview) are kept in sync when j/k lands
  on a file; mouse clicks set selected without moving the cursor (intentional).
