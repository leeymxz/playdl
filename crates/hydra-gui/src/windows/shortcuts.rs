// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! Help > Keyboard Shortcuts: every action with its editable combo.
//! `cmd` means ⌘ on macOS and Ctrl on Windows/Linux; edits persist to the
//! configuration immediately.

use crate::app::{App, El, Message, WinKind};
use crate::model::SHORTCUT_ACTIONS;
use crate::windows::dlg_btn_primary;
use crate::{i18n::tr, theme};
use iced::widget::{column, container, row, scrollable, text, text_input};
use iced::Length;

pub fn view(app: &App) -> El<'_> {
    let mut list = column![].spacing(8);
    list = list.push(
        text(tr("cmd means Command on macOS and Ctrl on Windows/Linux. Click a field and type a combo like cmd+shift+v."))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
    );
    for (id, _default, label) in SHORTCUT_ACTIONS {
        let value = app.cfg.shortcuts.get(id).cloned().unwrap_or_default();
        let action = id.to_string();
        list = list.push(
            row![
                text(tr(label)).size(theme::FONT_SIZE).width(Length::Fill),
                text_input("", &value)
                    .on_input(move |v| Message::ShortcutEdit(action.clone(), v))
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(150.0),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        );
    }
    container(
        column![
            // The table grows with every new action, and View > Font scales
            // every row: scroll rather than push OK off the bottom.
            scrollable(list).height(Length::Fill),
            row![
                iced::widget::space::horizontal(),
                dlg_btn_primary(
                    tr("OK"),
                    app.win_of(WinKind::Shortcuts).map(Message::CloseThis)
                ),
            ],
        ]
        .spacing(10)
        .padding(16),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}
