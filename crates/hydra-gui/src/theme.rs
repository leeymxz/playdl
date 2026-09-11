// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! Widget styling.
//!
//! Classic Windows 11 dialogs are the reference: `#F0F0F0` dialog chrome, white
//! content surfaces, `1px #ADADAD` buttons that light up `#E5F1FB`/`#0078D7`
//! on hover, `#CCE8FF` selection, hairline `#E5E5E5` grid lines. Every style
//! function branches on the palette so the View > Theme setting restyles
//! the same widgets without a second style set.

use crate::model::ThemeMode;
use iced::widget::{button, checkbox, container, pick_list, progress_bar, text_input};
use iced::{Background, Border, Color, Theme};

/// The text size the whole layout is written against: every `.size(...)` in
/// the UI, and every padding, row height and column width sitting next to
/// one, assumes this. View > Font moves the interface off it by scaling
/// everything at once (see [`ui_scale`]) rather than by resizing text inside
/// boxes that would then be the wrong height for it.
pub const FONT_SIZE: f32 = 13.0;

/// The View > Font entries: the label and the text size it stands for. Both
/// menu surfaces (`ui::menu` in-window, `macos_menu` native) build their Font
/// group from this, so the tick lines up with the setting on either.
pub const FONT_CHOICES: [(&str, u16); 3] = [("Small", 12), ("Medium", 13), ("Large", 15)];

/// The View > Theme entries: the label and the mode it stands for. Both menu
/// surfaces (`ui::menu` in-window, `macos_menu` native) build their Theme
/// group from this, so the tick lines up with the setting on either.
pub const THEME_CHOICES: [(&str, ThemeMode); 3] = [
    ("System Default", ThemeMode::System),
    ("Light", ThemeMode::Light),
    ("Dark", ThemeMode::Dark),
];

/// The window scale factor for a View > Font choice: the ratio of the chosen
/// size to the [`FONT_SIZE`] the layout was drawn at (12 -> 0.92, 13 -> 1.0,
/// 15 -> 1.15). iced multiplies the whole interface by it, so text, the rows
/// and buttons around it and the dialogs those sit in all grow together.
///
/// Clamped either side of a factor of two, and a config missing the field
/// (`font_size` 0) reads as the base size, so a hand-edited config.toml
/// cannot leave the app unreadable or the windows off-screen.
pub fn ui_scale(font_size: u16) -> f32 {
    if font_size == 0 {
        return 1.0;
    }
    (f32::from(font_size) / FONT_SIZE).clamp(0.5, 2.0)
}

/// The OS appearance right now, for View > Theme > System Default.
///
/// iced reports the system theme too, but only once a window exists —
/// `iced::system::theme()` answers `Mode::None` before that — and a session
/// that starts in System Default must paint the right palette on its first
/// frame rather than flash light and correct itself. Flips after startup
/// arrive through `iced::system::theme_changes`; this is the starting value.
pub fn system_is_dark() -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        matches!(dark_light::detect(), Ok(dark_light::Mode::Dark))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

pub fn is_dark(theme: &Theme) -> bool {
    theme.extended_palette().background.base.text.r > 0.5
}

fn c(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// The light palette (reference), with dark counterparts chosen inline.
pub const WINDOW_BG: u32 = 0xF0F0F0;
pub const WINDOW_BG_DARK: u32 = 0x2B2B2B;
pub const SURFACE: u32 = 0xFFFFFF;
pub const SURFACE_DARK: u32 = 0x1E1E1E;
pub const TEXT: u32 = 0x1A1A1A;
pub const TEXT_DARK: u32 = 0xE6E6E6;
pub const TEXT_DIM: u32 = 0x6D6D6D;
pub const BORDER: u32 = 0xADADAD;
pub const BORDER_SOFT: u32 = 0xDCDCDC;
pub const GRID: u32 = 0xE5E5E5;
pub const GRID_DARK: u32 = 0x3A3A3A;
pub const ACCENT: u32 = 0x0067C0;
pub const HOVER_BG: u32 = 0xE5F1FB;
pub const HOVER_BORDER: u32 = 0x0078D7;
pub const SELECT_BG: u32 = 0xCCE8FF;
pub const SELECT_BG_DARK: u32 = 0x264F78;
pub const PROGRESS_GREEN: u32 = 0x3A9E3A;
pub const CHUNK_BLUE: u32 = 0x3B6FD4;
pub const CHUNK_TRACK: u32 = 0xDDE6F5;
pub const MENU_BG: u32 = 0xF9F9F9;
pub const MENU_BG_DARK: u32 = 0x2D2D2D;

pub fn window_bg(theme: &Theme) -> Color {
    if is_dark(theme) {
        c(WINDOW_BG_DARK)
    } else {
        c(WINDOW_BG)
    }
}

pub fn surface(theme: &Theme) -> Color {
    if is_dark(theme) {
        c(SURFACE_DARK)
    } else {
        c(SURFACE)
    }
}

pub fn text_color(theme: &Theme) -> Color {
    if is_dark(theme) {
        c(TEXT_DARK)
    } else {
        c(TEXT)
    }
}

pub fn dim_text(_theme: &Theme) -> Color {
    c(TEXT_DIM)
}

pub fn grid_line(theme: &Theme) -> Color {
    if is_dark(theme) {
        c(GRID_DARK)
    } else {
        c(GRID)
    }
}

fn border(color: Color, width: f32, radius: f32) -> Border {
    Border {
        color,
        width,
        radius: radius.into(),
    }
}

// ---------------------------------------------------------------- containers

/// Whole-window chrome (`#F0F0F0`).
pub fn window(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(window_bg(theme))),
        text_color: Some(text_color(theme)),
        ..Default::default()
    }
}

/// White content surface: the download table, tree panel, list boxes.
pub fn panel(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(surface(theme))),
        border: border(
            if is_dark(theme) {
                c(GRID_DARK)
            } else {
                c(BORDER_SOFT)
            },
            1.0,
            0.0,
        ),
        text_color: Some(text_color(theme)),
        ..Default::default()
    }
}

/// Dropdown menu / context menu panel.
pub fn menu_panel(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(if is_dark(theme) {
            c(MENU_BG_DARK)
        } else {
            c(MENU_BG)
        })),
        // No shadow: tiny-skia (the default renderer) draws blurred shadows
        // as a heavy dark slab around the panel, so a crisp 1px border is
        // the classic-Windows-menu look we actually want.
        border: border(
            if is_dark(theme) {
                c(GRID_DARK)
            } else {
                c(0xC8C8C8)
            },
            1.0,
            4.0,
        ),
        text_color: Some(text_color(theme)),
        ..Default::default()
    }
}

/// The blue strip segments of "start positions and progress by connections".
pub fn chunk_on(theme: &Theme) -> container::Style {
    let _ = theme;
    container::Style {
        background: Some(Background::Color(c(CHUNK_BLUE))),
        ..Default::default()
    }
}

pub fn chunk_off(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(if is_dark(theme) {
            c(0x333B4A)
        } else {
            c(CHUNK_TRACK)
        })),
        ..Default::default()
    }
}

// ------------------------------------------------------------------- buttons

/// Classic dialog push button (OK / Cancel / Browse...).
pub fn btn(theme: &Theme, status: button::Status) -> button::Style {
    let dark = is_dark(theme);
    let (bg, bd, txt) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (c(HOVER_BG), c(HOVER_BORDER), c(TEXT))
        }
        button::Status::Disabled => (
            if dark { c(0x333333) } else { c(0xF5F5F5) },
            if dark { c(GRID_DARK) } else { c(BORDER_SOFT) },
            c(TEXT_DIM),
        ),
        _ => (
            if dark { c(0x3C3C3C) } else { c(SURFACE) },
            if dark { c(0x5A5A5A) } else { c(BORDER) },
            if dark { c(TEXT_DARK) } else { c(TEXT) },
        ),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: txt,
        border: border(bd, 1.0, 4.0),
        ..Default::default()
    }
}

/// Default button of a dialog (OK): accent border like Win11.
pub fn btn_primary(theme: &Theme, status: button::Status) -> button::Style {
    let mut s = btn(theme, status);
    if !matches!(status, button::Status::Disabled) {
        s.border = border(c(ACCENT), 1.0, 4.0);
    }
    s
}

/// Toolbar button: flat, hover-tinted.
pub fn btn_toolbar(theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(Background::Color(if is_dark(theme) {
                c(0x3A3A3A)
            } else {
                c(0xE8E8E8)
            }))
        }
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: match status {
            button::Status::Disabled => c(TEXT_DIM),
            _ => text_color(theme),
        },
        border: border(Color::TRANSPARENT, 0.0, 4.0),
        ..Default::default()
    }
}

/// Menu-bar title ("Tasks", "File", ...) and dropdown row.
pub fn btn_menu(theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(Background::Color(if is_dark(theme) {
                c(0x094771)
            } else {
                c(SELECT_BG)
            }))
        }
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: match status {
            button::Status::Disabled => c(TEXT_DIM),
            _ => text_color(theme),
        },
        border: border(Color::TRANSPARENT, 0.0, 3.0),
        ..Default::default()
    }
}

/// A row of the download table / tree / any list box.
/// A download-list row. The rows are containers rather than buttons — a
/// button paints the hand cursor and reports its press only on release, which
/// kills drag-selection — so hover is passed in from `App::hover_row`.
pub fn row_cell(selected: bool, hovered: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let dark = is_dark(theme);
        let bg = if selected {
            Some(Background::Color(if dark {
                c(SELECT_BG_DARK)
            } else {
                c(SELECT_BG)
            }))
        } else if hovered {
            Some(Background::Color(if dark {
                c(0x2A3A4A)
            } else {
                c(0xE9F3FD)
            }))
        } else {
            None
        };
        container::Style {
            background: bg,
            text_color: Some(text_color(theme)),
            ..Default::default()
        }
    }
}

/// The rubber-band rectangle a drag-selection paints over the list:
/// translucent selection fill inside a solid 1 px edge, like a listview
/// marquee.
pub fn band(theme: &Theme) -> container::Style {
    let dark = is_dark(theme);
    let edge = if dark { c(0x4A9EFF) } else { c(0x3399FF) };
    container::Style {
        background: Some(Background::Color(Color { a: 0.25, ..edge })),
        border: border(edge, 1.0, 0.0),
        ..Default::default()
    }
}

/// Column header cell of the download list: flat on the surface colour like
/// the classic listview header — no box border, the hairline separators
/// between columns are drawn by the grip strips. A container rather than a
/// button, so the header keeps the arrow cursor the rest of the list has.
pub fn header_cell(hovered: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let dark = is_dark(theme);
        let bg = if hovered {
            if dark {
                c(0x2E2E2E)
            } else {
                c(0xF5F9FF)
            }
        } else {
            surface(theme)
        };
        container::Style {
            background: Some(Background::Color(bg)),
            text_color: Some(text_color(theme)),
            ..Default::default()
        }
    }
}

pub fn btn_row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let dark = is_dark(theme);
        let bg = if selected {
            Some(Background::Color(if dark {
                c(SELECT_BG_DARK)
            } else {
                c(SELECT_BG)
            }))
        } else if matches!(status, button::Status::Hovered) {
            Some(Background::Color(if dark {
                c(0x2A3A4A)
            } else {
                c(0xE9F3FD)
            }))
        } else {
            None
        };
        button::Style {
            background: bg,
            text_color: text_color(theme),
            border: border(Color::TRANSPARENT, 0.0, 0.0),
            ..Default::default()
        }
    }
}

/// Tab header of a tab strip (Options dialog, progress dialog).
pub fn btn_tab(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let dark = is_dark(theme);
        let bg = if selected {
            surface(theme)
        } else if matches!(status, button::Status::Hovered) {
            if dark {
                c(0x3A3A3A)
            } else {
                c(0xE8E8E8)
            }
        } else {
            window_bg(theme)
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: text_color(theme),
            border: border(if dark { c(GRID_DARK) } else { c(BORDER_SOFT) }, 1.0, 0.0),
            ..Default::default()
        }
    }
}

// ------------------------------------------------------------ inputs & misc

pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let dark = is_dark(theme);
    text_input::Style {
        background: Background::Color(surface(theme)),
        border: border(
            match status {
                text_input::Status::Focused { .. } => c(ACCENT),
                text_input::Status::Hovered => c(HOVER_BORDER),
                _ if dark => c(0x5A5A5A),
                _ => c(0x7A7A7A),
            },
            1.0,
            2.0,
        ),
        icon: dim_text(theme),
        placeholder: c(TEXT_DIM),
        value: text_color(theme),
        selection: c(SELECT_BG),
    }
}

pub fn check(theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let dark = is_dark(theme);
    let checked = matches!(
        status,
        checkbox::Status::Active { is_checked: true }
            | checkbox::Status::Hovered { is_checked: true }
    );
    let disabled = matches!(status, checkbox::Status::Disabled { .. });
    checkbox::Style {
        background: Background::Color(if checked {
            c(0x0B5CAB)
        } else if dark {
            c(0x3C3C3C)
        } else {
            c(SURFACE)
        }),
        icon_color: Color::WHITE,
        border: border(
            if disabled {
                c(BORDER_SOFT)
            } else if checked {
                c(0x0B5CAB)
            } else {
                c(0x7A7A7A)
            },
            1.0,
            3.0,
        ),
        text_color: Some(if disabled {
            c(TEXT_DIM)
        } else {
            text_color(theme)
        }),
    }
}

pub fn progress(theme: &Theme) -> progress_bar::Style {
    progress_bar::Style {
        background: Background::Color(if is_dark(theme) {
            c(0x3A3A3A)
        } else {
            c(0xE0E0E0)
        }),
        bar: Background::Color(c(PROGRESS_GREEN)),
        border: border(
            if is_dark(theme) {
                c(GRID_DARK)
            } else {
                c(0xC8C8C8)
            },
            1.0,
            0.0,
        ),
    }
}

/// Track of the indeterminate virus-scan bar: the same well the determinate
/// progress bar draws, so the swap is invisible except for the motion.
pub fn scan_track(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(if is_dark(theme) {
            c(0x3A3A3A)
        } else {
            c(0xE0E0E0)
        })),
        border: border(
            if is_dark(theme) {
                c(GRID_DARK)
            } else {
                c(0xC8C8C8)
            },
            1.0,
            0.0,
        ),
        ..Default::default()
    }
}

/// The block sliding inside [`scan_track`].
pub fn scan_block(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(c(PROGRESS_GREEN))),
        ..Default::default()
    }
}

pub fn picker(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let dark = is_dark(theme);
    pick_list::Style {
        text_color: text_color(theme),
        placeholder_color: c(TEXT_DIM),
        handle_color: text_color(theme),
        background: Background::Color(surface(theme)),
        border: border(
            match status {
                pick_list::Status::Hovered | pick_list::Status::Opened { .. } => c(HOVER_BORDER),
                _ if dark => c(0x5A5A5A),
                _ => c(0x7A7A7A),
            },
            1.0,
            2.0,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_choices_scale_by_their_ratio() {
        // The View > Font entries (ui/menu.rs, macos_menu.rs).
        assert_eq!(ui_scale(13), 1.0);
        assert!((ui_scale(12) - 12.0 / 13.0).abs() < f32::EPSILON);
        assert!((ui_scale(15) - 15.0 / 13.0).abs() < f32::EPSILON);
        // A config written before the field existed, and hand-edited
        // nonsense, both stay usable.
        assert_eq!(ui_scale(0), 1.0);
        assert_eq!(ui_scale(2), 0.5);
        assert_eq!(ui_scale(400), 2.0);
    }
}
