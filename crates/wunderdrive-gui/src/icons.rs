//! Bundled Lucide SVG icons, monochrome-tinted via svg::Style { color }.
#![allow(dead_code)]

use iced::widget::svg::{self, Handle};
use iced::Color;
use iced::Length;

macro_rules! icon_handle {
    ($name:literal) => {
        Handle::from_memory(include_bytes!(concat!("../assets/icons/", $name, ".svg")).to_vec())
    };
}

fn svg_icon(handle: Handle, color: Color, size: f32) -> svg::Svg<'static, iced::Theme> {
    svg::Svg::new(handle)
        .style(move |_theme, _status| svg::Style { color: Some(color) })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
}

macro_rules! icon_fn {
    ($name:ident, $file:literal) => {
        pub fn $name(color: Color) -> svg::Svg<'static, iced::Theme> {
            svg_icon(icon_handle!($file), color, 16.0)
        }
    };
}

/// Same as refresh_cw but with a rotation in radians (for animated sync glyph).
pub fn refresh_cw_rotated(color: Color, radians: f32) -> svg::Svg<'static, iced::Theme> {
    svg_icon(icon_handle!("refresh-cw"), color, 16.0).rotation(radians)
}

icon_fn!(folder, "folder");
icon_fn!(file, "file");
icon_fn!(file_text, "file-text");
icon_fn!(image, "image");
icon_fn!(check_circle, "check-circle-2");
icon_fn!(refresh_cw, "refresh-cw");
icon_fn!(clock, "clock");
icon_fn!(alert_triangle, "alert-triangle");
icon_fn!(x_circle, "x-circle");
icon_fn!(cloud, "cloud");
icon_fn!(search, "search");
icon_fn!(settings, "settings");
icon_fn!(arrow_left, "arrow-left");
icon_fn!(layout_grid, "layout-grid");
icon_fn!(list_icon, "list");
icon_fn!(pause, "pause");
icon_fn!(play, "play");
icon_fn!(chevron_right, "chevron-right");
icon_fn!(plus, "plus");
icon_fn!(x, "x");
icon_fn!(copy, "copy");
icon_fn!(scissors, "scissors");
icon_fn!(clipboard_paste, "clipboard-paste");
icon_fn!(trash, "trash");
icon_fn!(pencil, "pencil");
icon_fn!(external_link, "external-link");

/// Type icon for a file key per the v1 mapping:
/// folder → folder, pdf/doc/txt/md → file-text, images → image, else → file.
pub fn type_icon(key: &str, color: Color, size: f32) -> svg::Svg<'static, iced::Theme> {
    let handle = if key.ends_with('/') {
        icon_handle!("folder")
    } else {
        let lower = key.to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        match ext {
            "pdf" | "doc" | "docx" | "txt" | "md" | "markdown" | "rtf" => icon_handle!("file-text"),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tiff" | "ico" => {
                icon_handle!("image")
            }
            _ => icon_handle!("file"),
        }
    };
    svg_icon(handle, color, size)
}
