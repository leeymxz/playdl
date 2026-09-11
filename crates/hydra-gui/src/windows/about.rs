// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! 鍏充簬 PlayDL.

use crate::app::{App, El, Message, WinKind};
use crate::windows::dlg_btn_primary;
use crate::{i18n::tr, theme};
use iced::widget::{column, container, image, row, text};
use iced::Length;

pub fn view(app: &App) -> El<'_> {
    container(
        column![
            row![
                image(image::Handle::from_bytes(
                    include_bytes!("../../../../docs/logo.png").as_slice()
                ))
                .width(64.0)
                .height(64.0),
                column![
                    text("PlayDL").size(22),
                    text(format!("{} {}", tr("Version"), env!("CARGO_PKG_VERSION")))
                        .size(theme::FONT_SIZE),
                ]
                .spacing(4),
            ]
            .spacing(14)
            .align_y(iced::Alignment::Center),
            text(tr("Multi-source download accelerator and file retriever")).size(theme::FONT_SIZE),
            text("© 2026 leeymxz — GPL-3.0-or-later")
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
            text("https://github.com/leeymxz/playdl")
                .size(theme::FONT_SIZE - 1.0)
                .color(iced::Color::from_rgb8(0x1F, 0x3F, 0xC4)),
            iced::widget::space::vertical(),
            row![
                iced::widget::space::horizontal(),
                dlg_btn_primary(tr("OK"), app.win_of(WinKind::About).map(Message::CloseThis)),
            ],
        ]
        .spacing(10)
        .padding(18),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}
