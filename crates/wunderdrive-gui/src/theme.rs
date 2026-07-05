#![allow(dead_code)]

use iced::border;
use iced::theme::Palette;
use iced::widget::{button, container, scrollable, text_input};
use iced::{color, Background, Border, Color, Theme};

pub const INTER: &[u8] = include_bytes!("../assets/fonts/InterVariable.ttf");
pub const INTER_NAME: &str = "Inter";
pub const JETBRAINS_MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
pub const MONO_NAME: &str = "JetBrains Mono";

pub fn mono_font() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Name(MONO_NAME),
        ..iced::Font::DEFAULT
    }
}

// ============================================================================
// TOKENS — single source of truth (design system §3.1)
// No hex literals belong anywhere outside this block.
// ============================================================================

// ---- Surfaces & strokes ----
pub const BG_APP: Color = color!(0x0B0D13);
pub const BG_SIDEBAR: Color = color!(0x0E1119);
pub const BG_SURFACE: Color = color!(0x151A26);
pub const BG_ELEVATED: Color = color!(0x1B2233);
pub const BG_HOVER: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.04);
pub const BG_SELECTED: Color = Color::from_rgba(0.545, 0.361, 0.965, 0.14);

pub const STROKE_SUBTLE: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.07);
pub const STROKE_STRONG: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.14);
pub const BG_KBD: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.05);

// ---- Text hierarchy ----
pub const TEXT_PRIMARY: Color = color!(0xEDF0F7);
pub const TEXT_SECONDARY: Color = color!(0x9AA3B8);
pub const TEXT_TERTIARY: Color = color!(0x5D6579);
pub const TEXT_ON_ACCENT: Color = color!(0xFFFFFF);

// ---- Accent (violet) ----
pub const ACCENT: Color = color!(0x8B5CF6);
pub const ACCENT_HOVER: Color = color!(0xA78BFA);
pub const ACCENT_ACTIVE: Color = color!(0x7C3AED);
pub const ACCENT_TEXT: Color = color!(0xC4B5FD);
pub const ACCENT_TINT: Color = Color::from_rgba(0.545, 0.361, 0.965, 0.14);

// ---- Sync-state language (design system §3.2) ----
pub const SYNC_SYNCED: Color = color!(0x34D399);
pub const SYNC_SYNCING: Color = ACCENT;
pub const SYNC_QUEUED: Color = color!(0x7B8496);
pub const SYNC_CONFLICT: Color = color!(0xFBBF24);
pub const SYNC_ERROR: Color = color!(0xF87171);
pub const SYNC_REMOTE: Color = color!(0x38BDF8);

// ---- Legacy aliases (map old names → new tokens, removed as views migrate) ----
// Only the names still referenced from app.rs are kept.
pub const INK_DULL: Color = TEXT_SECONDARY;
pub const INK_FAINT: Color = TEXT_TERTIARY;
pub const SUCCESS: Color = SYNC_SYNCED;
pub const WARNING: Color = SYNC_CONFLICT;
pub const ERROR: Color = SYNC_ERROR;
pub const SIDEBAR_DIVIDER: Color = STROKE_SUBTLE;
pub const SCROLLBAR_THUMB: Color = color!(0x393948);

// ---- Theme ----

pub fn theme() -> Theme {
    Theme::custom(
        "Wunderdrive Dark",
        Palette {
            background: BG_APP,
            text: TEXT_PRIMARY,
            primary: ACCENT,
            success: SYNC_SYNCED,
            warning: SYNC_CONFLICT,
            danger: SYNC_ERROR,
        },
    )
}

// ============================================================================
// STYLES — closure-based styling per iced 0.14 API
// ============================================================================

// ---- Button styles ----

pub fn row_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => BG_HOVER,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_PRIMARY,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn selected_row_button(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(BG_SELECTED)),
        text_color: TEXT_PRIMARY,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn grid_cell_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => BG_HOVER,
        _ => BG_SURFACE,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_PRIMARY,
        border: Border {
            color: STROKE_SUBTLE,
            width: 1.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn grid_cell_button_selected(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(BG_SELECTED)),
        text_color: TEXT_PRIMARY,
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
        button::Status::Hovered => ACCENT_HOVER,
        button::Status::Pressed => ACCENT_ACTIVE,
        _ => ACCENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_ON_ACCENT,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn gray_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => BG_HOVER,
        _ => BG_SURFACE,
    };
    let border_color = match status {
        button::Status::Hovered | button::Status::Pressed => STROKE_STRONG,
        _ => STROKE_SUBTLE,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_PRIMARY,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn subtle_button(_theme: &Theme, status: button::Status) -> button::Style {
    let (bg, border_color) = match status {
        button::Status::Hovered | button::Status::Pressed => (Color::TRANSPARENT, STROKE_STRONG),
        _ => (Color::TRANSPARENT, Color::TRANSPARENT),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_SECONDARY,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn icon_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => BG_HOVER,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_SECONDARY,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

// ---- Container styles ----

pub fn sidebar_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_SIDEBAR)),
        text_color: Some(TEXT_PRIMARY),
        border: Border {
            color: STROKE_SUBTLE,
            width: 0.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn top_bar_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_APP)),
        text_color: Some(TEXT_PRIMARY),
        border: Border {
            color: STROKE_SUBTLE,
            width: 0.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn status_bar_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_APP)),
        text_color: Some(TEXT_SECONDARY),
        border: Border {
            color: STROKE_SUBTLE,
            width: 1.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn badge_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color {
            a: 0.2,
            ..SYNC_CONFLICT
        })),
        text_color: Some(SYNC_CONFLICT),
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
        background: Some(Background::Color(BG_ELEVATED)),
        text_color: Some(TEXT_PRIMARY),
        border: Border {
            color: STROKE_SUBTLE,
            width: 1.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn search_pill_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_SURFACE)),
        text_color: Some(TEXT_PRIMARY),
        border: Border {
            color: STROKE_SUBTLE,
            width: 1.0,
            radius: border::radius(8.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn card_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_SURFACE)),
        text_color: Some(TEXT_PRIMARY),
        border: Border {
            color: STROKE_SUBTLE,
            width: 1.0,
            radius: border::radius(12.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

pub fn conflict_edge_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: None,
        text_color: None,
        border: Border {
            color: SYNC_CONFLICT,
            width: 0.0,
            radius: border::radius(0.0),
        },
        shadow: Default::default(),
        snap: true,
    }
}

// ---- Text input style ----

pub fn borderless_input(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let border_color = match status {
        text_input::Status::Focused { .. } => ACCENT,
        _ => Color::TRANSPARENT,
    };
    let border_width = match status {
        text_input::Status::Focused { .. } => 2.0,
        _ => 0.0,
    };
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border {
            color: border_color,
            width: border_width,
            radius: border::radius(0.0),
        },
        icon: TEXT_TERTIARY,
        placeholder: TEXT_TERTIARY,
        value: TEXT_PRIMARY,
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
            icon: TEXT_TERTIARY,
        },
    }
}
