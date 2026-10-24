//! `kind = "command"`: a project's own check script, run from the root
//! (core._command), and the shared runner with Python's subprocess manners.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{RuleErr, Res};
use crate::config::{self, file_of, id, list_or, s, Rule};
use crate::glob::match_any;
use crate::project::{Finding, Project};
use crate::pyre::Rx;

pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `subprocess.run(cmd, shell=True, cwd=cwd, capture_output=True, text=True,
/// timeout=t)`: None on a timeout.
pub fn run_shell(cmd: &str, cwd: &std::path::Path, timeout: f64) -> Option<Ran> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let t2 = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if start.elapsed().as_secs_f64() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    };
    let norm = |b: Vec<u8>| {
        let s = String::from_utf8_lossy(&b).into_owned();
        if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s }
    };
    let stdout = norm(t1.join().unwrap_or_default());
    let stderr = norm(t2.join().unwrap_or_default());
    use std::os::unix::process::ExitStatusExt;
    let code = status.code().unwrap_or_else(|| -status.signal().unwrap_or(1));
    Some(Ran { code, stdout, stderr })
}

/// Python `str.splitlines()`.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b: Vec<(usize, char)> = s.char_indices().collect();
    let mut i = 0;
    while i < b.len() {
        let (pos, c) = b[i];
        let brk = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if brk {
            out.push(&s[start..pos]);
            let mut next = pos + c.len_utf8();
            if c == '\r' && i + 1 < b.len() && b[i + 1].1 == '\n' {
                next += 1;
                i += 1;
            }
            start = next;
        }
        i += 1;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn env_missing() -> &'static Rx {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Rx::lit(
            r"ERR_MODULE_NOT_FOUND|Cannot find (?:module|package) '[^']+'|command not found|No such file or directory: '?node_modules|\(Mix\) Could not find|could not compile dependency",
        )
    })
}

pub fn py_num(v: Option<&serde_json::Value>, d: f64) -> (f64, String) {
    match v {
        Some(serde_json::Value::Number(n)) => {
            (n.as_f64().unwrap_or(d), if n.is_f64() { format_float(n.as_f64().unwrap()) } else { n.to_string() })
        }
        _ => (d, format!("{}", d as i64)),
    }
}

fn format_float(f: f64) -> String {
    let s = format!("{f}");
    if s.contains('.') || s.contains('e') { s } else { format!("{s}.0") }
}

pub fn command(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let rid = id(r);
    if let Some(changed) = &p.gate_changed {
        let when = list_or(r, "when", &[]);
        if !when.is_empty() && !changed.iter().any(|f| match_any(f, &when)) {
            return Ok(vec![]);
        }
    }
    let run = super::need_str(r, "run")?;
    let (timeout, tshow) = py_num(r.get("timeout"), 300.0);
    let Some(res) = run_shell(run, &p.root, timeout) else {
        return Ok(vec![Finding::new(rid, "error", "", 0, "", format!("`{run}` took longer than {tshow} s"))]);
    };
    if res.code == 0 {
        return Ok(vec![]);
    }
    let both = format!("{}\n{}", res.stdout, res.stderr);
    if res.code == 127 || env_missing().is_match(&both) {
        let why = env_missing().search(&both).map(|m| m.group0().to_string()).unwrap_or_else(|| "exit 127".into());
        return Err(RuleErr::Env(format!("`{run}` couldn't run: {why} (install this checkout's deps)")));
    }
    let rx = match s(r, "files") {
        Some(f) if !f.is_empty() => Some(super::compile(f)?),
        _ => None,
    };
    let prefix = s(r, "prefix").unwrap_or("");
    let sev = config::severity(r);
    let fix = s(r, "fix").filter(|f| !f.is_empty());
    let mut out = Vec::new();
    for line in splitlines(&both) {
        let line = crate::pyre::strip(line);
        if line.is_empty() || line.ends_with(':') {
            continue;
        }
        let rel = match rx.as_ref().and_then(|x| x.search(line)) {
            Some(m) => format!("{prefix}{}", if m.ngroups() > 0 { m.s(1) } else { m.group0() }),
            None => String::new(),
        };
        out.push(Finding::new(
            rid,
            &sev,
            &rel,
            0,
            "",
            format!("{line}{}", fix.map(|f| format!("  (fix: {f})")).unwrap_or_default()),
        ));
    }
    if out.is_empty() {
        out.push(Finding::new(rid, &sev, "", 0, "", format!("`{run}` exited {} with no output", res.code)));
    }
    let _ = file_of(r);
    Ok(out)
}
