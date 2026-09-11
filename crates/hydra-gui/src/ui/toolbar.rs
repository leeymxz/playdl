// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The toolbar: big gradient-outline icons with labels underneath.

use crate::app::{App, El, MenuAction, Message};
use crate::model::DlState;
use crate::{i18n::tr, icons, theme};
use iced::widget::{button, column, row, svg, text};
use iced::{Alignment, Length};

fn tool<'a>(icon: svg::Handle, label: String, msg: Option<Message>) -> El<'a> {
    let content = column![
        svg(icon).width(34.0).height(34.0),
        text(label).size(theme::FONT_SIZE),
    ]
    .spacing(3)
    .align_x(Alignment::Center);
    let mut b = button(content).padding([6, 10]).style(theme::btn_toolbar);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

pub fn view(app: &App) -> El<'_> {
    let sel_some = !app.selected.is_empty();
    let in_sel = |d: &&crate::model::DownloadItem| app.selected.contains(&d.id);
    let sel_active = app
        .state
        .downloads
        .iter()
        .filter(in_sel)
        .any(|d| d.state.is_active());
    let sel_resumable = app
        .state
        .downloads
        .iter()
        .filter(in_sel)
        .any(|d| matches!(d.state, DlState::Paused | DlState::Error | DlState::Queued));
    let any_active = app.state.downloads.iter().any(|d| d.state.is_active());
    let any_queue_running = app.cfg.queues.iter().any(|q| q.running);
    let main_q = app
        .cfg
        .queues
        .first()
        .map(|q| q.name.clone())
        .unwrap_or_else(|| "Main download queue".into());

    row![
        tool(
            icons::add_url(true),
            tr("Add URL"),
            Some(Message::Menu(MenuAction::AddNewDownload)),
        ),
        tool(
            icons::resume(sel_resumable),
            tr("Resume"),
            sel_resumable.then_some(Message::ToolbarResume),
        ),
        tool(
            icons::stop(sel_active),
            tr("Stop"),
            sel_active.then_some(Message::ToolbarStop),
        ),
        tool(
            icons::stop_all(any_active),
            tr("Stop All"),
            any_active.then_some(Message::Menu(MenuAction::StopAll)),
        ),
        tool(
            icons::delete(sel_some),
            tr("Delete"),
            sel_some.then_some(Message::ToolbarDelete),
        ),
        tool(
            icons::delete_completed(true),
            tr("Delete Completed"),
            Some(Message::Menu(MenuAction::DeleteAllCompleted)),
        ),
        tool(
            icons::options(true),
            tr("Options"),
            Some(Message::Menu(MenuAction::Options)),
        ),
        // Shortcut into Options > Extensions: the browser add-on is how most
        // downloads reach PlayDL, so it gets a toolbar entry of its own.
        tool(
            icons::extensions(true),
            tr("Extensions"),
            Some(Message::Menu(MenuAction::Extensions)),
        ),
        tool(
            icons::scheduler(true),
            tr("Scheduler"),
            Some(Message::Menu(MenuAction::Scheduler)),
        ),
        // Split buttons: the big button acts on the MAIN queue,
        // the arrow opens the list of all queues.
        tool(
            icons::start_queue(!any_queue_running),
            tr("Start Queue"),
            (!any_queue_running).then_some(Message::Menu(MenuAction::StartQueue(main_q.clone()))),
        ),
        button(text("▾").size(theme::FONT_SIZE))
            .padding([4, 3])
            .style(theme::btn_toolbar)
            .on_press(Message::QueueMenuOpen(true)),
        tool(
            icons::stop_queue(any_queue_running),
            tr("Stop Queue"),
            any_queue_running.then_some(Message::Menu(MenuAction::StopQueue(main_q))),
        ),
        button(text("▾").size(theme::FONT_SIZE))
            .padding([4, 3])
            .style(theme::btn_toolbar)
            .on_press(Message::QueueMenuOpen(false)),
    ]
    .spacing(4)
    .padding([4, 8])
    .width(Length::Fill)
    .into()
}
