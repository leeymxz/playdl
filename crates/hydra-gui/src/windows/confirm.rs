// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! Small confirmation / warning dialogs.

use crate::app::{App, ConfirmKind, El, Message, WinKind};
use crate::windows::{dlg_btn, dlg_btn_auto, dlg_btn_auto_primary, dlg_btn_primary};
use crate::{i18n::tr, icons, theme};
use iced::widget::{checkbox, column, container, row, svg, text};
use iced::Length;

pub fn view(app: &App) -> El<'_> {
    let (msg, yes_no) = match &app.confirm {
        Some(ConfirmKind::DeleteItems(ids)) if ids.len() == 1 => {
            let name = app
                .item(ids[0])
                .map(|d| d.file_name.clone())
                .unwrap_or_default();
            (
                format!("{} \"{name}\"?", tr("Are you sure you want to delete")),
                true,
            )
        }
        Some(ConfirmKind::DeleteItems(ids)) => (
            format!(
                "{} {} {}?",
                tr("Are you sure you want to delete"),
                ids.len(),
                tr("files")
            ),
            true,
        ),
        Some(ConfirmKind::DeleteCompleted) => (
            tr("Do you want to delete all successfully completed files from the list?"),
            true,
        ),
        Some(ConfirmKind::NoneChecked) => (
            tr("You have not checked any file to download!"),
            false,
        ),
        Some(ConfirmKind::StopWarn { ids, .. }) if ids.len() == 1 => {
            let name = app
                .item(ids[0])
                .map(|d| d.file_name.clone())
                .unwrap_or_default();
            (
                format!("{} \"{name}\"?", tr("Are you sure you want to stop downloading")),
                true,
            )
        }
        Some(ConfirmKind::StopWarn { ids, .. }) => (
            format!(
                "{} {} {}?",
                tr("Are you sure you want to stop downloading"),
                ids.len(),
                tr("files")
            ),
            true,
        ),
        Some(ConfirmKind::PermissionWarn { dir }) => (
            format!(
                "{} ({dir})",
                tr("PlayDL cannot write into the download folder. Grant access when macOS asks, allow PlayDL in System Settings > Privacy & Security, or pick another folder per download.")
            ),
            false,
        ),
        Some(ConfirmKind::UpToDate) => (
            format!(
                "{} (PlayDL {})",
                tr("You are using the latest version of PlayDL."),
                env!("CARGO_PKG_VERSION")
            ),
            false,
        ),
        Some(ConfirmKind::UpdateCheckFailed(e)) => (
            format!(
                "{}\n\n{e}",
                tr("Could not check for updates. Check your internet connection and try again.")
            ),
            false,
        ),
        Some(ConfirmKind::Duplicate { existing, file }) => {
            let msg = match (existing, file) {
                (Some(_), _) => tr("This address is already in the download list. What do you want to do?"),
                // Name the file it found. The collision is checked in the
                // CATEGORY folder, which is not always where the user last
                // looked: a finished copy under `Compressed/` prompted this
                // dialog for someone who had just deleted the one in the
                // download folder, and "a file with this name" gave them no
                // way to find it.
                (None, Some(path)) => format!(
                    "{}\n\n{path}",
                    tr("A file with this name already exists. What do you want to do?")
                ),
                _ => String::new(),
            };
            (msg, false)
        }
        None => (String::new(), false),
    };

    // Deleting a list entry does not touch the finished file unless asked.
    let offer_remove_file = matches!(
        app.confirm,
        Some(ConfirmKind::DeleteItems(_)) | Some(ConfirmKind::DeleteCompleted)
    );
    let remove_file: El<'_> = if offer_remove_file {
        checkbox(app.confirm_remove_file)
            .label(tr("Also remove file from disk"))
            .on_toggle(Message::ConfirmRemoveFile)
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check)
            .into()
    } else {
        iced::widget::space::horizontal().height(0.0).into()
    };

    let close = app.win_of(WinKind::Confirm).map(Message::CloseThis);
    if matches!(&app.confirm, Some(ConfirmKind::PermissionWarn { .. })) {
        let buttons = row![
            dlg_btn_primary(tr("Permissions guide"), Some(Message::OpenPermissions)),
            dlg_btn(tr("OK"), close.clone()),
        ]
        .spacing(10);
        return container(
            column![
                row![
                    svg(icons::warning()).width(36.0).height(36.0),
                    text(msg).size(theme::FONT_SIZE),
                ]
                .spacing(14)
                .align_y(iced::Alignment::Center),
                iced::widget::space::vertical(),
                row![iced::widget::space::horizontal(), buttons],
            ]
            .spacing(10)
            .padding(18),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into();
    }
    if let Some(ConfirmKind::Duplicate { existing, file }) = &app.confirm {
        // Label-sized buttons, centred: fixed widths wrapped the longer
        // captions onto two lines.
        let mut r = row![].spacing(10);
        if existing.is_some() {
            r = r.push(dlg_btn_auto_primary(
                tr("Resume existing"),
                Some(Message::DupResume),
            ));
        } else if file.is_some() {
            r = r.push(dlg_btn_auto_primary(
                tr("Open existing file"),
                Some(Message::DupOpen),
            ));
        }
        r = r.push(dlg_btn_auto(
            tr("Download as new file"),
            Some(Message::DupNew),
        ));
        r = r.push(dlg_btn_auto(tr("Cancel"), close));
        return container(
            column![
                row![
                    svg(icons::warning()).width(36.0).height(36.0),
                    text(msg).size(theme::FONT_SIZE),
                ]
                .spacing(14)
                .align_y(iced::Alignment::Center),
                iced::widget::space::vertical(),
                row![
                    iced::widget::space::horizontal(),
                    r,
                    iced::widget::space::horizontal()
                ],
            ]
            .spacing(10)
            .padding(18),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into();
    }
    let buttons: El<'_> = if yes_no {
        row![
            dlg_btn_primary(tr("Yes"), Some(Message::ConfirmYes)),
            dlg_btn(tr("No"), close),
        ]
        .spacing(10)
        .into()
    } else {
        row![dlg_btn_primary(tr("OK"), close)].into()
    };

    // Good news gets the info bubble; everything else warns.
    let icon = match &app.confirm {
        Some(ConfirmKind::UpToDate) => icons::info(),
        _ => icons::warning(),
    };
    container(
        column![
            row![
                svg(icon).width(36.0).height(36.0),
                text(msg).size(theme::FONT_SIZE),
            ]
            .spacing(14)
            .align_y(iced::Alignment::Center),
            remove_file,
            iced::widget::space::vertical(),
            row![iced::widget::space::horizontal(), buttons],
        ]
        .spacing(10)
        .padding(18),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}
