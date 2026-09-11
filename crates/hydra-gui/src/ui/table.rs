// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! The download list: File Name | Q | Size | Status | Time left |
//! Transfer rate | Last Try Date | Description, with a flat listview-style
//! header, sortable columns and a horizontal scrollbar. Q is the
//! icon-only queue-membership strip.
//!
//! The header and the ruled empty grid below the last download are drawn as
//! layers floating over the rows (see [`view`]) rather than as content of
//! their own: the header then stays at the top of the viewport instead of
//! scrolling away, and the grid stops adding height the rows do not need, so
//! the vertical scrollbar shows up only once the downloads really overflow
//! the window.
//!
//! Only the rows inside the scrolled viewport are built (see [`view`]): iced
//! rebuilds and relayouts the whole tree on every message, so a rubber-band
//! sweep over a few hundred downloads otherwise rebuilt a few hundred rows —
//! ~17 widgets each — for every pointer motion event.

use std::collections::HashSet;

use crate::app::{App, El, Message, SortKey};
use crate::model::{DlId, DownloadItem};
use crate::{fmt, i18n::tr, icons, theme};
use iced::widget::{column, container, mouse_area, row, scrollable, stack, svg, text};
use iced::Length;

const COLS: [(&str, f32, Option<SortKey>); 8] = [
    ("File Name", 300.0, Some(SortKey::Name)),
    // "Q" strip: icon-only, so the width stays at the resize minimum.
    ("Q", 40.0, Some(SortKey::Queue)),
    ("Size", 110.0, Some(SortKey::Size)),
    ("Status", 120.0, Some(SortKey::Status)),
    ("Time left", 130.0, Some(SortKey::TimeLeft)),
    ("Transfer rate", 150.0, Some(SortKey::Rate)),
    ("Last Try Date", 175.0, Some(SortKey::LastTry)),
    ("Description", 200.0, Some(SortKey::Description)),
];

/// Width of the draggable divider between header cells.
const GRIP: f32 = 6.0;

/// Height of one cell, and of the 1 px rule drawn under it.
const CELL_H: f32 = 24.0;
/// Pitch of the list: what one row (header row included) advances by. The
/// virtual window is measured in these, so it must match the real layout.
const ROW_H: f32 = CELL_H + 1.0;
/// Rows built above and below the viewport, so a scroll or a resize that
/// lands between two frames never uncovers a gap.
const OVERSCAN: usize = 6;

pub fn default_widths() -> Vec<f32> {
    COLS.iter().map(|c| c.1).collect()
}

fn widths(app: &App) -> Vec<f32> {
    if app.col_widths.len() == COLS.len() {
        app.col_widths.clone()
    } else {
        default_widths()
    }
}

fn total_width(w: &[f32]) -> f32 {
    w.iter().sum::<f32>() + GRIP * w.len() as f32
}

fn header<'a>(app: &App, w: &[f32], tw: f32) -> El<'a> {
    let mut r = row![].spacing(0);
    for (i, (label, _, key)) in COLS.iter().enumerate() {
        let arrow = match key {
            Some(k) if app.sort.0 == *k => {
                if app.sort.1 {
                    " ▴"
                } else {
                    " ▾"
                }
            }
            _ => "",
        };
        let cell = container(
            text(format!("{}{arrow}", tr(label)))
                .size(theme::FONT_SIZE)
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .padding([3, 6])
        .width(w[i])
        .height(CELL_H)
        .clip(true)
        .style(theme::header_cell(app.hover_col == Some(i)));
        // A container under a `mouse_area`, not a `button`: a button forces the
        // hand cursor, and `mouse_area::interaction` can only override a child
        // that asks for none.
        let cell = mouse_area(cell)
            .interaction(iced::mouse::Interaction::Idle)
            .on_enter(Message::HeaderEnter(i))
            .on_exit(Message::HeaderExit(i));
        r = match key {
            Some(k) => r.push(cell.on_press(Message::SortBy(*k))),
            None => r.push(cell),
        };
        // Drag grip on the column's right edge, drawing the same hairline the
        // body rows do so the separators run continuously like a listview.
        r = r.push(
            mouse_area(rule_grip())
                .interaction(iced::mouse::Interaction::ResizingHorizontally)
                .on_press(Message::ColResizeStart(i)),
        );
    }
    // The strip is painted as a whole, not cell by cell: the header floats
    // over the rows, and the drag grips between the cells are transparent, so
    // an unpainted header would show the list moving through its seams.
    column![
        container(r).width(tw).style(theme::header_cell(false)),
        row_line(tw)
    ]
    .into()
}

fn cell<'a>(content: El<'a>, w: f32) -> El<'a> {
    container(content)
        .width(w)
        .height(CELL_H)
        .padding([2, 6])
        .clip(true)
        .into()
}

/// The divider strip between columns: full drag-handle width, drawing one
/// centred 1 px vertical line — a single hairline, no boxed gaps.
fn rule_grip<'a>() -> El<'a> {
    container(hairline())
        .width(GRIP)
        .height(CELL_H)
        .padding(iced::Padding {
            left: (GRIP - 1.0) / 2.0,
            ..iced::Padding::ZERO
        })
        .into()
}

/// A bare 1 px vertical line, as tall as it is given.
fn hairline<'a>() -> El<'a> {
    container(iced::widget::space::horizontal())
        .width(1.0)
        .height(Length::Fill)
        .style(|t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(theme::grid_line(t))),
            ..Default::default()
        })
        .into()
}

/// Where the hairline between column `i` and the next one sits, measured from
/// the left edge of the list.
fn line_x(w: &[f32], i: usize) -> f32 {
    w[..=i].iter().sum::<f32>() + GRIP * i as f32 + (GRIP - 1.0) / 2.0
}

/// 1 px horizontal rule under a row.
fn row_line<'a>(tw: f32) -> El<'a> {
    container(iced::widget::space::horizontal())
        .width(tw)
        .height(1.0)
        .style(|t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(theme::grid_line(t))),
            ..Default::default()
        })
        .into()
}

/// Blank block standing in for rows outside the viewport, at exactly the
/// height they would take — the content height, and with it the scrollbar,
/// must not depend on how much of the list is currently built.
fn spacer<'a>(tw: f32, h: f32) -> El<'a> {
    container(iced::widget::space::horizontal())
        .width(tw)
        .height(h)
        .into()
}

/// The ruled empty grid below the last download, as one block rather than a
/// row of widgets per line: every filler row was an identical press target,
/// so `rows` of them cost ~17 widgets each for nothing.
///
/// Layer 1 draws the horizontal rules and fixes the block's height; layer 2
/// runs the column hairlines down it. The block is drawn beside the list
/// rather than inside it (see [`view`]), so it takes the list's sideways
/// scroll offset `off` and places its own hairlines; `rows` is an upper bound
/// on what can be seen, and iced's flex layout drops whatever does not fit in
/// the space left below the last download.
fn filler_block<'a>(w: &[f32], tw: f32, off: f32, rows: usize) -> El<'a> {
    let rw = (tw - off).max(0.0);
    let mut lines = column![].width(rw);
    for _ in 0..rows {
        lines = lines.push(spacer(rw, CELL_H));
        lines = lines.push(row_line(rw));
    }
    let mut verts = row![].spacing(0);
    let mut placed = 0.0;
    for i in 0..w.len() {
        let x = line_x(w, i) - off;
        if x < placed {
            continue;
        }
        verts = verts.push(
            container(iced::widget::space::horizontal())
                .width(x - placed)
                .height(Length::Fill),
        );
        verts = verts.push(hairline());
        placed = x + 1.0;
    }
    mouse_area(stack![lines, verts.height(Length::Fill)])
        .interaction(iced::mouse::Interaction::Idle)
        .on_press(Message::EmptyPress)
        .on_enter(Message::EmptyEnter)
        .into()
}

/// File-type glyph for a row: the item's category (or, when unset, the
/// extension-based guess) picks the same icon the category tree uses.
fn file_icon(app: &App, d: &DownloadItem) -> iced::widget::svg::Handle {
    let cat = d
        .category
        .clone()
        .or_else(|| crate::model::categorize(&d.file_name, &app.cfg.categories));
    match cat.as_deref() {
        Some(c) => crate::ui::categories::cat_icon(c),
        None => icons::file_generic(),
    }
}

/// The Q cell: shows the queue's folder icon (in the queue's own colour)
/// while the item belongs to a queue and drops it once the download
/// completes — no queue-name text.
fn queue_glyph<'a>(app: &App, d: &DownloadItem) -> El<'a> {
    if let Some(q) = d
        .queue
        .as_deref()
        .filter(|_| d.state != crate::model::DlState::Complete)
    {
        container(
            svg(icons::queue_folder(app.queue_color(q)))
                .width(14.0)
                .height(14.0),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::Alignment::Center)
        .align_y(iced::Alignment::Center)
        .into()
    } else {
        text("").size(theme::FONT_SIZE).into()
    }
}

fn data_row<'a>(app: &App, d: &'a DownloadItem, w: &[f32], tw: f32, selected: bool) -> El<'a> {
    let size_txt = d.size.map(fmt::size2).unwrap_or_default();
    let eta_txt = match d.state {
        crate::model::DlState::Receiving => d.eta_secs.map(fmt::eta).unwrap_or_default(),
        _ => String::new(),
    };
    let rate_txt = if d.state.is_active() && d.rate > 0.0 {
        fmt::rate_steady(d.rate, app.effective_limit(d))
    } else {
        String::new()
    };
    let grip = rule_grip;
    let content = row![
        cell(
            row![
                svg(file_icon(app, d)).width(15.0).height(15.0),
                text(&d.file_name)
                    .size(theme::FONT_SIZE)
                    .wrapping(iced::widget::text::Wrapping::None),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center)
            .into(),
            w[0],
        ),
        grip(),
        cell(queue_glyph(app, d), w[1]),
        grip(),
        cell(text(size_txt).size(theme::FONT_SIZE).into(), w[2]),
        grip(),
        cell(text(d.status_text()).size(theme::FONT_SIZE).into(), w[3]),
        grip(),
        cell(text(eta_txt).size(theme::FONT_SIZE).into(), w[4]),
        grip(),
        cell(text(rate_txt).size(theme::FONT_SIZE).into(), w[5]),
        grip(),
        cell(
            text(d.last_try.map(fmt::date).unwrap_or_default())
                .size(theme::FONT_SIZE)
                .into(),
            w[6]
        ),
        grip(),
        cell(text(&d.description).size(theme::FONT_SIZE).into(), w[7]),
        grip(),
    ]
    .spacing(0);

    // The row is a plain container, not a button: `button` reports its press
    // only on mouse-*release* (so the drag never knows where it started) and
    // always paints the hand cursor. `mouse_area` presses on the way down,
    // which is what both drag-selection and the arrow cursor need.
    column![
        mouse_area(
            container(content)
                .width(tw)
                .style(theme::row_cell(selected, app.hover_row == Some(d.id))),
        )
        .interaction(iced::mouse::Interaction::Idle)
        .on_press(Message::RowClick(d.id))
        .on_right_press(Message::RowRightClick(d.id))
        .on_enter(Message::RowEnter(d.id))
        .on_exit(Message::RowExit(d.id)),
        row_line(tw),
    ]
    .into()
}

pub fn view(app: &App) -> El<'_> {
    let w = widths(app);
    let tw = total_width(&w);
    let items = app.visible();
    let n = items.len();
    // Membership lookup for the selected-row styling: `Vec::contains` per row
    // made painting quadratic once a sweep had selected the whole list.
    let sel: HashSet<DlId> = app.selected.iter().copied().collect();

    // Rows the viewport can show. `table_vh` comes from the scrollable itself
    // and is 0 until it first reports one (a list shorter than the viewport
    // never scrolls, and then nothing is virtualised anyway), so fall back to
    // the window height — an over-estimate only ever builds spare rows.
    let vh = if app.table_vh > 1.0 {
        app.table_vh
    } else {
        app.main_size.height.max(1200.0)
    };
    let per_screen = (vh / ROW_H).ceil() as usize + 1;
    // The header sits over the first row of the content, so it offsets the
    // first data row.
    let first = ((app.table_scroll - ROW_H).max(0.0) / ROW_H).floor() as usize;
    let first = first.saturating_sub(OVERSCAN).min(n);
    let last = first.saturating_add(per_screen + 2 * OVERSCAN).min(n);

    // Both spacers are pushed even at zero height: iced matches widget state
    // to children by position, so a leading child that comes and goes would
    // shift every row's `mouse_area` state by one as the window slides.
    let mut rows = column![].width(tw);
    // The header's slot. The header itself is a layer floating over the rows,
    // but it still owns the first row of the content: that keeps the scroll
    // range long enough for the last download to clear it.
    rows = rows.push(spacer(tw, ROW_H));
    rows = rows.push(spacer(tw, first as f32 * ROW_H));
    for d in &items[first..last] {
        rows = rows.push(data_row(app, d, &w, tw, sel.contains(&d.id)));
    }
    rows = rows.push(spacer(tw, (n - last) as f32 * ROW_H));

    // The header, pushed back down by exactly what the list has scrolled: it
    // stays pinned to the top of the viewport while the rows run under it.
    // As a layer it adds no height of its own, so it cannot lengthen the list.
    let head = column![spacer(tw, app.table_scroll), header(app, &w, tw)].width(tw);

    // Rows first: `stack` takes its size from the bottom layer, so only the
    // rows decide how far the list scrolls.
    let list = scrollable(stack![rows, head].width(tw))
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        // Feeds the virtual window above, and the empty grid below. The
        // scrollable only reports a viewport when it actually scrolls, and it
        // drops repeats, so an idle list produces no messages.
        .on_scroll(|v| {
            let at = v.absolute_offset();
            Message::TableScrolled(at.y, at.x, v.bounds().height)
        })
        .width(Length::Fill)
        .height(Length::Fill);

    // The ruled empty grid below the last download, under the list rather than
    // in it: as content it would make the list taller than the window whatever
    // it holds, and the vertical scrollbar would never go away. It starts where
    // the rows end, and once they fill the window there is nothing left for it.
    let below = (ROW_H + n as f32 * ROW_H - app.table_scroll).max(0.0);
    let fillers = (vh.max(app.main_size.height) / ROW_H).ceil() as usize + 1;
    let grid = column![
        spacer(tw, below),
        filler_block(&w, tw, app.table_scroll_x, fillers)
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    container(stack![grid, list])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::panel)
        .into()
}
