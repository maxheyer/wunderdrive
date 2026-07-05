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

### Phase 2b: Lazy download + stale index fix (uncommitted, 43 tests green)
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

# Desktop Client — Tauri 2 + React

## Locked decisions
- **TUI**: removed entirely (crate deleted)
- **Architecture**: daemon + GUI client. Daemon serves IPC over local socket;
  Tauri app connects as a client
- **Daemon lifecycle**: Tauri spawns the daemon internally (refactor daemon
  into a lib + binary so Tauri runs it in a background thread). Single-app UX.
- **Shell**: Tauri 2 (same as Spacedrive — native webview, small binary)
- **Frontend**: React 19 + TypeScript + Vite
- **Styling**: Tailwind CSS v4 + shadcn/ui (Radix-based, MIT, accessible)
- **Data layer**: TanStack Query (caching, optimistic updates, background refetch)
- **Virtualization**: @tanstack/react-virtual (100k+ files at 60fps)
- **Animations**: Framer Motion
- **Type bridge**: Specta + tauri-specta (auto-generate TS types from Rust wire types)
- **MVP features**: file browser (grid+list), full-text search, sync status + controls,
  materialize (lazy download), conflict resolution, quick preview

## Architecture
```
wunderdrive/
├── crates/
│   ├── wunderdrive-engine/     # Core (unchanged)
│   └── wunderdrive-daemon/     # lib.rs (start_daemon) + main.rs (binary)
├── apps/
│   └── desktop/                # NEW — Tauri 2 app
│       ├── src/                # React frontend
│       │   ├── components/     # FileGrid, SearchBar, SyncStatus, etc.
│       │   ├── hooks/          # useSnapshot, useSearch, useMaterialize
│       │   ├── lib/            # daemon client wrapper
│       │   └── App.tsx
│       ├── src-tauri/
│       │   └── src/            # main.rs, ipc.rs (socket client), commands.rs
│       ├── package.json
│       └── tailwind.config.ts
└── packages/
    └── ts-types/               # Specta-generated TypeScript types
```

Data flow: `React → Tauri command → Unix socket → daemon → Engine`

## Sub-phases (each ships a working app)

### 3a. Scaffold (~2h)
- [ ] Delete `crates/wunderdrive-tui/`, update workspace Cargo.toml
- [ ] Refactor `wunderdrive-daemon` into `lib.rs` (`start_daemon(socket, engine)`)
  + thin `main.rs` binary
- [ ] Add Specta `#[derive(Type)]` to engine wire types (Snapshot, Status,
  SearchHit, FileStat, FileStatus, ActivityEntry, Resolution)
- [ ] `cargo create-tauri-app` → `apps/desktop/` with React + TS template
- [ ] Tauri Rust backend: spawn daemon thread on startup, connect socket
- [ ] NixOS `flake.nix` (webkit2gtk_4_1, gtk3, nodejs, bun, cargo)
- [ ] Minimal "connected to daemon" screen — app boots, daemon health check
- [ ] cargo fmt + test still green

### 3b. File browser (~3h)
- [ ] `useSnapshot()` hook (TanStack Query, 10Hz poll or event-driven)
- [ ] Virtualized file grid (@tanstack/react-virtual)
- [ ] Virtualized file list (toggle grid/list)
- [ ] Status icons (synced ✓, uploading ↑, new +, remote-only ☁, conflict !)
- [ ] Sort by name / size / mtime
- [ ] Path breadcrumb (navigate subdirectories)

### 3c. Search + sync (~3h)
- [ ] Search bar (always visible, 150ms debounce)
- [ ] Live results dropdown (snippet + highlighted match)
- [ ] Dedicated search results view (Enter)
- [ ] Sync status bar (endpoint, bucket, last sync, paused)
- [ ] Pause/Resume button
- [ ] Sync Now button
- [ ] Activity feed (streaming via Tauri event channel)

### 3d. Lazy + conflicts (~2h)
- [ ] Materialize flow (click cloud icon → progress → synced)
- [ ] Conflict badge / count in sidebar
- [ ] Conflict view: local vs remote side-by-side
- [ ] Keep Local / Keep Remote / Keep Both buttons

### 3e. Quick preview (~3h)
- [ ] Preview pane (right side or modal)
- [ ] Text/code: syntax highlighting (Shiki)
- [ ] Images: direct render from local mirror
- [ ] PDF: pdf.js viewer
- [ ] Other: metadata card (size, hash, status, mtime)

### 3f. Polish (~3h)
- [ ] Dark mode (default) + light mode toggle
- [ ] Smooth transitions (Framer Motion: layout, enter/exit)
- [ ] Responsive layout (resize handles, min widths)
- [ ] Keyboard shortcuts (j/k navigate, / search, space preview, etc.)
- [ ] App icon + window title
- [ ] Loading states + error boundaries

## Review
(filled in after each sub-phase)
