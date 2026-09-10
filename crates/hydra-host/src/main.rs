// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Chrome/Firefox native-messaging host for Hydra.
//!
//! The browser spawns this binary and speaks the native-messaging framing on
//! stdio: 4-byte little-endian length, then a JSON document, each way. Every
//! request is forwarded to the running hydra-gui over the loopback socket it
//! publishes in `<app_dir>/ipc.json` (adding the secret token from that
//! file — the browser never sees it), and the GUI's reply is framed back.
//!
//! When the GUI is not running the host launches it minimized and waits for
//! the socket to appear, so clicking a download in the browser "just works"
//! exactly like monitor.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn app_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("hydra")
    }
    #[cfg(not(target_os = "windows"))]
    {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join(".config")
            .join("hydra")
    }
}

/// (port, token) out of an ipc.json body. A file from a crashed run, a
/// half-written one, or one from a build that spelled the fields
/// differently all read as "no instance" rather than as a bad address.
fn parse_ipc(text: &str) -> Option<(u16, String)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let port = u16::try_from(v.get("port")?.as_u64()?).ok()?;
    let token = v.get("token")?.as_str()?.to_string();
    Some((port, token))
}

/// (port, token) from ipc.json, if the file exists and parses.
fn read_ipc() -> Option<(u16, String)> {
    parse_ipc(&std::fs::read_to_string(app_dir().join("ipc.json")).ok()?)
}

/// Try to connect to the GUI right now. The file may be stale from a
/// previous run, so a parse success still has to survive the connect.
fn connect_once() -> Option<(TcpStream, String)> {
    let (port, token) = read_ipc()?;
    let stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(600),
    )
    .ok()?;
    stream.set_nodelay(true).ok();
    Some((stream, token))
}

/// A minimized GUI launch, stdio detached from ours.
fn gui_command(program: &std::ffi::OsStr) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.arg("--minimized")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Detached spawn of a GUI binary; true when the process started.
///
/// Windows: the browser runs a native-messaging host inside a JOB OBJECT and
/// kills the job when the host exits — which, for the one-shot
/// `sendNativeMessage` the extension calls us through, is the moment we
/// answer. Every process we started goes with us, so the GUI launched for
/// this very capture died seconds after starting, before any window of it
/// appeared. `CREATE_BREAKAWAY_FROM_JOB` is what Mozilla and Chrome
/// prescribe for children that must outlive the host; `DETACHED_PROCESS`
/// keeps the GUI off the console the browser gave us.
fn spawn_direct(program: std::ffi::OsString) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        if gui_command(&program)
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB | DETACHED_PROCESS)
            .spawn()
            .is_ok()
        {
            return true;
        }
        // A job created without JOB_OBJECT_LIMIT_BREAKAWAY_OK refuses the
        // flag outright (ERROR_ACCESS_DENIED) rather than ignoring it. Fall
        // back to an ordinary spawn — the pre-existing behaviour, and still
        // the right answer on any browser that runs us outside a job.
    }
    gui_command(&program).spawn().is_ok()
}

/// Launch hydra-gui minimized: the capture dialog is the only surface that
/// should appear. On macOS the app bundle comes first — it carries the TCC
/// identity the user granted folder access to; a raw sibling binary would
/// hit EACCES on ~/Downloads. `open -ga` exits non-zero when the app is not
/// installed, so its exit status (not spawn success) is the real signal.
fn launch_gui() {
    if let Some(p) = std::env::var_os("HYDRA_GUI_BIN") {
        if spawn_direct(p) {
            return;
        }
    }
    #[cfg(target_os = "macos")]
    {
        let ok = std::process::Command::new("open")
            .args(["-ga", "Hydra Download Manager", "--args", "--minimized"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return;
        }
    }
    // Dev layout: hydra-host sits next to hydra-gui in target/release.
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let sibling = dir.join(if cfg!(windows) {
                "hydra-gui.exe"
            } else {
                "hydra-gui"
            });
            if sibling.exists() && spawn_direct(sibling.into()) {
                return;
            }
        }
    }
    spawn_direct("hydra-gui".into());
}

/// Connect, launching the GUI and polling if needed.
fn connect(launch: bool) -> Option<(TcpStream, String)> {
    if let Some(c) = connect_once() {
        return Some(c);
    }
    if !launch {
        return None;
    }
    launch_gui();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(300));
        if let Some(c) = connect_once() {
            return Some(c);
        }
    }
    None
}

/// One native-messaging frame from the browser. None on clean EOF.
fn read_frame(stdin: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    stdin.read_exact(&mut len).ok()?;
    let len = u32::from_le_bytes(len) as usize;
    // Chrome caps extension->host messages well below this; anything larger
    // is framing corruption, and exiting lets the browser respawn us.
    if len == 0 || len > 64 * 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; len];
    stdin.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn write_frame(stdout: &mut impl Write, payload: &[u8]) {
    let _ = stdout.write_all(&(payload.len() as u32).to_le_bytes());
    let _ = stdout.write_all(payload);
    let _ = stdout.flush();
}

fn error_reply(msg: &str) -> Vec<u8> {
    format!("{{\"ok\":false,\"error\":\"{msg}\"}}").into_bytes()
}

/// Send one request over an established GUI connection, returning the reply
/// line. Any IO failure returns None so the caller can reconnect once.
fn round_trip(conn: &mut (TcpStream, String), req: &mut serde_json::Value) -> Option<String> {
    req["token"] = serde_json::Value::String(conn.1.clone());
    let mut line = serde_json::to_string(req).ok()?;
    line.push('\n');
    conn.0.write_all(line.as_bytes()).ok()?;
    let mut reader = BufReader::new(conn.0.try_clone().ok()?);
    let mut reply = String::new();
    reader.read_line(&mut reply).ok()?;
    if reply.trim().is_empty() {
        return None;
    }
    Some(reply)
}

fn main() {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    // One GUI connection kept across frames: connectNative ports send many
    // requests through a single host process.
    let mut conn: Option<(TcpStream, String)> = None;

    while let Some(frame) = read_frame(&mut stdin) {
        let mut req: serde_json::Value = match serde_json::from_slice(&frame) {
            Ok(serde_json::Value::Object(o)) => serde_json::Value::Object(o),
            _ => {
                write_frame(&mut stdout, &error_reply("bad json"));
                continue;
            }
        };
        // Pings probe state; they must not boot the app. Everything else
        // (a capture the browser already cancelled!) must reach a GUI.
        let launch = req.get("type").and_then(|t| t.as_str()) != Some("ping");

        let mut reply = None;
        for attempt in 0..2 {
            if conn.is_none() {
                conn = connect(launch && attempt == 0);
            }
            let Some(c) = conn.as_mut() else { break };
            match round_trip(c, &mut req) {
                Some(r) => {
                    reply = Some(r);
                    break;
                }
                // Stale connection (GUI restarted): drop and retry fresh.
                None => conn = None,
            }
        }
        match reply {
            Some(r) => write_frame(&mut stdout, r.trim().as_bytes()),
            None => write_frame(&mut stdout, &error_reply("hydra is not running")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// ipc.json outlives the instance that wrote it, so the file alone is
    /// never the answer to "is hydra running": the port has to accept a
    /// connection, or some unrelated process that inherited the number
    /// would be handed the user's downloads.
    ///
    /// Sets HOME/APPDATA, which `app_dir` reads and nothing else in this
    /// binary does.
    #[test]
    fn a_stale_ipc_file_is_not_mistaken_for_a_running_app() {
        let home = std::env::temp_dir().join(format!("hydra-host-{}", std::process::id()));
        let cfg = home.join(".config");
        std::fs::create_dir_all(cfg.join("hydra")).expect("test app dir");
        std::env::set_var("HOME", &home);
        std::env::set_var("APPDATA", &cfg);
        let publish = |port: u16| {
            std::fs::write(
                app_dir().join("ipc.json"),
                format!(r#"{{"port":{port},"token":"s3cret"}}"#),
            )
            .expect("write ipc.json");
        };

        // A port nobody is listening on any more: the file is what a
        // crashed instance leaves behind.
        let dead = {
            let l = TcpListener::bind(("127.0.0.1", 0)).expect("free port");
            l.local_addr().expect("address").port()
        };
        publish(dead);
        assert!(
            connect(false).is_none(),
            "a stale file must not read as a running app"
        );

        let live = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
        publish(live.local_addr().expect("address").port());
        let (_stream, token) = connect(false).expect("a listening instance is reachable");
        assert_eq!(token, "s3cret", "the token travels with the connection");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The framing is the whole contract with the browser: four little-endian
    /// bytes, then exactly that many bytes of JSON.
    #[test]
    fn a_frame_survives_the_round_trip() {
        let payload = br#"{"type":"download","url":"https://example.invalid/f.zip"}"#;
        let mut wire = Vec::new();
        write_frame(&mut wire, payload);

        assert_eq!(&wire[..4], &(payload.len() as u32).to_le_bytes());
        assert_eq!(read_frame(&mut &wire[..]).as_deref(), Some(&payload[..]));
    }

    /// Anything the browser could not have sent ends the session instead of
    /// being guessed at — the browser then respawns us with a clean stream.
    #[test]
    fn a_frame_the_browser_could_not_have_sent_is_refused() {
        let framed = |len: u32, body: &[u8]| {
            let mut v = len.to_le_bytes().to_vec();
            v.extend_from_slice(body);
            v
        };

        assert_eq!(read_frame(&mut &[][..]), None, "clean EOF");
        assert_eq!(read_frame(&mut &[0u8, 1][..]), None, "half a length");
        assert_eq!(read_frame(&mut &framed(0, b"")[..]), None, "empty message");
        assert_eq!(
            read_frame(&mut &framed(64 * 1024 * 1024 + 1, b"")[..]),
            None,
            "a length no legitimate message has"
        );
        assert_eq!(
            read_frame(&mut &framed(8, b"short")[..]),
            None,
            "body shorter than its own length"
        );
    }

    /// ipc.json is the only thing standing between us and connecting to a
    /// port some unrelated process now owns, so a file that does not name a
    /// real endpoint has to read as "no instance".
    #[test]
    fn ipc_json_answers_only_when_it_names_a_real_endpoint() {
        assert_eq!(
            parse_ipc(r#"{"port":50726,"token":"cafe","ws_port":6799,"pid":42}"#),
            Some((50726, "cafe".to_string()))
        );
        for bad in [
            r#"{"port":50726}"#,                  // no token
            r#"{"token":"cafe"}"#,                // no port
            r#"{"port":"50726","token":"cafe"}"#, // port as a string
            r#"{"port":70000,"token":"cafe"}"#,   // beyond a port number
            r#"{"port":50726,"token":7}"#,        // token as a number
            "{\"port\":50726,",                   // half-written file
            "",
        ] {
            assert_eq!(parse_ipc(bad), None, "should be unusable: {bad}");
        }
    }

    /// The token lives in a file only this user can read; the browser never
    /// sees it and never sends it. Stamping it on is this process's whole
    /// reason for existing.
    #[test]
    fn a_request_is_stamped_with_the_token_the_browser_never_sees() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
        let addr = listener.local_addr().expect("listener address");
        let gui = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the host connects");
            let mut out = stream.try_clone().expect("write half");
            let mut line = String::new();
            BufReader::new(stream)
                .read_line(&mut line)
                .expect("one request line");
            writeln!(out, r#"{{"ok":true,"capture":true}}"#).expect("reply");
            line
        });

        let mut conn = (
            TcpStream::connect(addr).expect("connect to the fake gui"),
            "s3cret".to_string(),
        );
        let mut req =
            serde_json::json!({"type": "download", "url": "https://example.invalid/f.zip"});
        assert!(req.get("token").is_none(), "the browser sends no token");

        let reply = round_trip(&mut conn, &mut req).expect("a reply line");
        assert_eq!(reply.trim(), r#"{"ok":true,"capture":true}"#);

        let on_the_wire: serde_json::Value =
            serde_json::from_str(&gui.join().expect("gui thread")).expect("json request");
        assert_eq!(on_the_wire["token"], "s3cret");
        assert_eq!(on_the_wire["url"], "https://example.invalid/f.zip");
    }

    /// The browser gets a JSON object even when there is nothing to talk to;
    /// the extension reads `ok` off it and keeps its own download.
    #[test]
    fn an_unreachable_app_still_answers_in_json() {
        let reply: serde_json::Value =
            serde_json::from_slice(&error_reply("hydra is not running")).expect("json");
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"], "hydra is not running");
    }

    /// `spawn_direct` reports whether a process actually started — that is
    /// what `launch_gui` walks its candidate paths on. The current test
    /// binary stands in for the GUI: it exists on every platform this
    /// builds for, and exits immediately on an argument it has no test for.
    #[test]
    fn a_gui_launch_reports_whether_the_process_started() {
        let me = std::env::current_exe().expect("test binary path");
        assert!(spawn_direct(me.into_os_string()), "a real program starts");
        assert!(
            !spawn_direct(
                std::env::temp_dir()
                    .join("hydra-gui-that-is-not-here")
                    .into_os_string()
            ),
            "a missing program does not"
        );
    }
}
