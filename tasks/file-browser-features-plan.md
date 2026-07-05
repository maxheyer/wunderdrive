# File Browser Standard Features — Implementation Plan

_Status: research + plan, awaiting approval before implementation._

## Goal

Bring the Files screen to parity with what a user expects from a native file
browser: multi-selection, context menus, drag-and-drop, clipboard operations,
and the keyboard/mouse interactions that make those usable.

This plan lives within the hard rules in `AGENTS.md`: **iced 0.14 only, no new
dependencies, UI-only changes in the GUI crate.** Every engine gap is stubbed
with `// TODO(engine):` — no engine or daemon edits.

---

## Research findings: iced 0.14 capability matrix

Before designing, we verified exactly what iced 0.14 gives us. Everything
below was confirmed against the vendored source in `~/.cargo/registry/src/`.

| Capability | Available? | How |
|---|---|---|
| Right-click events | ✅ | `mouse::Button::Right` in `iced::event::listen_with`; `widget::mouse_area` with `on_right_press` / `on_right_release` |
| Overlay / popover positioning | ✅ | `iced::overlay::Element` + `iced_widget::overlay::menu::Menu` for dropdown-style menus; we compose custom overlay for positioned context menus |
| Double-click | ✅ | `mouse_area::on_double_click` |
| OS file drag-and-drop INTO window | ✅ (not Wayland) | `iced::Event::Window(window::Event::{FileHovered, FileDropped, FilesHoveredLeft})` via `iced::event::listen_with` |
| Internal widget drag (move items) | ⚠️ manual | No generic DnD framework. `PaneGrid` has pane DnD but that's irrelevant. We build a custom drag layer with `mouse_area` + `on_press`/`on_move`/`on_release` + overlay positioning. |
| Multi-select list | ❌ no built-in | Must build: `BTreeSet<usize>` selection set + `keyboard::Modifiers` tracking (shift = range, ctrl/cmd = toggle) |
| Rubber-band selection in grid | ❌ | Not available; deferred — scope creep for v1 |
| Clipboard read/write | ✅ | `iced::clipboard::{read, write}` returning `Task` |

**Conclusion:** all four features are achievable with stock iced 0.14. No
dependency additions needed. The work is in state management and overlay
composition, not in fighting missing APIs.

---

## Architecture: where state lives

All new state goes on the existing `Conn` struct in `app.rs`. No new crates,
no new modules beyond optionally splitting `app.rs` if it gets unwieldy (it's
already 1700 lines — splitting view helpers into `views.rs` is a nice-to-have
but not required for this feature).

### New `Conn` fields

```rust
struct Conn {
    // ... existing fields ...

    /// Multi-selection: set of item keys (file keys + folder names) currently
    /// selected. The single `selected: Option<String>` field is replaced by
    /// this. `anchor` tracks the last clicked item for shift-range selection.
    selection: BTreeSet<String>,
    selection_anchor: Option<String>,

    /// The currently open context menu, if any. Stores the position (screen
    /// coordinates from the right-click) and what the menu is attached to
    /// (the file list background vs. a specific item).
    context_menu: Option<ContextMenu>,

    /// Drag-and-drop: internal file/folder drag state. `None` when idle.
    drag: Option<DragState>,

    /// OS file drop indicator: true while the OS is hovering files over the
    /// window, to show a drop-zone overlay.
    file_hover: bool,

    /// Clipboard: the last "copied" item reference (for paste-in-same-app).
    /// OS clipboard is used via `iced::clipboard`; this is the in-app
    /// bookkeeping for paste operations that need structured data.
    clipboard: Option<ClipboardOp>,

    /// Tracks whether shift/ctrl/cmd are currently held, so mouse clicks can
    /// branch on modifier state. Updated from `keyboard::on_key_pressed` and
    /// `keyboard::on_key_released`.
    modifiers: iced::keyboard::Modifiers,
}
```

### New types

```rust
#[derive(Debug, Clone)]
struct ContextMenu {
    /// Screen-space position for the overlay anchor (from the mouse event).
    position: iced::Point,
    /// What the menu operates on.
    target: MenuTarget,
}

#[derive(Debug, Clone)]
enum MenuTarget {
    /// Right-clicked on empty space in the file list.
    FileListBackground,
    /// Right-clicked on a specific item (it's included in selection).
    Item(String),
}

#[derive(Debug, Clone)]
struct DragState {
    /// The keys being dragged (from the current selection).
    items: Vec<String>,
    /// Current cursor position, updated on mouse move.
    position: iced::Point,
    /// Where the drag started (for delta calculations / threshold).
    origin: iced::Point,
}

#[derive(Debug, Clone, Copy)]
enum ClipboardOp {
    Copy,
    Cut,
}
```

---

## Feature 1: Multi-selection

### Selection model

- **Single click** → select only that item (clears rest). Sets `selection_anchor`.
- **Ctrl/Cmd+click** → toggle that item in the set. Sets anchor.
- **Shift+click** → range select from `selection_anchor` to clicked item, using
  the current visible-items order (same order as `visible_items()` already
  computes).
- **Click on empty space** → clear selection.
- **Esc** → clear selection (extends existing `Message::Escape`).

### Implementation

1. Replace `selected: Option<String>` with `selection: BTreeSet<String>` and
   `selection_anchor: Option<String>`.
2. Wrap each `file_row` / `folder_row` / `grid_cell` button in a `mouse_area`
   that captures `on_press` (left button) with modifier logic, and
   `on_right_press` for context menu.
3. Add `Message::SelectItem(String, SelectionMode)` where `SelectionMode` is
   `Single | ToggleAdd | Range`.
4. `Message::ClearSelection` for empty-space clicks.
5. The `cursor` field (keyboard navigation) stays as-is. When the cursor
  moves via j/k, it updates `selection` to a single-item set (the existing
  vim-style behavior).

### Key: modifier tracking

Currently `map_key` only handles key presses. We add modifier tracking:
- `keyboard::on_key_pressed` / `on_key_released` → update `c.modifiers`.
- This is read in the `SelectItem` message handler to branch on mode.
- Alternatively, capture modifiers directly in the mouse event (iced provides
  `keyboard::Modifiers` in `mouse_area`'s press callback via the global event
  listener). We'll use the event-listener approach to get current modifiers
  at click time.

### Rendering changes

- `file_row` / `folder_row` / `grid_cell` currently take a `selected: bool`.
  Change to `is_selected: bool` computed from `selection.contains(key)`.
- No visual change to the styling (existing `theme::selected_row_button` /
  `theme::grid_cell_button_selected` apply).

---

## Feature 2: Context menu

### Trigger

`mouse_area::on_right_press` on each row and on the list background. The
handler emits `Message::OpenContextMenu { target, position }`.

### Menu content

**On a file item:**
- Open (single item only) → `// TODO(engine): open with system default app`
- Copy → `Message::Clipboard(Copy)`
- Cut → `Message::Clipboard(Cut)`
- Rename → `// TODO(engine): rename / move` (deferred — see notes below)
- Delete → `// TODO(engine): delete` (deferred)
- Materialize (if `RemoteOnly`) → `Message::Materialize(key)` (already exists)
- Resolve conflict (if `Conflict`) → existing resolve submenu
- Copy path → `iced::clipboard::write(full_path)`

**On a folder item:**
- Open → `Message::Open(name)` (already exists)
- Copy → `Message::Clipboard(Copy)`
- Cut → `Message::Clipboard(Cut)`
- Copy path → clipboard

**On file list background (empty space):**
- Paste → `Message::Paste` (only if `clipboard` is `Some`)
- Select all → `Message::SelectAll`
- Sync now → `Message::SyncNow`

### Rendering the menu

iced 0.14 has no built-in context-menu widget. We compose:

1. A semi-transparent full-window overlay (`stack` top layer) that captures
   clicks outside the menu to dismiss it.
2. A positioned `container` at `context_menu.position` rendering a
   `column` of menu-item buttons styled as `theme::card_container` with
   `theme::row_button`-style items.
3. The overlay is rendered in `main_layout` via `stack` when
   `c.context_menu.is_some()` — same pattern as the existing toast overlay.

### Dismissal

- Click outside (the background overlay captures `on_press` →
  `Message::CloseContextMenu`).
- Esc key (extends `Message::Escape` to close menu first).
- Any menu action closes the menu.

### Key bindings

- Right-click is the primary trigger.
- Menu button shortcuts: Cmd+C / Ctrl+C (copy), Cmd+X (cut), Cmd+V (paste),
  Cmd+A (select all), Delete (delete — stubbed). These are added to `map_key`.

---

## Feature 3: Drag and drop

### 3a. OS file drag-and-drop INTO the window

**Detection:** Extend `map_mouse` (or add a parallel event listener) to handle
`iced::Event::Window(window::Event::FileDropped(path))` etc. These come through
`iced::event::listen_with`.

**Behavior:**
- `FileHovered(_)` → set `c.file_hover = true`. Render a drop-zone overlay
  (dashed border over the file list area, "Drop to upload" text).
- `FileDropped(path)` → `// TODO(engine): ingest file into mirror`. The GUI
  can't touch the filesystem per the architecture rules. This emits a new
  `Message::FileDropped(PathBuf)` that, for now, shows a toast:
  "File ingest not yet wired to engine." The engine would need an IPC method
  like `METHOD_INGEST` — that's an engine change, out of scope here.
- `FilesHoveredLeft` → set `c.file_hover = false`.

**Scope note:** The drop indicator and message plumbing are GUI work. The
actual file ingest is `// TODO(engine):`. This is the one feature that has a
hard dependency on a future engine IPC method. We implement the full GUI side
(toast + overlay + wiring) and stub the ingest.

### 3b. Internal drag (move files between folders)

iced 0.14 has no generic DnD. We build a minimal custom implementation:

**Drag start:** `mouse_area::on_press` on a file row, with left button held +
mouse moves > N pixels (drag threshold, ~5px). This avoids starting a drag on
every click. The drag captures the current `selection`.

**Drag visual:** A floating overlay (same `stack` mechanism as toasts/menus)
rendering a small card at the cursor position showing the item count being
dragged ("3 items" or the single filename). This overlay is rendered when
`c.drag.is_some()`.

**Drag over:** `mouse_area::on_move` on folder rows sets a `drag_over` state
to highlight the target folder.

**Drop:** `mouse_area::on_release` on a folder → `Message::DropOnFolder {
items, folder }`. This emits `// TODO(engine): move files` — same stub
pattern as file ingest.

**Scope note:** Internal file-move requires an engine IPC method
(`METHOD_MOVE`). The GUI side (drag visual, drop detection, hover highlight)
is fully implementable now. The actual move is stubbed.

**Pragmatic recommendation:** Implement 3a (OS drop) fully on the GUI side.
For 3b (internal drag), implement the drag visuals and drop detection but
mark the engine move as `// TODO(engine):`. Internal drag is lower priority —
consider deferring it to a second pass if we want to ship 1–4 faster.

---

## Feature 4: Clipboard operations (copy/cut/paste)

### Copy path (always available, no engine needed)

Right-click → "Copy path" writes the full local path to the OS clipboard via
`iced::clipboard::write(path)`. This works immediately — no engine dependency.

### Copy/Cut/Paste (structured, for in-app move/copy)

**Copy** → stores the selected keys + `ClipboardOp::Copy` in `c.clipboard`.
**Cut** → same with `ClipboardOp::Cut`.
**Paste** → reads `c.clipboard`:
  - `Copy` → `// TODO(engine): copy files to new location`
  - `Cut` → `// TODO(engine): move files to new location`

Both require engine IPC methods (`METHOD_COPY`, `METHOD_MOVE`) that don't
exist yet. The GUI plumbing (menu items, keyboard shortcuts, state tracking)
is implementable now. The actual operations are stubbed.

### OS clipboard integration

`iced::clipboard::write(path)` for "Copy path" is the only immediately
functional clipboard feature. The structured copy/cut/paste within the app
is internal state only — it doesn't go through the OS clipboard because the
structured data (selected keys) isn't a string the OS would understand.

---

## Implementation phases

### Phase 1: Multi-selection (no engine deps, fully functional)

- Replace `selected: Option<String>` with `selection: BTreeSet<String>` +
  `selection_anchor`.
- Add `Message::SelectItem(String, SelectionMode)`, `Message::ClearSelection`,
  `Message::SelectAll`.
- Wrap rows in `mouse_area` with modifier-aware `on_press`.
- Update all `file_row` / `folder_row` / `grid_cell` callsites to use
  `selection.contains(key)`.
- Add Cmd/Ctrl+A for select all, Esc clears selection.
- Keyboard cursor (j/k) updates `selection` to single-item set.

**Deliverable:** Click, shift-click, ctrl-click, select-all, clear — all work.

### Phase 2: Context menu (no engine deps for UI; actions partially stubbed)

- Add `Conn::context_menu`, `ContextMenu`, `MenuTarget` types.
- Add `Message::OpenContextMenu { position, target }`, `Message::CloseContextMenu`,
  and action messages for each menu item.
- Wrap rows and list background in `mouse_area` with `on_right_press`.
- Render the menu as an overlay in `main_layout` (stack top layer).
- Wire functional actions: Open, Materialize, Copy path, Resolve conflict,
  Select all.
- Stub actions with `// TODO(engine):`: Rename, Delete, structured Copy/Cut/Paste.
- Add keyboard shortcuts: Cmd+C/X/V/A, Delete.

**Deliverable:** Right-click shows a styled context menu; functional items work,
engine-dependent items show a toast.

### Phase 3: OS file drag-and-drop INTO window (GUI complete, ingest stubbed)

- Add `Conn::file_hover: bool`.
- Extend the event listener (`iced::event::listen_with`) to capture
  `Window(FileHovered/FileDropped/FilesHoveredLeft)`.
- Render drop-zone overlay when `file_hover` is true.
- `Message::FileDropped(PathBuf)` → toast "File ingest not yet wired" (or
  queue for engine).

**Deliverable:** Dragging files from the OS file manager into wunderdrive shows
a drop zone; dropping shows feedback. Actual ingest is `// TODO(engine):`.

### Phase 4: Internal drag-and-drop (GUI complete, move stubbed)

- Add `Conn::drag: Option<DragState>`, `DragState` type.
- Implement drag threshold detection via `mouse_area` press + move.
- Render drag overlay (floating item-count card at cursor).
- Highlight folder targets on drag-over.
- `Message::DropOnFolder { items, folder }` → toast "Move not yet wired" or
  engine call.

**Deliverable:** Dragging files onto folders shows drag visuals and drop
targeting. Actual move is `// TODO(engine):`.

---

## What's NOT in this plan (deferred)

Per `spec.md` §3 and `DESIGN.md` §7:

- **Rubber-band selection in grid view** — not in iced 0.14 without significant
  custom widget work. Deferred.
- **Rename** — needs an inline text editor in the row + engine rename IPC.
  Separate feature; deferred to its own task.
- **Delete** — needs engine delete IPC + confirmation dialog. Deferred.
- **Thumbnails/previews** — explicitly forbidden by design anti-scope.
- **Properties/info panel** — not a standard browser feature in scope here;
  could be a context-menu item in a future task.

---

## Files to modify

| File | Changes |
|---|---|
| `crates/wunderdrive-gui/src/app.rs` | All state, messages, update logic, and view rendering for the four features. This is the primary file. |
| `crates/wunderdrive-gui/src/theme.rs` | New styles: `context_menu_container`, `menu_item_button`, `menu_item_button_hover`, `drop_zone_overlay`, `drag_card`. |
| `crates/wunderdrive-gui/src/icons.rs` | New icons needed: `copy`, `scissors` (cut), `clipboard-paste`, `trash`, `pencil` (rename), `external-link` (open). Download from Lucide. |

**No engine, daemon, or protocol changes.** All engine gaps are stubbed with
`// TODO(engine):` comments.

---

## Risk assessment

| Risk | Mitigation |
|---|---|
| `mouse_area` on every row may hurt performance with large file lists | The existing `column` already renders every row; `mouse_area` is a thin wrapper. If perf is an issue, the real fix is virtualized scrolling, which is a separate task. |
| Context menu overlay positioning may be tricky with scroll offsets | Use the mouse event's screen coordinates directly; iced overlays are positioned relative to the window, not scroll containers. Test on a real desktop. |
| Modifier tracking via event listener may have edge cases on different platforms | All major platforms report modifiers consistently in iced 0.14. Test on Linux + macOS. |
| Internal DnD (Phase 4) is the most complex and least certain | It's last; Phases 1–3 deliver value independently. Can defer 4 if it proves unstable. |

---

## Verification

After each phase:
```bash
CARGO_HOME=/tmp/wd-cargo cargo check -p wunderdrive-gui
CARGO_HOME=/tmp/wd-cargo cargo fmt -p wunderdrive-gui
```

Then manual test on a real desktop (GUI can't run headless):
- Phase 1: click/shift-click/ctrl-click/select-all/clear in both list and grid view.
- Phase 2: right-click on file/folder/empty space; verify menu styling and functional items.
- Phase 3: drag files from file manager into window; verify drop zone + toast.
- Phase 4: drag files onto folders; verify drag visual + target highlight.
