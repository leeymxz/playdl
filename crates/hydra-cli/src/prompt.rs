//! What to do when the output file already exists.
//!
//! # Why this is a prompt and not a default
//!
//! There are four defensible behaviours when `hydra http://host/file.iso` finds
//! `file.iso` already present, and every one of them is wrong in some situation:
//! resuming corrupts the file if the remote object changed, restarting throws away a
//! 4 GB partial transfer, renaming leaves the user with `file.iso.1` they did not ask
//! for, and refusing is useless in a script.
//!
//! So an interactive terminal asks. A non-interactive one must not: a prompt with
//! nobody to answer it is a hang, which in a cron job or a CI step is worse than any
//! of the four choices. When stdin is not a terminal the decision falls back to the
//! flags, and the flags are always honoured without asking — an explicit `-c` or
//! `--no-clobber` is already an answer, and re-asking would be ignoring it.
//!
//! # Why resume is not offered unconditionally
//!
//! Resume is only sound when the remote object can be proven to be the same object:
//! same size, same strong validator. Offering "continue" for a file whose validator
//! has changed invites silent corruption, which is exactly the failure class this
//! project keeps finding in other tools. When resume is not sound the prompt says so
//! and does not offer it.

use std::fmt;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

/// What to do about an existing output file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existing {
    /// Continue from the bytes already on disk.
    Resume,
    /// Discard what is there and fetch the whole object.
    Restart,
    /// Write to a fresh name (`file.iso.1`, `file.iso.2`, ...).
    Rename,
    /// Do nothing and exit successfully.
    Skip,
    /// Check the existing file against the server without re-downloading it.
    Verify,
}

impl fmt::Display for Existing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Existing::Resume => "resume",
            Existing::Restart => "restart",
            Existing::Rename => "rename",
            Existing::Skip => "skip",
            Existing::Verify => "verify",
        })
    }
}

/// Why resume may not be on the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeOffer {
    /// Resume is sound: this many bytes are already held, proven by a sidecar record.
    Sound(u64),
    /// No sidecar, but the bytes on disk can be CHECKED against the server before
    /// being trusted: re-read a window ending at the current length and compare.
    ///
    /// This is what makes `hydra <url>` on a half-finished file useful. Refusing to
    /// resume just because hydra did not write the partial file is unhelpful — the
    /// bytes are probably a valid prefix (that is what every interrupted download
    /// leaves behind), and "probably" can be upgraded to "verified" with one small
    /// range request instead of re-fetching gigabytes.
    Verifiable(u64),
    /// The file is already the full size, so it may simply be finished.
    ///
    /// Restarting here is the wrong default: the common case is a completed download
    /// being re-run, and re-fetching the whole object to discover it was already there
    /// wastes the entire transfer.
    LooksComplete(u64),
    /// Resume is unsafe or impossible, with the reason to show the user.
    Refused(String),
}

/// The flags that can pre-empt the prompt.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    /// `-c` / `--continue`: resume without asking.
    pub resume: bool,
    /// `--no-clobber`: never touch an existing file.
    pub no_clobber: bool,
    /// `--force`: overwrite without asking.
    pub force: bool,
    /// `-q`/`--quiet` or a non-interactive run: never prompt.
    pub assume_default: bool,
}

/// Decide what to do, asking only when a terminal is there to answer.
///
/// `read_line` is injected so the decision logic is testable without a tty; the
/// caller passes a real stdin reader.
pub fn decide<R: BufRead, W: Write>(
    path: &Path,
    on_disk: u64,
    remote_size: u64,
    offer: &ResumeOffer,
    flags: Flags,
    interactive: bool,
    input: &mut R,
    out: &mut W,
) -> io::Result<Existing> {
    // Flags are answers. Honour them without asking, in the order a user would
    // expect: refuse-to-touch beats resume beats force.
    if flags.no_clobber {
        return Ok(Existing::Skip);
    }
    if flags.resume {
        return Ok(match offer {
            // A verifiable file resumes under -c too: the verification step is what
            // makes it safe, so there is nothing left to ask about.
            ResumeOffer::Sound(_) | ResumeOffer::Verifiable(_) => Existing::Resume,
            ResumeOffer::LooksComplete(_) => Existing::Verify,
            // -c on a file that genuinely cannot be resumed: restarting is a safe fallback,
            // and it is what the user asked for in spirit — they want the file.
            ResumeOffer::Refused(_) => Existing::Restart,
        });
    }
    if flags.force {
        return Ok(Existing::Restart);
    }

    if !interactive || flags.assume_default {
        // No terminal. A piped answer is still an answer, though — `echo c | hydra ...`
        // and an expect-style driver both arrive this way, and refusing to read them
        // makes the prompt untestable and unscriptable. So try one non-blocking read of
        // whatever is queued; only fall back when there is genuinely nothing.
        let mut line = String::new();
        if !flags.assume_default && input.read_line(&mut line)? > 0 {
            if let Some(c) = parse_answer(line.trim(), offer) {
                return Ok(c);
            }
        }
        // Nothing to read. Choose the option that cannot destroy data: keep the
        // existing file and write beside it (avoiding accidental overwrite or
        // unbounded file loss).
        return Ok(Existing::Rename);
    }

    let name = path.display();
    writeln!(
        out,
        "hydra: {name} already exists ({} on disk, remote object is {}).",
        crate::progress::human(on_disk),
        crate::progress::human(remote_size)
    )?;
    match offer {
        ResumeOffer::Sound(held) => {
            writeln!(
                out,
                "  [c] continue from {}   [r] restart from zero   [n] save as a new name   [s] skip",
                crate::progress::human(*held)
            )?;
        }
        ResumeOffer::Verifiable(held) => {
            writeln!(
                out,
                "  no partial-transfer record, so the bytes on disk will be checked \
                 against the server first"
            )?;
            writeln!(
                out,
                "  [c] continue from {} (verify first)   [r] restart from zero   \
                 [n] save as a new name   [s] skip",
                crate::progress::human(*held)
            )?;
        }
        ResumeOffer::LooksComplete(held) => {
            writeln!(
                out,
                "  the sizes match, so this download may already be finished"
            )?;
            writeln!(
                out,
                "  [v] verify it against the server   [r] download again from zero   \
                 [n] save as a new name   [s] keep it as-is",
            )?;
            let _ = held;
        }
        ResumeOffer::Refused(why) => {
            writeln!(out, "  cannot continue: {why}")?;
            writeln!(
                out,
                "  [r] restart from zero   [n] save as a new name   [s] skip"
            )?;
        }
    }

    loop {
        write!(out, "hydra: what would you like to do? ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            // EOF mid-prompt (piped input that ran out, or ^D). Same reasoning as the
            // non-interactive case: do not destroy anything.
            writeln!(out, "\n(no answer; writing to a new name)")?;
            return Ok(Existing::Rename);
        }
        let ans = line.trim().to_ascii_lowercase();
        let choice = parse_answer(&ans, offer);
        match choice {
            Some(c) => return Ok(c),
            // parse_answer rejects "continue" when the offer forbids it, so an
            // unusable answer and an unknown one land here together. Distinguish them:
            // "c" on an unresumable file deserves the reason, not "did not understand".
            None if matches!(ans.as_str(), "c" | "continue" | "y" | "yes") => {
                if let ResumeOffer::Refused(why) = offer {
                    writeln!(out, "  cannot continue: {why} — pick r, n, or s")?;
                }
            }
            None => {
                writeln!(out, "  did not understand {ans:?}")?;
            }
        }
    }
}

/// Map a typed answer to an action, honouring which options this offer allows.
fn parse_answer(ans: &str, offer: &ResumeOffer) -> Option<Existing> {
    let a = ans.to_ascii_lowercase();
    let want = match a.as_str() {
        "c" | "continue" | "y" | "yes" => Existing::Resume,
        "r" | "restart" | "o" | "overwrite" => Existing::Restart,
        "n" | "new" | "rename" => Existing::Rename,
        "v" | "verify" => Existing::Verify,
        // A bare Enter is the safe choice, not a destructive one: someone holding the
        // key down must not lose a partial download.
        "s" | "skip" | "q" | "quit" | "" => Existing::Skip,
        _ => return None,
    };
    Some(match (want, offer) {
        // "continue" on a full-size file means "check it", not "fetch it again".
        (Existing::Resume, ResumeOffer::LooksComplete(_)) => Existing::Verify,
        (Existing::Resume, ResumeOffer::Refused(_)) => return None,
        (w, _) => w,
    })
}

/// Is stdin a terminal that can answer a question?
pub fn stdin_is_interactive() -> bool {
    io::stdin().is_terminal()
}

/// Prompt on the real stdin/stderr.
///
/// Written to stderr, not stdout: stdout may be the payload itself (`--stdout`), and a
/// prompt mixed into a downloaded file would corrupt it.
pub fn ask(
    path: &Path,
    on_disk: u64,
    remote_size: u64,
    offer: &ResumeOffer,
    flags: Flags,
) -> io::Result<Existing> {
    let stdin = io::stdin();
    let mut lock = stdin.lock();
    let mut err = io::stderr();
    decide(
        path,
        on_disk,
        remote_size,
        offer,
        flags,
        stdin_is_interactive(),
        &mut lock,
        &mut err,
    )
}

/// First free name in the `path`, `path.1`, `path.2`, ... series.
///
/// Numeric suffix collision avoidance convention. Bounded rather than looping forever: a directory with a million
/// collisions is a bug somewhere else, and spinning on `stat` is not a useful response
/// to it.
pub fn next_free_name(path: &Path) -> Option<std::path::PathBuf> {
    if !path.exists() {
        return Some(path.to_path_buf());
    }
    let s = path.to_string_lossy();
    (1..=9999)
        .map(|i| std::path::PathBuf::from(format!("{s}.{i}")))
        .find(|c| !c.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sound() -> ResumeOffer {
        ResumeOffer::Sound(1024)
    }

    fn run(input: &str, flags: Flags, interactive: bool, offer: &ResumeOffer) -> Existing {
        let mut r = Cursor::new(input.as_bytes().to_vec());
        let mut w = Vec::new();
        decide(
            Path::new("/tmp/file.iso"),
            1024,
            4096,
            offer,
            flags,
            interactive,
            &mut r,
            &mut w,
        )
        .unwrap()
    }

    #[test]
    fn answers_map_to_actions() {
        for (ans, want) in [
            ("c\n", Existing::Resume),
            ("continue\n", Existing::Resume),
            ("y\n", Existing::Resume),
            ("r\n", Existing::Restart),
            ("overwrite\n", Existing::Restart),
            ("n\n", Existing::Rename),
            ("s\n", Existing::Skip),
            ("q\n", Existing::Skip),
        ] {
            assert_eq!(
                run(ans, Flags::default(), true, &sound()),
                want,
                "answer {ans:?}"
            );
        }
    }

    #[test]
    fn a_bare_newline_is_the_safe_choice_not_a_destructive_one() {
        // Someone holding down Enter must not lose a partial download.
        assert_eq!(run("\n", Flags::default(), true, &sound()), Existing::Skip);
    }

    #[test]
    fn unrecognised_answers_reprompt_rather_than_guessing() {
        let mut r = Cursor::new(b"maybe\nwhat\nr\n".to_vec());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f"),
            1,
            2,
            &sound(),
            Flags::default(),
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Restart);
        let shown = String::from_utf8(w).unwrap();
        assert_eq!(
            shown.matches("did not understand").count(),
            2,
            "each bad answer should be reported once"
        );
    }

    #[test]
    fn resume_is_not_offered_when_it_would_be_unsafe() {
        let refused = ResumeOffer::Refused("validator changed".into());
        let mut r = Cursor::new(b"c\nr\n".to_vec());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f"),
            1,
            2,
            &refused,
            Flags::default(),
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Restart, "the 'c' answer must be rejected");
        let shown = String::from_utf8(w).unwrap();
        assert!(
            shown.contains("validator changed"),
            "the reason must be shown: {shown}"
        );
        assert!(
            !shown.contains("[c] continue"),
            "an unsafe option must not be on the menu"
        );
    }

    #[test]
    fn a_non_interactive_run_never_prompts_and_never_destroys() {
        // The hang risk is the point: a prompt in cron would block forever.
        let mut r = Cursor::new(Vec::new());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f"),
            1,
            2,
            &sound(),
            Flags::default(),
            false,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Rename);
        assert!(w.is_empty(), "nothing should be printed when not asking");
    }

    #[test]
    fn eof_mid_prompt_does_not_destroy_data() {
        assert_eq!(run("", Flags::default(), true, &sound()), Existing::Rename);
    }

    #[test]
    fn flags_are_answers_and_are_not_re_asked() {
        let f = |f: Flags| {
            let mut r = Cursor::new(b"c\n".to_vec());
            let mut w = Vec::new();
            let got = decide(Path::new("/tmp/f"), 1, 2, &sound(), f, true, &mut r, &mut w).unwrap();
            (got, w.is_empty())
        };
        assert_eq!(
            f(Flags {
                resume: true,
                ..Default::default()
            }),
            (Existing::Resume, true),
            "-c means resume, without a question"
        );
        assert_eq!(
            f(Flags {
                no_clobber: true,
                ..Default::default()
            }),
            (Existing::Skip, true),
            "--no-clobber means do not touch it"
        );
        assert_eq!(
            f(Flags {
                force: true,
                ..Default::default()
            }),
            (Existing::Restart, true),
            "--force means overwrite"
        );
    }

    #[test]
    fn no_clobber_wins_over_resume() {
        // Contradictory flags need a defined order; refusing to touch the file is the
        // conservative reading.
        let mut r = Cursor::new(Vec::new());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f"),
            1,
            2,
            &sound(),
            Flags {
                resume: true,
                no_clobber: true,
                ..Default::default()
            },
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Skip);
    }

    #[test]
    fn resume_flag_on_an_unresumable_file_restarts_rather_than_failing() {
        let mut r = Cursor::new(Vec::new());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f"),
            1,
            2,
            &ResumeOffer::Refused("size changed".into()),
            Flags {
                resume: true,
                ..Default::default()
            },
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Restart);
    }

    #[test]
    fn the_prompt_states_both_sizes_so_the_choice_is_informed() {
        let mut r = Cursor::new(b"s\n".to_vec());
        let mut w = Vec::new();
        decide(
            Path::new("/tmp/big.iso"),
            1_048_576,
            10_485_760,
            &ResumeOffer::Sound(1_048_576),
            Flags::default(),
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        let shown = String::from_utf8(w).unwrap();
        assert!(shown.contains("big.iso"), "name the file: {shown}");
        // human() uses three significant figures, so 1 MiB prints as "1.00 MiB".
        assert!(shown.contains("1.00 MiB"), "state what is on disk: {shown}");
        assert!(shown.contains("10.0 MiB"), "state the remote size: {shown}");
    }

    #[test]
    fn rename_finds_the_first_free_slot() {
        let dir = std::env::temp_dir().join(format!("hydra_rename_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("f.bin");
        assert_eq!(
            next_free_name(&base).unwrap(),
            base,
            "no collision, same name"
        );
        std::fs::write(&base, b"x").unwrap();
        assert_eq!(
            next_free_name(&base).unwrap(),
            dir.join("f.bin.1"),
            "numeric suffix collision avoidance convention"
        );
        std::fs::write(dir.join("f.bin.1"), b"x").unwrap();
        assert_eq!(next_free_name(&base).unwrap(), dir.join("f.bin.2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sidecar_less_partial_file_offers_verified_continue() {
        // The case that prompted this: an interrupted download from another tool. The
        // old behaviour refused to continue at all, which meant re-fetching everything.
        let mut r = Cursor::new(b"c\n".to_vec());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f.iso"),
            50_000_000,
            121_700_000,
            &ResumeOffer::Verifiable(50_000_000),
            Flags::default(),
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Resume);
        let shown = String::from_utf8(w).unwrap();
        assert!(
            shown.contains("checked against the server"),
            "the user must know the bytes get verified: {shown}"
        );
        assert!(
            shown.contains("[c] continue"),
            "continue must be offered: {shown}"
        );
    }

    #[test]
    fn a_full_size_file_offers_verification_not_a_silent_refetch() {
        // Re-running a finished download must not cost the whole object again.
        let mut r = Cursor::new(b"v\n".to_vec());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f.iso"),
            121_700_000,
            121_700_000,
            &ResumeOffer::LooksComplete(121_700_000),
            Flags::default(),
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Verify);
        let shown = String::from_utf8(w).unwrap();
        assert!(
            shown.contains("may already be finished"),
            "say why verification is the sensible choice: {shown}"
        );
    }

    #[test]
    fn continue_on_a_complete_file_becomes_verify() {
        // "c" on a full-size file has nothing to continue; verifying is the intent.
        let mut r = Cursor::new(b"c\n".to_vec());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f"),
            100,
            100,
            &ResumeOffer::LooksComplete(100),
            Flags::default(),
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Verify);
    }

    #[test]
    fn resume_flag_verifies_a_sidecar_less_file_rather_than_restarting() {
        let mut r = Cursor::new(Vec::new());
        let mut w = Vec::new();
        let got = decide(
            Path::new("/tmp/f"),
            500,
            1000,
            &ResumeOffer::Verifiable(500),
            Flags {
                resume: true,
                ..Default::default()
            },
            true,
            &mut r,
            &mut w,
        )
        .unwrap();
        assert_eq!(got, Existing::Resume, "-c must not throw away 500 bytes");
        assert!(w.is_empty(), "-c is already an answer");
    }
}
