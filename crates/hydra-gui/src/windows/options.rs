// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Configuration window: General, File types, Save to, Downloads,
//! Connection, Proxy/Socks, Sites Logins, Extensions, Sounds — two-row
//! tab strip.

use crate::app::{App, El, Message, OptField, OptTab, WinKind};
use crate::model::ProxyMode;
use crate::windows::{dlg_btn, dlg_btn_auto, dlg_btn_auto_primary, dlg_btn_primary};
use crate::{i18n::tr, theme};
use iced::widget::{
    button, checkbox, column, container, pick_list, radio, row, scrollable, text, text_editor,
    text_input, tooltip,
};
use iced::Length;

fn o(f: OptField) -> Message {
    Message::OptDraft(f)
}

fn tab_btn<'a>(label: String, tab: OptTab, cur: OptTab) -> El<'a> {
    button(crate::windows::centered(label, theme::FONT_SIZE))
        .padding([4, 10])
        .width(Length::Fill)
        .style(theme::btn_tab(tab == cur))
        .on_press(Message::OptTabSet(tab))
        .into()
}

/// Wrap a control with a hover hint.
fn hinted<'a>(el: impl Into<El<'a>>, hint: String) -> El<'a> {
    tooltip(
        el,
        container(text(hint).size(theme::FONT_SIZE - 1.0))
            .padding(8)
            .max_width(360.0)
            .style(theme::menu_panel),
        tooltip::Position::Bottom,
    )
    .into()
}

/// Palette entries are packed hex; unpack one for a `text` colour.
fn hex(v: u32) -> iced::Color {
    iced::Color::from_rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn section<'a>(title: String) -> El<'a> {
    row![text(title).size(theme::FONT_SIZE + 2.0),]
        .width(Length::Fill)
        .into()
}

fn general(app: &App) -> El<'_> {
    let s = &app.options.draft;
    // Which extensions are talking to PlayDL right now. A tick with nothing
    // beside it means the box is set but no extension has connected — which
    // is the difference between "capture is off" and "capture cannot happen",
    // and the list gave no way to tell them apart before.
    let live = crate::extbus::live_browsers();
    let mut browsers = column![].spacing(4);
    for (i, (name, on)) in s.capture_browsers.iter().enumerate() {
        let connected = live.iter().any(|b| b.eq_ignore_ascii_case(name));
        browsers = browsers.push(
            row![
                container(
                    checkbox(*on)
                        .label(name.clone())
                        .on_toggle(move |b| o(OptField::Browser(i, b)))
                        .size(15.0)
                        .text_size(theme::FONT_SIZE)
                        .style(theme::check),
                )
                .width(Length::Fill),
                text(if connected {
                    tr("extension connected")
                } else {
                    String::new()
                })
                .size(theme::FONT_SIZE - 1.0)
                .color(hex(theme::PROGRESS_GREEN)),
            ]
            .align_y(iced::Alignment::Center),
        );
    }
    // Dock/taskbar visibility exists only where the tray can take over the
    // app while no window is open.
    #[cfg(target_os = "macos")]
    let hide_taskbar: Option<El<'_>> = Some(hinted(
        checkbox(s.hide_from_taskbar)
            .label(tr("Hide Dock icon"))
            .on_toggle(|b| o(OptField::HideTaskbar(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        tr("Removes PlayDL from the Dock and Cmd-Tab while it runs in the tray; the Dock icon and menu bar return while a window is open."),
    ));
    #[cfg(target_os = "windows")]
    let hide_taskbar: Option<El<'_>> = Some(hinted(
        checkbox(s.hide_from_taskbar)
            .label(tr("Hide from taskbar"))
            .on_toggle(|b| o(OptField::HideTaskbar(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        tr("PlayDL windows get no taskbar button; reach the app from the tray icon. Applies to windows opened after the change."),
    ));
    // Linux: an X11 window-manager hint per window. Wayland has no
    // skip-taskbar protocol at all — a checkbox that cannot act would only
    // look broken, so it exists on X11 sessions only. (winit picks Wayland
    // exactly when WAYLAND_DISPLAY is set, so that is the session test.)
    #[cfg(target_os = "linux")]
    let hide_taskbar: Option<El<'_>> = std::env::var_os("WAYLAND_DISPLAY")
        .is_none()
        .then(|| {
            hinted(
                checkbox(s.hide_from_taskbar)
                    .label(tr("Hide from taskbar"))
                    .on_toggle(|b| o(OptField::HideTaskbar(b)))
                    .size(15.0)
                    .text_size(theme::FONT_SIZE)
                    .style(theme::check),
                tr("Keeps PlayDL windows out of the taskbar and the workspace switcher; reach the app from the tray icon. Wayland has no way to hide an open window, so there it applies while PlayDL runs in the tray."),
            )
        });
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let hide_taskbar: Option<El<'_>> = None;
    let mut col =
        column![
        section(tr("Browser/System Integration")),
        hinted(
            checkbox(s.launch_on_startup).label(tr("Launch PlayDL on startup"))
                .on_toggle(|b| o(OptField::LaunchStartup(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Registers PlayDL as a login item so downloads and queues continue after a reboot."),
        ),
        hinted(
            checkbox(s.start_in_tray).label(tr("Launch minimized to system tray"))
                .on_toggle(|b| o(OptField::StartInTray(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Autostart launches stay in the tray; open the window from the tray icon."),
        ),
        hinted(
            checkbox(s.close_to_tray).label(tr("Close to system tray"))
                .on_toggle(|b| o(OptField::CloseToTray(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Closing the main window leaves PlayDL running in the tray, where queues and transfers carry on; open it again from the tray icon. Off: closing the window exits PlayDL."),
        ),
    ];
    // Straight after "Close to system tray": both decide what the app looks
    // like once its window is gone.
    if let Some(el) = hide_taskbar {
        col = col.push(el);
    }
    col.extend([
        hinted(
            checkbox(s.check_updates_on_startup).label(tr("Check for updates on startup"))
                .on_toggle(|b| o(OptField::CheckUpdates(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Asks the release server for a newer PlayDL when the app starts. Only the check is automatic; installing always waits for your confirmation."),
        ),
        hinted(
            checkbox(s.beta_channel).label(tr("Download Beta channel"))
                .on_toggle(|b| o(OptField::BetaChannel(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Update checks also offer release candidates (-rc tags) when one is ahead of the stable release; otherwise the stable release is used. Beta builds may be less stable."),
        ),
        hinted(
            checkbox(s.power_save).label(tr("Power save mode"))
                .on_toggle(|b| o(OptField::PowerSave(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Fewer wakeups: slower interface refresh, no progress animation, coarser transfer ticks. Download speed is unchanged."),
        ),
        hinted(
            checkbox(s.gpu_render).label(tr("Use GPU render for smoother interface"))
                .on_toggle(|b| o(OptField::GpuRender(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("GPU rendering is smoother on very large windows but uses considerably more memory and the graphics processor. Takes effect after restart."),
        ),
        hinted(
            checkbox(s.monitor_clipboard).label(tr("Automatically start downloading of URLs placed to clipboard"))
                .on_toggle(|b| o(OptField::Clipboard(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Watches the clipboard for download links by file type and known download sites; one link opens the file dialog, many open the batch list."),
        ),
        text(tr("Capture downloads from the following browsers:"))
            .size(theme::FONT_SIZE)
            .into(),
        container(browsers)
            .padding(10)
            .width(Length::Fill)
            .style(theme::panel)
            .into(),
        text(tr(
            "PlayDL registers itself with these browsers automatically; install the PlayDL extension in each one you tick."
        ))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light))
            .into(),
    ])
    .spacing(10)
    .into()
}

fn file_types(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let _ = s;
    column![
        section(tr("Downloaded file types")),
        text(tr("Automatically start downloading the following file types:")).size(theme::FONT_SIZE),
        text_editor(&app.options.auto_types_edit)
            .on_action(|a| o(OptField::AutoTypesEdit(a)))
            .size(theme::FONT_SIZE)
            .height(90.0),
        text(tr("Don't start downloading automatically from the following sites:"))
            .size(theme::FONT_SIZE),
        text_editor(&app.options.sites_edit)
            .on_action(|a| o(OptField::SitesEdit(a)))
            .size(theme::FONT_SIZE)
            .height(70.0),
        text(tr("(separate with commas or spaces)")).size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
        checkbox(s.show_exception_dialog).label(tr("Show the dialog to add an address to the list of exceptions for a twice-cancelled download"))
        .on_toggle(|b| o(OptField::ExcDialog(b)))
        .size(15.0)
        .text_size(theme::FONT_SIZE)
        .style(theme::check),
    ]
    .spacing(10)
    .into()
}

fn save_to(app: &App) -> El<'_> {
    let st = &app.options;
    let cats: Vec<String> = st.draft_cats.iter().map(|c| c.name.clone()).collect();
    let cur = st
        .draft_cats
        .iter()
        .find(|c| c.name == st.sel_category)
        .cloned()
        .unwrap_or_else(|| st.draft_cats[0].clone());
    let exts = if cur.exts.is_empty() {
        tr("The file types that are not listed in any other category")
    } else {
        cur.exts.join(" ").to_uppercase()
    };
    column![
        section(tr("Categories, file types, folders")),
        text(tr("Category")).size(theme::FONT_SIZE),
        pick_list(cats, Some(st.sel_category.clone()), |c| o(OptField::SelCategory(c)))
            .text_size(theme::FONT_SIZE)
            .style(theme::picker)
            .width(300.0),
        text(format!(
            "{} \"{}\" {}:",
            tr("Automatically put in"),
            st.sel_category,
            tr("category the following file types")
        ))
        .size(theme::FONT_SIZE),
        container(text(exts).size(theme::FONT_SIZE)).padding(8).width(Length::Fill).style(theme::panel),
        text(format!(
            "{} \"{}\" {}",
            tr("Default download directory for"),
            st.sel_category,
            tr("category")
        ))
        .size(theme::FONT_SIZE),
        row![
            text_input("", &cur.dir)
                .on_input(|v| o(OptField::CatDir(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            dlg_btn(tr("Browse"), Some(o(OptField::BrowseCatDir))),
        ]
        .spacing(8),
        hinted(
            checkbox(app.options.draft.no_category_dirs)
                .label(tr("Do not create category folders — save everything in the default folder"))
                .on_toggle(|b| o(OptField::NoCatDirs(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            format!(
                "{}\n{}",
                tr("Off (default): a download is filed in its category folder, e.g. Downloads/Video."),
                tr("On: the folders above are ignored and new downloads are saved directly in the General category folder. Downloads already on the list keep their folder."),
            ),
        ),
        checkbox(app.options.draft.remember_last_dir)
            .label(format!(
                "{} \"{}\" {}",
                tr("Change folder for"),
                app.options.sel_category,
                tr("category on last selected")
            ))
            .on_toggle(|b| o(OptField::RememberLast(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        checkbox(app.options.draft.server_file_date).label(tr("Set file creation date as provided by the server"))
            .on_toggle(|b| o(OptField::ServerDate(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        text(tr("File parts are stored next to the destination as \"<name>.part\" and renamed in place on completion — no temporary directory is needed."))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
    ]
    .spacing(8)
    .into()
}

fn downloads(app: &App) -> El<'_> {
    let s = &app.options.draft;
    column![
        section(tr("Customize \"Download progress\" dialog")),
        hinted(
            checkbox(s.start_minimized).label(tr("Start download progress dialog minimized"))
                .on_toggle(|b| o(OptField::StartMinimized(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("New progress windows open minimized to the Dock/taskbar instead of in front."),
        ),
        hinted(
            checkbox(s.show_file_info_dialog).label(tr("Show \"Download File Info\" dialog before starting"))
                .on_toggle(|b| o(OptField::ShowFileInfo(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Adding a link first shows name/category/folder while the transfer already runs in the background; off = downloads start immediately."),
        ),
        hinted(
            checkbox(s.bg_download).label(tr("Download in background while choosing options"))
                .on_toggle(|b| o(OptField::BgDownload(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Off = nothing is fetched until \"Start Download\" is pressed, so a rename or a change of folder happens before the transfer, not during it."),
        ),
        hinted(
            checkbox(s.show_speed_tab).label(tr("Show \"Speed Limiter\" tab"))
                .on_toggle(|b| o(OptField::SpeedTab(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Shows or hides the Speed Limiter tab of the progress window."),
        ),
        hinted(
            checkbox(s.show_completion_tab).label(tr("Show \"Options on completion\" tab"))
                .on_toggle(|b| o(OptField::CompletionTab(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Shows or hides the Options-on-completion tab of the progress window."),
        ),
        hinted(
            checkbox(s.show_hide_buttons).label(tr("Show \"Hide tab\" buttons"))
                .on_toggle(|b| o(OptField::HideButtons(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Shows a Hide-tab button inside the Speed Limiter and Options-on-completion tabs."),
        ),
        hinted(
            checkbox(s.show_complete_dialog).label(tr("Show download complete dialog"))
                .on_toggle(|b| o(OptField::CompleteDialog(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Pops the completion dialog with Open / Open folder when a download finishes."),
        ),
        hinted(
            checkbox(s.remove_completed).label(tr("Remove completed downloads from the list"))
                .on_toggle(|b| o(OptField::RemoveCompleted(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("A finished download drops off the list on its own — once the complete dialog is closed, when that dialog is shown. The downloaded file is kept."),
        ),
        section(tr("Virus checking")),
        text(tr("Virus scanner program")).size(theme::FONT_SIZE),
        row![
            text_input("", &s.virus_scanner)
                .on_input(|v| o(OptField::VirusScanner(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            dlg_btn(tr("Browse"), Some(o(OptField::BrowseVirus))),
        ]
        .spacing(8),
        text(tr("Command line parameters")).size(theme::FONT_SIZE),
        text_input("", &s.virus_args)
            .on_input(|v| o(OptField::VirusArgs(v)))
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill),
        text(tr("User-Agent for manually added downloads:")).size(theme::FONT_SIZE),
        text_input("", &s.user_agent)
            .on_input(|v| o(OptField::UserAgent(v)))
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill),
    ]
    .spacing(8)
    .into()
}

/// One-line readout of the live window beneath the limit controls: what the
/// running limit has counted and when it rolls over. Reads the *saved*
/// settings, not the draft — the draft is not in force until OK is pressed.
fn quota_line(app: &App) -> String {
    let s = &app.cfg.settings;
    if !s.dl_limit_enabled {
        return tr("Limit off: transfers are not counted.");
    }
    let q = &app.state.dl_quota;
    if q.window_start == 0 {
        return tr("The period starts with the next downloaded byte.");
    }
    let left = crate::app::quota_window_secs(s)
        .saturating_sub(crate::fmt::now_unix().saturating_sub(q.window_start))
        .max(0) as u64;
    format!(
        "{} {} / {} \u{2014} {} {}",
        tr("Used this period:"),
        crate::fmt::size2(q.used),
        crate::fmt::size2(crate::app::quota_cap(s).unwrap_or(0)),
        tr("resets in"),
        crate::fmt::eta(left),
    )
}

fn connection(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let st = &app.options;
    let conn_opts: Vec<usize> = vec![1, 2, 4, 8, 16, 32];
    let mut exc = column![].spacing(2);
    for (server, n) in &s.conn_exceptions {
        exc = exc.push(
            row![
                container(text(server.clone()).size(theme::FONT_SIZE)).width(Length::Fill),
                container(text(n.to_string()).size(theme::FONT_SIZE)).width(70.0),
            ]
            .spacing(6),
        );
    }
    column![
        section(tr("Connections and Limits")),
        row![
            text(tr("Default max. conn. number")).size(theme::FONT_SIZE),
            pick_list(conn_opts, Some(s.default_conns), |n| o(
                OptField::DefaultConns(n)
            ))
            .text_size(theme::FONT_SIZE)
            .style(theme::picker)
            .width(90.0),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        hinted(
            checkbox(s.adaptive_conns)
                .label(tr("Measure and adapt connection count"))
                .on_toggle(|b| o(OptField::AdaptiveConns(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Starts each transfer with one connection and adds more only while they measurably improve speed; the default max. number acts as a ceiling."),
        ),
        text(tr("Exceptions:")).size(theme::FONT_SIZE),
        container(exc)
            .padding(8)
            .width(Length::Fill)
            .height(120.0)
            .style(theme::panel),
        row![
            text_input(&tr("Server"), &st.conn_exc_server)
                .on_input(|v| o(OptField::ExcServer(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            text_input(&tr("Number"), &st.conn_exc_n)
                .on_input(|v| o(OptField::ExcConns(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(90.0),
            dlg_btn(tr("New"), Some(o(OptField::ExcAdd))),
        ]
        .spacing(8),
        section(tr("Download limits")),
        hinted(
            checkbox(s.dl_limit_enabled)
                .label(tr("Download limits"))
                .on_toggle(|b| o(OptField::DlLimit(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            tr("Caps how much PlayDL may transfer per period — for metered or capped connections. Transfers pause when the cap is reached and resume by themselves when the next period starts."),
        ),
        row![
            text(tr("Download no more than")).size(theme::FONT_SIZE),
            text_input("200", &st.dl_limit_mb_txt)
                .on_input(|v| o(OptField::DlLimitMb(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(80.0),
            text(tr("MBytes every")).size(theme::FONT_SIZE),
            text_input("5", &st.dl_limit_hours_txt)
                .on_input(|v| o(OptField::DlLimitHours(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(60.0),
            text(tr("hours")).size(theme::FONT_SIZE),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        text(quota_line(app))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
        checkbox(s.warn_before_stop)
            .label(tr("Show warning before stopping downloads"))
            .on_toggle(|b| o(OptField::WarnStop(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
    ]
    .spacing(8)
    .into()
}

fn proxy(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let mode = s.proxy_mode;
    column![
        section(tr("Proxy / socks configuration")),
        radio(tr("No proxy/socks"), ProxyMode::None, Some(mode), |m| o(
            OptField::ProxyMode(m)
        ))
        .size(15.0)
        .text_size(theme::FONT_SIZE),
        radio(
            tr("Use system settings"),
            ProxyMode::System,
            Some(mode),
            |m| { o(OptField::ProxyMode(m)) }
        )
        .size(15.0)
        .text_size(theme::FONT_SIZE),
        radio(
            tr("Use automatic configuration script"),
            ProxyMode::Script,
            Some(mode),
            |m| o(OptField::ProxyMode(m)),
        )
        .size(15.0)
        .text_size(theme::FONT_SIZE),
        row![
            text(tr("Address")).size(theme::FONT_SIZE).width(80.0),
            text_input("", &s.proxy_script)
                .on_input(|v| o(OptField::ProxyScript(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
        ]
        .spacing(8),
        radio(
            tr("Manual proxy/socks configuration"),
            ProxyMode::Manual,
            Some(mode),
            |m| o(OptField::ProxyMode(m)),
        )
        .size(15.0)
        .text_size(theme::FONT_SIZE),
        row![
            column![
                text(tr("Proxy server address")).size(theme::FONT_SIZE),
                text_input("", &s.proxy_host)
                    .on_input(|v| o(OptField::ProxyHost(v)))
                    .size(theme::FONT_SIZE)
                    .style(theme::input),
            ]
            .spacing(4)
            .width(Length::Fill),
            column![
                text(tr("Port")).size(theme::FONT_SIZE),
                text_input("", &s.proxy_port)
                    .on_input(|v| o(OptField::ProxyPort(v)))
                    .size(theme::FONT_SIZE)
                    .style(theme::input),
            ]
            .spacing(4)
            .width(90.0),
            column![
                text(tr("UserName")).size(theme::FONT_SIZE),
                text_input("", &s.proxy_user)
                    .on_input(|v| o(OptField::ProxyUser(v)))
                    .size(theme::FONT_SIZE)
                    .style(theme::input),
            ]
            .spacing(4)
            .width(140.0),
            column![
                text(tr("Password")).size(theme::FONT_SIZE),
                text_input("", &s.proxy_pass)
                    .on_input(|v| o(OptField::ProxyPass(v)))
                    .secure(true)
                    .size(theme::FONT_SIZE)
                    .style(theme::input),
            ]
            .spacing(4)
            .width(140.0),
        ]
        .spacing(10),
        text(tr("Use this proxy for the following protocols:")).size(theme::FONT_SIZE),
        row![
            checkbox(s.proxy_http)
                .label("http")
                .on_toggle(|b| o(OptField::ProxyHttp(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            checkbox(s.proxy_https)
                .label("https")
                .on_toggle(|b| o(OptField::ProxyHttps(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            checkbox(s.proxy_ftp)
                .label("ftp")
                .on_toggle(|b| o(OptField::ProxyFtp(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
        ]
        .spacing(20),
        checkbox(s.ftp_pasv)
            .label(tr("Use FTP in PASV mode"))
            .on_toggle(|b| o(OptField::FtpPasv(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
    ]
    .spacing(8)
    .into()
}

fn sites(app: &App) -> El<'_> {
    let st = &app.options;
    let mut list = column![].spacing(2);
    list = list.push(
        row![
            container(text(tr("Site/path")).size(theme::FONT_SIZE)).width(Length::Fill),
            container(text(tr("User")).size(theme::FONT_SIZE)).width(140.0),
            container(text(tr("Password")).size(theme::FONT_SIZE)).width(120.0),
        ]
        .spacing(6),
    );
    for (i, l) in st.draft.logins.iter().enumerate() {
        let selected = st.sel_login == Some(i);
        list = list.push(
            button(
                row![
                    container(text(l.site.clone()).size(theme::FONT_SIZE)).width(Length::Fill),
                    container(text(l.user.clone()).size(theme::FONT_SIZE)).width(140.0),
                    container(text("•••".to_string()).size(theme::FONT_SIZE)).width(120.0),
                ]
                .spacing(6),
            )
            .padding([1, 2])
            .width(Length::Fill)
            .style(theme::btn_row(selected))
            .on_press(o(OptField::LoginSel(i))),
        );
    }
    column![
        section(tr("User names and passwords for servers/sites")),
        container(scrollable(list).height(220.0))
            .padding(6)
            .width(Length::Fill)
            .style(theme::panel),
        row![
            text_input(&tr("Site/path"), &st.login_site)
                .on_input(|v| o(OptField::LoginSite(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            text_input(&tr("User"), &st.login_user)
                .on_input(|v| o(OptField::LoginUser(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(130.0),
            text_input(&tr("Password"), &st.login_pass)
                .on_input(|v| o(OptField::LoginPass(v)))
                .secure(true)
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(130.0),
        ]
        .spacing(8),
        row![
            dlg_btn(tr("New"), Some(o(OptField::LoginAdd))),
            dlg_btn(tr("Remove"), Some(o(OptField::LoginRemove))),
        ]
        .spacing(10),
    ]
    .spacing(10)
    .into()
}

/// The Chrome Web Store listing. Edge has a store of its own (below); the
/// remaining Chromium browsers install the same item from here.
const CHROME_STORE: &str =
    "https://chromewebstore.google.com/detail/playdl-download-manager-in/oieelfilllghmbnhofajpgpmmilfihmo";

/// The Firefox Add-ons listing.
/// The setup guide behind the FFmpeg row. A wiki page rather than a
/// paragraph in this dialog: what to install differs per platform, and it
/// changes faster than the app ships.
const FFMPEG_WIKI: &str = "https://github.com/ja7ad/playdl/wiki/PlayDL-ffmpeg-integration";

/// The "not present" counterpart to `theme::PROGRESS_GREEN`.
const OFFLINE_RED: u32 = 0xB3462E;

const FIREFOX_STORE: &str = "https://addons.mozilla.org/en-US/firefox/addon/hdm-integration/";

/// The Microsoft Edge Add-ons listing: same extension, Edge's own store.
const EDGE_STORE: &str =
    "https://microsoftedge.microsoft.com/addons/detail/playdl-download-manager-in/obemipfpeenmhkdpkobdkeedhdakaoai";

/// One extension row: brand mark on the left, what the extension does in the
/// middle, the link button on the right. `link` is `None` for a browser with
/// neither a listing nor a guide, which leaves the button disabled.
fn ext_row<'a>(
    icon: iced::widget::svg::Handle,
    name: &'static str,
    about: String,
    label: String,
    link: Option<&'static str>,
) -> El<'a> {
    let msg = link.map(Message::OptExtStore);
    container(
        row![
            iced::widget::svg(icon).width(30.0).height(30.0),
            column![
                text(name).size(theme::FONT_SIZE + 1.0),
                text(about)
                    .size(theme::FONT_SIZE - 1.0)
                    // A translated description can hold one word longer than
                    // the row is wide, and word wrapping alone lets it run
                    // out past the panel edge.
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .color(theme::dim_text(&iced::Theme::Light)),
            ]
            .spacing(3)
            .width(Length::Fill),
            // Auto width, not the fixed dialog-button width: store labels
            // run long once translated and a fixed 132 px clips them.
            match msg {
                Some(m) => dlg_btn_auto_primary(label, Some(m)),
                None => dlg_btn_auto(label, None),
            },
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
    )
    .padding(10)
    .width(Length::Fill)
    .style(theme::panel)
    .into()
}

/// Trim a filesystem path down to one readable line: the root, an ellipsis,
/// and the tail that actually identifies the binary.
///
/// Windows package managers bury ffmpeg absurdly deep: winget's copy sits
/// under `%LOCALAPPDATA%\Microsoft\WinGet\Packages`, behind a package folder
/// and a versioned build folder, for 120-odd characters that no dialog row
/// can hold. The head and the last few components say where it came from
/// and what it is; the full string is one hover away.
fn elide_path(path: &std::path::Path, budget: usize) -> String {
    let full = path.display().to_string();
    if full.chars().count() <= budget {
        return full;
    }
    let sep = if full.contains('\\') { '\\' } else { '/' };
    let parts: Vec<&str> = full.split(sep).collect();
    // The head is the drive on Windows (`C:`) and the empty string before
    // the leading slash on Unix, which is exactly what makes `/…/bin/ffmpeg`
    // come out right.
    let head = parts.first().copied().unwrap_or_default();
    let mut tail: Vec<&str> = Vec::new();
    // The head, plus the separator on either side of the ellipsis.
    let mut used = head.chars().count() + 3;
    for part in parts.iter().skip(1).rev() {
        let cost = part.chars().count() + 1;
        // The file name goes in whatever it costs: a row that elides down to
        // "C:\…" has told the reader nothing at all.
        if !tail.is_empty() && used + cost > budget {
            break;
        }
        used += cost;
        tail.push(part);
    }
    tail.reverse();
    format!("{head}{sep}\u{2026}{sep}{}", tail.join(&sep.to_string()))
}

/// A status word beside the FFmpeg title: the dot carries the same meaning,
/// and a dot alone is no answer for anyone who cannot tell the two colours
/// apart.
fn status_badge<'a>(label: String, colour: iced::Color) -> El<'a> {
    row![
        text("\u{25CF}").size(theme::FONT_SIZE - 3.0).color(colour),
        text(label).size(theme::FONT_SIZE - 1.0).color(colour),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}

/// What the FFmpeg row has to say, worked out before anything is laid out.
///
/// A built row is a tree of shapes, and none of the questions worth asking
/// about this one survive being turned into one: is the guide offered only
/// while it has something to tell you? is a path shown only when there is
/// one? So the answers are decided here, and the widgets below merely draw
/// them.
struct FfmpegRow {
    found: bool,
    badge: String,
    about: String,
    /// The path as the row shows it and as the clipboard gets it: elided for
    /// the eye, whole for the paste.
    path: Option<(String, String)>,
    action: String,
    /// The setup guide, offered only when it can still help.
    guide: Option<&'static str>,
}

impl FfmpegRow {
    fn of(found: Option<&std::path::Path>) -> Self {
        match found {
            Some(path) => Self {
                found: true,
                badge: tr("Installed"),
                about: tr(
                    "MPEG-TS is remuxed to MP4, and DASH video and audio are merged into one file.",
                ),
                path: Some((elide_path(path, 58), path.display().to_string())),
                action: tr("Copy path"),
                guide: None,
            },
            None => Self {
                found: false,
                badge: tr("Not found"),
                about: tr("Not found on PATH. HLS and DASH still download, but MPEG-TS is saved as .ts instead of .mp4, and DASH audio stays in its own file."),
                path: None,
                action: tr("FFmpeg setup guide"),
                guide: Some(FFMPEG_WIKI),
            },
        }
    }
}

/// The FFmpeg row, which reports what is actually on THIS machine rather
/// than describing ffmpeg in the abstract: a green badge and the path it was
/// found at, or a red one and the guide.
///
/// The path is a line of its own rather than a tail glued to the status
/// sentence. Glued on, a Windows path wraps as one unbreakable word, runs
/// out past the panel and straight under the button — and it buried the part
/// that matters (which ffmpeg is this?) inside a paragraph.
fn ffmpeg_row<'a>(found: Option<std::path::PathBuf>) -> El<'a> {
    let st = FfmpegRow::of(found.as_deref());
    let colour = if st.found {
        hex(theme::PROGRESS_GREEN)
    } else {
        hex(OFFLINE_RED)
    };
    let action = match st.guide {
        Some(url) => dlg_btn_auto_primary(st.action, Some(Message::OptExtStore(url))),
        None => dlg_btn_auto(
            st.action,
            st.path
                .as_ref()
                .map(|(_, full)| Message::OptCopy(full.clone())),
        ),
    };
    let mut info = column![
        row![
            text("FFmpeg").size(theme::FONT_SIZE + 1.0),
            status_badge(st.badge, colour)
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        text(st.about)
            .size(theme::FONT_SIZE - 1.0)
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
            .color(theme::dim_text(&iced::Theme::Light)),
    ]
    .spacing(3)
    .width(Length::Fill);
    if let Some((shown, full)) = st.path {
        info = info.push(tooltip(
            text(shown)
                .size(theme::FONT_SIZE - 1.0)
                .wrapping(iced::widget::text::Wrapping::None)
                .color(theme::dim_text(&iced::Theme::Light)),
            container(
                text(full)
                    .size(theme::FONT_SIZE - 1.0)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            )
            .padding(8)
            .max_width(420.0)
            .style(theme::menu_panel),
            tooltip::Position::Top,
        ));
    }
    container(
        row![
            iced::widget::svg(crate::icons::folder_video())
                .width(30.0)
                .height(30.0),
            info,
            action,
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
    )
    .padding(10)
    .width(Length::Fill)
    .style(theme::panel)
    .into()
}

fn extensions(_app: &App) -> El<'_> {
    column![
        section(tr("Browser extensions")),
        text(tr(
            "Install the PlayDL extension to capture downloads straight from your browser."
        ))
        .size(theme::FONT_SIZE),
        ext_row(
            crate::icons::browser_chrome(),
            "Google Chrome",
            tr("Automatic download capture, right-click downloads and media sniffing. Published on the Chrome Web Store."),
            tr("Open Chrome Web Store"),
            Some(CHROME_STORE),
        ),
        ext_row(
            crate::icons::browser_firefox(),
            "Mozilla Firefox",
            tr("Automatic download capture, right-click downloads and media sniffing. Published on the Firefox Addons."),
            tr("Open Firefox Addons"),
            Some(FIREFOX_STORE),
        ),
        ext_row(
            crate::icons::browser_edge(),
            "Microsoft Edge",
            tr("Automatic download capture, right-click downloads and media sniffing. Published on Microsoft Edge Add-ons."),
            tr("Open Edge Add-ons"),
            Some(EDGE_STORE),
        ),
        ext_row(
            crate::icons::browser_chromium(),
            "Brave / Vivaldi / Opera / Arc / Chromium",
            tr("Chromium browsers install the very same extension from the Chrome Web Store."),
            tr("Open Chrome Web Store"),
            Some(CHROME_STORE),
        ),
        ext_row(
            crate::icons::browser_safari(),
            "Safari",
            tr("Ships inside the macOS app; enable PlayDL under Safari > Settings > Extensions."),
            tr("Not on store yet"),
            None,
        ),
        section(tr("Media tools")),
        ffmpeg_row(pdl_stream::ffmpeg()),
    ]
    .spacing(10)
    .into()
}

fn sounds(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let mut list = column![].spacing(4);
    list = list.push(
        row![
            container(text(tr("Event")).size(theme::FONT_SIZE)).width(Length::Fill),
            container(text(tr("Sound file")).size(theme::FONT_SIZE)).width(200.0),
        ]
        .spacing(6),
    );
    for (i, snd) in s.sounds.iter().enumerate() {
        let file_label = if snd.file.is_empty() {
            tr("(default chime)")
        } else {
            snd.file.clone()
        };
        list = list.push(
            row![
                checkbox(snd.enabled)
                    .label(tr(&snd.event))
                    .on_toggle(move |b| o(OptField::Sound(i, b)))
                    .size(15.0)
                    .text_size(theme::FONT_SIZE)
                    .style(theme::check)
                    .width(Length::Fill),
                container(
                    text(file_label)
                        .size(theme::FONT_SIZE - 1.0)
                        .wrapping(iced::widget::text::Wrapping::None)
                )
                .width(230.0)
                .clip(true),
                dlg_btn(tr("Browse"), Some(o(OptField::SoundBrowse(i)))),
                dlg_btn(tr("Play"), Some(o(OptField::SoundPlay(i)))),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        );
    }
    column![
        section(tr("Sound settings")),
        text(tr("Select sounds for download events")).size(theme::FONT_SIZE),
        text(tr("Supported formats: .wav and .ogg. A built-in chime plays when no file is set or the file is missing."))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
        container(list).padding(10).width(Length::Fill).style(theme::panel),
    ]
    .spacing(10)
    .into()
}

pub fn view(app: &App) -> El<'_> {
    let cur = app.options.tab;
    let tabs_top = row![
        tab_btn(tr("General"), OptTab::General, cur),
        tab_btn(tr("File types"), OptTab::FileTypes, cur),
        tab_btn(tr("Save to"), OptTab::SaveTo, cur),
        tab_btn(tr("Downloads"), OptTab::Downloads, cur),
        tab_btn(tr("Connection"), OptTab::Connection, cur),
    ]
    .spacing(1);
    let tabs_bottom = row![
        tab_btn(tr("Proxy / Socks"), OptTab::Proxy, cur),
        tab_btn(tr("Sites Logins"), OptTab::Sites, cur),
        tab_btn(tr("Extensions"), OptTab::Extensions, cur),
        tab_btn(tr("Sounds"), OptTab::Sounds, cur),
    ]
    .spacing(1);

    let body: El<'_> = match cur {
        OptTab::General => general(app),
        OptTab::FileTypes => file_types(app),
        OptTab::SaveTo => save_to(app),
        OptTab::Downloads => downloads(app),
        OptTab::Connection => connection(app),
        OptTab::Proxy => proxy(app),
        OptTab::Sites => sites(app),
        OptTab::Extensions => extensions(app),
        OptTab::Sounds => sounds(app),
    };

    // Notebook metaphor: the row holding the selected tab sits adjacent to
    // the content pane (physically swaps the rows on selection), so the
    // active tab always joins its page.
    let active_in_top = matches!(
        cur,
        OptTab::General
            | OptTab::FileTypes
            | OptTab::SaveTo
            | OptTab::Downloads
            | OptTab::Connection
    );
    let (first_row, second_row) = if active_in_top {
        (tabs_bottom, tabs_top)
    } else {
        (tabs_top, tabs_bottom)
    };
    container(
        column![
            first_row,
            second_row,
            container(scrollable(container(body).padding(14).width(Length::Fill)))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::panel),
            row![
                iced::widget::space::horizontal(),
                dlg_btn_primary(tr("OK"), Some(Message::OptOk)),
                dlg_btn(
                    tr("Cancel"),
                    app.win_of(WinKind::Options).map(Message::CloseThis)
                ),
            ]
            .spacing(10),
        ]
        .spacing(6)
        .padding(10),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn short_paths_are_left_alone() {
        let p = Path::new("/usr/local/bin/ffmpeg");
        assert_eq!(elide_path(p, 58), "/usr/local/bin/ffmpeg");
    }

    #[test]
    fn a_deep_windows_path_keeps_the_drive_and_the_binary() {
        let p = Path::new(
            "C:\\Users\\javad\\AppData\\Local\\Microsoft\\WinGet\\Packages\\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe\\ffmpeg-7.1-full_build\\bin\\ffmpeg.exe",
        );
        let out = elide_path(p, 58);
        assert!(out.starts_with("C:\\\u{2026}\\"), "lost the drive: {out}");
        assert!(out.ends_with("\\bin\\ffmpeg.exe"), "lost the binary: {out}");
        assert!(out.chars().count() <= 58, "still too long: {out}");
    }

    #[test]
    fn a_deep_unix_path_keeps_its_leading_slash() {
        let p =
            Path::new("/home/javad/.local/share/some/rather/deeply/nested/vendor/tree/bin/ffmpeg");
        let out = elide_path(p, 40);
        assert!(out.starts_with("/\u{2026}/"), "lost the root: {out}");
        assert!(out.ends_with("/bin/ffmpeg"), "lost the binary: {out}");
    }

    #[test]
    fn an_installed_ffmpeg_shows_which_one_and_offers_no_guide() {
        let path = Path::new("/opt/homebrew/bin/ffmpeg");
        let st = FfmpegRow::of(Some(path));
        assert!(st.found);
        assert_eq!(st.badge, tr("Installed"));
        assert_eq!(st.action, tr("Copy path"));
        // Nothing to guide anyone to: it is already here.
        assert_eq!(st.guide, None);
        let (shown, full) = st.path.expect("an installed ffmpeg shows its path");
        assert_eq!(shown, "/opt/homebrew/bin/ffmpeg");
        // The clipboard gets the whole thing, never the elided line.
        assert_eq!(full, "/opt/homebrew/bin/ffmpeg");
    }

    #[test]
    fn a_deep_install_is_elided_for_the_row_but_not_for_the_clipboard() {
        let full = "C:\\Users\\javad\\AppData\\Local\\Microsoft\\WinGet\\Packages\\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe\\ffmpeg-7.1-full_build\\bin\\ffmpeg.exe";
        let st = FfmpegRow::of(Some(Path::new(full)));
        let (shown, copied) = st.path.expect("an installed ffmpeg shows its path");
        assert!(shown.chars().count() <= 58, "row line too long: {shown}");
        assert!(shown.contains('\u{2026}'), "not elided: {shown}");
        assert_eq!(copied, full);
    }

    #[test]
    fn a_missing_ffmpeg_offers_the_guide_and_shows_no_path() {
        let st = FfmpegRow::of(None);
        assert!(!st.found);
        assert_eq!(st.badge, tr("Not found"));
        assert_eq!(st.guide, Some(FFMPEG_WIKI));
        assert_eq!(st.action, tr("FFmpeg setup guide"));
        assert!(st.path.is_none(), "no path to show, and none shown");
        // The sentence has to say what still works, not just what is absent.
        assert!(st.about.len() > st.badge.len());
    }

    /// Both states build. The row is the one place in this dialog that
    /// changes shape — a third line and a different button — and a widget
    /// tree that does not survive being built takes the whole window down
    /// with it.
    #[test]
    fn both_states_of_the_row_lay_out() {
        let _found: El<'_> = ffmpeg_row(Some("/usr/local/bin/ffmpeg".into()));
        let _missing: El<'_> = ffmpeg_row(None);
    }

    /// A budget the file name alone cannot meet still shows the file name:
    /// eliding down to "C:\…" would answer nothing.
    #[test]
    fn the_file_name_survives_an_impossible_budget() {
        let p = Path::new("C:\\Program Files\\ffmpeg\\bin\\ffmpeg-with-a-long-name.exe");
        let out = elide_path(p, 10);
        assert_eq!(out, "C:\\\u{2026}\\ffmpeg-with-a-long-name.exe");
    }
}
