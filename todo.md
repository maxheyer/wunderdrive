# wunderdrive — TODO

## Done

- [x] **Implement file opening** — double-click, Enter, and context-menu "Open"
  now spawn the OS default handler (`open` / `xdg-open` / `explorer`) on
  `local_root/<key>`. Remote-only files materialize first; conflicts expand
  their resolver. (`app.rs` — `Message::OpenFile`, `open_path`)

- [x] **Fix: context menu rerenders thousands of times on hover** — root cause
  was `CursorMoved` firing on every pixel of mouse movement, unconditionally
  producing a `Message::CursorMoved`, which triggers a full re-view (including
  the context menu overlay) each time. Fix: the event listener now tracks
  cursor position in atomics and only forwards `CursorMoved` while the left
  mouse button is held (drag detection). Right-click positioning reads from
  the atomics via `last_cursor_pos()` instead of stale `Conn::cursor_pos`.

---

## Blocked — conflicts with DESIGN.md §7 anti-scope

These two items are **forbidden by the current design system**. DESIGN.md §7
states:

> **No previews or thumbnails.** The design forbids them entirely. Do not add
> a preview pane, image thumbnails, or a preview toggle.

Before implementing either, the design system must be updated (or the
anti-scope rule explicitly revised). The analysis below assumes that revision.

### File preview (even for little icons)

**What it means:** Generate and show OS-native thumbnails in the file list /
grid, replacing the four-category Lucide type icons (`file`, `file-text`,
`image`, `folder`) with actual mini-previews for images, PDFs, videos, etc.

**Conflict:** Directly violates §7. The `type_icon()` function in `icons.rs`
is the entire icon system; replacing it with thumbnails is a fundamental
design change.

**If approved, the minimal path:**
1. Add a thumbnail cache in the engine (daemon-side), keyed by S3 key +
   blake3 hash. Generate on first request, store under
   `~/.cache/wunderdrive/thumbs/`.
2. New IPC method `METHOD_THUMBNAIL` → returns PNG bytes (or a file path).
3. GUI fetches thumbnails lazily (only for visible rows/tiles).
4. Keep the Lucide icon as the fallback / placeholder while loading and for
   non-previewable types.

**Scope estimate:** Medium. Engine thumbnail generation + cache + IPC + GUI
async fetch + cache. ~2-3 days.

### Photo mode (Apple Photos-style, same speed)

**What it means:** A dedicated full-bleed photo gallery view — grid of
thumbnails, click to enlarge, arrow-key navigation, instant scrolling through
thousands of images.

**Conflict:** Requires thumbnails (§7). Also a major new screen not in the
current §4 Screens table.

**If approved, the minimal path:**
1. Depends on the thumbnail cache above.
2. New `Screen::Photos` that filters `snapshot.files` to image extensions
   (same set as `type_icon`'s image branch).
3. Virtualized grid — iced doesn't virtualize natively, so a large photo
   library will be slow without a custom widget. This is the hard part for
   "same speed" as Apple Photos (which uses metal-accelerated tiling).
4. Full-screen lightbox overlay with left/right navigation.

**Scope estimate:** Large. The virtualization problem is the blocker — iced
0.14's `scrollable` + `column` builds every element. For thousands of photos
this will not match Apple Photos speed without a custom virtualized scroll
widget. ~1-2 weeks, and may need an iced contribution upstream.

---

## Research: macOS Finder concepts to apply

Researched Finder's UX patterns. The following are **applicable to wunderdrive
without violating the design system**, ranked by impact:

### High-impact, design-system-friendly

1. **Sort by clicking column headers (List view)** — currently files are
   unsorted (BTreeSet gives alphabetical folders, files in snapshot order).
   Add clickable "Name / Size / Modified / Kind" column headers in list view
   that sort the file list. Low effort, high value.

2. **Grouping / sections** — group files by "Today / Previous 7 Days /
   Previous 30 Days / Older" (Finder's time buckets) or by Kind. Render as
   sticky section headers in the scrollable list. This is the single biggest
   visual upgrade for browsing.

3. **Relative dates** — replace "3 Jul" with "Today / Yesterday / 2 days ago"
   for recent files. Finder does this and it reads much faster.

4. **Path bar (toggleable)** — a thin breadcrumb at the bottom of the file
   list showing the current path with clickable segments. DESIGN.md §3.2 has
   the breadcrumb in the toolbar, but Finder's bottom path bar is an optional
   alternative some users prefer. Could be a setting.

5. **⌘1 / ⌘2 for view switching** — currently `ToggleViewMode` is bound to
   the toolbar button only. Add ⌘1 (list) / ⌘2 (grid) keyboard shortcuts.

### Medium-impact

6. **Folder size calculation** — show recursive folder sizes (Finder's
   "Calculate all sizes"). Needs engine support to walk the tree.

7. **Per-folder view memory** — remember list vs grid per folder path.
   Currently `view_mode` is global on `Conn`.

8. **Column view (Miller Columns)** — a third view mode showing the hierarchy
   horizontally. This is a significant addition but solves the deep-navigation
   problem elegantly. Would need DESIGN.md §3 and §1.7 updates.

9. **Tags / colored labels** — a cross-folder organizational layer. Needs
    engine support (tag storage in the journal). Big feature, high value for
    a sync tool where folder structure is mirrored from S3.

### Already present in wunderdrive

- ✓ Back/forward navigation (browser history)
- ✓ Search (⌘K)
- ✓ Grid/list toggle
- ✓ Sidebar with nav items
- ✓ Context menu (copy, cut, paste, open, delete)
- ✓ Multi-select (shift, ⌘/ctrl)
- ✓ Drag-and-drop
- ✓ Keyboard navigation (j/k, enter, esc)

### Not recommended

- **Disclosure triangles in list view** — DESIGN.md §7 explicitly forbids
  them: "No disclosure triangles. Folders use enter-and-breadcrumb navigation."
- **Free-form icon placement** — wunderdrive's grid is a wrap layout, not
  spatial. This is correct for a sync tool; spatial arrangement doesn't
  survive sync.
