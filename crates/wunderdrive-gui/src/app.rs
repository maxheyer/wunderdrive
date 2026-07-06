use std::collections::BTreeSet;
use std::time::Duration;

use iced::keyboard::{self, key::Named, Key, Modifiers};
use iced::widget::{
    button, column, container, mouse_area, row, scrollable, space::Space, stack, text, text_input,
};
use iced::{Alignment, Element, Length, Point, Subscription, Task};
use wunderdrive_engine::protocol::Resolution;
use wunderdrive_engine::{ActivityEntry, FileStat, FileStatus, SearchHit, Snapshot, Status};

use crate::{icons, ipc, theme};

const SOCKET: &str = "wunderdrive";
const SEARCH_LIMIT: usize = 100;
const SEARCH_DEBOUNCE_MS: u64 = 150;
const SEARCH_ID: iced::widget::Id = iced::widget::Id::new("search");
const SIDEBAR_WIDTH: f32 = 232.0;
const TOOLBAR_HEIGHT: f32 = 48.0;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
static LEFT_DOWN: AtomicBool = AtomicBool::new(false);
static CURSOR_X: AtomicU32 = AtomicU32::new(0);
static CURSOR_Y: AtomicU32 = AtomicU32::new(0);

/// Read the last known cursor position captured by the event listener.
fn last_cursor_pos() -> Point {
    Point::new(
        f32::from_bits(CURSOR_X.load(Ordering::Relaxed)),
        f32::from_bits(CURSOR_Y.load(Ordering::Relaxed)),
    )
}

pub struct App {
    state: AppState,
}

enum AppState {
    Connecting,
    Connected(Conn),
    Error(String),
}

/// The five navigable screens in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Files,
    Search,
    Conflicts,
    Activity,
    Settings,
}

struct Conn {
    status: Status,
    snapshot: Snapshot,
    path: String,
    nav_history: Vec<String>,
    nav_future: Vec<String>,
    search_query: String,
    search_hits: Vec<SearchHit>,
    last_error: Option<String>,
    expanded_conflict: Option<String>,
    selection: BTreeSet<String>,
    selection_anchor: Option<String>,
    view_mode: ViewMode,
    sort_by: SortBy,
    sort_dir: SortDir,
    cursor: Option<usize>,
    first_snapshot: bool,
    screen: Screen,
    activity: Vec<ActivityEntry>,
    sync_phase: f32,
    toast: Option<Toast>,
    context_menu: Option<ContextMenu>,
    drag: Option<DragState>,
    file_hover: bool,
    clipboard_op: Option<ClipboardEntry>,
    modifiers: Modifiers,
    press_origin: Option<Point>,
}

#[derive(Debug, Clone)]
struct ContextMenu {
    position: Point,
    target: MenuTarget,
}

#[derive(Debug, Clone)]
pub enum MenuTarget {
    FileListBackground,
    Item(String),
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DragState {
    items: Vec<String>,
    position: Point,
    origin: Point,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ClipboardEntry {
    op: ClipboardOp,
    keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardOp {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
struct Toast {
    message: String,
    created_at: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Size,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Message {
    StatusFetched(Option<Status>, Option<String>),
    SnapshotFetched(Option<Snapshot>, Option<String>),
    Open(String),
    OpenFile(String),
    NavigateUp,
    Retry,
    SearchQuery(String),
    SearchResults(String, Vec<SearchHit>),
    SyncNow,
    PauseResume,
    Materialize(String),
    ResolveConflict(String, Resolution),
    ToggleConflict(String),
    SelectFile(String),
    FocusSearch,
    Navigate(Screen),
    NavigateBack,
    NavigateForward,
    Escape,
    Backspace,
    MoveCursor(i32),
    ActivateCursor,
    ToggleViewMode,
    SetViewMode(ViewMode),
    SortBy_(SortBy),
    ActionResult(Result<(), String>),
    ActivityFetched(Vec<ActivityEntry>),
    Tick,
    FontLoaded,
    // Multi-selection
    SelectItem(String, SelectionMode),
    ClearSelection,
    SelectAll,
    ModifiersChanged(Modifiers),
    CursorMoved(Point),
    // Context menu
    RightClickedItem(String),
    RightClickedBackground,
    OpenContextMenu { target: MenuTarget, position: Point },
    CloseContextMenu,
    CopyPath(String),
    ClipboardCopy,
    ClipboardCut,
    ClipboardPaste,
    // File drag-drop (OS)
    FileHovered(std::path::PathBuf),
    FileDropped(std::path::PathBuf),
    FilesHoveredLeft,
    // Internal drag
    FilePressed(String),
    MouseReleased,
    DragStarted { items: Vec<String>, origin: Point },
    DragMoved(Point),
    DragDroppedOnFolder(String),
    DragEnded,
    FolderDropTarget(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    Single,
    ToggleAdd,
    Range,
}

pub fn new() -> (App, Task<Message>) {
    (
        App {
            state: AppState::Connecting,
        },
        Task::batch(vec![
            iced::font::load(theme::INTER).map(|_| Message::FontLoaded),
            iced::font::load(theme::JETBRAINS_MONO).map(|_| Message::FontLoaded),
            Task::perform(ipc::fetch_status(SOCKET.into()), map_status),
        ]),
    )
}

pub fn subscription(state: &App) -> Subscription<Message> {
    let keys = keyboard::listen().filter_map(map_key);
    let mouse = iced::event::listen_with(map_event);

    // Only run the animation tick when something actually needs it:
    // syncing (rotating glyph) or a toast (auto-dismiss countdown).
    // Otherwise every 50ms tick forces a full re-view — the context menu
    // rebuild storm.
    let need_tick = match &state.state {
        AppState::Connected(c) => {
            let syncing = !c.status.paused && c.status.last_sync_millis.is_none();
            syncing || c.toast.is_some()
        }
        _ => false,
    };

    let ticks = if need_tick {
        Some(iced::time::every(std::time::Duration::from_millis(50)).map(|_| Message::Tick))
    } else {
        None
    };

    let mut subs = vec![keys, mouse];
    if let Some(t) = ticks {
        subs.push(t);
    }
    Subscription::batch(subs)
}

pub fn update(state: &mut App, msg: Message) -> Task<Message> {
    match msg {
        Message::StatusFetched(Some(status), _) => {
            state.state = AppState::Connected(Conn {
                status,
                snapshot: Snapshot::default(),
                path: String::new(),
                nav_history: Vec::new(),
                nav_future: Vec::new(),
                search_query: String::new(),
                search_hits: Vec::new(),
                last_error: None,
                expanded_conflict: None,
                selection: BTreeSet::new(),
                selection_anchor: None,
                view_mode: ViewMode::List,
                sort_by: SortBy::Name,
                sort_dir: SortDir::Asc,
                cursor: None,
                first_snapshot: true,
                screen: Screen::Files,
                activity: Vec::new(),
                sync_phase: 0.0,
                toast: None,
                context_menu: None,
                drag: None,
                file_hover: false,
                clipboard_op: None,
                modifiers: Modifiers::default(),
                press_origin: None,
            });
            poll_snapshot()
        }
        Message::StatusFetched(_, Some(e)) => {
            state.state = AppState::Error(e);
            Task::none()
        }
        Message::StatusFetched(None, None) => {
            state.state = AppState::Error("unknown error".into());
            Task::none()
        }
        Message::SnapshotFetched(Some(snap), _) => {
            if let AppState::Connected(c) = &mut state.state {
                c.snapshot = snap;
                c.first_snapshot = false;
            }
            Task::perform(
                async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    ipc::fetch_snapshot(SOCKET.into()).await
                },
                map_snapshot,
            )
        }
        Message::SnapshotFetched(None, Some(e)) => {
            tracing::warn!("snapshot poll failed: {e}");
            Task::perform(
                async {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    ipc::fetch_snapshot(SOCKET.into()).await
                },
                map_snapshot,
            )
        }
        Message::SnapshotFetched(None, None) => Task::none(),
        Message::Open(name) => {
            if let AppState::Connected(c) = &mut state.state {
                if name.ends_with('/') {
                    c.nav_history.push(c.path.clone());
                    c.nav_future.clear();
                    c.path.push_str(&name);
                    c.cursor = None;
                    c.selection.clear();
                    c.selection_anchor = None;
                    c.context_menu = None;
                    c.screen = Screen::Files;
                }
            }
            Task::none()
        }
        Message::OpenFile(key) => {
            if let AppState::Connected(c) = &mut state.state {
                let path = std::path::Path::new(&c.status.local_root).join(&key);
                open_path(&path);
                c.context_menu = None;
            }
            Task::none()
        }
        Message::NavigateUp => {
            if let AppState::Connected(c) = &mut state.state {
                if !c.path.is_empty() {
                    c.nav_history.push(c.path.clone());
                    c.nav_future.clear();
                    if let Some(idx) = c.path.trim_end_matches('/').rfind('/') {
                        c.path.truncate(idx + 1);
                    } else {
                        c.path.clear();
                    }
                    c.cursor = None;
                    c.selection.clear();
                    c.selection_anchor = None;
                    c.context_menu = None;
                }
            }
            Task::none()
        }
        Message::NavigateBack => {
            if let AppState::Connected(c) = &mut state.state {
                if let Some(prev) = c.nav_history.pop() {
                    c.nav_future.push(c.path.clone());
                    c.path = prev;
                    c.cursor = None;
                    c.selection.clear();
                    c.selection_anchor = None;
                    c.context_menu = None;
                }
            }
            Task::none()
        }
        Message::NavigateForward => {
            if let AppState::Connected(c) = &mut state.state {
                if let Some(next) = c.nav_future.pop() {
                    c.nav_history.push(c.path.clone());
                    c.path = next;
                    c.cursor = None;
                    c.selection.clear();
                    c.selection_anchor = None;
                    c.context_menu = None;
                }
            }
            Task::none()
        }
        Message::Retry => {
            state.state = AppState::Connecting;
            Task::perform(ipc::fetch_status(SOCKET.into()), map_status)
        }
        Message::SearchQuery(q) => {
            if let AppState::Connected(c) = &mut state.state {
                c.search_query = q.clone();
                c.search_hits.clear();
                if q.trim().is_empty() {
                    return Task::none();
                }
                // Switch to the search screen so results are visible
                // regardless of which screen the user was on.
                c.screen = Screen::Search;
                run_search(q)
            } else {
                Task::none()
            }
        }
        Message::SearchResults(q, hits) => {
            if let AppState::Connected(c) = &mut state.state {
                if c.search_query == q {
                    c.search_hits = hits;
                }
            }
            Task::none()
        }
        Message::SyncNow => Task::perform(ipc::sync_now(SOCKET.into()), map_action),
        Message::PauseResume => {
            if let AppState::Connected(c) = &state.state {
                let socket = SOCKET.to_string();
                if c.status.paused {
                    Task::perform(ipc::resume(socket), map_action)
                } else {
                    Task::perform(ipc::pause(socket), map_action)
                }
            } else {
                Task::none()
            }
        }
        Message::Materialize(key) => {
            Task::perform(ipc::materialize(SOCKET.into(), key), map_action)
        }
        Message::ResolveConflict(key, resolution) => {
            if let AppState::Connected(c) = &mut state.state {
                c.expanded_conflict = None;
            }
            Task::perform(
                ipc::resolve_conflict(SOCKET.into(), key, resolution),
                map_action,
            )
        }
        Message::ToggleConflict(key) => {
            if let AppState::Connected(c) = &mut state.state {
                if c.expanded_conflict.as_deref() == Some(key.as_str()) {
                    c.expanded_conflict = None;
                } else {
                    c.expanded_conflict = Some(key);
                }
            }
            Task::none()
        }
        Message::SelectFile(key) => {
            if let AppState::Connected(c) = &mut state.state {
                c.selection.clear();
                c.selection.insert(key);
                c.selection_anchor = c.selection.iter().last().cloned();
            }
            Task::none()
        }
        Message::FocusSearch => {
            if let AppState::Connected(c) = &mut state.state {
                c.screen = Screen::Search;
            }
            iced::widget::operation::focus(SEARCH_ID)
        }
        Message::Navigate(screen) => {
            if let AppState::Connected(c) = &mut state.state {
                c.screen = screen;
                if screen == Screen::Search {
                    return iced::widget::operation::focus(SEARCH_ID);
                }
                if screen == Screen::Activity || screen == Screen::Overview {
                    return Task::perform(ipc::fetch_activity(SOCKET.into()), |r| {
                        Message::ActivityFetched(match r {
                            Ok(v) => v,
                            Err(_) => Vec::new(),
                        })
                    });
                }
            }
            Task::none()
        }
        Message::Escape => {
            if let AppState::Connected(c) = &mut state.state {
                if c.context_menu.is_some() {
                    c.context_menu = None;
                } else if !c.search_query.is_empty() {
                    c.search_query.clear();
                    c.search_hits.clear();
                } else if c.screen == Screen::Search {
                    c.screen = Screen::Files;
                } else {
                    c.cursor = None;
                    c.selection.clear();
                    c.selection_anchor = None;
                }
            }
            Task::none()
        }
        Message::Backspace => {
            if let AppState::Connected(c) = &state.state {
                if !c.search_query.is_empty() {
                    return Task::none();
                }
            }
            if let AppState::Connected(c) = &mut state.state {
                if let Some(idx) = c.path.trim_end_matches('/').rfind('/') {
                    c.path.truncate(idx + 1);
                } else {
                    c.path.clear();
                }
                c.cursor = None;
            }
            Task::none()
        }
        Message::MoveCursor(delta) => {
            if let AppState::Connected(c) = &mut state.state {
                move_cursor(c, delta);
            }
            Task::none()
        }
        Message::ActivateCursor => {
            if let AppState::Connected(c) = &state.state {
                let task = activate_cursor(c);
                if task.is_some() {
                    return task.unwrap();
                }
            }
            Task::none()
        }
        Message::ToggleViewMode => {
            if let AppState::Connected(c) = &mut state.state {
                c.view_mode = match c.view_mode {
                    ViewMode::List => ViewMode::Grid,
                    ViewMode::Grid => ViewMode::List,
                };
            }
            Task::none()
        }
        Message::SetViewMode(mode) => {
            if let AppState::Connected(c) = &mut state.state {
                c.view_mode = mode;
            }
            Task::none()
        }
        Message::SortBy_(by) => {
            if let AppState::Connected(c) = &mut state.state {
                if c.sort_by == by {
                    c.sort_dir = match c.sort_dir {
                        SortDir::Asc => SortDir::Desc,
                        SortDir::Desc => SortDir::Asc,
                    };
                } else {
                    c.sort_by = by;
                    c.sort_dir = SortDir::Asc;
                }
            }
            Task::none()
        }
        Message::ActionResult(Ok(())) => {
            if let AppState::Connected(c) = &mut state.state {
                c.last_error = None;
            }
            Task::none()
        }
        Message::ActionResult(Err(e)) => {
            tracing::warn!("action failed: {e}");
            if let AppState::Connected(c) = &mut state.state {
                c.last_error = Some(e.clone());
                c.toast = Some(Toast {
                    message: e,
                    created_at: std::time::Instant::now(),
                });
            }
            Task::none()
        }
        Message::ActivityFetched(entries) => {
            if let AppState::Connected(c) = &mut state.state {
                c.activity = entries;
            }
            Task::none()
        }
        Message::Tick => {
            if let AppState::Connected(c) = &mut state.state {
                c.sync_phase = (c.sync_phase + 0.1) % (std::f32::consts::TAU);
                if let Some(t) = &c.toast {
                    if t.created_at.elapsed() > std::time::Duration::from_secs(4) {
                        c.toast = None;
                    }
                }
            }
            Task::none()
        }
        Message::FontLoaded => Task::none(),
        // ---- Multi-selection ----
        Message::SelectItem(key, mode) => {
            if let AppState::Connected(c) = &mut state.state {
                match mode {
                    SelectionMode::Single => {
                        c.selection.clear();
                        c.selection.insert(key.clone());
                        c.selection_anchor = Some(key);
                    }
                    SelectionMode::ToggleAdd => {
                        if c.selection.contains(&key) {
                            c.selection.remove(&key);
                        } else {
                            c.selection.insert(key.clone());
                        }
                        c.selection_anchor = Some(key);
                    }
                    SelectionMode::Range => {
                        let items = visible_items(c);
                        let anchor_key = c.selection_anchor.clone().unwrap_or(key.clone());
                        let anchor_idx = items.iter().position(|i| match i {
                            Item::Folder(n) => *n == anchor_key,
                            Item::File(k) => *k == anchor_key,
                        });
                        let target_idx = items.iter().position(|i| match i {
                            Item::Folder(n) => *n == key,
                            Item::File(k) => *k == key,
                        });
                        if let (Some(a), Some(t)) = (anchor_idx, target_idx) {
                            let (lo, hi) = if a <= t { (a, t) } else { (t, a) };
                            c.selection.clear();
                            for item in &items[lo..=hi] {
                                if let Item::File(k) = item {
                                    c.selection.insert(k.clone());
                                }
                            }
                        }
                        c.selection_anchor = Some(anchor_key);
                    }
                }
                c.context_menu = None;
            }
            Task::none()
        }
        Message::ClearSelection => {
            if let AppState::Connected(c) = &mut state.state {
                c.selection.clear();
                c.selection_anchor = None;
                c.context_menu = None;
            }
            Task::none()
        }
        Message::SelectAll => {
            if let AppState::Connected(c) = &mut state.state {
                let (_, files) = split_dirs(&c.snapshot, &c.path);
                c.selection.clear();
                for f in &files {
                    c.selection.insert(f.key.clone());
                }
                c.selection_anchor = files.first().map(|f| f.key.clone());
                c.context_menu = None;
            }
            Task::none()
        }
        Message::ModifiersChanged(mods) => {
            if let AppState::Connected(c) = &mut state.state {
                c.modifiers = mods;
            }
            Task::none()
        }
        Message::CursorMoved(pos) => {
            if let AppState::Connected(c) = &mut state.state {
                // Detect drag threshold: if we have a press_origin and the
                // cursor moved > 5px, start a drag with the current selection.
                if let Some(origin) = c.press_origin {
                    if c.drag.is_none() {
                        let dx = pos.x - origin.x;
                        let dy = pos.y - origin.y;
                        if (dx * dx + dy * dy).sqrt() > 5.0 {
                            let items: Vec<String> = c.selection.iter().cloned().collect();
                            if !items.is_empty() {
                                c.drag = Some(DragState {
                                    items,
                                    position: pos,
                                    origin,
                                });
                            }
                        }
                    } else if let Some(d) = &mut c.drag {
                        d.position = pos;
                    }
                }
            }
            Task::none()
        }
        // ---- Context menu ----
        Message::RightClickedItem(key) => {
            if let AppState::Connected(c) = &mut state.state {
                if !c.selection.contains(&key) {
                    c.selection.clear();
                    c.selection.insert(key.clone());
                    c.selection_anchor = Some(key.clone());
                }
                c.context_menu = Some(ContextMenu {
                    position: last_cursor_pos(),
                    target: MenuTarget::Item(key),
                });
            }
            Task::none()
        }
        Message::RightClickedBackground => {
            if let AppState::Connected(c) = &mut state.state {
                c.context_menu = Some(ContextMenu {
                    position: last_cursor_pos(),
                    target: MenuTarget::FileListBackground,
                });
            }
            Task::none()
        }
        Message::OpenContextMenu { target, position } => {
            if let AppState::Connected(c) = &mut state.state {
                // Right-clicking an item not in selection selects just it first
                if let MenuTarget::Item(ref key) = target {
                    if !c.selection.contains(key) {
                        c.selection.clear();
                        c.selection.insert(key.clone());
                        c.selection_anchor = Some(key.clone());
                    }
                }
                c.context_menu = Some(ContextMenu { position, target });
            }
            Task::none()
        }
        Message::CloseContextMenu => {
            if let AppState::Connected(c) = &mut state.state {
                c.context_menu = None;
            }
            Task::none()
        }
        Message::CopyPath(key) => {
            let path = if let AppState::Connected(c) = &state.state {
                format!("{}/{}", c.status.local_root, key)
            } else {
                String::new()
            };
            if let AppState::Connected(c) = &mut state.state {
                c.context_menu = None;
            }
            iced::clipboard::write(path)
        }
        Message::ClipboardCopy => {
            if let AppState::Connected(c) = &mut state.state {
                c.clipboard_op = Some(ClipboardEntry {
                    op: ClipboardOp::Copy,
                    keys: c.selection.iter().cloned().collect(),
                });
                c.context_menu = None;
            }
            Task::none()
        }
        Message::ClipboardCut => {
            if let AppState::Connected(c) = &mut state.state {
                c.clipboard_op = Some(ClipboardEntry {
                    op: ClipboardOp::Cut,
                    keys: c.selection.iter().cloned().collect(),
                });
                c.context_menu = None;
            }
            Task::none()
        }
        Message::ClipboardPaste => {
            if let AppState::Connected(c) = &mut state.state {
                c.context_menu = None;
                if let Some(entry) = &c.clipboard_op {
                    let label = match entry.op {
                        ClipboardOp::Copy => "copy",
                        ClipboardOp::Cut => "move",
                    };
                    let n = entry.keys.len();
                    // TODO(engine): wire to METHOD_COPY / METHOD_MOVE when added.
                    c.toast = Some(Toast {
                        message: format!("Paste {n} item(s) ({label}) — awaiting engine support"),
                        created_at: std::time::Instant::now(),
                    });
                }
            }
            Task::none()
        }
        // ---- OS file drag-drop ----
        Message::FileHovered(_) => {
            if let AppState::Connected(c) = &mut state.state {
                c.file_hover = true;
            }
            Task::none()
        }
        Message::FileDropped(path) => {
            if let AppState::Connected(c) = &mut state.state {
                c.file_hover = false;
                // TODO(engine): wire to METHOD_INGEST when added.
                c.toast = Some(Toast {
                    message: format!(
                        "Dropped: {} — ingest awaiting engine support",
                        path.display()
                    ),
                    created_at: std::time::Instant::now(),
                });
            }
            Task::none()
        }
        Message::FilesHoveredLeft => {
            if let AppState::Connected(c) = &mut state.state {
                c.file_hover = false;
            }
            Task::none()
        }
        // ---- Internal drag ----
        Message::FilePressed(key) => {
            if let AppState::Connected(c) = &mut state.state {
                // Apply the same modifier-based selection logic as SelectItem
                let mode = if c.modifiers.shift() {
                    SelectionMode::Range
                } else if c.modifiers.control() || c.modifiers.command() {
                    SelectionMode::ToggleAdd
                } else {
                    SelectionMode::Single
                };
                match mode {
                    SelectionMode::Single => {
                        c.selection.clear();
                        c.selection.insert(key.clone());
                        c.selection_anchor = Some(key.clone());
                    }
                    SelectionMode::ToggleAdd => {
                        if c.selection.contains(&key) {
                            c.selection.remove(&key);
                        } else {
                            c.selection.insert(key.clone());
                        }
                        c.selection_anchor = Some(key.clone());
                    }
                    SelectionMode::Range => {
                        let items = visible_items(c);
                        let anchor_key = c.selection_anchor.clone().unwrap_or(key.clone());
                        let anchor_idx = items.iter().position(|i| match i {
                            Item::Folder(n) => *n == anchor_key,
                            Item::File(k) => *k == anchor_key,
                        });
                        let target_idx = items.iter().position(|i| match i {
                            Item::Folder(n) => *n == key,
                            Item::File(k) => *k == key,
                        });
                        if let (Some(a), Some(t)) = (anchor_idx, target_idx) {
                            let (lo, hi) = if a <= t { (a, t) } else { (t, a) };
                            c.selection.clear();
                            for item in &items[lo..=hi] {
                                if let Item::File(k) = item {
                                    c.selection.insert(k.clone());
                                }
                            }
                        }
                        c.selection_anchor = Some(anchor_key);
                    }
                }
                c.press_origin = Some(last_cursor_pos());
                c.context_menu = None;
            }
            Task::none()
        }
        Message::MouseReleased => {
            if let AppState::Connected(c) = &mut state.state {
                c.press_origin = None;
                // If a drag was active and the release didn't hit a folder
                // (FolderDropTarget would have fired first via mouse_area),
                // cancel the drag.
                c.drag = None;
            }
            Task::none()
        }
        Message::DragStarted { items, origin } => {
            if let AppState::Connected(c) = &mut state.state {
                c.drag = Some(DragState {
                    items,
                    position: origin,
                    origin,
                });
            }
            Task::none()
        }
        Message::DragMoved(pos) => {
            if let AppState::Connected(c) = &mut state.state {
                if let Some(d) = &mut c.drag {
                    d.position = pos;
                }
            }
            Task::none()
        }
        Message::DragDroppedOnFolder(folder) => {
            if let AppState::Connected(c) = &mut state.state {
                if let Some(d) = c.drag.take() {
                    let n = d.items.len();
                    // TODO(engine): wire to METHOD_MOVE when added.
                    c.toast = Some(Toast {
                        message: format!("Move {n} item(s) to {folder} — awaiting engine support"),
                        created_at: std::time::Instant::now(),
                    });
                }
                c.context_menu = None;
            }
            Task::none()
        }
        Message::DragEnded => {
            if let AppState::Connected(c) = &mut state.state {
                c.drag = None;
            }
            Task::none()
        }
        Message::FolderDropTarget(folder) => {
            if let AppState::Connected(c) = &mut state.state {
                c.press_origin = None;
                if let Some(d) = c.drag.take() {
                    let n = d.items.len();
                    // TODO(engine): wire to METHOD_MOVE when added.
                    c.toast = Some(Toast {
                        message: format!("Move {n} item(s) to {folder} — awaiting engine support"),
                        created_at: std::time::Instant::now(),
                    });
                }
            }
            Task::none()
        }
    }
}

// ---- View ----

pub fn view(state: &App) -> Element<'_, Message> {
    match &state.state {
        AppState::Connecting => centered_text("Connecting to daemon…", 14.0),
        AppState::Connected(c) => main_layout(c),
        AppState::Error(e) => container(
            column![
                text(format!("{e}")).size(14).color(theme::SYNC_ERROR),
                button(text("Retry").size(13))
                    .on_press(Message::Retry)
                    .padding([6, 16])
                    .style(theme::primary_button),
            ]
            .spacing(16)
            .align_x(Alignment::Center),
        )
        .padding(60)
        .center(Length::Fill)
        .into(),
    }
}

fn main_layout(c: &Conn) -> Element<'_, Message> {
    let mut layers: Vec<Element<'_, Message>> = vec![row![sidebar(c), content(c)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()];

    // Drop zone overlay (when OS files hover over the window)
    if c.file_hover {
        layers.push(
            container(
                column![
                    icons::cloud(theme::ACCENT),
                    text("Drop to upload").size(16).color(theme::ACCENT_TEXT),
                ]
                .spacing(12)
                .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(40.0)
            .center(Length::Fill)
            .style(theme::drop_zone_overlay)
            .into(),
        );
    }

    // Drag overlay (internal item being dragged)
    if let Some(drag) = &c.drag {
        let label = if drag.items.len() == 1 {
            drag.items[0]
                .rsplit('/')
                .next()
                .unwrap_or(&drag.items[0])
                .to_string()
        } else {
            format!("{} items", drag.items.len())
        };
        let drag_pos = drag.position;
        layers.push(
            container(
                container(
                    row![
                        icons::folder(theme::ACCENT_TEXT),
                        text(label).size(13).color(theme::TEXT_PRIMARY),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .padding([8, 12])
                .style(theme::drag_card_container),
            )
            .padding(iced::Padding::new(0.0).top(drag_pos.y).left(drag_pos.x))
            .into(),
        );
    }

    // Toast overlay
    if let Some(toast) = &c.toast {
        layers.push(
            container(toast_view(&toast.message))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(iced::alignment::Horizontal::Right)
                .align_y(iced::alignment::Vertical::Bottom)
                .padding(iced::Padding::new(0.0).bottom(24.0).right(24.0))
                .into(),
        );
    }

    // Context menu overlay (top layer)
    if let Some(menu) = &c.context_menu {
        layers.push(context_menu_overlay(menu, c));
    }

    stack(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn toast_view(message: &str) -> Element<'static, Message> {
    container(
        row![
            icons::alert_triangle(theme::SYNC_ERROR),
            text(message.to_string())
                .size(13)
                .color(theme::TEXT_PRIMARY),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([12, 16])
    .style(theme::card_container)
    .width(Length::Shrink)
    .max_width(400)
    .into()
}

// ---- Context menu overlay ----

fn context_menu_overlay<'a>(menu: &'a ContextMenu, c: &'a Conn) -> Element<'a, Message> {
    // Click-through dismiss layer
    let dismiss = mouse_area(
        container(Space::new().width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_press(Message::CloseContextMenu)
    .on_right_press(Message::CloseContextMenu);

    let items = context_menu_items(&menu.target, c);

    let menu_card = container(column!(items).spacing(2).padding(6))
        .width(Length::Fixed(200.0))
        .style(theme::context_menu_container);

    // Position the menu using padding offsets from top-left
    let pos = menu.position;
    let positioned = container(menu_card).padding(iced::Padding::new(0.0).top(pos.y).left(pos.x));

    // ponytail: stack dismiss layer + positioned menu card
    stack![dismiss, positioned,]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn context_menu_items<'a>(target: &'a MenuTarget, c: &'a Conn) -> Element<'a, Message> {
    match target {
        MenuTarget::FileListBackground => {
            let mut col = column![].spacing(1);
            if c.clipboard_op.is_some() {
                col = col.push(menu_item(
                    "Paste",
                    Some(Message::ClipboardPaste),
                    icons::clipboard_paste(theme::TEXT_SECONDARY),
                ));
            } else {
                col = col.push(menu_item(
                    "Paste",
                    None,
                    icons::clipboard_paste(theme::TEXT_TERTIARY),
                ));
            }
            col = col.push(menu_item(
                "Select All",
                Some(Message::SelectAll),
                icons::check_circle(theme::TEXT_SECONDARY),
            ));
            col.into()
        }
        MenuTarget::Item(key) => {
            let is_folder = key.ends_with('/');
            let is_remote = c
                .snapshot
                .files
                .iter()
                .any(|f| f.key == *key && f.status == FileStatus::RemoteOnly);
            let is_conflict = c
                .snapshot
                .files
                .iter()
                .any(|f| f.key == *key && f.status == FileStatus::Conflict);
            let multi = c.selection.len() > 1;

            let mut col = column![].spacing(1);

            if !multi {
                if is_folder {
                    col = col.push(menu_item(
                        "Open",
                        Some(Message::Open(key.clone())),
                        icons::folder(theme::TEXT_SECONDARY),
                    ));
                } else {
                    col = col.push(menu_item(
                        "Open",
                        Some(Message::OpenFile(key.clone())),
                        icons::external_link(theme::TEXT_SECONDARY),
                    ));
                }
            }

            if is_remote {
                col = col.push(menu_item(
                    "Download",
                    Some(Message::Materialize(key.clone())),
                    icons::cloud(theme::TEXT_SECONDARY),
                ));
            }

            col = col.push(menu_item(
                "Copy",
                Some(Message::ClipboardCopy),
                icons::copy(theme::TEXT_SECONDARY),
            ));
            col = col.push(menu_item(
                "Cut",
                Some(Message::ClipboardCut),
                icons::scissors(theme::TEXT_SECONDARY),
            ));
            col = col.push(menu_item(
                "Copy path",
                Some(Message::CopyPath(key.clone())),
                icons::file_text(theme::TEXT_SECONDARY),
            ));

            if is_conflict {
                col = col.push(menu_item(
                    "Resolve conflict",
                    Some(Message::ToggleConflict(key.clone())),
                    icons::alert_triangle(theme::SYNC_CONFLICT),
                ));
            }

            // TODO(engine): rename + delete need engine IPC
            col = col.push(menu_item(
                "Rename",
                None,
                icons::pencil(theme::TEXT_TERTIARY),
            ));
            col = col.push(menu_item(
                "Delete",
                None,
                icons::trash(theme::TEXT_TERTIARY),
            ));

            col.into()
        }
    }
}

fn menu_item(
    label: &str,
    on_press: Option<Message>,
    icon: iced::widget::svg::Svg<'static, iced::Theme>,
) -> Element<'static, Message> {
    let content = row![
        icon,
        text(label.to_string()).size(13).color(theme::TEXT_PRIMARY)
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .width(Length::Fill);

    let mut btn = button(content).width(Length::Fill).padding([8, 12]);
    if let Some(msg) = on_press {
        btn = btn.on_press(msg).style(theme::menu_item_button);
    } else {
        btn = btn.style(theme::menu_item_disabled);
    }
    btn.into()
}

// ---- Sidebar (design §3.4) ----

fn sidebar(c: &Conn) -> Element<'_, Message> {
    let conflicts = conflict_count(&c.snapshot);

    let header = row![
        text(c.status.bucket.clone())
            .size(14)
            .font(iced::Font {
                weight: iced::font::Weight::Semibold,
                ..iced::Font::DEFAULT
            })
            .color(theme::TEXT_PRIMARY)
            .width(Length::Fill),
        icons::chevron_right(theme::TEXT_TERTIARY),
    ]
    .align_y(Alignment::Center)
    .padding([0, 4]);

    let nav = column![
        nav_item("Overview", Screen::Overview, c.screen, false),
        nav_item("Files", Screen::Files, c.screen, false),
        nav_item("Search", Screen::Search, c.screen, false),
        nav_item("Conflicts", Screen::Conflicts, c.screen, conflicts > 0),
        nav_item("Activity", Screen::Activity, c.screen, false),
    ]
    .spacing(2);

    let folders_label = text("FOLDERS")
        .size(11)
        .font(iced::Font {
            weight: iced::font::Weight::Bold,
            ..iced::Font::DEFAULT
        })
        .color(theme::TEXT_TERTIARY);

    let mirror_root = c
        .status
        .local_root
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("Mirror")
        .to_string();

    let folders_section = column![
        folders_label,
        sidebar_folder_row(&mirror_root),
        add_folder_button(),
    ]
    .spacing(2);

    let settings_row = button(
        row![
            icons::settings(theme::TEXT_SECONDARY),
            text("Settings").size(13).color(theme::TEXT_SECONDARY),
            Space::new().width(Length::Fill),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .on_press(Message::Navigate(Screen::Settings))
    .width(Length::Fill)
    .padding([6, 8])
    .style(theme::subtle_button);

    let sync_pill = sync_pill(c);

    container(
        column![
            container(header).padding(
                iced::Padding::new(0.0)
                    .top(20)
                    .right(16)
                    .bottom(16)
                    .left(16)
            ),
            container(nav).padding(iced::Padding::new(0.0).right(8).left(8)),
            Space::new().height(Length::Fixed(16.0)),
            container(folders_section).padding(iced::Padding::new(0.0).right(8).left(8)),
            Space::new().height(Length::Fill),
            container(
                column![
                    container(Space::new().height(1.0))
                        .width(Length::Fill)
                        .style(divider_style),
                    settings_row,
                    sync_pill,
                ]
                .spacing(8)
            )
            .padding(iced::Padding::new(0.0).top(8.0).right(8).bottom(16).left(8)),
        ]
        .height(Length::Fill),
    )
    .width(Length::Fixed(SIDEBAR_WIDTH))
    .height(Length::Fill)
    .style(theme::sidebar_container)
    .into()
}

fn nav_item(
    label: &str,
    screen: Screen,
    current: Screen,
    has_badge: bool,
) -> Element<'static, Message> {
    let is_active = screen == current;
    let text_color = if is_active {
        theme::ACCENT_TEXT
    } else {
        theme::TEXT_PRIMARY
    };
    let icon_color = if is_active {
        theme::ACCENT_TEXT
    } else {
        theme::TEXT_SECONDARY
    };

    let icon = match screen {
        Screen::Overview => icons::layout_grid(icon_color),
        Screen::Files => icons::folder(icon_color),
        Screen::Search => icons::search(icon_color),
        Screen::Conflicts => icons::alert_triangle(icon_color),
        Screen::Activity => icons::refresh_cw(icon_color),
        Screen::Settings => icons::settings(icon_color),
    };

    let badge: Option<Element<'static, Message>> = if has_badge {
        Some(
            container(text("!").size(11).color(theme::SYNC_CONFLICT))
                .padding([2, 6])
                .style(theme::badge_container)
                .into(),
        )
    } else {
        None
    };

    let row_content: Element<'static, Message> = if let Some(b) = badge {
        row![
            icon,
            text(label.to_string()).size(13).color(text_color),
            Space::new().width(Length::Fill),
            b,
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
    } else {
        row![
            icon,
            text(label.to_string()).size(13).color(text_color),
            Space::new().width(Length::Fill),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
    };

    let mut btn = button(row_content)
        .on_press(Message::Navigate(screen))
        .width(Length::Fill)
        .padding([6, 8]);

    if is_active {
        btn = btn.style(theme::selected_row_button);
    } else {
        btn = btn.style(theme::row_button);
    }

    btn.into()
}

fn sidebar_folder_row(name: &str) -> Element<'static, Message> {
    button(
        row![
            icons::folder(theme::ACCENT_TEXT),
            text(name.to_string()).size(13).color(theme::TEXT_PRIMARY),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .on_press(Message::Navigate(Screen::Files))
    .width(Length::Fill)
    .padding([6, 8])
    .style(theme::row_button)
    .into()
}

fn add_folder_button() -> Element<'static, Message> {
    // TODO(engine): wire to real "add pinned folder" action.
    button(
        row![
            icons::plus(theme::TEXT_TERTIARY),
            text("Add Folder").size(13).color(theme::TEXT_TERTIARY),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([6, 8])
    .style(theme::subtle_button)
    .into()
}

/// Sync pill: dot + label, plus Sync-now / Pause icon-buttons.
fn sync_pill(c: &Conn) -> Element<'_, Message> {
    let is_syncing = !c.status.paused && c.status.last_sync_millis.is_none();

    let (dot_color, label) = if c.status.paused {
        (theme::SYNC_QUEUED, "Paused".to_string())
    } else if let Some(ms) = c.status.last_sync_millis {
        (theme::SYNC_SYNCED, format!("Synced · {}", ms_ago(ms)))
    } else {
        (theme::SYNC_SYNCING, "Syncing".to_string())
    };

    let pill = row![
        status_dot(dot_color),
        text(label).size(12).color(theme::TEXT_SECONDARY),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let pause_icon = if c.status.paused {
        icons::play(theme::TEXT_SECONDARY)
    } else {
        icons::pause(theme::TEXT_SECONDARY)
    };

    let sync_icon = if is_syncing {
        icons::refresh_cw_rotated(theme::SYNC_SYNCING, c.sync_phase)
    } else {
        icons::refresh_cw(theme::TEXT_SECONDARY)
    };

    let controls = row![
        button(sync_icon)
            .on_press(Message::SyncNow)
            .padding([4, 6])
            .style(theme::icon_button),
        button(pause_icon)
            .on_press(Message::PauseResume)
            .padding([4, 6])
            .style(theme::icon_button),
    ]
    .spacing(2);

    row![pill, Space::new().width(Length::Fill), controls]
        .align_y(Alignment::Center)
        .padding([4, 4])
        .into()
}

// ---- Content area: toolbar + screen body ----

fn content(c: &Conn) -> Element<'_, Message> {
    let body: Element<'_, Message> = match c.screen {
        Screen::Files | Screen::Search => files_body(c),
        Screen::Overview => overview_screen(c),
        Screen::Conflicts => conflicts_screen(c),
        Screen::Activity => activity_screen(c),
        Screen::Settings => settings_screen(c),
    };

    container(
        column![
            toolbar(c),
            scrollable(body)
                .height(Length::Fill)
                .style(theme::thin_scrollable),
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Toolbar (48px): back · breadcrumb + count · centered search · grid/list toggle.
fn toolbar(c: &Conn) -> Element<'_, Message> {
    let can_go_back = !c.path.is_empty();
    let (folders, files) = split_dirs(&c.snapshot, &c.path);
    let total = folders.len() + files.len();

    let breadcrumb = if c.path.is_empty() {
        c.status.bucket.clone()
    } else {
        format!("{} / {}", c.status.bucket, c.path.trim_end_matches('/'))
    };

    let count_caption = format!("{total} items");

    let view_icon = match c.view_mode {
        ViewMode::List => icons::layout_grid(theme::TEXT_SECONDARY),
        ViewMode::Grid => icons::list_icon(theme::TEXT_SECONDARY),
    };

    container(
        row![
            button(icons::arrow_left(theme::TEXT_SECONDARY))
                .on_press_maybe(if can_go_back {
                    Some(Message::NavigateUp)
                } else {
                    None
                })
                .padding([6, 8])
                .style(theme::icon_button),
            column![
                text(breadcrumb).size(16).color(theme::TEXT_PRIMARY),
                text(count_caption).size(12).color(theme::TEXT_TERTIARY),
            ]
            .spacing(0),
            Space::new().width(Length::Fill),
            search_pill(&c.search_query),
            Space::new().width(Length::Fixed(8.0)),
            button(view_icon)
                .on_press(Message::ToggleViewMode)
                .padding([6, 8])
                .style(theme::icon_button),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([0, 16])
    .height(Length::Fixed(TOOLBAR_HEIGHT))
    .style(theme::top_bar_container)
    .into()
}

fn search_pill(query: &str) -> Element<'_, Message> {
    container(
        row![
            icons::search(theme::TEXT_TERTIARY),
            text_input("Search documents…", query)
                .id(SEARCH_ID)
                .on_input(Message::SearchQuery)
                .size(13)
                .style(theme::borderless_input)
                .width(Length::Fixed(280.0)),
            kbd_chip(if cfg!(target_os = "macos") {
                "⌘K"
            } else {
                "Ctrl+K"
            }),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 14])
    .style(theme::search_pill_container)
    .into()
}

fn kbd_chip(label: &str) -> Element<'_, Message> {
    container(
        text(label.to_string())
            .size(11)
            .font(theme::mono_font())
            .color(theme::TEXT_TERTIARY),
    )
    .padding([2, 6])
    .style(kbd_container)
    .into()
}

fn kbd_container(_theme: &iced::Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(theme::BG_KBD)),
        text_color: Some(theme::TEXT_TERTIARY),
        border: iced::Border {
            color: theme::STROKE_SUBTLE,
            width: 1.0,
            radius: iced::border::radius(4.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

// ---- Screen bodies ----

fn files_body(c: &Conn) -> Element<'_, Message> {
    let searching = !c.search_query.trim().is_empty();
    if searching {
        return search_results_view(&c.search_hits);
    }

    let (folders, mut files) = split_dirs(&c.snapshot, &c.path);
    sort_files(&mut files, c.sort_by, c.sort_dir);
    let total = folders.len() + files.len();

    if total == 0 {
        if c.first_snapshot {
            return centered_text("Loading…", 14.0);
        }
        return empty_state();
    }

    file_list_view(
        &c.path,
        &folders,
        &files,
        c.expanded_conflict.as_deref(),
        c.view_mode,
        c.sort_by,
        c.sort_dir,
        c.cursor,
        &c.selection,
        c.modifiers,
        c.drag.is_some(),
    )
}

fn overview_screen(c: &Conn) -> Element<'_, Message> {
    // TODO(engine): expose per-state counts for a richer stat strip.
    let total = c.snapshot.files.len();
    let conflicts = conflict_count(&c.snapshot);
    let synced = c
        .snapshot
        .files
        .iter()
        .filter(|f| f.status == FileStatus::Synced)
        .count();

    let last_error = c
        .last_error
        .as_ref()
        .map(|e| {
            text(format!("Last error: {e}"))
                .size(12)
                .color(theme::SYNC_ERROR)
        })
        .unwrap_or_else(|| text("").size(12));

    column![
        text("Overview").size(20).color(theme::TEXT_PRIMARY),
        Space::new().height(Length::Fixed(16.0)),
        row![
            stat_card("Total files", &total.to_string()),
            stat_card("Synced", &synced.to_string()),
            stat_card("Conflicts", &conflicts.to_string()),
        ]
        .spacing(12),
        Space::new().height(Length::Fixed(16.0)),
        sync_card(c),
        last_error,
        Space::new().height(Length::Fixed(16.0)),
        text("Recent activity").size(14).color(theme::TEXT_PRIMARY),
        recent_activity_list(c),
    ]
    .spacing(4)
    .padding(32)
    .into()
}

fn recent_activity_list(c: &Conn) -> Element<'_, Message> {
    if c.activity.is_empty() {
        return text("No recent activity")
            .size(12)
            .color(theme::TEXT_TERTIARY)
            .into();
    }
    let show = c.activity.iter().take(5);
    let mut col = column![].spacing(1);
    for entry in show {
        col = col.push(activity_row(entry));
    }
    col.into()
}

fn stat_card(label: &str, value: &str) -> Element<'static, Message> {
    container(
        column![
            text(value.to_string())
                .size(24)
                .font(iced::Font {
                    weight: iced::font::Weight::Bold,
                    ..iced::Font::DEFAULT
                })
                .color(theme::TEXT_PRIMARY),
            text(label.to_string()).size(12).color(theme::TEXT_TERTIARY),
        ]
        .spacing(4),
    )
    .padding(16)
    .width(Length::Fixed(120.0))
    .style(theme::card_container)
    .into()
}

fn sync_card(c: &Conn) -> Element<'static, Message> {
    let state = if c.status.paused {
        "Paused".to_string()
    } else if let Some(ms) = c.status.last_sync_millis {
        format!("Synced · {}", ms_ago(ms))
    } else {
        "Syncing".to_string()
    };
    let dot_color = if c.status.paused {
        theme::SYNC_QUEUED
    } else if c.status.last_sync_millis.is_some() {
        theme::SYNC_SYNCED
    } else {
        theme::SYNC_SYNCING
    };

    let pause_label = if c.status.paused { "Resume" } else { "Pause" };
    let pause_icon = if c.status.paused {
        icons::play(theme::TEXT_ON_ACCENT)
    } else {
        icons::pause(theme::TEXT_ON_ACCENT)
    };

    container(column![
        row![
            status_dot(dot_color),
            text(state.to_string()).size(14).color(theme::TEXT_PRIMARY),
            Space::new().width(Length::Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        Space::new().height(Length::Fixed(12.0)),
        row![
            button(
                row![
                    icons::refresh_cw(theme::TEXT_ON_ACCENT),
                    text("Sync now").size(13).color(theme::TEXT_ON_ACCENT),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .on_press(Message::SyncNow)
            .padding([8, 16])
            .style(theme::primary_button),
            button(
                row![
                    pause_icon,
                    text(pause_label.to_string())
                        .size(13)
                        .color(theme::TEXT_PRIMARY)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .on_press(Message::PauseResume)
            .padding([8, 16])
            .style(theme::gray_button),
        ]
        .spacing(8),
    ])
    .padding(20)
    .style(theme::card_container)
    .into()
}

fn conflicts_screen(c: &Conn) -> Element<'_, Message> {
    let conflicts: Vec<&FileStat> = c
        .snapshot
        .files
        .iter()
        .filter(|f| f.status == FileStatus::Conflict)
        .collect();

    if conflicts.is_empty() {
        return empty_state_for("No conflicts", "All files are in sync.");
    }

    let mut list = column![].spacing(1).padding([8, 0]);
    for f in &conflicts {
        let is_expanded = c.expanded_conflict.as_deref() == Some(f.key.as_str());
        list = list.push(file_row(
            f.key.clone(),
            f.key.clone(),
            f.size,
            f.mtime_millis,
            f.status,
            is_expanded,
            c.selection.contains(&f.key),
            c.modifiers,
        ));
    }
    list.into()
}

fn activity_screen(c: &Conn) -> Element<'_, Message> {
    if c.activity.is_empty() {
        return empty_state_for("Activity", "Recent sync activity will appear here.");
    }

    let mut list = column![].spacing(1).padding([8, 0]);
    for entry in &c.activity {
        list = list.push(activity_row(entry));
    }
    list.into()
}

fn activity_row(entry: &ActivityEntry) -> Element<'static, Message> {
    let time = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(entry.ts_millis as i64)
        .map(|d| d.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "?".into());

    row![
        text(time)
            .size(11)
            .font(theme::mono_font())
            .color(theme::TEXT_TERTIARY)
            .width(Length::Fixed(80.0)),
        text(entry.kind.clone())
            .size(11)
            .color(theme::TEXT_SECONDARY)
            .width(Length::Fixed(100.0)),
        text(entry.key.clone()).size(12).color(theme::TEXT_PRIMARY),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .padding([6, 16])
    .into()
}

fn settings_screen(c: &Conn) -> Element<'_, Message> {
    let endpoint = c.status.endpoint.clone().unwrap_or_else(|| "—".into());
    let local_root = if c.status.local_root.is_empty() {
        "—".to_string()
    } else {
        c.status.local_root.clone()
    };

    column![
        text("Settings").size(20).color(theme::TEXT_PRIMARY),
        Space::new().height(Length::Fixed(16.0)),
        settings_row("Bucket", &c.status.bucket),
        settings_row("Endpoint", &endpoint),
        settings_row("Prefix", &c.status.prefix),
        settings_row("Local root", &local_root),
        Space::new().height(Length::Fixed(16.0)),
        text("wunderdrive v0.1.0")
            .size(11)
            .color(theme::TEXT_TERTIARY),
    ]
    .spacing(4)
    .padding(32)
    .into()
}

fn settings_row(label: &str, value: &str) -> Element<'static, Message> {
    row![
        text(label.to_string())
            .size(12)
            .color(theme::TEXT_TERTIARY)
            .width(Length::Fixed(120.0)),
        text(value.to_string())
            .size(13)
            .font(theme::mono_font())
            .color(theme::TEXT_PRIMARY),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .padding([6, 0])
    .into()
}

fn empty_state_for(title: &str, subtitle: &str) -> Element<'static, Message> {
    column![
        text(title.to_string()).size(16).color(theme::TEXT_PRIMARY),
        text(subtitle.to_string())
            .size(12)
            .color(theme::TEXT_SECONDARY),
    ]
    .spacing(8)
    .align_x(Alignment::Center)
    .padding(60)
    .into()
}

fn search_results_view(hits: &[SearchHit]) -> Element<'static, Message> {
    if hits.is_empty() {
        return centered_text("No matches", 13.0);
    }
    let mut list = column![].spacing(1).padding([8, 0]);
    for h in hits {
        let parent = parent_folder(&h.key);
        list = list.push(search_row(&h.key, h.snippet.as_deref(), parent));
    }
    list.into()
}

fn search_row(key: &str, snippet: Option<&str>, parent: String) -> Element<'static, Message> {
    let mut col = column![row![
        icons::file_text(theme::TEXT_SECONDARY),
        text(key.to_string()).size(13).color(theme::TEXT_PRIMARY),
    ]
    .spacing(8)
    .align_y(Alignment::Center)]
    .spacing(2);
    if let Some(s) = snippet {
        let clean = strip_marks(s);
        col = col.push(text(clean).size(11).color(theme::TEXT_TERTIARY));
    }
    button(col)
        .on_press(Message::Open(parent))
        .width(Length::Fill)
        .padding([8, 16])
        .style(theme::row_button)
        .into()
}

fn column_header(sort_by: SortBy, sort_dir: SortDir) -> Element<'static, Message> {
    let arrow = if sort_dir == SortDir::Asc {
        "\u{25B2}" // ▲
    } else {
        "\u{25BC}" // ▼
    };
    let mk = |label: &str, col: SortBy| -> Element<'static, Message> {
        let active = sort_by == col;
        let color = if active {
            theme::TEXT_SECONDARY
        } else {
            theme::TEXT_TERTIARY
        };
        let mut r = row![]
            .spacing(4)
            .align_y(Alignment::Center)
            .push(text(label.to_string()).size(11).color(color));
        if active {
            r = r.push(text(arrow).size(9).color(theme::ACCENT_TEXT));
        }
        button(r)
            .on_press(Message::SortBy_(col))
            .padding([4, 4])
            .style(theme::header_button)
            .into()
    };
    // Match file row: padding [0, 16], then glyph(16) + spacing(12) + icon(22) + spacing(12) = 62
    row![
        Space::new().width(Length::Fixed(62.0)),
        mk("Name", SortBy::Name),
        Space::new().width(Length::Fill),
        container(mk("Modified", SortBy::Modified))
            .width(Length::Fixed(90.0))
            .align_x(iced::alignment::Horizontal::Right),
        Space::new().width(Length::Fixed(8.0)),
        container(mk("Size", SortBy::Size))
            .width(Length::Fixed(70.0))
            .align_x(iced::alignment::Horizontal::Right),
    ]
    .align_y(Alignment::Center)
    .padding([0, 16])
    .height(Length::Fixed(28.0))
    .into()
}

fn sort_files(files: &mut [&FileStat], by: SortBy, dir: SortDir) {
    files.sort_by(|a, b| {
        let ord = match by {
            SortBy::Name => a.key.cmp(&b.key),
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Modified => a.mtime_millis.cmp(&b.mtime_millis),
        };
        if dir == SortDir::Asc {
            ord
        } else {
            ord.reverse()
        }
    });
}

fn file_list_view(
    path: &str,
    folders: &[String],
    files: &[&FileStat],
    expanded_conflict: Option<&str>,
    view_mode: ViewMode,
    sort_by: SortBy,
    sort_dir: SortDir,
    _cursor: Option<usize>,
    selection: &BTreeSet<String>,
    modifiers: Modifiers,
    dragging: bool,
) -> Element<'static, Message> {
    let mut list = column![].spacing(1).padding([8, 0]);

    if !path.is_empty() {
        list = list.push(
            button(
                row![
                    icons::arrow_left(theme::ACCENT_TEXT),
                    text("Back").size(12).color(theme::ACCENT_TEXT),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .on_press(Message::NavigateUp)
            .padding([6, 16])
            .style(theme::subtle_button),
        );
    }

    if view_mode == ViewMode::Grid {
        let mut wrap_row = row![].spacing(8);
        for name in folders.iter() {
            wrap_row = wrap_row.push(grid_cell(
                name,
                selection.contains(name),
                modifiers,
                dragging,
            ));
        }
        for f in files.iter() {
            let display = f.key[path.len()..].to_string();
            let is_selected = selection.contains(&f.key);
            wrap_row = wrap_row.push(file_grid_cell(
                f.key.clone(),
                display,
                f.status,
                is_selected,
                modifiers,
            ));
        }
        list = list.push(wrap_row.wrap().vertical_spacing(8));
    } else {
        list = list.push(column_header(sort_by, sort_dir));
        for name in folders.iter() {
            list = list.push(folder_row(
                name,
                selection.contains(name),
                modifiers,
                dragging,
            ));
        }
        for f in files.iter() {
            let display = f.key[path.len()..].to_string();
            let is_expanded = expanded_conflict == Some(f.key.as_str());
            let is_selected = selection.contains(&f.key);
            list = list.push(file_row(
                f.key.clone(),
                display,
                f.size,
                f.mtime_millis,
                f.status,
                is_expanded,
                is_selected,
                modifiers,
            ));
        }
    }

    // Wrap the list in a mouse_area so clicking empty space clears selection
    // and right-clicking empty space opens the background context menu.
    mouse_area(list)
        .on_press(Message::ClearSelection)
        .on_right_press(Message::RightClickedBackground)
        .into()
}

fn folder_row(
    name: &str,
    selected: bool,
    modifiers: Modifiers,
    dragging: bool,
) -> Element<'static, Message> {
    let display = name.trim_end_matches('/');
    let inner: Element<'static, Message> = button(
        row![
            // Reserve space to align with file rows (sync glyph 16 + spacing 12)
            Space::new().width(Length::Fixed(16.0)),
            icons::folder(theme::ACCENT_TEXT),
            text(display.to_string())
                .size(14)
                .color(theme::TEXT_PRIMARY)
                .width(Length::Fill),
            icons::chevron_right(theme::TEXT_TERTIARY),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(40.0))
    .padding([0, 16])
    .style(if dragging {
        theme::drop_target_row_button
    } else if selected {
        theme::selected_row_button
    } else {
        theme::row_button
    })
    .into();

    let key = name.to_string();
    let key_rc = key.clone();
    let key_rc2 = key.clone();
    let key_rc3 = key.clone();

    mouse_area(inner)
        .on_press(select_message(key, modifiers))
        .on_double_click(Message::Open(key_rc))
        .on_right_press(Message::RightClickedItem(key_rc2.clone()))
        .on_release(Message::FolderDropTarget(key_rc3.clone()))
        .into()
}

fn grid_cell(
    name: &str,
    selected: bool,
    modifiers: Modifiers,
    dragging: bool,
) -> Element<'static, Message> {
    let display = name.trim_end_matches('/');
    let inner: Element<'static, Message> = button(
        column![
            icons::folder(theme::ACCENT_TEXT),
            text(display.to_string())
                .size(12)
                .color(theme::TEXT_PRIMARY),
        ]
        .spacing(10)
        .align_x(Alignment::Center),
    )
    .width(Length::Fixed(152.0))
    .height(Length::Fixed(152.0))
    .padding([24, 12])
    .style(if dragging || selected {
        theme::grid_cell_button_selected
    } else {
        theme::grid_cell_button
    })
    .into();

    let key = name.to_string();
    let key_rc = key.clone();
    let key_rc2 = key.clone();

    mouse_area(inner)
        .on_press(select_message(key, modifiers))
        .on_double_click(Message::Open(key_rc))
        .on_right_press(Message::RightClickedItem(key_rc2.clone()))
        .into()
}

fn file_grid_cell(
    key: String,
    name: String,
    status: FileStatus,
    selected: bool,
    _modifiers: Modifiers,
) -> Element<'static, Message> {
    let (glyph, color) = status_glyph_icon(status);
    let inner: Element<'static, Message> = button(
        column![
            icons::type_icon(&key, theme::TEXT_SECONDARY, 56.0),
            text(name).size(12).color(theme::TEXT_PRIMARY),
            glyph(color),
        ]
        .spacing(10)
        .align_x(Alignment::Center),
    )
    .width(Length::Fixed(152.0))
    .height(Length::Fixed(152.0))
    .padding([24, 12])
    .style(if selected {
        theme::grid_cell_button_selected
    } else {
        theme::grid_cell_button
    })
    .into();

    let key_rc = key.clone();
    let key_rc2 = key.clone();
    let key_rc3 = key.clone();

    let mut area = mouse_area(inner)
        .on_press(Message::FilePressed(key_rc))
        .on_right_press(Message::RightClickedItem(key_rc2.clone()));

    if status == FileStatus::RemoteOnly {
        area = area.on_double_click(Message::Materialize(key_rc3));
    } else {
        area = area.on_double_click(Message::OpenFile(key_rc3));
    }

    area.into()
}

fn file_row(
    key: String,
    name: String,
    size: u64,
    mtime: u64,
    status: FileStatus,
    conflict_expanded: bool,
    selected: bool,
    _modifiers: Modifiers,
) -> Element<'static, Message> {
    let (glyph_fn, glyph_color) = status_glyph_icon(status);
    let size_text = if status == FileStatus::RemoteOnly {
        "remote".to_string()
    } else {
        human_size(size)
    };
    let date_text = if status == FileStatus::RemoteOnly {
        String::new()
    } else {
        short_date(mtime)
    };

    let inner: Element<'static, Message> = button(
        row![
            glyph_fn(glyph_color),
            icons::type_icon(&key, theme::TEXT_SECONDARY, 22.0),
            text(name)
                .size(14)
                .color(theme::TEXT_PRIMARY)
                .width(Length::Fill),
            text(date_text)
                .size(12)
                .color(theme::TEXT_TERTIARY)
                .width(Length::Fixed(90.0))
                .align_x(iced::alignment::Horizontal::Right),
            Space::new().width(Length::Fixed(8.0)),
            text(size_text)
                .size(12)
                .font(theme::mono_font())
                .color(theme::TEXT_TERTIARY)
                .width(Length::Fixed(70.0))
                .align_x(iced::alignment::Horizontal::Right),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(40.0))
    .padding([0, 16])
    .style(if selected {
        theme::selected_row_button
    } else {
        theme::row_button
    })
    .into();

    let key_rc = key.clone();
    let key_rc2 = key.clone();
    let key_rc3 = key.clone();

    let mut area = mouse_area(inner)
        .on_press(Message::FilePressed(key_rc))
        .on_right_press(Message::RightClickedItem(key_rc2.clone()));

    if status == FileStatus::RemoteOnly {
        area = area.on_double_click(Message::Materialize(key_rc3));
    } else if status == FileStatus::Conflict {
        area = area.on_double_click(Message::ToggleConflict(key_rc3));
    } else {
        area = area.on_double_click(Message::OpenFile(key_rc3));
    }

    if status == FileStatus::Conflict && conflict_expanded {
        let actions = row![
            res_button("Keep local", key.clone(), Resolution::KeepLocal),
            res_button("Keep remote", key.clone(), Resolution::KeepRemote),
            res_button("Keep both", key.clone(), Resolution::KeepBoth),
        ]
        .spacing(6)
        .padding(iced::Padding::new(0.0).left(38.0).right(12.0).bottom(6.0));
        column![conflict_edge(area.into()), actions]
            .spacing(1)
            .into()
    } else if status == FileStatus::Conflict {
        conflict_edge(area.into()).into()
    } else {
        area.into()
    }
}

/// Computes the selection message based on current modifier state.
fn select_message(key: String, modifiers: Modifiers) -> Message {
    if modifiers.shift() {
        Message::SelectItem(key, SelectionMode::Range)
    } else if modifiers.control() || modifiers.command() {
        Message::SelectItem(key, SelectionMode::ToggleAdd)
    } else {
        Message::SelectItem(key, SelectionMode::Single)
    }
}

/// Wraps a row button with a 2px amber left edge for conflict rows.
fn conflict_edge(content: Element<'static, Message>) -> Element<'static, Message> {
    row![
        container(Space::new().width(Length::Fixed(2.0)).height(Length::Fill))
            .style(conflict_bar_style)
            .height(Length::Fill),
        container(content).width(Length::Fill),
    ]
    .spacing(0)
    .width(Length::Fill)
    .into()
}

fn conflict_bar_style(_theme: &iced::Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(theme::SYNC_CONFLICT)),
        text_color: None,
        border: iced::Border::default(),
        shadow: Default::default(),
        snap: true,
    }
}

fn res_button(label: &str, key: String, resolution: Resolution) -> Element<'static, Message> {
    button(text(label.to_string()).size(11))
        .on_press(Message::ResolveConflict(key, resolution))
        .padding([4, 10])
        .style(theme::subtle_button)
        .into()
}

fn centered_text(msg: &str, size: f32) -> Element<'_, Message> {
    container(text(msg).size(size).color(theme::TEXT_SECONDARY))
        .center(Length::Fill)
        .into()
}

fn empty_state() -> Element<'static, Message> {
    column![
        text("No files synced yet")
            .size(16)
            .color(theme::TEXT_PRIMARY),
        text("Drop files into your local mirror folder, or sync the remote bucket.")
            .size(12)
            .color(theme::TEXT_SECONDARY),
        text("Press / to search")
            .size(11)
            .color(theme::TEXT_TERTIARY),
    ]
    .spacing(8)
    .align_x(Alignment::Center)
    .padding(60)
    .into()
}

fn divider_style(_theme: &iced::Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(theme::STROKE_SUBTLE)),
        text_color: None,
        border: iced::Border::default(),
        shadow: Default::default(),
        snap: true,
    }
}

fn status_dot(color: iced::Color) -> Element<'static, Message> {
    text("\u{25CF}").size(8.0).color(color).into()
}

// ---- helpers ----

fn split_dirs<'a>(snapshot: &'a Snapshot, prefix: &str) -> (Vec<String>, Vec<&'a FileStat>) {
    let mut folders = BTreeSet::new();
    let mut files = Vec::new();
    for f in &snapshot.files {
        if !f.key.starts_with(prefix) {
            continue;
        }
        let rest = &f.key[prefix.len()..];
        if let Some(slash) = rest.find('/') {
            folders.insert(rest[..slash + 1].to_string());
        } else {
            files.push(f);
        }
    }
    (folders.into_iter().collect(), files)
}

fn conflict_count(snapshot: &Snapshot) -> usize {
    snapshot
        .files
        .iter()
        .filter(|f| f.status == FileStatus::Conflict)
        .count()
}

fn parent_folder(key: &str) -> String {
    match key.rfind('/') {
        Some(i) => key[..=i].to_string(),
        None => String::new(),
    }
}

fn strip_marks(s: &str) -> String {
    s.replace("<mark>", "").replace("</mark>", "")
}

/// Maps engine FileStatus to the six-state design-system sync glyph + color.
/// Returns a function that builds the icon, paired with its tint color.
fn status_glyph_icon(
    status: FileStatus,
) -> (
    fn(iced::Color) -> iced::widget::svg::Svg<'static, iced::Theme>,
    iced::Color,
) {
    match status {
        FileStatus::Synced => (icons::check_circle, theme::SYNC_SYNCED),
        // PendingUpload/NewLocal map to Syncing (violet) per spec derivation.
        FileStatus::PendingUpload | FileStatus::NewLocal => {
            (icons::refresh_cw, theme::SYNC_SYNCING)
        }
        FileStatus::DeletedPending => (icons::clock, theme::SYNC_QUEUED),
        FileStatus::Conflict => (icons::alert_triangle, theme::SYNC_CONFLICT),
        FileStatus::RemoteOnly => (icons::cloud, theme::SYNC_REMOTE),
    }
}

fn short_date(mtime_millis: u64) -> String {
    let Some(dt) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(mtime_millis as i64)
    else {
        return "?".into();
    };
    let now = chrono::Utc::now();
    let delta = now - dt;
    if delta.num_days() == 0 {
        "Today".into()
    } else if delta.num_days() == -1 {
        "Yesterday".into()
    } else if delta.num_days() < 0 {
        format!("{} days ago", -delta.num_days())
    } else {
        dt.format("%-d %b").to_string()
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "—".into();
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn ms_ago(ms: u64) -> String {
    let now = chrono::Local::now().timestamp_millis() as u64;
    let secs = now.saturating_sub(ms) / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

fn poll_snapshot() -> Task<Message> {
    Task::perform(ipc::fetch_snapshot(SOCKET.into()), map_snapshot)
}

enum Item {
    Folder(String),
    File(String),
}

fn visible_items(c: &Conn) -> Vec<Item> {
    let (folders, files) = split_dirs(&c.snapshot, &c.path);
    let mut v: Vec<Item> = folders.into_iter().map(Item::Folder).collect();
    v.extend(files.iter().map(|f| Item::File(f.key.clone())));
    v
}

fn move_cursor(c: &mut Conn, delta: i32) {
    if !c.search_query.trim().is_empty() {
        return;
    }
    let items = visible_items(c);
    if items.is_empty() {
        c.cursor = None;
        return;
    }
    let n = items.len() as i32;
    let next = match c.cursor {
        None => {
            if delta > 0 {
                0
            } else {
                (n - 1) as usize
            }
        }
        Some(cur) => ((cur as i32 + delta).rem_euclid(n)) as usize,
    };
    c.cursor = Some(next);
    c.selection.clear();
    c.selection_anchor = match &items[next] {
        Item::Folder(n) => Some(n.clone()),
        Item::File(k) => Some(k.clone()),
    };
    if let Item::File(k) = &items[next] {
        c.selection.insert(k.clone());
    }
}

fn activate_cursor(c: &Conn) -> Option<Task<Message>> {
    let items = visible_items(c);
    let idx = c.cursor?;
    match items.get(idx)? {
        Item::Folder(name) => Some(Task::done(Message::Open(name.clone()))),
        Item::File(key) => Some(Task::done(Message::OpenFile(key.clone()))),
    }
}

fn map_event(
    event: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(button)) => match button {
            iced::mouse::Button::Back => Some(Message::NavigateBack),
            iced::mouse::Button::Forward => Some(Message::NavigateForward),
            iced::mouse::Button::Left => {
                LEFT_DOWN.store(true, Ordering::Relaxed);
                None
            }
            _ => None,
        },
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
            LEFT_DOWN.store(false, Ordering::Relaxed);
            if status == iced::event::Status::Ignored {
                Some(Message::MouseReleased)
            } else {
                None
            }
        }
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
            CURSOR_X.store(position.x.to_bits(), Ordering::Relaxed);
            CURSOR_Y.store(position.y.to_bits(), Ordering::Relaxed);
            // Only forward cursor movement as a message while the left button
            // is held (drag tracking). Forwarding every mouse-move would
            // trigger a full re-view — including any open context menu — on
            // every pixel of movement.
            if LEFT_DOWN.load(Ordering::Relaxed) {
                Some(Message::CursorMoved(position))
            } else {
                None
            }
        }
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(mods)) => {
            Some(Message::ModifiersChanged(mods))
        }
        iced::Event::Window(iced::window::Event::FileHovered(path)) => {
            Some(Message::FileHovered(path))
        }
        iced::Event::Window(iced::window::Event::FileDropped(path)) => {
            Some(Message::FileDropped(path))
        }
        iced::Event::Window(iced::window::Event::FilesHoveredLeft) => {
            Some(Message::FilesHoveredLeft)
        }
        _ => None,
    }
}

fn map_key(event: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed {
        key,
        modifiers,
        repeat: _,
        ..
    } = event
    else {
        return None;
    };

    // ⌘K / Ctrl+K opens search palette.
    if (modifiers.control() || modifiers.command()) && key == Key::Character("k".into()) {
        return Some(Message::FocusSearch);
    }

    // Alt+Left / Alt+Right for back/forward navigation.
    if modifiers.alt() {
        return match key {
            Key::Named(Named::ArrowLeft) => Some(Message::NavigateBack),
            Key::Named(Named::ArrowRight) => Some(Message::NavigateForward),
            _ => None,
        };
    }

    if modifiers.control() || modifiers.command() {
        return match key {
            Key::Character(ref c) => match c.as_str() {
                "a" => Some(Message::SelectAll),
                "c" => Some(Message::ClipboardCopy),
                "x" => Some(Message::ClipboardCut),
                "v" => Some(Message::ClipboardPaste),
                "1" => Some(Message::SetViewMode(ViewMode::List)),
                "2" => Some(Message::SetViewMode(ViewMode::Grid)),
                _ => None,
            },
            _ => None,
        };
    }
    match key {
        Key::Named(Named::Escape) => Some(Message::Escape),
        Key::Named(Named::Backspace) => Some(Message::Backspace),
        Key::Named(Named::Enter) => Some(Message::ActivateCursor),
        Key::Character(ref c) => match c.as_str() {
            "/" => Some(Message::FocusSearch),
            "j" => Some(Message::MoveCursor(1)),
            "k" => Some(Message::MoveCursor(-1)),
            _ => None,
        },
        _ => None,
    }
}

fn run_search(query: String) -> Task<Message> {
    let socket = SOCKET.to_string();
    Task::perform(
        async move {
            tokio::time::sleep(Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
            let r = ipc::search(socket, query.clone(), SEARCH_LIMIT).await;
            (query, r)
        },
        |(query, r)| match r {
            Ok(hits) => Message::SearchResults(query, hits),
            Err(_) => Message::SearchResults(query, Vec::new()),
        },
    )
}

fn map_status(r: Result<Status, anyhow::Error>) -> Message {
    match r {
        Ok(s) => Message::StatusFetched(Some(s), None),
        Err(e) => Message::StatusFetched(None, Some(e.to_string())),
    }
}

fn map_snapshot(r: Result<Snapshot, anyhow::Error>) -> Message {
    match r {
        Ok(s) => Message::SnapshotFetched(Some(s), None),
        Err(e) => Message::SnapshotFetched(None, Some(e.to_string())),
    }
}

fn map_action(r: Result<(), anyhow::Error>) -> Message {
    match r {
        Ok(()) => Message::ActionResult(Ok(())),
        Err(e) => Message::ActionResult(Err(e.to_string())),
    }
}

/// Open a file or folder with the OS default handler.
fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";

    let _ = std::process::Command::new(cmd).arg(path).spawn();
}
