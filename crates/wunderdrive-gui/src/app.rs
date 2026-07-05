use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use iced::keyboard::{self, key::Named, Key};
use iced::widget::{
    button, column, container, row, scrollable, space::Space, text, text_input, Grid,
};
use iced::{Alignment, Element, Length, Subscription, Task};
use wunderdrive_engine::protocol::Resolution;
use wunderdrive_engine::{FileStat, FileStatus, SearchHit, Snapshot, Status};

use crate::{ipc, theme};

const SOCKET: &str = "wunderdrive";
const SEARCH_LIMIT: usize = 100;
const SEARCH_DEBOUNCE_MS: u64 = 150;
const SEARCH_ID: iced::widget::Id = iced::widget::Id::new("search");

pub struct App {
    state: AppState,
}

enum AppState {
    Connecting,
    Connected(Conn),
    Error(String),
}

struct Conn {
    status: Status,
    snapshot: Snapshot,
    path: String,
    search_query: String,
    search_hits: Vec<SearchHit>,
    last_error: Option<String>,
    expanded_conflict: Option<String>,
    show_preview: bool,
    selected: Option<String>,
    preview: Option<(String, PreviewContent)>,
    view_mode: ViewMode,
    cursor: Option<usize>,
    first_snapshot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    List,
    Grid,
}

#[derive(Debug, Clone)]
pub enum PreviewContent {
    Text(String),
    Binary,
    Error(String),
}

#[derive(Debug, Clone)]
pub enum Message {
    StatusFetched(Option<Status>, Option<String>),
    SnapshotFetched(Option<Snapshot>, Option<String>),
    Open(String),
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
    TogglePreview,
    PreviewLoaded(String, PreviewContent),
    FocusSearch,
    Escape,
    Backspace,
    MoveCursor(i32),
    ActivateCursor,
    ToggleViewMode,
    ActionResult(Result<(), String>),
}

pub fn new() -> (App, Task<Message>) {
    (
        App {
            state: AppState::Connecting,
        },
        Task::perform(ipc::fetch_status(SOCKET.into()), map_status),
    )
}

pub fn subscription(_state: &App) -> Subscription<Message> {
    keyboard::listen().filter_map(map_key)
}

pub fn update(state: &mut App, msg: Message) -> Task<Message> {
    match msg {
        Message::StatusFetched(Some(status), _) => {
            state.state = AppState::Connected(Conn {
                status,
                snapshot: Snapshot::default(),
                path: String::new(),
                search_query: String::new(),
                search_hits: Vec::new(),
                last_error: None,
                expanded_conflict: None,
                show_preview: false,
                selected: None,
                preview: None,
                view_mode: ViewMode::List,
                cursor: None,
                first_snapshot: true,
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
                    c.path.push_str(&name);
                    c.cursor = None;
                    c.selected = None;
                }
            }
            Task::none()
        }
        Message::NavigateUp => {
            if let AppState::Connected(c) = &mut state.state {
                if let Some(idx) = c.path.trim_end_matches('/').rfind('/') {
                    c.path.truncate(idx + 1);
                } else {
                    c.path.clear();
                }
                c.cursor = None;
                c.selected = None;
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
                c.selected = Some(key.clone());
                c.preview = None;
                if let Some(f) = c.snapshot.files.iter().find(|f| f.key == key) {
                    if f.status != FileStatus::RemoteOnly && is_text_ext(&key) {
                        return load_preview(c.status.local_root.clone(), key);
                    }
                }
            }
            Task::none()
        }
        Message::TogglePreview => {
            if let AppState::Connected(c) = &mut state.state {
                c.show_preview = !c.show_preview;
            }
            Task::none()
        }
        Message::PreviewLoaded(key, content) => {
            if let AppState::Connected(c) = &mut state.state {
                if c.selected.as_deref() == Some(key.as_str()) {
                    c.preview = Some((key, content));
                }
            }
            Task::none()
        }
        Message::FocusSearch => iced::widget::operation::focus(SEARCH_ID),
        Message::Escape => {
            if let AppState::Connected(c) = &mut state.state {
                if !c.search_query.is_empty() {
                    c.search_query.clear();
                    c.search_hits.clear();
                } else if c.show_preview {
                    c.show_preview = false;
                } else {
                    c.cursor = None;
                    c.selected = None;
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
        Message::ActionResult(Ok(())) => {
            if let AppState::Connected(c) = &mut state.state {
                c.last_error = None;
            }
            Task::none()
        }
        Message::ActionResult(Err(e)) => {
            tracing::warn!("action failed: {e}");
            if let AppState::Connected(c) = &mut state.state {
                c.last_error = Some(e);
            }
            Task::none()
        }
    }
}

pub fn view(state: &App) -> Element<'_, Message> {
    match &state.state {
        AppState::Connecting => centered_text("Connecting to daemon…", 24.0),
        AppState::Connected(c) => main_layout(c),
        AppState::Error(e) => column![
            text(format!("Error: {e}")).size(18),
            button(text("Retry")).on_press(Message::Retry),
        ]
        .padding(40)
        .spacing(16)
        .into(),
    }
}

fn main_layout(c: &Conn) -> Element<'_, Message> {
    if c.show_preview {
        row![sidebar(c), content(c), preview_pane(c),]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        row![sidebar(c), content(c),]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn sidebar(c: &Conn) -> Element<'_, Message> {
    let conflicts = conflict_count(&c.snapshot);
    let mut top = column![text("wunderdrive").size(16)].spacing(4).padding(16);

    top = top.push(
        text(c.status.bucket.clone())
            .size(13)
            .color(theme::TEXT_SECONDARY),
    );
    top = top.push(
        text(format!("{} files", c.snapshot.files.len()))
            .size(12)
            .color(theme::TEXT_SECONDARY),
    );
    if conflicts > 0 {
        top = top.push(
            container(
                text(format!(
                    "{} conflict{}",
                    conflicts,
                    if conflicts == 1 { "" } else { "s" }
                ))
                .size(11),
            )
            .style(theme::badge_container)
            .padding([2, 8]),
        );
    }

    let sync_label = if c.status.paused { "Paused" } else { "Syncing" };
    let toggle_label = if c.status.paused { "Resume" } else { "Pause" };

    let controls = column![
        text(sync_label.to_string())
            .size(11)
            .color(theme::TEXT_SECONDARY),
        button(text("Sync now").size(12))
            .on_press(Message::SyncNow)
            .width(Length::Fill)
            .padding([6, 10])
            .style(theme::primary_button),
        button(text(toggle_label.to_string()).size(12))
            .on_press(Message::PauseResume)
            .width(Length::Fill)
            .padding([6, 10])
            .style(theme::ghost_button),
    ]
    .spacing(6)
    .padding(16);

    container(column![top, Space::new().height(Length::Fill), controls].height(Length::Fill))
        .width(Length::Fixed(200.0))
        .height(Length::Fill)
        .style(theme::sidebar_container)
        .into()
}

fn content(c: &Conn) -> Element<'_, Message> {
    let searching = !c.search_query.trim().is_empty();
    let (folders, files) = split_dirs(&c.snapshot, &c.path);
    let total = folders.len() + files.len();

    let body: Element<'_, Message> = if searching {
        search_results_view(&c.search_hits)
    } else if total == 0 {
        if c.first_snapshot {
            centered_text("Loading…", 16.0)
        } else {
            empty_state()
        }
    } else {
        file_list_view(
            &c.path,
            &folders,
            &files,
            c.expanded_conflict.as_deref(),
            c.view_mode,
            c.cursor,
        )
    };

    container(
        column![
            search_bar(&c.search_query),
            breadcrumb(&c.status, &c.path, searching, c.view_mode, c.show_preview),
            body,
            status_bar(total, &c.status, c.last_error.as_deref()),
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn search_bar(query: &str) -> Element<'_, Message> {
    container(
        text_input("Search files by name or content…", query)
            .id(SEARCH_ID)
            .on_input(Message::SearchQuery)
            .size(14)
            .padding([8, 12]),
    )
    .padding([10, 16])
    .into()
}

fn breadcrumb<'a>(
    status: &'a Status,
    path: &'a str,
    searching: bool,
    view_mode: ViewMode,
    preview_on: bool,
) -> Element<'a, Message> {
    let label = if searching {
        format!("Search: {}", status.bucket)
    } else if path.is_empty() {
        status.bucket.clone()
    } else {
        format!("{} / {}", status.bucket, path.trim_end_matches('/'))
    };
    let view_label = match view_mode {
        ViewMode::List => "Grid",
        ViewMode::Grid => "List",
    };
    let preview_label = if preview_on { "Hide" } else { "Preview" };
    container(
        row![
            text(label)
                .size(14)
                .color(theme::TEXT_SECONDARY)
                .width(Length::Fill),
            button(text(view_label.to_string()).size(12))
                .on_press(Message::ToggleViewMode)
                .padding([4, 10])
                .style(theme::ghost_button),
            button(text(preview_label.to_string()).size(12))
                .on_press(Message::TogglePreview)
                .padding([4, 10])
                .style(theme::ghost_button),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    )
    .padding([10, 16])
    .into()
}

fn status_bar<'a>(
    count: usize,
    status: &'a Status,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    let sync = status
        .last_sync_millis
        .map(ms_ago)
        .unwrap_or_else(|| "never".into());
    let msg = match error {
        Some(e) => format!("⚠ {e}"),
        None => format!("{count} items · last sync: {sync}"),
    };
    container(text(msg).size(12).color(theme::TEXT_SECONDARY))
        .padding([6, 16])
        .into()
}

fn file_list_view(
    path: &str,
    folders: &[String],
    files: &[&FileStat],
    expanded_conflict: Option<&str>,
    view_mode: ViewMode,
    cursor: Option<usize>,
) -> Element<'static, Message> {
    let n_folders = folders.len();

    let mut list = column![].spacing(1).padding(4);

    if !path.is_empty() {
        list = list.push(
            button(text("← Back").size(13))
                .on_press(Message::NavigateUp)
                .style(theme::back_button)
                .padding([6, 12]),
        );
    }

    if view_mode == ViewMode::Grid {
        let mut g = Grid::new().fluid(120.0).spacing(8);
        for (i, name) in folders.iter().enumerate() {
            g = g.push(grid_cell(name, cursor == Some(i)));
        }
        for (i, f) in files.iter().enumerate() {
            let display = f.key[path.len()..].to_string();
            let selected = cursor == Some(n_folders + i);
            g = g.push(file_grid_cell(f.key.clone(), display, f.status, selected));
        }
        list = list.padding(8).push(g);
    } else {
        for (i, name) in folders.iter().enumerate() {
            list = list.push(folder_row(name, cursor == Some(i)));
        }
        for (i, f) in files.iter().enumerate() {
            let display = f.key[path.len()..].to_string();
            let is_expanded = expanded_conflict == Some(f.key.as_str());
            let selected = cursor == Some(n_folders + i);
            list = list.push(file_row(
                f.key.clone(),
                display,
                f.size,
                f.status,
                is_expanded,
                selected,
            ));
        }
    }

    scrollable(list).height(Length::Fill).into()
}

fn search_results_view(hits: &[SearchHit]) -> Element<'static, Message> {
    if hits.is_empty() {
        return centered_text("No matches", 14.0);
    }
    let mut list = column![].spacing(1).padding(4);
    for h in hits {
        let parent = parent_folder(&h.key);
        list = list.push(search_row(&h.key, h.snippet.as_deref(), parent));
    }
    scrollable(list).height(Length::Fill).into()
}

fn search_row(key: &str, snippet: Option<&str>, parent: String) -> Element<'static, Message> {
    let mut col = column![text(key.to_string()).size(13)].spacing(2);
    if let Some(s) = snippet {
        let clean = strip_marks(s);
        col = col.push(text(clean).size(11).color(theme::TEXT_SECONDARY));
    }
    button(col)
        .on_press(Message::Open(parent))
        .width(Length::Fill)
        .padding([6, 12])
        .style(theme::row_button)
        .into()
}

fn folder_row(name: &str, selected: bool) -> Element<'static, Message> {
    let display = name.trim_end_matches('/');
    button(
        row![
            text("📁").width(Length::Fixed(24.0)),
            text(display.to_string()).size(14),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .on_press(Message::Open(name.to_string()))
    .width(Length::Fill)
    .padding([6, 12])
    .style(if selected {
        theme::selected_row_button
    } else {
        theme::row_button
    })
    .into()
}

fn grid_cell(name: &str, selected: bool) -> Element<'static, Message> {
    let display = name.trim_end_matches('/');
    button(
        column![text("📁").size(28.0), text(display.to_string()).size(11),]
            .spacing(6)
            .align_x(Alignment::Center),
    )
    .on_press(Message::Open(name.to_string()))
    .width(Length::Fill)
    .padding([12, 6])
    .style(if selected {
        theme::selected_row_button
    } else {
        theme::row_button
    })
    .into()
}

fn file_grid_cell(
    key: String,
    name: String,
    status: FileStatus,
    selected: bool,
) -> Element<'static, Message> {
    let glyph_color = match status {
        FileStatus::Conflict => theme::DANGER,
        FileStatus::Synced => theme::ACCENT,
        _ => theme::TEXT_SECONDARY,
    };
    let icon = match status {
        FileStatus::RemoteOnly => "☁",
        FileStatus::Conflict => "⚠",
        _ => "📄",
    };
    let mut btn = button(
        column![
            text(icon.to_string()).size(28.0).color(glyph_color),
            text(name).size(11),
        ]
        .spacing(6)
        .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([12, 6])
    .style(if selected {
        theme::selected_row_button
    } else {
        theme::row_button
    });
    if status == FileStatus::RemoteOnly {
        btn = btn.on_press(Message::Materialize(key));
    } else {
        btn = btn.on_press(Message::SelectFile(key));
    }
    btn.into()
}

fn file_row(
    key: String,
    name: String,
    size: u64,
    status: FileStatus,
    conflict_expanded: bool,
    selected: bool,
) -> Element<'static, Message> {
    let glyph = status_glyph(status);
    let size_text = if status == FileStatus::RemoteOnly {
        "remote · click to download".to_string()
    } else {
        human_size(size)
    };
    let glyph_color = match status {
        FileStatus::Synced => theme::ACCENT,
        FileStatus::Conflict => theme::DANGER,
        FileStatus::RemoteOnly => theme::TEXT_SECONDARY,
        _ => theme::TEXT_SECONDARY,
    };

    let mut row_btn = button(
        row![
            text(glyph).width(Length::Fixed(24.0)).color(glyph_color),
            text(name).size(14).width(Length::Fill),
            text(size_text).size(12).color(theme::TEXT_SECONDARY),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([6, 12])
    .style(if selected {
        theme::selected_row_button
    } else {
        theme::row_button
    });

    match status {
        FileStatus::RemoteOnly => {
            row_btn = row_btn
                .on_press(Message::Materialize(key.clone()))
                .style(if selected {
                    theme::selected_row_button
                } else {
                    theme::back_button
                });
        }
        FileStatus::Conflict => {
            row_btn = row_btn.on_press(Message::ToggleConflict(key.clone()));
        }
        _ => {
            row_btn = row_btn.on_press(Message::SelectFile(key.clone()));
        }
    }

    if status == FileStatus::Conflict && conflict_expanded {
        let actions = row![
            res_button("Keep local", key.clone(), Resolution::KeepLocal),
            res_button("Keep remote", key.clone(), Resolution::KeepRemote),
            res_button("Keep both", key.clone(), Resolution::KeepBoth),
        ]
        .spacing(6)
        .padding(iced::Padding::new(0.0).left(36.0).right(12.0).bottom(6.0));
        column![row_btn, actions].spacing(1).into()
    } else {
        row_btn.into()
    }
}

fn res_button(label: &str, key: String, resolution: Resolution) -> Element<'static, Message> {
    button(text(label.to_string()).size(12))
        .on_press(Message::ResolveConflict(key, resolution))
        .padding([4, 10])
        .style(theme::ghost_button)
        .into()
}

fn preview_pane(c: &Conn) -> Element<'_, Message> {
    let body: Element<'_, Message> = match &c.selected {
        None => centered_text("No file selected", 13.0),
        Some(key) => match c.snapshot.files.iter().find(|f| f.key == *key) {
            None => centered_text("File not found", 13.0),
            Some(f) => {
                if f.status == FileStatus::RemoteOnly {
                    preview_remote_only(f)
                } else if is_text_ext(key) {
                    preview_text(key, c.preview.as_ref())
                } else {
                    preview_metadata(f)
                }
            }
        },
    };

    container(
        column![
            preview_header(c.selected.as_deref()),
            scrollable(body).height(Length::Fill),
        ]
        .height(Length::Fill),
    )
    .width(Length::FillPortion(2))
    .height(Length::Fill)
    .style(theme::preview_container)
    .into()
}

fn preview_header(selected: Option<&str>) -> Element<'static, Message> {
    let title = selected
        .and_then(|k| k.rsplit('/').next())
        .unwrap_or("Preview")
        .to_string();
    container(
        row![
            text(title).size(14).width(Length::Fill),
            button(text("×".to_string()).size(16))
                .on_press(Message::TogglePreview)
                .padding([2, 8])
                .style(theme::ghost_button),
        ]
        .align_y(Alignment::Center),
    )
    .padding([10, 16])
    .into()
}

fn preview_remote_only(f: &FileStat) -> Element<'static, Message> {
    column![
        text("☁ Not downloaded".to_string())
            .size(14)
            .color(theme::TEXT_SECONDARY),
        text("This file exists only in the remote bucket.".to_string())
            .size(12)
            .color(theme::TEXT_SECONDARY),
        button(text("Download now".to_string()).size(12))
            .on_press(Message::Materialize(f.key.clone()))
            .padding([6, 12])
            .style(theme::primary_button),
    ]
    .spacing(10)
    .padding(16)
    .into()
}

fn preview_text<'a>(
    key: &'a str,
    preview: Option<&'a (String, PreviewContent)>,
) -> Element<'a, Message> {
    match preview {
        Some((k, PreviewContent::Text(s))) if k == key => text(s.clone()).size(12).into(),
        Some((k, PreviewContent::Error(e))) if k == key => {
            text(format!("Could not read file: {e}"))
                .size(12)
                .color(theme::DANGER)
                .into()
        }
        _ => centered_text("Loading…", 13.0),
    }
}

fn preview_metadata(f: &FileStat) -> Element<'static, Message> {
    let mtime = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(f.mtime_millis as i64)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "?".into());
    let status_name = match f.status {
        FileStatus::Synced => "Synced",
        FileStatus::PendingUpload => "Pending upload",
        FileStatus::NewLocal => "New local",
        FileStatus::DeletedPending => "Deleted (pending)",
        FileStatus::Conflict => "Conflict",
        FileStatus::RemoteOnly => "Remote only",
    };
    column![
        meta_row("Key", f.key.clone()),
        meta_row("Size", human_size(f.size)),
        meta_row("Status", status_name.to_string()),
        meta_row("Modified", mtime),
    ]
    .spacing(8)
    .padding(16)
    .into()
}

fn meta_row(label: &str, value: String) -> Element<'static, Message> {
    row![
        text(label.to_string())
            .size(12)
            .color(theme::TEXT_SECONDARY)
            .width(Length::Fixed(80.0)),
        text(value).size(12),
    ]
    .spacing(8)
    .into()
}

fn centered_text(msg: &str, size: f32) -> Element<'_, Message> {
    container(text(msg).size(size)).padding(40).into()
}

fn empty_state() -> Element<'static, Message> {
    column![
        text("No files synced yet".to_string()).size(18),
        text("Drop files into your local mirror folder, or sync the remote bucket.".to_string())
            .size(13)
            .color(theme::TEXT_SECONDARY),
        text("Tip: press / to search.".to_string())
            .size(12)
            .color(theme::TEXT_SECONDARY),
    ]
    .spacing(8)
    .align_x(Alignment::Center)
    .padding(60)
    .into()
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

fn status_glyph(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Synced => "✓",
        FileStatus::PendingUpload => "↑",
        FileStatus::NewLocal => "+",
        FileStatus::DeletedPending => "−",
        FileStatus::Conflict => "!",
        FileStatus::RemoteOnly => "☁",
    }
}

fn is_text_ext(key: &str) -> bool {
    const TEXT_EXTS: &[&str] = &[
        "txt",
        "md",
        "markdown",
        "rs",
        "py",
        "json",
        "toml",
        "yaml",
        "yml",
        "js",
        "ts",
        "tsx",
        "jsx",
        "sh",
        "bash",
        "zsh",
        "c",
        "h",
        "cpp",
        "hpp",
        "cc",
        "go",
        "java",
        "rb",
        "css",
        "scss",
        "html",
        "htm",
        "xml",
        "log",
        "csv",
        "tsv",
        "ini",
        "cfg",
        "conf",
        "sql",
        "kt",
        "swift",
        "lua",
        "vim",
        "nf",
        "env",
        "gitignore",
        "dockerfile",
        "makefile",
    ];
    let lower = key.to_ascii_lowercase();
    if let Some(name) = lower.rsplit('/').next() {
        if let Some(dot) = name.rfind('.') {
            return TEXT_EXTS.contains(&&name[dot + 1..]);
        }
        return matches!(name, "dockerfile" | "makefile" | ".gitignore");
    }
    false
}

fn load_preview(local_root: String, key: String) -> Task<Message> {
    Task::perform(
        async move {
            let path = Path::new(&local_root).join(&key);
            if !is_text_ext(&key) {
                return (key, PreviewContent::Binary);
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    const CAP: usize = 256 * 1024;
                    let truncated = bytes.len() > CAP;
                    let slice = if truncated { &bytes[..CAP] } else { &bytes[..] };
                    let mut s = String::from_utf8_lossy(slice).into_owned();
                    if truncated {
                        s.push_str("\n\n… (truncated)");
                    }
                    (key, PreviewContent::Text(s))
                }
                Err(e) => (key, PreviewContent::Error(e.to_string())),
            }
        },
        |(key, content)| Message::PreviewLoaded(key, content),
    )
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
    c.selected = match &items[next] {
        Item::Folder(_) => None,
        Item::File(k) => Some(k.clone()),
    };
}

fn activate_cursor(c: &Conn) -> Option<Task<Message>> {
    let items = visible_items(c);
    let idx = c.cursor?;
    match items.get(idx)? {
        Item::Folder(name) => Some(Task::done(Message::Open(name.clone()))),
        Item::File(key) => Some(Task::batch(vec![
            Task::done(Message::SelectFile(key.clone())),
            Task::done(Message::TogglePreview),
        ])),
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
    if modifiers.control() || modifiers.command() || modifiers.alt() {
        return None;
    }
    match key {
        Key::Named(Named::Escape) => Some(Message::Escape),
        Key::Named(Named::Backspace) => Some(Message::Backspace),
        Key::Named(Named::Enter) => Some(Message::ActivateCursor),
        Key::Named(Named::Space) => Some(Message::TogglePreview),
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
