# AGENTS.md

Guide for AI coding agents working on wunderdrive. Read this before editing.

## Quick orientation

- **`spec.md`** — the product: what wunderdrive is, the sync engine, S3 mirror,
  invariants. Read this to understand what the app does.
- **`DESIGN.md`** — the GUI design system: tokens (colors, fonts, spacing),
  component anatomy, sync-state language, icon set, anti-scope. This is the
  authoritative spec for the GUI. Read this before touching anything in
  `crates/wunderdrive-gui/`.
- **`tasks/todo.md`** — completed work and known debt.

## Architecture

Three crates, one workspace:

```
wunderdrive-engine   # Core: S3 mirror, blake3 hashing, journal, reconcile, search index
wunderdrive-daemon   # Binary: owns the engine, serves IPC over local socket
wunderdrive-gui      # Binary: iced 0.14 desktop client, connects to daemon via IPC
```

The GUI is a **thin client**. It renders state and sends commands — it never
touches the engine, S3, or the filesystem directly. All data flows through IPC.

## Hard rules

1. **iced 0.14 API, not older.** Most iced code in training data targets
   ≤0.12 and will not compile. Use the `iced::application()` builder,
   closure-based styling (`.style(|theme, status| …)`), `border::radius()`,
   `Task`, not `Command`. When unsure, read the iced 0.14 source in
   `~/.cargo/registry/src/` or check `docs.rs/iced/0.14`.
2. **Never change the iced version** to make code compile. Fix the code.
3. **UI-only changes in the GUI crate.** Do not edit engine, sync, journal,
   index, or S3 modules. Missing engine data → stub with `// TODO(engine):`.
4. **No new dependencies.** No `iced_aw`, no styling crates. Fonts as bytes,
   icons as bundled SVGs, `Row::wrap()` is native.
5. **Hex literals outside `theme.rs` are defects.** Before finishing, grep
   views for `color!` / `Color::from_rgb` — hits must only be in `theme.rs`.
6. **Green never means "syncing".** See `DESIGN.md` §1.4 for the six-state
   sync language.
7. **No previews or thumbnails.** The design anti-scope forbids them entirely.

## Build & verify

```bash
# Cargo home may be read-only in some environments — use a writable one:
CARGO_HOME=/tmp/wd-cargo cargo check -p wunderdrive-gui
CARGO_HOME=/tmp/wd-cargo cargo fmt -p wunderdrive-gui
CARGO_HOME=/tmp/wd-cargo cargo test -p wunderdrive-engine
```

The GUI can't run in headless sandboxes (needs Wayland/X11). Test on a real
desktop.

## Design system reference

All tokens, icons, component anatomy, and interaction patterns are documented in
**`DESIGN.md`**. When working on the GUI:

- Read `DESIGN.md` first.
- Colors come from `theme.rs` constants — never hardcode.
- Icons come from `icons.rs` — all are bundled Lucide SVGs.
- Fonts are Inter (default) + JetBrains Mono (mono).
