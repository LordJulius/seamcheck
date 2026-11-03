//! `seamcheck trace | run | fuzz` (runcmd.py): feed a program's SEAMCHECK
//! lines to the runtime rules. Only the config is read, never the code, so
//! these start at once on any repo.
//!
//! `trace LOG...` judges finished runs' output (`-` is stdin); `run -- CMD`
//! runs a command and judges what it prints (stdout and stderr), killing its
//! whole process group at `--timeout` and two seconds after the command
//! itself exits (a grandchild holding the pipe never hangs the run); `fuzz
//! --seeds 1-8 -- CMD` runs it once per seed (`{seed}` in CMD, and
//! `$SEAMCHECK_SEED`), `--jobs` at a time, and prints the replay command of
//! every seed that broke a rule.

use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::pyfmt::{dump_str, float_repr};
use super::{format_violation, Checker, Violation};
use crate::config;

/// The flags these commands read.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub cmd: String,
    pub args: Vec<String>,
    pub config: Vec<String>,
    pub json: bool,
    pub strict: bool,
    pub log: Option<String>,
    pub echo: bool,
    pub timeout: f64,
    pub seeds: String,
    pub jobs: i64,
}

/// Python's text-mode line iteration: `\n`, `\r\n` and a lone `\r` each end
/// a line (universal newlines). Each line comes without its ending, and
/// whether it had one (a last line may not).
pub struct Lines<R: Read> {
    src: R,
    buf: Vec<u8>,
    eof: bool,
}

impl<R: Read> Lines<R> {
    pub fn new(src: R) -> Self {
        Lines { src, buf: Vec::new(), eof: false }
    }

    fn fill(&mut self) -> bool {
        let mut chunk = [0u8; 8192];
        loop {
            match self.src.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    return false;
                }
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.eof = true;
                    return false;
                }
            }
        }
    }
}

impl<R: Read> Iterator for Lines<R> {
    type Item = (String, bool);

    fn next(&mut self) -> Option<(String, bool)> {
        let mut from = 0;
        loop {
            if let Some(k) = self.buf[from..].iter().position(|b| *b == b'\n' || *b == b'\r') {
                let k = from + k;
                if self.buf[k] == b'\r' && k + 1 == self.buf.len() && !self.eof {
                    // a `\r` at the end of what came so far: `\r\n` or a lone `\r`?
                    from = k;
                    self.fill();
                    continue;
                }
                let skip = if self.buf[k] == b'\r' && self.buf.get(k + 1) == Some(&b'\n') { 2 } else { 1 };
                let line = String::from_utf8_lossy(&self.buf[..k]).into_owned();
                self.buf.drain(..k + skip);
                return Some((line, true));
            }
            from = self.buf.len();
            if self.eof || !self.fill() {
                if self.buf.is_empty() {
                    return None;
                }
                let line = String::from_utf8_lossy(&self.buf).into_owned();
                self.buf.clear();
                return Some((line, false));
            }
        }
    }
}

/// `json.dumps([v.__dict__ for v in vs], indent=1)`.
pub fn violations_json(vs: &[Violation]) -> String {
    if vs.is_empty() {
        return "[]".into();
    }
    let mut out = String::from("[\n");
    for (i, v) in vs.iter().enumerate() {
        out.push_str(" {\n");
        let s = |k: &str, x: &str, out: &mut String| {
            out.push_str(&format!("  \"{k}\": "));
            dump_str(x, out);
            out.push_str(",\n");
        };
        s("rule", &v.rule, &mut out);
        s("kind", &v.kind, &mut out);
        s("severity", &v.severity, &mut out);
        out.push_str(&format!("  \"t\": {},\n", float_repr(v.t)));
        s("subject", &v.subject, &mut out);
        s("message", &v.message, &mut out);
        out.push_str(&format!("  \"line\": {},\n  \"count\": {}\n }}", v.line, v.count));
        out.push_str(if i + 1 < vs.len() { ",\n" } else { "\n" });
    }
    out.push(']');
    out
}

fn report(chk: &Checker, o: &Opts, extra: &str) -> i32 {
    let vs = chk.finish();
    let errs = vs.iter().filter(|v| v.severity == "error").count();
    if o.json {
        println!("{}", violations_json(&vs));
    } else {
        for v in &vs {
            let why = chk.why(&v.rule).map(|w| format!("\n    why: {w}")).unwrap_or_default();
            println!("{}{why}", format_violation(v));
        }
        println!("\nseamcheck runtime: {}; {errs} errors, {} warnings{extra}", chk.summary(), vs.len() - errs);
    }
    (errs > 0) as i32
}

/// Python's `str(OSError)` of a failed start: `[Errno 2] No such file or directory: 'x'`.
fn os_error(e: &std::io::Error, file: &str) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let text = e.to_string();
            let desc = text.split(" (os error").next().unwrap_or(&text).to_string();
            format!("[Errno {n}] {desc}: {}", crate::pyre::repr(file))
        }
        None => e.to_string(),
    }
}

fn kill_group(pid: i32) {
    // SAFETY: killpg only sends a signal; a group that is gone is ESRCH, ignored.
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
}

/// One run of a command: its lines judged, its exit code (`-N` for a
/// signal, 127 when it won't start) and its seconds.
pub fn run_one(cmd: &[String], cfg: &Value, strict: bool, log: Option<&Path>, echo: bool, timeout: f64, seed: Option<&str>) -> (Checker, i32, f64) {
    let mut chk = Checker::new(cfg, strict);
    let t0 = Instant::now();
    let mut logf = log.and_then(|p| std::fs::File::create(p).ok());
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]).stdin(Stdio::inherit()).stdout(Stdio::piped());
    if let Some(s) = seed {
        c.env("SEAMCHECK_SEED", s);
    }
    // stderr into the same pipe (stderr=STDOUT), and its own session: a
    // timeout (or the end) takes the children it started too
    let (rd, wr) = match std::io::pipe() {
        Ok(p) => p,
        Err(e) => {
            println!("[error] can't start {}: {e}", crate::pyre::repr(&cmd[0]));
            return (chk, 127, 0.0);
        }
    };
    let wr2 = match wr.try_clone() {
        Ok(w) => w,
        Err(e) => {
            println!("[error] can't start {}: {e}", crate::pyre::repr(&cmd[0]));
            return (chk, 127, 0.0);
        }
    };
    c.stdout(wr).stderr(wr2);
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            println!("[error] can't start {}: {}", crate::pyre::repr(&cmd[0]), os_error(&e, &cmd[0]));
            return (chk, 127, 0.0);
        }
    };
    drop(c); // (our copies of the pipe's write end: EOF once the command's side closes)
    let pid = child.id() as i32;
    let (done_tx, done_rx) = mpsc::channel::<()>();
    if timeout > 0.0 {
        std::thread::spawn(move || {
            if done_rx.recv_timeout(Duration::from_secs_f64(timeout)).is_err() {
                kill_group(pid);
            }
        });
    }
    // the command's own exit ends the run even if something it left behind still holds the pipe
    let (code_tx, code_rx) = mpsc::channel::<i32>();
    std::thread::spawn(move || {
        let st = child.wait();
        let code = match st {
            Ok(s) => s.code().unwrap_or_else(|| -s.signal().unwrap_or(0)),
            Err(_) => -1,
        };
        let _ = code_tx.send(code);
        std::thread::sleep(Duration::from_secs(2));
        kill_group(pid);
    });
    let out = std::io::stdout();
    for (line, ended) in Lines::new(rd) {
        let nl = if ended { "\n" } else { "" };
        if let Some(f) = logf.as_mut() {
            let _ = write!(f, "{line}{nl}");
        }
        if echo {
            let mut o = out.lock();
            let _ = write!(o, "{line}{nl}");
            let _ = o.flush();
        }
        chk.feed_line(&line);
    }
    let code = code_rx.recv().unwrap_or(-1);
    let _ = done_tx.send(());
    (chk, code, t0.elapsed().as_secs_f64())
}

/// `--seeds 1-8` / `3,7,19`, as runcmd._seeds reads them.
pub fn seeds(spec: &str) -> Result<Vec<i64>, String> {
    let int = |s: &str| -> Result<i64, String> {
        crate::pyre::strip(s).replace('_', "").parse::<i64>().map_err(|_| format!("invalid literal for int() with base 10: {}", crate::pyre::repr(s)))
    };
    let mut out = Vec::new();
    for part in spec.split(',') {
        let st = crate::pyre::strip(part);
        if st.chars().skip(1).any(|c| c == '-') {
            let (lo, hi) = part.split_once('-').unwrap();
            out.extend(int(lo)?..=int(hi)?);
        } else if !st.is_empty() {
            out.push(int(part)?);
        }
    }
    Ok(out)
}

/// Python's `tempfile.gettempdir()`: $TMPDIR, $TEMP, $TMP, then /tmp, /var/tmp, /usr/tmp.
fn tempdir() -> PathBuf {
    let mut dirs: Vec<PathBuf> = ["TMPDIR", "TEMP", "TMP"].iter().filter_map(|k| std::env::var_os(k)).filter(|v| !v.is_empty()).map(PathBuf::from).collect();
    dirs.extend(["/tmp", "/var/tmp", "/usr/tmp"].map(PathBuf::from));
    for d in dirs {
        let d = normpath(&std::path::absolute(&d).unwrap_or(d));
        let probe = d.join(format!(".seamcheck-probe-{}", std::process::id()));
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(&probe);
            return d;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// `os.path.normpath` of an absolute path.
fn normpath(p: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::Normal(x) => out.push(x),
            _ => {}
        }
    }
    out
}

/// `shlex.join`.
pub fn shlex_join(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty() {
                "''".to_string()
            } else if a.bytes().all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\"'\"'"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn main(o: &Opts, root: &Path) -> i32 {
    let (cfg, problems) = config::load(root, &o.config);
    for p in &problems {
        println!("[error] config: {p}");
    }
    let bad = (!problems.is_empty()) as i32;
    if cfg.list("invariant").is_empty() && cfg.list("contract").is_empty() {
        let files: Vec<String> = config::files_for(root, &o.config).iter().map(|p| p.to_string_lossy().into_owned()).collect();
        println!("no [[invariant]] or [[contract]] in {}", if files.is_empty() { "(no config found)".to_string() } else { files.join(", ") });
        return 2; // nothing to check with: never a pass
    }
    let cfgv = Value::Object(cfg.v.clone());

    if o.cmd == "trace" {
        if o.args.is_empty() {
            println!("trace needs a log file (or - for stdin)");
            return 2;
        }
        let mut code = 0;
        for path in &o.args {
            let mut chk = Checker::new(&cfgv, o.strict);
            if path == "-" {
                for (l, _) in Lines::new(std::io::stdin().lock()) {
                    chk.feed_line(&l);
                }
            } else {
                match std::fs::File::open(path) {
                    Ok(f) => {
                        for (l, _) in Lines::new(std::io::BufReader::new(f)) {
                            chk.feed_line(&l);
                        }
                    }
                    Err(e) => {
                        eprintln!("seamcheck: {}", os_error(&e, path));
                        return 1;
                    }
                }
            }
            if o.args.len() > 1 {
                println!("== {path}");
            }
            code |= report(&chk, o, "");
        }
        return code | bad;
    }

    if o.args.is_empty() {
        println!("{} needs a command after --", o.cmd);
        return 2;
    }

    if o.cmd == "run" {
        let log = o.log.as_ref().map(PathBuf::from);
        let (chk, code, secs) = run_one(&o.args, &cfgv, o.strict, log.as_deref(), o.echo, o.timeout, None);
        let extra = format!("  (command exit {code}, {secs:.0} s{})", o.log.as_ref().map(|l| format!(", log {l}")).unwrap_or_default());
        return report(&chk, o, &extra) | bad;
    }

    // fuzz: the same command once per seed, a few at a time
    let seeds = match seeds(&o.seeds) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("seamcheck: --seeds: {e}");
            return 1;
        }
    };
    let logs = tempdir().join("seamcheck-fuzz");
    let _ = std::fs::create_dir_all(&logs);
    let has_slot = o.args.iter().any(|x| x.contains("{seed}"));
    let jobs = o.jobs.max(1) as usize;
    let (tx, rx) = mpsc::channel();
    let next = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seeds = std::sync::Arc::new(seeds);
    for _ in 0..jobs.min(seeds.len().max(1)) {
        let (tx, next, seeds, o, cfgv, logs) = (tx.clone(), next.clone(), seeds.clone(), o.clone(), cfgv.clone(), logs.clone());
        std::thread::spawn(move || loop {
            let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let Some(&seed) = seeds.get(i) else { break };
            let s = seed.to_string();
            let cmd: Vec<String> = o.args.iter().map(|x| x.replace("{seed}", &s)).collect();
            let log = logs.join(format!("seed-{seed}.log"));
            let (chk, code, secs) = run_one(&cmd, &cfgv, o.strict, Some(&log), false, o.timeout, Some(&s));
            if tx.send((i, seed, cmd, chk, code, secs)).is_err() {
                break;
            }
        });
    }
    drop(tx);
    // printed in seed order, each as soon as it and those before it are done
    let mut ready: std::collections::BTreeMap<usize, (i64, Vec<String>, Checker, i32, f64)> = Default::default();
    let mut shown = 0;
    let mut failed: Vec<(i64, Vec<String>)> = Vec::new();
    for (i, seed, cmd, chk, code, secs) in rx {
        ready.insert(i, (seed, cmd, chk, code, secs));
        while let Some((seed, cmd, chk, code, secs)) = ready.remove(&shown) {
            shown += 1;
            let vs = chk.finish();
            let errs: Vec<&Violation> = vs.iter().filter(|v| v.severity == "error").collect();
            println!(
                "seed {seed:<5} {}  {} states, {} actions, {} errors  ({secs:.0} s, exit {code})",
                if errs.is_empty() { "ok  " } else { "FAIL" },
                chk.states,
                chk.acts,
                errs.len()
            );
            for v in errs.iter().take(4) {
                println!("    {}", format_violation(v).replace('\n', "\n    "));
            }
            if !errs.is_empty() {
                failed.push((seed, cmd));
            }
        }
    }
    println!("\nseamcheck fuzz: {}/{} seeds clean. Logs: {}", seeds.len() - failed.len(), seeds.len(), logs.display());
    for (seed, cmd) in &failed {
        let env = if has_slot { String::new() } else { format!("SEAMCHECK_SEED={seed} ") };
        println!("  replay seed {seed}: {env}seamcheck run -- {}", shlex_join(cmd));
    }
    (!failed.is_empty()) as i32 | bad
}
