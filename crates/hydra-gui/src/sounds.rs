// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! Event sounds: user-selected .wav/.ogg per event, or a generated chime
//! when no file is configured (or the configured file has gone missing).
//!
//! Playback is fire-and-forget on a throwaway thread — an audio failure must
//! never affect a download, so every error path is a silent log line.

use std::io::Cursor;

/// Which event is chiming; selects the built-in melody when the user set no
/// sound file.
#[derive(Clone, Copy)]
pub enum Event {
    DownloadComplete = 0,
    DownloadFailed = 1,
    QueueStarted = 2,
    QueueStopped = 3,
}

impl Event {
    pub fn from_index(i: usize) -> Event {
        match i {
            1 => Event::DownloadFailed,
            2 => Event::QueueStarted,
            3 => Event::QueueStopped,
            _ => Event::DownloadComplete,
        }
    }

    /// (frequency Hz, duration s) notes — each event gets its own shape:
    /// success rises, failure falls low, queue start runs up an arpeggio,
    /// queue stop steps back down.
    fn notes(self) -> &'static [(f32, f32)] {
        match self {
            Event::DownloadComplete => &[(659.25, 0.12), (880.0, 0.20)],
            Event::DownloadFailed => &[(311.13, 0.16), (233.08, 0.26)],
            Event::QueueStarted => &[(523.25, 0.09), (659.25, 0.09), (783.99, 0.16)],
            Event::QueueStopped => &[(783.99, 0.12), (523.25, 0.22)],
        }
    }
}

/// Generate the event's melody as an in-memory WAV: sine notes with a
/// per-note fade so nothing clicks.
fn default_chime(event: Event) -> Vec<u8> {
    const RATE: u32 = 44_100;
    let notes = event.notes();
    let total: usize = notes.iter().map(|(_, d)| (RATE as f32 * d) as usize).sum();
    let mut data = Vec::with_capacity(44 + total * 2);
    let byte_len = (total * 2) as u32;
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&(36 + byte_len).to_le_bytes());
    data.extend_from_slice(b"WAVEfmt ");
    data.extend_from_slice(&16u32.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes()); // PCM
    data.extend_from_slice(&1u16.to_le_bytes()); // mono
    data.extend_from_slice(&RATE.to_le_bytes());
    data.extend_from_slice(&(RATE * 2).to_le_bytes());
    data.extend_from_slice(&2u16.to_le_bytes());
    data.extend_from_slice(&16u16.to_le_bytes());
    data.extend_from_slice(b"data");
    data.extend_from_slice(&byte_len.to_le_bytes());
    for &(freq, dur) in notes {
        let n = (RATE as f32 * dur) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            // Quick attack, gentle release.
            let pos = i as f32 / n as f32;
            let env = (pos * 25.0).min(1.0) * (1.0 - pos).powf(1.5);
            let v = (t * freq * std::f32::consts::TAU).sin() * env * 0.4;
            data.extend_from_slice(&((v * i16::MAX as f32) as i16).to_le_bytes());
        }
    }
    data
}

/// Play `file` when it exists, the event's own chime otherwise.
pub fn play(file: Option<String>, event: Event) {
    std::thread::spawn(move || {
        // rodio 0.22: a device sink owns the output; `play` decodes and
        // returns a Player we block on until the clip ends.
        let Ok(device) = rodio::DeviceSinkBuilder::open_default_sink() else {
            crate::log::debug("sound: no output device");
            return;
        };
        let from_file = file
            .filter(|p| std::path::Path::new(p).exists())
            .and_then(|p| std::fs::File::open(p).ok());
        let player = match from_file {
            Some(f) => rodio::play(device.mixer(), std::io::BufReader::new(f)),
            None => rodio::play(device.mixer(), Cursor::new(default_chime(event))),
        };
        match player {
            Ok(p) => p.sleep_until_end(),
            Err(e) => crate::log::debug(&format!("sound: {e}")),
        }
    });
}
