// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The update dialog: "a new version is available", its release notes, and
//! the Update Now / Cancel pair. While an update runs the button row is
//! replaced by a progress bar; the notes stay on screen throughout.

use crate::app::{App, El, Message, UpdatePhase};
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, theme};
use iced::widget::{button, column, container, image, progress_bar, row, scrollable, text};
use iced::Length;

pub fn view(app: &App) -> El<'_> {
    let Some(info) = &app.updater.info else {
        // The window can outlive a cancelled offer by one frame.
        return container(text(""))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::window)
            .into();
    };

    let header = row![
        image(image::Handle::from_bytes(
            include_bytes!("../../../../docs/logo.png").as_slice()
        ))
        .width(48.0)
        .height(48.0),
        column![
            text(tr("A new version of PlayDL is available")).size(theme::FONT_SIZE + 3.0),
            text(format!(
                "{} {}   —   {} {}",
                tr("New version:"),
                info.version,
                tr("You have:"),
                env!("CARGO_PKG_VERSION")
            ))
            .size(theme::FONT_SIZE)
            .color(theme::dim_text(&iced::Theme::Light)),
        ]
        .spacing(4),
    ]
    .spacing(12)
    .align_y(iced::Alignment::Center);

    // Release notes, rendered from the API's markdown.
    let notes = container(scrollable(notes_body(&info.notes)).width(Length::Fill))
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::panel);

    // A packaged install (deb/rpm in /usr/bin) cannot be rewritten by the
    // user's own process, so the dialog offers the distro package instead of
    // an in-place update.
    let download_line = |name: &str, size: u64| {
        text(format!(
            "{} {} ({})",
            tr("Download:"),
            name,
            fmt::size2(size)
        ))
        .size(theme::FONT_SIZE - 1.0)
        .color(theme::dim_text(&iced::Theme::Light))
    };
    let status: El<'_> = match &app.updater.phase {
        UpdatePhase::Idle if !info.in_place => {
            let mut col = column![text(tr(
                "This copy of PlayDL cannot update itself in place. Download the new installer and install it the way you installed this one."
            ))
            .size(theme::FONT_SIZE - 1.0)
            .color(iced::Color::from_rgb8(0x9A, 0x6A, 0x00))]
            .spacing(4);
            if let Some((name, _, size)) = &info.package {
                col = col.push(download_line(name, *size));
            }
            col.into()
        }
        // A root-owned install (a tarball unpacked with sudo) is still
        // PlayDL's to replace — the finisher just has to ask first, and
        // saying so before the download beats an unexplained password panel
        // after the app has quit.
        UpdatePhase::Idle if info.needs_auth => column![
            download_line(&info.asset_name, info.size),
            text(tr(
                "PlayDL is installed for all users; finishing the update will ask for your administrator password."
            ))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
        ]
        .spacing(4)
        .into(),
        UpdatePhase::Idle => download_line(&info.asset_name, info.size).into(),
        UpdatePhase::Downloading { got, total } => {
            let frac = match total {
                Some(t) if *t > 0 => *got as f32 / *t as f32,
                _ => 0.0,
            };
            let label = match total {
                Some(t) if *t > 0 => format!(
                    "{} {} / {} ({})",
                    tr("Downloading update..."),
                    fmt::size2(*got),
                    fmt::size2(*t),
                    fmt::pct(*got, *t)
                ),
                _ => format!("{} {}", tr("Downloading update..."), fmt::size2(*got)),
            };
            column![
                progress_bar(0.0..=1.0, frac.clamp(0.0, 1.0))
                    .girth(14.0)
                    .style(theme::progress),
                text(label).size(theme::FONT_SIZE - 1.0),
            ]
            .spacing(6)
            .into()
        }
        UpdatePhase::Verifying => text(tr("Verifying the downloaded file..."))
            .size(theme::FONT_SIZE)
            .into(),
        UpdatePhase::Preparing => text(tr("Preparing the update..."))
            .size(theme::FONT_SIZE)
            .into(),
        UpdatePhase::Restarting => text(tr("PlayDL will now restart to finish the update."))
            .size(theme::FONT_SIZE)
            .into(),
        UpdatePhase::Failed(e) => text(format!("{} {e}", tr("Update failed:")))
            .size(theme::FONT_SIZE)
            .color(iced::Color::from_rgb8(0xC4, 0x2B, 0x1C))
            .into(),
    };

    // The release page link is always harmless; the action buttons depend on
    // the phase.
    let page_btn: El<'_> = button(
        text(tr("View release page"))
            .size(theme::FONT_SIZE - 1.0)
            .color(iced::Color::from_rgb8(0x1F, 0x3F, 0xC4)),
    )
    .padding([4, 2])
    .style(|_t, _s| button::Style::default())
    .on_press(Message::UpdateOpenPage)
    .into();

    let buttons: El<'_> = match &app.updater.phase {
        // Nothing to run in place: the package is downloaded in a browser
        // and installed by the package manager, which is the only thing that
        // may write /usr/bin. Without a package for this machine the release
        // page is the whole offer.
        UpdatePhase::Idle if !info.in_place => row![
            page_btn,
            iced::widget::space::horizontal(),
            dlg_btn_primary(
                tr("Download Installer"),
                info.package
                    .as_ref()
                    .map(|(_, url, _)| Message::UpdateOpenUrl(url.clone()))
            ),
            dlg_btn(tr("Close"), Some(Message::UpdateCancel)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into(),
        UpdatePhase::Idle => row![
            page_btn,
            iced::widget::space::horizontal(),
            dlg_btn_primary(tr("Update Now"), Some(Message::UpdateNow)),
            dlg_btn(tr("Cancel"), Some(Message::UpdateCancel)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into(),
        UpdatePhase::Downloading { .. } => row![
            iced::widget::space::horizontal(),
            dlg_btn(tr("Cancel"), Some(Message::UpdateCancel)),
        ]
        .spacing(8)
        .into(),
        UpdatePhase::Verifying | UpdatePhase::Preparing | UpdatePhase::Restarting => row![
            iced::widget::space::horizontal(),
            dlg_btn(tr("Cancel"), None),
        ]
        .spacing(8)
        .into(),
        UpdatePhase::Failed(_) => row![
            page_btn,
            iced::widget::space::horizontal(),
            dlg_btn_primary(tr("Try Again"), Some(Message::UpdateNow)),
            dlg_btn(tr("Close"), Some(Message::UpdateCancel)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into(),
    };

    container(
        column![header, notes, status, buttons]
            .spacing(12)
            .padding(16),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

/// Render GitHub-flavoured markdown release notes as simple styled lines:
/// headings become emphasised text, list markers become bullets, and inline
/// `**bold**`/`` `code` ``/link syntax is stripped down to its text. Good
/// enough to read a changelog; anything fancier belongs in the browser via
/// "View release page".
fn notes_body(notes: &str) -> El<'_> {
    let mut col = column![].spacing(5);
    let mut prev_blank = false;
    for raw in notes.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            // Collapse runs of blank lines into one paragraph gap.
            if !prev_blank {
                col = col.push(text("").size(4.0));
            }
            prev_blank = true;
            continue;
        }
        prev_blank = false;
        let trimmed = line.trim_start();
        let el: El<'_> = if let Some(h) = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("## "))
            .or_else(|| trimmed.strip_prefix("# "))
        {
            text(clean_inline(h)).size(theme::FONT_SIZE + 2.0).into()
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            row![
                text("•").size(theme::FONT_SIZE),
                text(shorten_refs(&clean_inline(item))).size(theme::FONT_SIZE),
            ]
            .spacing(6)
            .into()
        } else if let Some((label, url)) = trailing_link(&clean_inline(trimmed)) {
            // "Full Changelog: https://…/compare/v0.3.3-rc...v0.3.4-rc" — a
            // URL that long is unreadable inline and useless as dead text.
            row![
                text(label).size(theme::FONT_SIZE),
                button(text(link_label(&url)).size(theme::FONT_SIZE))
                    .padding(0)
                    .style(link_button)
                    .on_press(Message::UpdateOpenUrl(url)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            text(shorten_refs(&clean_inline(trimmed)))
                .size(theme::FONT_SIZE)
                .into()
        };
        col = col.push(el);
    }
    col.width(Length::Fill).into()
}

/// A line ending in a bare URL, split into the text before it and the URL:
/// `("Full Changelog:", "https://…")`. `None` when the line does not end in
/// one, so ordinary prose is left alone.
fn trailing_link(line: &str) -> Option<(String, String)> {
    let last = line.split_whitespace().next_back()?;
    if !(last.starts_with("https://") || last.starts_with("http://")) {
        return None;
    }
    let label = line[..line.rfind(last)?].trim_end().to_string();
    Some((label, last.to_string()))
}

/// What a link shows: a compare URL is its `v0.3.3-rc...v0.3.4-rc` tail,
/// which says what the changelog spans without the 60 characters of host and
/// path around it. Anything else shows the URL itself.
fn link_label(url: &str) -> String {
    match url.split_once("/compare/") {
        Some((_, range)) => range.to_string(),
        None => url.to_string(),
    }
}

/// A borderless, blue, clickable run of text.
fn link_button(_theme: &iced::Theme, status: button::Status) -> button::Style {
    let color = match status {
        button::Status::Hovered | button::Status::Pressed => {
            iced::Color::from_rgb8(0x0B, 0x2A, 0x9E)
        }
        _ => iced::Color::from_rgb8(0x1F, 0x3F, 0xC4),
    };
    button::Style {
        background: None,
        text_color: color,
        ..button::Style::default()
    }
}

/// Collapse GitHub pull/issue URLs to `(#14)`, dropping the `in` that
/// introduces them: `by @ja7ad in https://…/pull/14` -> `by @ja7ad (#14)`.
/// The notes are read in a 500 px dialog, and the URL is one click away on
/// the release page anyway.
fn shorten_refs(s: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for word in s.split_whitespace() {
        match issue_number(word) {
            Some(n) => {
                if words.last().map(String::as_str) == Some("in") {
                    words.pop();
                }
                words.push(format!("(#{n})"));
            }
            None => words.push(word.to_string()),
        }
    }
    words.join(" ")
}

/// The number in a `github.com/<owner>/<repo>/pull|issues/<n>` URL.
fn issue_number(word: &str) -> Option<&str> {
    let w = word.trim_end_matches(['.', ',', ')', ']']);
    let (_, tail) = w.split_once("github.com/")?;
    let mut parts = tail.split('/');
    let (_owner, _repo, kind, n) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if kind != "pull" && kind != "issues" {
        return None;
    }
    (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then_some(n)
}

/// Strip the inline markdown that would otherwise render as literal noise:
/// `**`, backticks, and `[text](url)` down to `text`.
fn clean_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '`' => {}
            '[' => {
                // `[text](url)` -> `text`; a bare `[` stays as-is.
                let rest: String = chars.clone().collect();
                if let Some(close) = rest.find(']') {
                    if rest[close..].starts_with("](") {
                        if let Some(end) = rest[close..].find(')') {
                            out.push_str(&rest[..close]);
                            for _ in 0..close + end + 1 {
                                chars.next();
                            }
                            continue;
                        }
                    }
                }
                out.push('[');
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_markdown_strips_to_text() {
        assert_eq!(clean_inline("**Bold** fix"), "Bold fix");
        assert_eq!(clean_inline("use `playdl update`"), "use playdl update");
        assert_eq!(
            clean_inline("by [someone](https://github.com/x) in #12"),
            "by someone in #12"
        );
        assert_eq!(clean_inline("a bare [ bracket"), "a bare [ bracket");
    }

    #[test]
    fn pr_urls_become_numbers() {
        assert_eq!(
            shorten_refs("Fix quota tracking by @ja7ad in https://github.com/ja7ad/playdl/pull/15"),
            "Fix quota tracking by @ja7ad (#15)"
        );
        assert_eq!(
            shorten_refs("closes https://github.com/ja7ad/playdl/issues/9."),
            "closes (#9)"
        );
        // Other URLs, and prose that merely contains "in", stay put.
        let keep = "See https://playdl.dev/docs in the manual";
        assert_eq!(shorten_refs(keep), keep);
    }

    #[test]
    fn changelog_line_splits_into_label_and_link() {
        let line = "Full Changelog: https://github.com/ja7ad/playdl/compare/v0.3.3-rc...v0.3.4-rc";
        let (label, url) = trailing_link(line).expect("trailing url");
        assert_eq!(label, "Full Changelog:");
        assert_eq!(link_label(&url), "v0.3.3-rc...v0.3.4-rc");
        assert_eq!(link_label("https://example.com/x"), "https://example.com/x");
        assert!(trailing_link("no link here").is_none());
    }
}
