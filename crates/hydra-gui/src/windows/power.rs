// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! The cancellable countdown shown before a "when done" power action.
//!
//! A queue or a download that was told to shut the machine down, log the
//! user off or put it to sleep does not do so on the spot: this dialog
//! counts ten seconds out first, so someone still at the desk can call it
//! off. See [`crate::app::App::arm_power_action`].

use crate::app::{App, El, Message, PowerPrompt};
use crate::model::PowerAction;
use crate::windows::{dlg_btn_auto, dlg_btn_auto_primary};
use crate::{i18n::tr, icons, theme};
use iced::widget::{column, container, row, svg, text};
use iced::Length;

/// The sentence describing what is about to happen, and the label of the
/// button that skips the wait.
fn wording(action: PowerAction) -> (String, String) {
    match action {
        PowerAction::Shutdown => (
            tr("The computer will shut down when the countdown ends."),
            tr("Shut down now"),
        ),
        PowerAction::LogOff => (
            tr("You will be logged off when the countdown ends."),
            tr("Log off now"),
        ),
        PowerAction::Sleep => (
            tr("The computer will go to sleep when the countdown ends."),
            tr("Sleep now"),
        ),
    }
}

pub fn view(app: &App) -> El<'_> {
    body(app.power)
}

/// The dialog for a given prompt. `None` — the window outliving its state,
/// a cancel racing the close — renders empty rather than panicking on the
/// missing prompt.
fn body<'a>(prompt: Option<PowerPrompt>) -> El<'a> {
    let Some(p) = prompt else {
        return container(text(""))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::window)
            .into();
    };
    let (msg, now_label) = wording(p.action);
    container(
        column![
            row![
                svg(icons::warning()).width(36.0).height(36.0),
                text(msg).size(theme::FONT_SIZE),
            ]
            .spacing(14)
            .align_y(iced::Alignment::Center),
            crate::windows::centered(p.secs.to_string(), theme::FONT_SIZE * 2.4),
            iced::widget::space::vertical(),
            row![
                iced::widget::space::horizontal(),
                row![
                    dlg_btn_auto_primary(tr("Cancel"), Some(Message::PowerCancel)),
                    dlg_btn_auto(now_label, Some(Message::PowerNow)),
                ]
                .spacing(10),
                iced::widget::space::horizontal(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_names_itself_in_the_countdown() {
        let all = [
            PowerAction::Shutdown,
            PowerAction::LogOff,
            PowerAction::Sleep,
        ];
        let mut seen: Vec<(String, String)> = vec![];
        for a in all {
            let (msg, now) = wording(a);
            assert!(!msg.is_empty() && !now.is_empty(), "{a:?} has no wording");
            // Each action says something of its own: a copy-paste that left
            // sleep describing a shutdown would read as a shutdown.
            assert!(
                !seen.iter().any(|(m, n)| *m == msg || *n == now),
                "{a:?} reuses another action's wording"
            );
            seen.push((msg, now));
        }
    }

    #[test]
    fn the_countdown_lays_out_armed_and_empty() {
        for action in [
            PowerAction::Shutdown,
            PowerAction::LogOff,
            PowerAction::Sleep,
        ] {
            let _armed: El<'_> = body(Some(PowerPrompt {
                action,
                secs: crate::app::POWER_COUNTDOWN_SECS,
                exit_after: false,
            }));
        }
        let _empty: El<'_> = body(None);
    }
}
