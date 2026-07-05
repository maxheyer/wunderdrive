use std::collections::BTreeSet;
use std::time::Duration;

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Task};
use wunderdrive_engine::{FileStat, FileStatus, Snapshot, Status};

use crate::{ipc, theme};

const SOCKET: &str = "wunderdrive";

pub struct App {
    state: AppState,
}

enum AppState {
    Connecting,
    Connected {
        status: Status,
        snapshot: Snapshot,
        path: String,
    },
    Error(String),
}

#[derive(Debug, Clone)]
pub enum Message {
    StatusFetched(Option<Status>, Option<String>),
    SnapshotFetched(Option<Snapshot>, Option<String>),
    Open(String),
    NavigateUp,
    Retry,
}

pub fn new() -> (App, Task<Message>) {
    (
        App {
            state: AppState::Connecting,
        },
        Task::perform(ipc::fetch_status(SOCKET.into()), map_status),
    )
}

pub fn update(state: &mut App, msg: Message) -> Task<Message> {
    match msg {
        Message::StatusFetched(Some(status), _) => {
            state.state = AppState::Connected {
                status,
                snapshot: Snapshot::default(),
                path: String::new(),
            };
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
            if let AppState::Connected { snapshot, .. } = &mut state.state {
                *snapshot = snap;
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
            if let AppState::Connected { path, .. } = &mut state.state {
                if name.ends_with('/') {
                    path.push_str(&name);
                }
            }
            Task::none()
        }
        Message::NavigateUp => {
            if let AppState::Connected { path, .. } = &mut state.state {
                if let Some(idx) = path.trim_end_matches('/').rfind('/') {
                    path.truncate(idx + 1);
                } else {
                    path.clear();
                }
            }
            Task::none()
        }
        Message::Retry => {
            state.state = AppState::Connecting;
            Task::perform(ipc::fetch_status(SOCKET.into()), map_status)
        }
    }
}

pub fn view(state: &App) -> Element<'_, Message> {
    match &state.state {
        AppState::Connecting => centered_text("Connecting to daemon…", 24.0),
        AppState::Connected {
            status,
            snapshot,
            path,
        } => main_layout(status, snapshot, path),
        AppState::Error(e) => column![
            text(format!("Error: {e}")).size(18),
            button(text("Retry")).on_press(Message::Retry),
        ]
        .padding(40)
        .spacing(16)
        .into(),
    }
}

fn main_layout(status: &Status, snapshot: &Snapshot, path: &str) -> Element<'static, Message> {
    row![sidebar(status, snapshot), content(status, snapshot, path),]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn sidebar(status: &Status, snapshot: &Snapshot) -> Element<'static, Message> {
    container(
        column![
            text("wunderdrive").size(16),
            text(status.bucket.clone())
                .size(13)
                .color(theme::TEXT_SECONDARY),
            text(format!("{} files", snapshot.files.len()))
                .size(12)
                .color(theme::TEXT_SECONDARY),
            text(if status.paused { "Paused" } else { "Syncing" })
                .size(12)
                .color(if status.paused {
                    theme::TEXT_SECONDARY
                } else {
                    theme::ACCENT
                }),
        ]
        .padding(16)
        .spacing(4),
    )
    .width(Length::Fixed(200.0))
    .height(Length::Fill)
    .style(theme::sidebar_container)
    .into()
}

fn content(status: &Status, snapshot: &Snapshot, path: &str) -> Element<'static, Message> {
    let (folders, files) = split_dirs(snapshot, path);
    let count = folders.len() + files.len();

    let mut list = column![].spacing(1).padding(4);

    if !path.is_empty() {
        list = list.push(
            button(text("← Back").size(13))
                .on_press(Message::NavigateUp)
                .style(theme::back_button)
                .padding([6, 12]),
        );
    }

    for name in &folders {
        list = list.push(folder_row(name));
    }
    for f in &files {
        let display = f.key[path.len()..].to_string();
        list = list.push(file_row(display, f.size, f.status));
    }

    if count == 0 {
        let msg: Element<'static, Message> = text("No files")
            .size(14)
            .color(theme::TEXT_SECONDARY)
            .into();
        list = list.push(msg);
    }

    container(
        column![
            breadcrumb(status, path),
            scrollable(list).height(Length::Fill),
            status_bar(count, status),
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn breadcrumb(status: &Status, path: &str) -> Element<'static, Message> {
    let display_path = if path.is_empty() {
        status.bucket.clone()
    } else {
        format!("{} / {}", status.bucket, path.trim_end_matches('/'))
    };
    container(text(display_path).size(14).color(theme::TEXT_SECONDARY))
        .padding([10, 16])
        .into()
}

fn status_bar(count: usize, status: &Status) -> Element<'static, Message> {
    let sync = status
        .last_sync_millis
        .map(ms_ago)
        .unwrap_or_else(|| "never".into());
    container(
        text(format!("{count} items · last sync: {sync}"))
            .size(12)
            .color(theme::TEXT_SECONDARY),
    )
    .padding([6, 16])
    .into()
}

fn folder_row(name: &str) -> Element<'static, Message> {
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
    .style(theme::row_button)
    .into()
}

fn file_row(name: String, size: u64, status: FileStatus) -> Element<'static, Message> {
    let glyph = status_glyph(status);
    let size_text = if status == FileStatus::RemoteOnly {
        "remote".to_string()
    } else {
        human_size(size)
    };
    let glyph_color = match status {
        FileStatus::Synced => theme::ACCENT,
        FileStatus::Conflict => iced::color!(0xff453a),
        FileStatus::RemoteOnly => theme::TEXT_SECONDARY,
        _ => theme::TEXT_SECONDARY,
    };

    button(
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
    .style(theme::row_button)
    .into()
}

fn centered_text(msg: &str, size: f32) -> Element<'_, Message> {
    container(text(msg).size(size)).padding(40).into()
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
