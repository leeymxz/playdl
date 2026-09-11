// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS + Windows tray backend: `tray-icon` (which re-exports muda as
//! `tray_icon::menu`, so tray and macOS menu bar share one muda instance and
//! therefore one global menu-event channel).

#![cfg(any(target_os = "macos", target_os = "windows"))]

use super::Entry;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

thread_local! {
    static CURRENT: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}
/// Readable from any thread, unlike `CURRENT` — `is_active` answers whether
/// closing the last window may leave the app running.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// The shared mono silhouette as a muda icon (see [`crate::icons::logo_mono_rgba`]).
fn mono_icon(white: bool) -> Option<tray_icon::Icon> {
    let (rgba, w, h) = crate::icons::logo_mono_rgba(white)?;
    tray_icon::Icon::from_rgba(rgba, w, h).ok()
}

/// Render the shared menu model into muda items. Boxed because a submenu's
/// children must outlive the `append_items` call that takes them by
/// reference.
fn render(entries: &[Entry]) -> Vec<Box<dyn IsMenuItem>> {
    entries
        .iter()
        .map(|e| -> Box<dyn IsMenuItem> {
            match e {
                Entry::Separator => Box::new(PredefinedMenuItem::separator()),
                Entry::Item { id, label } => {
                    Box::new(MenuItem::with_id(id.clone(), label, true, None))
                }
                Entry::Check { id, label, checked } => Box::new(CheckMenuItem::with_id(
                    id.clone(),
                    label,
                    true,
                    *checked,
                    None,
                )),
                Entry::Sub { label, items } => {
                    let sub = Submenu::new(label, true);
                    let children = render(items);
                    let refs: Vec<&dyn IsMenuItem> = children.iter().map(|c| c.as_ref()).collect();
                    let _ = sub.append_items(&refs);
                    Box::new(sub)
                }
            }
        })
        .collect()
}

fn build_menu(entries: &[Entry]) -> Menu {
    let menu = Menu::new();
    let children = render(entries);
    let refs: Vec<&dyn IsMenuItem> = children.iter().map(|c| c.as_ref()).collect();
    let _ = menu.append_items(&refs);
    menu
}

pub fn install(entries: Vec<Entry>) {
    let installed = CURRENT.with(|c| c.borrow().is_some());
    if installed {
        return;
    }
    crate::menubus::ensure_menu_handler();
    install_with_menu(build_menu(&entries));
}

pub fn reinstall(entries: Vec<Entry>) {
    CURRENT.with(|c| {
        if let Some(tray) = c.borrow().as_ref() {
            tray.set_menu(Some(Box::new(build_menu(&entries))));
        }
    });
}

pub fn is_active() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

fn install_with_menu(menu: Menu) {
    // Left-click on the icon brings the main window back; the menu is
    // right-click only (see `with_menu_on_left_click` below), matching the
    // Linux backend's `activate`.
    let tx = crate::menubus::sender();
    TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
            ..
        } = ev
        {
            let _ = tx.send("show_main".into());
        }
    }));

    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("PlayDL");
    // tray-icon pops the menu on either button by default, while the Click
    // event fires regardless — so a left-click used to raise the window
    // *behind* an unwanted menu. Left-click activates, right-click opens the
    // menu, as the Windows shell does and as ksni already does on Linux.
    builder = builder.with_menu_on_left_click(false);
    // macOS: a TEMPLATE image — black + alpha that AppKit recolors itself
    // for the light/dark menu bar (and inverts while highlighted). Windows
    // has no template concept, so pick white/black from the system theme
    // once at startup.
    #[cfg(target_os = "macos")]
    {
        if let Some(icon) = mono_icon(false) {
            builder = builder.with_icon(icon).with_icon_as_template(true);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let dark_taskbar = matches!(dark_light::detect(), Ok(dark_light::Mode::Dark));
        if let Some(icon) = mono_icon(dark_taskbar) {
            builder = builder.with_icon(icon);
        }
    }
    match builder.build() {
        Ok(tray) => {
            CURRENT.with(|c| *c.borrow_mut() = Some(tray));
            INSTALLED.store(true, Ordering::Relaxed);
        }
        Err(e) => crate::log::warn(&format!("tray icon unavailable: {e}")),
    }
}
