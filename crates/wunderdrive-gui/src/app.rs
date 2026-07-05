use iced::widget::{button, column, text};
use iced::{Element, Task};

use crate::ipc;

const SOCKET: &str = "wunderdrive";

pub struct App {
    state: AppState,
}

enum AppState {
    Connecting,
    Connected(wunderdrive_engine::Status),
    Error(String),
}

#[derive(Debug, Clone)]
pub enum Message {
    StatusFetched(Option<wunderdrive_engine::Status>, Option<String>),
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
            state.state = AppState::Connected(status);
            Task::none()
        }
        Message::StatusFetched(_, Some(e)) => {
            state.state = AppState::Error(e);
            Task::none()
        }
        Message::StatusFetched(None, None) => {
            state.state = AppState::Error("unknown error".into());
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
        AppState::Connecting => column![text("Connecting to daemon…").size(24)]
            .padding(40)
            .into(),
        AppState::Connected(s) => column![
            text("wunderdrive").size(28),
            text(format!("Bucket: {}", s.bucket)),
            text(format!(
                "Endpoint: {}",
                s.endpoint.as_deref().unwrap_or("default")
            )),
            text(format!("Local: {}", s.local_root)),
            text(if s.paused { "Paused" } else { "Syncing" }),
        ]
        .padding(40)
        .spacing(8)
        .into(),
        AppState::Error(e) => column![
            text(format!("Error: {e}")).size(18),
            button("Retry").on_press(Message::Retry),
        ]
        .padding(40)
        .spacing(16)
        .into(),
    }
}

fn map_status(r: Result<wunderdrive_engine::Status, anyhow::Error>) -> Message {
    match r {
        Ok(s) => Message::StatusFetched(Some(s), None),
        Err(e) => Message::StatusFetched(None, Some(e.to_string())),
    }
}
