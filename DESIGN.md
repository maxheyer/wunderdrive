# wunderdrive — GUI Design System

_A Spacedrive-inspired dark shell: calm, layered near-black surfaces, violet
accent, a strict six-state sync language, Lucide type icons, search as the hero
feature._

This document is the **authoritative spec** for the wunderdrive GUI. The token
values, icon set, and component anatomy defined here must match the code in
`crates/wunderdrive-gui/src/theme.rs` (tokens) and `app.rs` / `icons.rs`
(components). If the code and this document disagree, the code wins — fix the
document.

---

## 1. Tokens (`theme.rs` — single source of truth)

All colors, fonts, spacing, and radii live as `pub const` in `theme.rs`. **Hex
literals outside `theme.rs` are defects.** Views reference tokens by name; the
grep `color!|Color::from_rgb` in `app.rs` or `icons.rs` must return zero hits.

### 1.1 Surfaces & strokes

| Token | Hex | Used for |
|---|---|---|
| `BG_APP` | `#0B0D13` | Window background, toolbar |
| `BG_SIDEBAR` | `#0E1119` | Sidebar |
| `BG_SURFACE` | `#151A26` | Cards, search pill, grid cells |
| `BG_ELEVATED` | `#1B2233` | Toasts, elevated panels |
| `BG_HOVER` | white @ 4% | Row / button hover |
| `BG_SELECTED` | accent @ 14% | Selected row, active nav item |
| `BG_KBD` | white @ 5% | Keyboard shortcut chips |
| `STROKE_SUBTLE` | white @ 7% | Default 1px borders |
| `STROKE_STRONG` | white @ 14% | Hover borders |

### 1.2 Text hierarchy

| Token | Hex | Role |
|---|---|---|
| `TEXT_PRIMARY` | `#EDF0F7` | File names, titles, breadcrumb |
| `TEXT_SECONDARY` | `#9AA3B8` | Body text, nav labels, pill label |
| `TEXT_TERTIARY` | `#5D6579` | Captions, meta, placeholders |
| `TEXT_ON_ACCENT` | `#FFFFFF` | Text on violet buttons |

### 1.3 Accent (violet)

| Token | Hex | Used for |
|---|---|---|
| `ACCENT` | `#8B5CF6` | Primary buttons, links, focus rings |
| `ACCENT_HOVER` | `#A78BFA` | Button hover state |
| `ACCENT_ACTIVE` | `#7C3AED` | Button pressed state |
| `ACCENT_TEXT` | `#C4B5FD` | Active nav item text/icon |
| `ACCENT_TINT` | accent @ 14% | Same as `BG_SELECTED` |

### 1.4 Sync-state language (the core system)

One state per file, same color + glyph everywhere (rows, tiles, sidebar pill,
activity). **Green is exclusively Synced — never use it for "syncing".**

| State | Token | Hex | Lucide glyph |
|---|---|---|---|
| Synced | `SYNC_SYNCED` | `#34D399` | `check-circle-2` |
| Syncing | `SYNC_SYNCING` | `#8B5CF6` (accent) | `refresh-cw` (animated rotation) |
| Queued | `SYNC_QUEUED` | `#7B8496` | `clock` |
| Conflict | `SYNC_CONFLICT` | `#FBBF24` | `alert-triangle` |
| Error | `SYNC_ERROR` | `#F87171` | `x-circle` |
| Remote-only | `SYNC_REMOTE` | `#38BDF8` | `cloud` |

Engine state mapping (`FileStatus` → sync state):

| Engine `FileStatus` | Design state | Derivation |
|---|---|---|
| `Synced` | Synced | Journal present + hashes match |
| `PendingUpload` | Syncing | Transfer in flight |
| `NewLocal` | Syncing | Awaiting sync loop |
| `DeletedPending` | Queued | Awaiting deletion |
| `Conflict` | Conflict | Conflict copy exists |
| `RemoteOnly` | Remote-only | Listed remotely, absent locally |

If per-file granularity isn't exposed by the engine, stub with `// TODO(engine)`.

### 1.5 Fonts

| Font | File | Role |
|---|---|---|
| **Inter** (variable) | `assets/fonts/InterVariable.ttf` | Default UI font |
| **JetBrains Mono** | `assets/fonts/JetBrainsMono-Regular.ttf` | Paths, sizes, hashes, kbd chips |

Both OFL. Loaded via `iced::font::load` at startup; Inter set as `default_font`
in `main.rs`.

### 1.6 Typography scale

| Style | Size | Weight |
|---|---|---|
| stat | 24 | Bold (700) |
| title | 16 | Semibold (600) |
| section | 13 | Semibold (600) |
| body | 13 | Regular (400) |
| body-strong | 15 | Medium (500) |
| caption | 12 | Regular, tertiary |
| label | 11 | Semibold, tertiary |
| mono | 12 | JetBrains Mono |

### 1.7 Layout dimensions

| Component | Value |
|---|---|
| Sidebar width | 232px |
| Toolbar height | 48px |
| List row height | 48px |
| Grid tile | 152×152px |
| Grid type icon | 56px |
| List type icon | 22px |
| Sync glyph | 16px |
| Min window | 960×640 (target) |

### 1.8 Radius & borders

| Element | Radius |
|---|---|
| Kbd chips | 4px |
| Buttons, inputs, nav items, tiles | 8px |
| Cards (stat, sync) | 12px |
| Pills (sync, badge) | full (999px) |

All borders are 1px, using `STROKE_SUBTLE` (default) or `STROKE_STRONG` (hover).

---

## 2. Icons (`icons.rs`)

Bundled Lucide SVGs from `assets/icons/`, rendered monochrome-tinted via
`svg::Style { color: Some(color) }`. Each icon is a function returning
`svg::Svg<'static, iced::Theme>` taking a `Color`.

### 2.1 Icon set (20 glyphs)

`folder`, `file`, `file-text`, `image`, `check-circle-2`, `refresh-cw`, `clock`,
`alert-triangle`, `x-circle`, `cloud`, `search`, `settings`, `arrow-left`,
`layout-grid`, `list`, `pause`, `play`, `chevron-right`, `plus`, `x`

### 2.2 Type-icon mapping (v1)

Four categories only — keep it simple:

| File type | Icon |
|---|---|
| folder | `folder` (may carry the violet gradient — brand signature) |
| pdf, doc, txt, md, rtf | `file-text` |
| png, jpg, jpeg, gif, webp, bmp, svg, tiff, ico | `image` |
| everything else | `file` |

### 2.3 Adding an icon

1. Download the SVG from the [Lucide](https://lucide.dev) icon set into
   `assets/icons/`.
2. Add an `icon_fn!(name, "file")` line in `icons.rs`.
3. Use it: `icons::name(theme::SOME_COLOR)`.

The SVG must use `stroke="currentColor"` (standard Lucide) so the tint works.

---

## 3. Shell anatomy

### 3.1 Sidebar (232px)

```
┌─────────────────────┐
│ bucket-name    ›    │  ← header (switcher style, 14px semibold)
│                     │
│ ◎ Overview          │  ← nav items (32px, radius 8)
│ 📁 Files            │     active = BG_SELECTED + ACCENT_TEXT
│ 🔍 Search           │     hover = BG_HOVER
│ ⚠ Conflicts    !    │     amber badge when conflicts > 0
│ ↻ Activity          │
│                     │
│ FOLDERS             │  ← label style (11px bold tertiary)
│ 📁 mirror-root      │
│ + Add Folder        │  ← dashed, stub action
│                     │
│                     │
│ ──────────────────  │  ← divider (STROKE_SUBTLE)
│ ⚙ Settings          │
│ ● Synced · 2m  ↻ ⏸ │  ← sync pill + icon-buttons
└─────────────────────┘
```

### 3.2 Toolbar (48px)

```
│ ← │ bucket / path  ·  24 items          [🔍 Search…  ⌘K]  [▦] │
```

- **Back chevron** — navigates up one folder (disabled at root)
- **Breadcrumb** — title style (16px), with item count caption (12px tertiary)
- **Search pill** — centered, `BG_SURFACE`, radius 8, placeholder "Search
  documents…", right-aligned kbd chip showing `⌘K` (macOS) / `Ctrl+K` (else)
- **Grid/List toggle** — icon button, toggles `ViewMode`

### 3.3 File row (48px)

```
│ ✓  📄  invoice-2024.pdf            65.3 KB · 3 Jul │
```

- Sync glyph (16px) · type icon (22px) · name (15px, Fill) · meta (12px mono,
  tertiary)
- Hover: `BG_HOVER`. Selected: `BG_SELECTED`.
- Conflict rows: 2px amber left edge.
- Folder rows: folder icon + name + chevron-right (no size). Click enters the
  folder.

### 3.4 Grid tile (152×152)

```
│       📄       │
│   report.pdf   │
│       ✓        │
```

Type icon (56px, centered) · name (12px) · sync glyph (16px). Built with native
`Row::wrap()` — tiles reflow at any window width.

---

## 4. Screens

| Screen | Content |
|---|---|
| **Overview** | Stat strip (total / synced / conflicts) · sync card with Sync-now + Pause/Resume · recent activity (last 5) |
| **Files** | File browser (list or grid), breadcrumb navigation |
| **Search** | Same search input, results replace file list when query non-empty |
| **Conflicts** | List of conflict rows with inline resolver (Keep Local / Keep Remote / Keep Both) |
| **Activity** | Timestamped log from `METHOD_ACTIVITY` |
| **Settings** | Bucket, endpoint, prefix, local root (key-value, mono font) |

Every screen has an empty state — never a blank pane.

---

## 5. Interaction patterns

### Navigation

- **Click folder** → enter folder (pushes to history)
- **Back chevron / Backspace / mouse back / Alt+←** → navigate up
- **Mouse forward / Alt+→** → navigate forward (history stack)
- **⌘K / Ctrl+K / `/`** → focus search
- **Esc** → clear search, or go back to Files from Search
- **`j` / `k`** → move cursor down / up
- **Enter** → activate cursor (open folder or select file)

### Sync controls

- **Sync now** (icon button in sidebar pill, or primary button on Overview) →
  triggers immediate sync cycle
- **Pause/Resume** (icon button in sidebar pill, or gray button on Overview) →
  toggles sync loop

### Mouse

- Back/forward mouse buttons (button 8/9) navigate folder history
- The whole window captures these via `iced::event::listen_with`

---

## 6. Animation

- **Syncing glyph** — rotates continuously via a 50ms tick subscription that
  advances `sync_phase`. Only animates when `SYNC_SYNCING` state is active.
- **Toasts** — error toasts appear bottom-right, auto-dismiss after 4 seconds.
- **Focus ring** — search input shows 2px accent border when focused.

---

## 7. Anti-scope (forbidden)

- **No previews or thumbnails.** The design forbids them entirely. Do not add a
  preview pane, image thumbnails, or a preview toggle.
- **No disclosure triangles.** Folders use enter-and-breadcrumb navigation.
- **No footer status strip.** Last-sync lives in the sidebar pill; item count
  in the breadcrumb caption.
- **No new styling dependencies.** No `iced_aw`, no styling crates. Fonts as
  bytes, icons as bundled SVGs, `Row::wrap()` is native.

---

## 8. File map

```
crates/wunderdrive-gui/
├── src/
│   ├── main.rs       # Entry: iced app builder, font loading
│   ├── app.rs        # State, Message, update, view (all screens)
│   ├── icons.rs      # Lucide SVG icon functions
│   ├── theme.rs      # Tokens + style functions (single color source)
│   └── ipc.rs        # Local socket client (reuses engine protocol types)
├── assets/
│   ├── fonts/        # InterVariable.ttf, JetBrainsMono-Regular.ttf
│   └── icons/        # 20 Lucide SVGs
└── Cargo.toml
```
