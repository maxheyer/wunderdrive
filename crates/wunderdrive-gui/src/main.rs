mod app;
mod ipc;
mod theme;

fn main() -> iced::Result {
    iced::application(app::new, app::update, app::view)
        .subscription(app::subscription)
        .title(|_: &app::App| String::from("wunderdrive"))
        .theme(|_: &app::App| theme::theme())
        .run()
}
