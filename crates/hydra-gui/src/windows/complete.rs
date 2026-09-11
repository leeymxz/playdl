// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Download complete" dialog: file, size, where it went; Open / Open folder.

use crate::app::{App, El, Message, WinKind};
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, icons, theme};
use iced::widget::{column, container, row, svg, text};
use iced::Length;

pub fn view(app: &App, id: crate::model::DlId) -> El<'_> {
    let Some(d) = app.item(id) else {
        return container(text("")).style(theme::window).into();
    };
    let body = column![
        row![
            svg(icons::folder_finished()).width(34.0).height(34.0),
            column![
                text(tr("Download complete")).size(theme::FONT_SIZE + 2.0),
                text(d.file_name.clone()).size(theme::FONT_SIZE),
                text(format!(
                    "{}  —  {}",
                    d.size.map(fmt::size2).unwrap_or_default(),
                    d.full_path().to_string_lossy()
                ))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
            ]
            .spacing(4),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
        iced::widget::space::vertical(),
        row![
            dlg_btn_primary(tr("Open"), Some(Message::OpenFile(id))),
            dlg_btn(tr("Open folder"), Some(Message::OpenFolder(id))),
            iced::widget::space::horizontal(),
            dlg_btn(
                tr("Close"),
                app.win_of(WinKind::Complete(id)).map(Message::CloseThis)
            ),
        ]
        .spacing(10),
    ]
    .spacing(10)
    .padding(16);

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into()
}
