use iced::border;
use iced::theme::Palette;
use iced::widget::{button, container, scrollable, text_input};
use iced::{color, Background, Border, Color, Theme};

pub const INTER: &[u8] = include_bytes!("../assets/fonts/InterVariable.ttf");
pub const INTER_NAME: &str = "Inter";
pub const JETBRAINS_MONO: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
pub const MONO_NAME: &str = "JetBrains Mono";

pub fn mono_font() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Name(MONO_NAME),
        ..iced::Font::DEFAULT
    }
}

pub fn theme() -> Theme {
    Theme::custom(
        "WunderDark",
        Palette {
            background: APP,
            text: INK,
            primary: ACCENT,
            success: SUCCESS,
            warning: WARNING,
            danger: ERROR,
        },
    )
}

// App surfaces (hue 235, blue-tinted darks)
pub const APP: Color = color!(0x1C1D26);
pub const APP_BOX: Color = color!(0x272835);
pub const APP_DARK_BOX: Color = color!(0x21222B);
pub const APP_INPUT: Color = color!(0x2C2D3B);
pub const APP_HOVER: Color = color!(0x292A37);
pub const APP_SELECTED: Color = color!(0x353646);
pub const APP_LINE: Color = color!(0x323343);
pub const APP_OVERLAY: Color = color!(0x252632);

// Sidebar (darkest surface)
pub const SIDEBAR: Color = color!(0x0F1015);
pub const SIDEBAR_BOX: Color = color!(0x232430);
pub const SIDEBAR_DIVIDER: Color = color!(0x252632);
pub const SIDEBAR_BUTTON: Color = color!(0x272835);

// Accent
pub const ACCENT: Color = color!(0x2599FF);
pub const ACCENT_FAINT: Color = color!(0x5DB4FF);

// Text hierarchy
pub const INK: Color = color!(0xE4E5F2);
pub const INK_DULL: Color = color!(0xABACBA);
pub const INK_FAINT: Color = color!(0x818398);

// Status
pub const SUCCESS: Color = color!(0x16A34A);
pub const WARNING: Color = color!(0xFBA517);
pub const ERROR: Color = color!(0xE5484D);

// Scrollbar
pub const SCROLLBAR_THUMB: Color = color!(0x393948);

// ---- Button styles ----

pub fn row_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => APP_HOVER,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: INK,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(6.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn selected_row_button(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(APP_SELECTED)),
        text_color: INK,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(6.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn grid_cell_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => APP_HOVER,
        _ => APP_BOX,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: INK,
        border: Border {
            color: APP_LINE,
            width: 1.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn grid_cell_button_selected(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(APP_SELECTED)),
        text_color: INK,
        border: Border {
            color: ACCENT,
            width: 1.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Disabled => Color { a: 0.4, ..ACCENT },
        _ => ACCENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: color!(0xFFFFFF),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(6.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn gray_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => APP_HOVER,
        _ => SIDEBAR_BUTTON,
    };
    let border_color = match status {
        button::Status::Hovered | button::Status::Pressed => APP_LINE,
        _ => Color { a: 0.5, ..APP_LINE },
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: INK,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: border::radius(6.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn subtle_button(_theme: &Theme, status: button::Status) -> button::Style {
    let (bg, border_color) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (Color::TRANSPARENT, APP_LINE)
        }
        _ => (Color::TRANSPARENT, Color::TRANSPARENT),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: INK_DULL,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: border::radius(6.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn icon_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => APP_HOVER,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: INK_DULL,
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
        background: Some(Background::Color(SIDEBAR)),
        text_color: Some(INK),
        border: Border {
            color: APP_LINE,
            width: 0.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn top_bar_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(APP)),
        text_color: Some(INK),
        border: Border {
            color: APP_LINE,
            width: 0.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn status_bar_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(APP)),
        text_color: Some(INK_DULL),
        border: Border {
            color: APP_LINE,
            width: 1.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn badge_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color { a: 0.2, ..ERROR })),
        text_color: Some(ERROR),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(999.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn preview_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(APP_DARK_BOX)),
        text_color: Some(INK),
        border: Border {
            color: APP_LINE,
            width: 1.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn search_pill_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(APP_OVERLAY)),
        text_color: Some(INK),
        border: Border {
            color: Color { a: 0.3, ..APP_LINE },
            width: 1.0,
            radius: border::radius(999.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

// ---- Text input style ----

pub fn borderless_input(_theme: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(0.0),
        },
        icon: INK_FAINT,
        placeholder: INK_FAINT,
        value: INK,
        selection: Color { a: 0.3, ..ACCENT },
    }
}

// ---- Scrollable style ----

pub fn thin_scrollable(_theme: &Theme, _status: scrollable::Status) -> scrollable::Style {
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: scrollable::Rail {
            background: Some(Background::Color(Color::TRANSPARENT)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(999.0),
            },
            scroller: scrollable::Scroller {
                background: Background::Color(SCROLLBAR_THUMB),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: border::radius(999.0),
                },
            },
        },
        horizontal_rail: scrollable::Rail {
            background: Some(Background::Color(Color::TRANSPARENT)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(999.0),
            },
            scroller: scrollable::Scroller {
                background: Background::Color(SCROLLBAR_THUMB),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: border::radius(999.0),
                },
            },
        },
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(Color::TRANSPARENT),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(0.0),
            },
            shadow: Default::default(),
            icon: INK_FAINT,
        },
    }
}
