//! `hydra compat-link`: install the `wget`/`curl` dialect entry points.
//!
//! The compatibility layer picks its dialect from `argv[0]` (see [`crate::compat`]),
//! so a link named `wget` or `curl` pointing at this binary is all it takes. The
//! help text used to say exactly that — `ln -s hydra curl` — and that command is
//! true but not sufficient, which made it read as broken:
//!
//! * `ln -s hydra curl` creates the link in the CURRENT directory. Unless that
//!   directory is on `$PATH`, typing `curl` still runs `/usr/bin/curl`; only
//!   `./curl` reaches hydra.
//! * Even in a `$PATH` directory, the real `curl`/`wget` usually already exists
//!   in another one. Whichever directory comes FIRST in `$PATH` wins, so a link
//!   in `~/.local/bin` is invisible when `/usr/bin` precedes it.
//! * `ln -s` refuses to overwrite an existing file, so on a machine that already
//!   has wget installed the command fails with `File exists`.
//!
//! None of those are failures of the dialect mechanism, and none of them are
//! visible from the error the user gets (which is usually no error at all —
//! just the other tool's output). This module does the link placement and then
//! answers the question the user actually has: *will typing `curl` now reach
//! hydra?* — by resolving the name against `$PATH` the same way the shell does.

use std::path::{Path, PathBuf};

/// The two names the dialect layer recognises without a prefix.
pub const DEFAULT_NAMES: &[&str] = &["wget", "curl"];

/// What creating one link would do, or did.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing is there; the link will be created.
    Create,
    /// A link with this name already points at this binary.
    AlreadyLinked,
    /// Something else occupies the path. Replaced only under `--force`.
    Occupied(String),
}

/// One planned entry point.
#[derive(Debug)]
pub struct LinkPlan {
    pub name: String,
    pub path: PathBuf,
    pub action: Action,
    /// What `$PATH` says will actually run when this name is typed, once the
    /// link exists. `None` when the link wins.
    pub shadowed_by: Option<PathBuf>,
}

/// This binary, resolved through any symlink used to invoke it.
///
/// Canonicalised deliberately: invoked as `curl`, `current_exe()` may report the
/// link, and a link-to-a-link is harder to reason about than a link to the real
/// file.
pub fn this_exe() -> Result<PathBuf, String> {
    let raw = std::env::current_exe().map_err(|e| format!("cannot locate this binary: {e}"))?;
    Ok(std::fs::canonicalize(&raw).unwrap_or(raw))
}

/// Whether `p` is a link (direct or chained) that ends at `exe`.
fn points_at(p: &Path, exe: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(p)
            .map(|resolved| resolved == exe)
            .unwrap_or(false),
        _ => false,
    }
}

/// Describe what already sits at `p`, for the "occupied" message.
fn describe(p: &Path) -> String {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(p) {
            Ok(t) => format!("symlink to {}", t.display()),
            Err(_) => "symlink".to_string(),
        },
        Ok(m) if m.is_dir() => "directory".to_string(),
        Ok(_) => "regular file".to_string(),
        Err(e) => format!("unreadable ({e})"),
    }
}

/// The `$PATH` entries, in the order the shell searches them.
fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// The first `$PATH` entry that provides `name`, i.e. what the shell will run.
///
/// Only existence is checked, not the execute bit: a non-executable file of the
/// right name is a different problem, and reporting the path is what helps here.
fn first_on_path(name: &str) -> Option<PathBuf> {
    path_dirs()
        .into_iter()
        .map(|d| d.join(name))
        .find(|c| c.exists())
}

/// Plan one link per name in `dir` (default: this binary's own directory).
pub fn plan(dir: Option<&Path>, names: &[String]) -> Result<(PathBuf, Vec<LinkPlan>), String> {
    let exe = this_exe()?;
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => exe
            .parent()
            .ok_or_else(|| "this binary has no parent directory".to_string())?
            .to_path_buf(),
    };
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);

    let mut plans = Vec::new();
    for name in names {
        let file = link_file_name(name);
        let path = dir.join(&file);
        let action = if points_at(&path, &exe) {
            Action::AlreadyLinked
        } else if std::fs::symlink_metadata(&path).is_ok() {
            Action::Occupied(describe(&path))
        } else {
            Action::Create
        };
        // What the shell resolves this name to, ignoring the link we are about
        // to write: another directory earlier in $PATH silently wins.
        let shadowed_by = match first_on_path(&file) {
            Some(found) if found != path => Some(found),
            Some(_) => None,
            // Not found anywhere means our directory is not on $PATH either.
            None => Some(PathBuf::from("<not on $PATH>")),
        };
        plans.push(LinkPlan {
            name: name.clone(),
            path,
            action,
            shadowed_by,
        });
    }
    Ok((exe, plans))
}

/// `wget` on unix, `wget.exe` on Windows — the shell only searches for the
/// suffixed form there.
fn link_file_name(name: &str) -> String {
    if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Create (or replace, under `force`) the link described by `p`.
///
/// Unix gets a symlink. Windows needs Developer Mode or elevation to create
/// one, so it gets a copy instead: the dialect layer only reads `argv[0]`, and
/// a copy carries the same name.
pub fn apply(p: &LinkPlan, exe: &Path, force: bool) -> Result<(), String> {
    match &p.action {
        Action::AlreadyLinked => return Ok(()),
        Action::Occupied(what) if !force => {
            return Err(format!(
                "{} already exists ({what}); pass --force to replace it, or pick another \
                 directory with --dir",
                p.path.display()
            ))
        }
        Action::Occupied(_) => {
            std::fs::remove_file(&p.path)
                .map_err(|e| format!("could not remove {}: {e}", p.path.display()))?;
        }
        Action::Create => {}
    }

    // A relative target keeps working if the whole directory is moved, which is
    // how package managers stage installs.
    let target: PathBuf = match (exe.parent(), exe.file_name()) {
        (Some(parent), Some(file)) if Some(parent) == p.path.parent() => PathBuf::from(file),
        _ => exe.to_path_buf(),
    };

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &p.path)
            .map_err(|e| format!("could not link {}: {e}", p.path.display()))
    }
    #[cfg(windows)]
    {
        let _ = &target;
        std::fs::copy(exe, &p.path)
            .map(|_| ())
            .map_err(|e| format!("could not copy to {}: {e}", p.path.display()))
    }
}

/// The advice line for a name another `$PATH` entry will keep answering.
pub fn shadow_note(p: &LinkPlan) -> Option<String> {
    let found = p.shadowed_by.as_ref()?;
    let dir = p.path.parent().map(|d| d.display().to_string())?;
    if found == Path::new("<not on $PATH>") {
        Some(format!(
            "typing `{}` will NOT reach hydra: {dir} is not on $PATH. Add it, or run \
             `{}` by path.",
            p.name,
            p.path.display()
        ))
    } else {
        Some(format!(
            "typing `{}` will still run {}: it comes earlier in $PATH than {dir}. Put \
             {dir} first in $PATH, or invoke {} by path.",
            p.name,
            found.display(),
            p.path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_names_are_the_ones_the_dialect_layer_detects() {
        for n in DEFAULT_NAMES {
            let p = crate::compat::detect(n, &[]);
            assert_ne!(
                p,
                crate::compat::Personality::Native,
                "{n} must select a dialect, otherwise linking it is pointless"
            );
        }
    }

    #[test]
    fn windows_names_carry_the_exe_suffix() {
        let got = link_file_name("curl");
        if cfg!(windows) {
            assert_eq!(got, "curl.exe");
        } else {
            assert_eq!(got, "curl");
        }
    }

    /// The regression this subcommand exists for: a link in a directory that is
    /// not on `$PATH`, or one shadowed by the real tool, must be REPORTED. A
    /// silent success there is what made `ln -s hydra curl` look broken.
    #[test]
    fn a_shadowed_link_is_reported_not_silently_accepted() {
        let p = LinkPlan {
            name: "curl".into(),
            path: PathBuf::from("/opt/hydra/bin/curl"),
            action: Action::Create,
            shadowed_by: Some(PathBuf::from("/usr/bin/curl")),
        };
        let note = shadow_note(&p).expect("a shadowed link must produce a note");
        assert!(note.contains("/usr/bin/curl"));

        let clear = LinkPlan {
            shadowed_by: None,
            ..p
        };
        assert!(shadow_note(&clear).is_none());
    }

    #[test]
    fn linking_into_a_temp_dir_creates_a_working_symlink() {
        let exe = this_exe().expect("test binary path");
        let dir = std::env::temp_dir().join(format!("hydra-compat-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let names: Vec<String> = DEFAULT_NAMES.iter().map(|s| s.to_string()).collect();
        let (exe_planned, plans) = plan(Some(&dir), &names).expect("plan");
        assert_eq!(exe_planned, exe);
        assert_eq!(plans.len(), 2);
        for p in &plans {
            assert_eq!(p.action, Action::Create);
            apply(p, &exe, false).expect("apply");
            assert!(points_at(&p.path, &exe), "link must resolve to this binary");
        }

        // Re-planning the same directory sees them as already ours, not as
        // occupied: running the command twice must not need --force.
        let (_, again) = plan(Some(&dir), &names).expect("replan");
        for p in &again {
            assert_eq!(p.action, Action::AlreadyLinked);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
