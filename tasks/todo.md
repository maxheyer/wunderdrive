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

### 3b. File browser (~3h)
- [ ] Subscription: poll METHOD_SNAPSHOT every 1s, emit SnapshotFetched
- [ ] Current-directory view: filter snapshot by path prefix
- [ ] Scrollable list (iced `scrollable` + `column`) of file rows
- [ ] Status glyphs per row: ✓ synced, ↑ uploading, + new, ☁ remote-only,
      ! conflict
- [ ] Sort by name / size / mtime (column header click)
- [ ] Path breadcrumb (navigate into/out of subdirs)

### 3c. Search + sync (~3h)
- [ ] Search bar (text_input, 150ms debounce via Subscription time throttle)
- [ ] Live results dropdown (METHOD_SEARCH, show snippet + key)
- [ ] Dedicated search results view (Enter → full-width list)
- [ ] Sync status bar (endpoint, bucket, last sync, paused indicator)
- [ ] Pause/Resume + Sync Now buttons (METHOD_PAUSE/RESUME/SYNC_NOW)
- [ ] Activity feed (poll METHOD_ACTIVITY, scrolling log)

### 3d. Lazy + conflicts (~2h)
- [ ] Click ☁ glyph → METHOD_MATERIALIZE → row flips to ↑ then ✓
- [ ] Conflict count badge in sidebar (count ! rows in snapshot)
- [ ] Conflict view: list conflicts, Keep Local / Keep Remote / Keep Both
      buttons (METHOD_RESOLVE_CONFLICT)

### 3e. Quick preview (~3h)
- [ ] Preview pane (right column, toggled)
- [ ] Text/code: read from local mirror, show in scrollable text widget
- [ ] Images: `iced::widget::image` from local mirror path
- [ ] Other: metadata card (size, blake3, status, mtime)

### 3f. Polish (~3h)
- [ ] Custom dark theme palette (iced `Theme` customization)
- [ ] Keyboard shortcuts (j/k navigate, / focus search, space preview, etc.)
- [ ] App icon + window title
- [ ] Loading/empty/error states
- [ ] Grid view (icons + names) vs list view toggle

## Review
(filled in after each sub-phase)
