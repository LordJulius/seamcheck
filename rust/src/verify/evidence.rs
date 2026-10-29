//! Where a run's evidence goes. Screenshots and trees hold whatever the app
//! showed (places, searches, mail), so by default they live outside the
//! repository, under `$XDG_CACHE_HOME/seamcheck/verify/<repo>/<target>/`.
//! `[evidence] dir` may point elsewhere; a dir inside the work tree is
//! refused unless git ignores it. Verify never deletes evidence.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::adb::expand;
use super::config::Target;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

/// The directory a target's runs go under (not yet created).
pub fn base(root: &Path, t: &Target) -> Result<PathBuf, String> {
    let configured = expand(&t.str("evidence", "dir"));
    if configured.is_empty() {
        let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".cache"));
        let repo = repo_name(root);
        return Ok(cache.join("seamcheck").join("verify").join(repo).join(&t.name));
    }
    let p = tilde(&configured);
    let p = if p.is_absolute() { p } else { root.join(p) };
    let p = std::path::absolute(&p).unwrap_or(p);
    let p = p.join(&t.name);
    refuse_tracked(root, &p)?;
    Ok(p)
}

/// The repository's name: the directory holding its main `.git` (so every worktree of one repo shares it).
fn repo_name(root: &Path) -> String {
    let common = Command::new("git").args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).current_dir(root).output();
    let from_git = common
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .and_then(|p| if p.ends_with(".git") { p.parent().map(Path::to_path_buf) } else { Some(p) })
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    from_git.unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into()))
}

/// A dir inside the work tree must be ignored by git, or evidence could be committed.
pub fn refuse_tracked(root: &Path, dir: &Path) -> Result<(), String> {
    let top = Command::new("git").args(["rev-parse", "--show-toplevel"]).current_dir(root).output();
    let Ok(top) = top else { return Ok(()) };
    if !top.status.success() {
        return Ok(()); // not a git work tree: nothing could commit it
    }
    let top = PathBuf::from(String::from_utf8_lossy(&top.stdout).trim());
    let top = std::fs::canonicalize(&top).unwrap_or(top);
    // the nearest existing ancestor decides where the dir really is
    let mut probe = dir.to_path_buf();
    let mut tail = PathBuf::new();
    while !probe.exists() {
        let Some(name) = probe.file_name().map(|n| n.to_os_string()) else { break };
        tail = PathBuf::from(name).join(tail);
        probe = match probe.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
    }
    let real = std::fs::canonicalize(&probe).unwrap_or(probe).join(tail);
    let Ok(rel) = real.strip_prefix(&top) else { return Ok(()) };
    let rel = rel.to_string_lossy().into_owned();
    let ignored = Command::new("git")
        .args(["check-ignore", "-q", "--no-index", &format!("{rel}/evidence")])
        .current_dir(&top)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ignored {
        Ok(())
    } else {
        Err(format!(
            "[evidence] dir {} is inside the work tree and not ignored by git: screenshots and trees hold what the app showed. \
             Leave dir unset (evidence goes to $XDG_CACHE_HOME/seamcheck/verify/) or add it to .gitignore",
            real.display()
        ))
    }
}

/// `YYYYMMDD-HHMMSS` and an ISO time, UTC.
pub fn now() -> (String, String) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    let (hh, mm, ss) = (rem / 3600, rem % 3600 / 60, rem % 60);
    (format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}"), format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z"))
}
