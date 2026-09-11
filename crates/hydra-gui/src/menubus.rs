// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! One channel for every native menu surface.
//!
//! The macOS menu bar and the system-tray menu are both muda menus, and muda
//! has exactly one global event handler — whoever sets it last wins. This
//! module owns that handler and fans every activation (by menu-item id) into
//! a single stream the iced subscription drains as `Message::NativeMenu`.

use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

static TX: OnceLock<UnboundedSender<String>> = OnceLock::new();
static RX: Mutex<Option<UnboundedReceiver<String>>> = Mutex::new(None);

pub fn sender() -> UnboundedSender<String> {
    TX.get_or_init(|| {
        let (tx, rx) = unbounded_channel();
        if let Ok(mut g) = RX.lock() {
            *g = Some(rx);
        }
        tx
    })
    .clone()
}

/// Take the receiving end (once). On platforms with no native menus the
/// channel simply never carries anything.
pub fn take_events() -> Option<UnboundedReceiver<String>> {
    let _ = sender();
    RX.lock().ok().and_then(|mut g| g.take())
}

/// Install the single global muda handler (idempotent).
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn ensure_menu_handler() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_ok() {
        let tx = sender();
        tray_icon::menu::MenuEvent::set_event_handler(Some(
            move |ev: tray_icon::menu::MenuEvent| {
                let _ = tx.send(ev.id().0.clone());
            },
        ));
    }
}
