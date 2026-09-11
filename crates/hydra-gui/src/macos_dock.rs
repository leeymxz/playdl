// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Dock-icon visibility on macOS.
//!
//! "Hide Dock icon" switches the NSApplication activation policy between
//! Regular and Accessory. AppKit never gives an Accessory app the system
//! menu bar, and on macOS all of PlayDL's menus live there — so going
//! Accessory while a window is open would leave the app unusable (the
//! original hide-Dock bug). The policy is therefore window-aware: Regular
//! whenever any PlayDL window is open (Dock tile + menu bar), Accessory only
//! while the app lives in the tray with no windows.

#![cfg(target_os = "macos")]

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

/// Re-assert the policy for the current preference + window state. Call on
/// every window open/close and when the setting changes. Main thread only
/// (the iced update loop qualifies); silently a no-op elsewhere rather than
/// crashing in AppKit.
pub fn sync(hide_dock: bool, windows_open: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let policy = if hide_dock && !windows_open {
        NSApplicationActivationPolicy::Accessory
    } else {
        NSApplicationActivationPolicy::Regular
    };
    if app.activationPolicy() == policy {
        return;
    }
    let _ = app.setActivationPolicy(policy);
    // Accessory -> Regular while a window is up: AppKit only attaches the
    // menu bar on activation, so without this nudge the menus stay missing
    // until the user clicks away and back.
    if policy == NSApplicationActivationPolicy::Regular && windows_open {
        app.activate();
    }
}
