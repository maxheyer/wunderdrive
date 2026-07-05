use iced::border;
use iced::theme::Palette;
use iced::widget::{button, container};
use iced::{color, Background, Border, Color, Theme};

/// Apple-like dark theme.
pub fn theme() -> Theme {
    Theme::custom(
        "WunderDark",
        Palette {
            background: color!(0x1e1e1e),
            text: color!(0xffffff),
            primary: color!(0x0a84ff),
            success: color!(0x30d158),
            warning: color!(0xff9f0a),
            danger: color!(0xff453a),
        },
    )
}

pub const BG_SIDEBAR: Color = color!(0x252526);
pub const BG_CONTENT: Color = color!(0x1e1e1e);
pub const BG_HOVER: Color = color!(0xffffff, 0.06);
pub const TEXT_SECONDARY: Color = color!(0x8e8e93);
pub const ACCENT: Color = color!(0x0a84ff);
pub const SEPARATOR: Color = color!(0xffffff, 0.08);

// ---- Button styles ----

pub fn row_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Active | button::Status::Disabled => Color::TRANSPARENT,
        button::Status::Hovered | button::Status::Pressed => BG_HOVER,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: color!(0xffffff),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(6.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn sidebar_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Active | button::Status::Disabled => Color::TRANSPARENT,
        button::Status::Hovered | button::Status::Pressed => BG_HOVER,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_SECONDARY,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(6.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn back_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Active | button::Status::Disabled => Color::TRANSPARENT,
        button::Status::Hovered | button::Status::Pressed => BG_HOVER,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: ACCENT,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(6.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

// ---- Container styles ----

pub fn sidebar_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_SIDEBAR)),
        text_color: Some(color!(0xffffff)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}
