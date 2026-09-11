// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! The main window: menu bar (in-window on Windows/Linux), toolbar,
//! categories tree, download table, plus dropdown/context-menu overlays.

use crate::app::{App, El};
use crate::theme;
use crate::ui::{categories, menu, table, toolbar};
use iced::widget::{column, container, mouse_area, row, rule};
use iced::Length;

pub fn view(app: &App) -> El<'_> {
    let mut root = column![].width(Length::Fill).height(Length::Fill);

    // macOS gets the native menu bar; other platforms draw the in-window bar.
    // HYDRA_GUI_INWIN_MENU=1 forces the in-window bar for testing it on macOS.
    if cfg!(not(target_os = "macos")) || std::env::var_os("HYDRA_GUI_INWIN_MENU").is_some() {
        root = root.push(menu::bar(app));
        root = root.push(rule::horizontal(1));
    }

    root = root.push(toolbar::view(app));
    root = root.push(rule::horizontal(1));

    let mut center = row![].spacing(4).padding(4);
    if app.cfg.settings.show_categories {
        center = center.push(categories::view(app));
    }
    center = center.push(table::view(app));
    root = root.push(center.width(Length::Fill).height(Length::Fill));

    // Zero-sized: keeps `app.cursor_now()` fresh for menu placement and for
    // the start of a drag without a message — and so without a rebuild of the
    // whole window — per pointer motion event.
    root = root.push(crate::ui::probe::cursor_probe(app.cursor_cell.clone()));

    // No `on_move`: a message per motion event repainted the whole window at
    // the pointer's event rate. The band and the column-resize drag sample
    // the probe on `Message::DragTick` instead (see `main::subscription`).
    let base: El<'_> = mouse_area(
        container(root)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::window),
    )
    .into();

    // Overlays: open dropdown menu, the row context menu, or the drag's
    // rubber band. The window's ROOT element must keep the same type and
    // child count whichever of them is showing: iced throws away the whole
    // widget-state tree when the root's tag changes, which took the download
    // list's scroll offset with it — pressing a row while scrolled down made
    // the band appear, the tree reset, and the list snap back to the top
    // mid-drag. So the stack is unconditional and an inert layer stands in
    // when there is nothing to overlay.
    let overlay: Option<El<'_>> = if let Some(kind) = app.open_menu {
        Some(menu::bar_overlay(app, kind))
    } else if let (Some(start), Some(at)) = (app.queue_menu, app.ctx_at) {
        let items: Vec<menu::Entry> = app
            .cfg
            .queues
            .iter()
            .map(|q| {
                let action = if start {
                    crate::app::MenuAction::StartQueue(q.name.clone())
                } else {
                    crate::app::MenuAction::StopQueue(q.name.clone())
                };
                menu::Entry::plain(
                    crate::i18n::tr(&q.name),
                    action,
                    if start { !q.running } else { q.running },
                )
            })
            .collect();
        Some(menu::overlay(app, items, at))
    } else if let Some((at, items)) = app
        .ctx_at
        .map(|at| (at, menu::context_entries(app)))
        .filter(|(_, items)| !items.is_empty())
    {
        Some(menu::overlay(app, items, at))
    } else {
        // Rubber-band box of a drag-selection in progress. It only paints,
        // never handles input: a plain container captures nothing, so the
        // rows below it keep receiving the sweep.
        app.band.and_then(|(a, b)| {
            let (x, y) = (a.x.min(b.x), a.y.min(b.y));
            let (w, h) = ((a.x - b.x).abs(), (a.y - b.y).abs());
            (w > 2.0 || h > 2.0).then(|| {
                let box_ = container(iced::widget::space::horizontal())
                    .width(w)
                    .height(h)
                    .style(theme::band);
                iced::widget::pin(box_).x(x).y(y).into()
            })
        })
    };

    iced::widget::stack![
        base,
        overlay.unwrap_or_else(|| iced::widget::space::horizontal().into())
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
