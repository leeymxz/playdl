// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! Human formatting: `121.66 MB`, `1.027 MB/sec`,
//! `3 min 32 sec`, `Aug 17 15:48:32 2026`.

use chrono::{DateTime, Local, TimeZone};

/// `121.66 MB` (two decimals — the download-list spelling).
pub fn size2(bytes: u64) -> String {
    size_n(bytes, 2)
}

/// `121.665 MB` (three decimals — the progress-dialog spelling).
pub fn size3(bytes: u64) -> String {
    size_n(bytes, 3)
}

fn size_n(bytes: u64, prec: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.prec$} GB", b / GB)
    } else if b >= MB {
        format!("{:.prec$} MB", b / MB)
    } else if b >= KB {
        format!("{:.prec$} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// `1.027 MB/sec`, `200.666 KB/sec`.
pub fn rate(bytes_per_sec: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes_per_sec >= MB {
        format!("{:.3} MB/sec", bytes_per_sec / MB)
    } else if bytes_per_sec >= 1.0 {
        format!("{:.3} KB/sec", bytes_per_sec / KB)
    } else {
        String::new()
    }
}

/// The transfer rate as the reading a capped transfer should show.
///
/// A capped transfer sits AT its cap, and the measurement jitters a percent or
/// two either side of it. Printing that measurement makes a steady, deliberately
/// limited transfer look unsteady — the figure flickers between 97 and 103
/// KB/sec while the user is looking at the 100 they typed. So the reading snaps
/// to the cap once the transfer is running at it, and falls back to the real
/// number when the transfer genuinely cannot reach the cap (a slow origin, a
/// congested link): a cap is a ceiling, not a promise, and hiding a shortfall
/// behind the requested figure would be the worse lie.
fn at_cap(bytes_per_sec: f64, cap: u64) -> f64 {
    let c = cap as f64;
    // Within a tenth of the cap counts as "at the cap". Wide enough to absorb
    // the smoothing window's ripple, narrow enough that a transfer running at
    // four fifths of what was asked still reports four fifths.
    if bytes_per_sec >= c * 0.9 {
        c
    } else {
        bytes_per_sec
    }
}

/// `100.000 KB/sec` — steady under a cap, honest below it. No suffix: for the
/// download list, whose Transfer rate column has no room for one.
pub fn rate_steady(bytes_per_sec: f64, cap: Option<u64>) -> String {
    match cap.filter(|c| *c > 0) {
        Some(c) => rate(at_cap(bytes_per_sec, c)),
        None => rate(bytes_per_sec),
    }
}

/// `100.000 KB/sec (Limited)` — as [`rate_steady`], saying why it is steady.
///
/// The suffix stays on even when the transfer is running below its cap, because
/// that is exactly when the user most needs to know a cap is in force: a slow
/// download with the Speed Limiter forgotten in the Options menu is otherwise
/// indistinguishable from a slow server.
pub fn rate_capped(bytes_per_sec: f64, cap: Option<u64>) -> String {
    let Some(cap) = cap.filter(|c| *c > 0) else {
        return rate(bytes_per_sec);
    };
    if bytes_per_sec < 1.0 {
        return String::new();
    }
    format!(
        "{} {}",
        rate(at_cap(bytes_per_sec, cap)),
        crate::i18n::tr("(Limited)")
    )
}

/// `3 min 32 sec`, `1 hr 12 min`, `45 sec`.
pub fn eta(secs: u64) -> String {
    if secs >= 3600 {
        format!("{} hr {} min", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{} min {} sec", secs / 60, secs % 60)
    } else {
        format!("{secs} sec")
    }
}

/// `5.80%` — the Status column while a transfer runs.
pub fn pct(done: u64, size: u64) -> String {
    if size == 0 {
        return String::new();
    }
    format!("{:.2}%", done as f64 * 100.0 / size as f64)
}

/// `Aug 17 15:48:32 2026` — the Last Try Date column.
pub fn date(unix: i64) -> String {
    match Local.timestamp_opt(unix, 0) {
        chrono::LocalResult::Single(t) => t.format("%b %d %H:%M:%S %Y").to_string(),
        _ => String::new(),
    }
}

/// Current unix time, seconds.
pub fn now_unix() -> i64 {
    Local::now().timestamp()
}

/// `15:48` for scheduler time fields.
pub fn hhmm(t: &DateTime<Local>) -> String {
    t.format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transfer sitting at its cap must read as the cap, not as the ripple
    /// around it — and a transfer that cannot reach the cap must read as itself.
    #[test]
    fn a_capped_rate_reads_steady_but_never_flatters() {
        let cap = Some(100 * 1024);
        // Measurement noise either side of the cap: all one figure.
        for measured in [97_000.0, 102_400.0, 105_000.0] {
            assert_eq!(rate_steady(measured, cap), "100.000 KB/sec");
        }
        // Genuinely slower than the cap: report what is really happening.
        assert_eq!(rate_steady(40.0 * 1024.0, cap), rate(40.0 * 1024.0));
        // No cap at all: unchanged from the plain reading.
        assert_eq!(rate_steady(1234.0, None), rate(1234.0));
        assert_eq!(rate_steady(1234.0, Some(0)), rate(1234.0));
    }

    #[test]
    fn the_limited_suffix_marks_a_cap_that_is_in_force() {
        let cap = Some(100 * 1024);
        assert_eq!(rate_capped(102_400.0, cap), "100.000 KB/sec (Limited)");
        // Below the cap the real figure is shown, still marked as capped.
        assert!(rate_capped(40.0 * 1024.0, cap).ends_with("(Limited)"));
        assert!(rate_capped(40.0 * 1024.0, cap).starts_with("40.000 KB/sec"));
        // An idle transfer has no rate to report, capped or not.
        assert!(rate_capped(0.0, cap).is_empty());
        assert_eq!(rate_capped(102_400.0, None), "100.000 KB/sec");
    }
}
