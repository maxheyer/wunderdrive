mod app;
mod ipc;

fn main() -> iced::Result {
    iced::application(app::new, app::update, app::view)
        .title(|_: &app::App| String::from("wunderdrive"))
        .theme(|_: &app::App| iced::Theme::Dark)
        .run()
}
