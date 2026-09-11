// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Add batch download" — paste a list of URLs, review them in an IDM-style
//! sortable table (name, type, size, source, destination), check the ones
//! to add, choose where they are saved ("Download All Links" flow).

use crate::app::{App, BatchRow, BatchSortKey, BatchState, El, Message, WinKind};
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, theme};
use iced::widget::{
    checkbox, column, container, mouse_area, pick_list, radio, row, scrollable, text, text_editor,
    text_input,
};
use iced::{Background, Length};

const CHECK_W: f32 = 30.0;
const NAME_W: f32 = 210.0;
const KIND_W: f32 = 130.0;
const SIZE_W: f32 = 84.0;
const SAVE_W: f32 = 200.0;
const CELL_H: f32 = 24.0;

/// The sortable columns after the checkbox strip. `None` width is the
/// source column, which takes whatever is left.
const COLS: [(&str, Option<f32>, BatchSortKey); 5] = [
    ("File Name", Some(NAME_W), BatchSortKey::Name),
    ("File Type", Some(KIND_W), BatchSortKey::Kind),
    ("Size", Some(SIZE_W), BatchSortKey::Size),
    ("Download from", None, BatchSortKey::Source),
    ("Save to", Some(SAVE_W), BatchSortKey::Dest),
];

pub fn view(app: &App) -> El<'_> {
    let st = &app.batch;
    let rows = app.batch_rows();

    let editor = text_editor(&st.text)
        .placeholder("https://example.com/file1.zip\nhttps://example.com/file2.zip")
        .on_action(Message::BatchEdit)
        .size(theme::FONT_SIZE)
        .height(150.0);

    let mut list = column![].spacing(1);
    for r in &rows {
        list = list.push(row_el(r));
    }
    let table = column![
        header(st, &rows),
        hairline(),
        scrollable(list).height(Length::Fill),
    ];

    let cats: Vec<String> = app.cfg.categories.iter().map(|c| c.name.clone()).collect();
    let mode = if st.to_category {
        1u8
    } else if st.to_dir {
        2
    } else {
        0
    };

    // "3/9 files selected — Total size: 322.51 MB (+2 with unknown size)":
    // what OK is about to add, and how much of it has been measured.
    let selected = rows.iter().filter(|r| r.checked).count();
    let known: u64 = rows
        .iter()
        .filter(|r| r.checked)
        .filter_map(|r| r.size)
        .sum();
    let unknown = rows
        .iter()
        .filter(|r| r.checked && r.size.is_none())
        .count();
    let mut summary = format!(
        "{selected}/{} {} — {} {}",
        rows.len(),
        tr("files selected"),
        tr("Total size:"),
        fmt::size2(known)
    );
    if unknown > 0 {
        summary.push_str(&format!(" (+{unknown} {})", tr("with unknown size")));
    }

    let save_to = column![
        text(tr("Save To")).size(theme::FONT_SIZE + 1.0),
        radio(
            tr("Every file to the directory according to the category of the file"),
            0u8,
            Some(mode),
            Message::BatchSaveMode,
        )
        .size(15.0)
        .text_size(theme::FONT_SIZE),
        row![
            radio(
                tr("All files to one category"),
                1u8,
                Some(mode),
                Message::BatchSaveMode
            )
            .size(15.0)
            .text_size(theme::FONT_SIZE),
            pick_list(cats, Some(st.category.clone()), Message::BatchCategory)
                .text_size(theme::FONT_SIZE)
                .style(theme::picker)
                .width(220.0),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
        row![
            radio(
                tr("All files to one directory"),
                2u8,
                Some(mode),
                Message::BatchSaveMode
            )
            .size(15.0)
            .text_size(theme::FONT_SIZE),
            text_input("", &st.dir)
                .on_input(Message::BatchDir)
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            dlg_btn(tr("Browse"), Some(Message::BatchBrowseDir)),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
    ]
    .spacing(8)
    .width(Length::Fill);

    let side = column![
        dlg_btn(tr("Check All"), Some(Message::BatchCheckAll(true))),
        dlg_btn(tr("Uncheck All"), Some(Message::BatchCheckAll(false))),
        checkbox(st.hide_html)
            .label(tr("Hide HTML files"))
            .on_toggle(Message::BatchHideHtml)
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        checkbox(st.hide_dups)
            .label(tr("Hide duplicate links"))
            .on_toggle(Message::BatchHideDups)
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
    ]
    .spacing(8);

    container(
        column![
            text(tr("Please check the links, which you want to add to the download list, and click OK button."))
                .size(theme::FONT_SIZE),
            editor,
            container(table)
                .padding(6)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::panel),
            row![save_to, side].spacing(16),
            // Only shown when the list actually contains a manifest: a
            // quality picker over a list of ordinary files would be noise.
            streams_row(st),
            row![
                text(summary).size(theme::FONT_SIZE),
                iced::widget::space::horizontal(),
                dlg_btn_primary(tr("OK"), Some(Message::BatchOk)),
                dlg_btn(tr("Cancel"), app.win_of(WinKind::Batch).map(Message::CloseThis)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(10)
        .padding(14),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

/// Column headers: a check-all box over the checkbox strip, then one
/// click-to-sort cell per column carrying the main list's ▴/▾ marker.
fn header<'a>(st: &BatchState, rows: &[BatchRow]) -> El<'a> {
    let all_on = !rows.is_empty() && rows.iter().all(|r| r.checked);
    let mut r = row![container(
        checkbox(all_on)
            .on_toggle(Message::BatchCheckAll)
            .size(15.0)
            .style(theme::check)
    )
    .width(CHECK_W)
    .height(CELL_H)
    .padding([3, 6])]
    .spacing(0)
    .align_y(iced::Alignment::Center);
    for (label, width, key) in COLS {
        let arrow = match st.sort {
            Some((k, asc)) if k == key => {
                if asc {
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
        .width(width.map(Length::from).unwrap_or(Length::Fill))
        .height(CELL_H)
        .clip(true)
        .style(theme::header_cell(false));
        // A container under a `mouse_area`, not a `button`, so the header
        // keeps the arrow cursor like the main list's.
        r = r.push(
            mouse_area(cell)
                .interaction(iced::mouse::Interaction::Idle)
                .on_press(Message::BatchSort(key)),
        );
    }
    r.into()
}

fn row_el<'a>(r: &BatchRow) -> El<'a> {
    let blocked_color = iced::Color::from_rgb8(0xC0, 0x2B, 0x2B);
    let from: El<'a> = if r.blocked {
        text(format!("{} ({})", r.url, tr("blocked site")))
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None)
            .color(blocked_color)
            .into()
    } else {
        text(r.url.clone())
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None)
            .into()
    };
    let idx = r.idx;
    row![
        container(
            checkbox(r.checked)
                .on_toggle(move |b| Message::BatchCheck(idx, b))
                .size(15.0)
                .style(theme::check)
        )
        .width(CHECK_W)
        .height(CELL_H)
        .padding([3, 6]),
        cell(plain(r.name.clone()), Some(NAME_W)),
        cell(plain(r.kind.clone()), Some(KIND_W)),
        cell(
            plain(r.size.map(fmt::size2).unwrap_or_default()),
            Some(SIZE_W)
        ),
        cell(from, None),
        cell(plain(r.save_to.clone()), Some(SAVE_W)),
    ]
    .spacing(0)
    .align_y(iced::Alignment::Center)
    .into()
}

fn plain<'a>(s: String) -> El<'a> {
    text(s)
        .size(theme::FONT_SIZE)
        .wrapping(iced::widget::text::Wrapping::None)
        .into()
}

fn cell<'a>(content: El<'a>, w: Option<f32>) -> El<'a> {
    container(content)
        .width(w.map(Length::from).unwrap_or(Length::Fill))
        .height(CELL_H)
        .padding([2, 6])
        .clip(true)
        .into()
}

/// The 1 px rule under the header, in the list's own grid colour.
fn hairline<'a>() -> El<'a> {
    container(iced::widget::space::horizontal())
        .width(Length::Fill)
        .height(1.0)
        .style(|t| container::Style {
            background: Some(Background::Color(theme::grid_line(t))),
            ..Default::default()
        })
        .into()
}

/// The one stream setting a batch needs, when it has any streams in it.
///
/// A picker per URL would mean probing every manifest to build it, and
/// answering the same question once per link. One preference for the batch
/// is resolved against each manifest's own ladder as it starts.
fn streams_row(st: &BatchState) -> El<'_> {
    let n = st
        .checks
        .iter()
        .filter(|(u, on)| *on && crate::app::manifest_address(u))
        .count();
    if n == 0 {
        return iced::widget::space::horizontal().height(0.0).into();
    }
    let container = if st.stream_container.is_empty() {
        "MP4".to_string()
    } else {
        st.stream_container.clone()
    };
    row![
        text(format!(
            "{n} {}",
            tr("stream(s) in this list — download at")
        ))
        .size(theme::FONT_SIZE),
        pick_list(
            crate::app::BatchQuality::ALL.to_vec(),
            Some(st.stream_quality),
            Message::BatchStreamQuality
        )
        .text_size(theme::FONT_SIZE)
        .style(theme::picker)
        .width(150.0),
        pick_list(
            vec!["MP4".to_string(), "TS".to_string()],
            Some(container),
            Message::BatchStreamContainer
        )
        .text_size(theme::FONT_SIZE)
        .style(theme::picker)
        .width(90.0),
    ]
    .spacing(12)
    .align_y(iced::Alignment::Center)
    .into()
}
