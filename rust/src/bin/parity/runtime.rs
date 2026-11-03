//! `parity --runtime DIR...`: `trace` / `run` / `fuzz` from both
//! implementations on scripted cases. Each DIR (or each sub-directory of
//! it holding a `cases` file) has a seamcheck.toml, the streams and scripts,
//! and `cases`: one JSON array of arguments per line (`-C DIR` is added), or
//! an object `{"args": [...], "stdin": FILE, "files": [FILE...]}` to feed
//! stdin and compare files the command writes. Stdout and the exit code must
//! be identical.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::Value;

/// The case directories under `dir` (itself when it has `cases`).
pub fn dirs(dir: &Path) -> Vec<PathBuf> {
    if dir.join("cases").exists() {
        return vec![dir.to_path_buf()];
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join("cases").exists()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
    files: Vec<String>,
    ms: u128,
}

fn one(mut cmd: Command, dir: &Path, args: &[String], stdin: Option<&str>, files: &[String]) -> Out {
    let mut a: Vec<String> = args.to_vec();
    let at = if a.is_empty() { 0 } else { 1 };
    a.insert(at, "-C".into());
    a.insert(at + 1, dir.to_string_lossy().into_owned());
    cmd.args(&a).current_dir(dir);
    cmd.stdin(match stdin {
        Some(f) => Stdio::from(std::fs::File::open(dir.join(f)).expect("stdin file")),
        None => Stdio::null(),
    });
    let t = Instant::now();
    let o = cmd.output().expect("run");
    let ms = t.elapsed().as_millis();
    let files = files
        .iter()
        .map(|f| {
            let p = dir.join(f);
            let s = std::fs::read_to_string(&p).unwrap_or_else(|_| "(missing)".into());
            let _ = std::fs::remove_file(&p);
            s
        })
        .collect();
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        code: o.status.code().unwrap_or(-1),
        files,
        ms,
    }
}

/// Differences on one case directory; prints each and a summary row.
pub fn parity(python: &Path, rust: &Path, dir: &Path, name: &str) -> usize {
    let text = std::fs::read_to_string(dir.join("cases")).unwrap_or_default();
    let (mut n, mut cases, mut pms, mut rms) = (0, 0, 0, 0);
    for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
        let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}/cases: {e}: {line}"));
        let (args, stdin, files) = match &v {
            Value::Array(_) => (v.clone(), None, vec![]),
            o => (
                o["args"].clone(),
                o.get("stdin").and_then(Value::as_str).map(String::from),
                o.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            ),
        };
        let args: Vec<String> = args.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
        cases += 1;
        let mut pc = Command::new("python3");
        pc.args(["-m", "seamcheck"]).env("PYTHONPATH", std::path::absolute(python).unwrap_or_else(|_| python.to_path_buf()));
        let p = one(pc, dir, &args, stdin.as_deref(), &files);
        let r = one(Command::new(rust), dir, &args, stdin.as_deref(), &files);
        pms += p.ms;
        rms += r.ms;
        let same = p.stdout == r.stdout && p.code == r.code && p.files == r.files;
        if !same {
            n += 1;
            println!("  case differs: {line}");
            if p.code != r.code {
                println!("    exit: python {} rust {}", p.code, r.code);
            }
            let (pl, rl): (Vec<&str>, Vec<&str>) = (p.stdout.lines().collect(), r.stdout.lines().collect());
            let mut shown = 0;
            for k in 0..pl.len().max(rl.len()) {
                let (a, b) = (pl.get(k).copied().unwrap_or("<none>"), rl.get(k).copied().unwrap_or("<none>"));
                if a != b && shown < 12 {
                    shown += 1;
                    println!("    line {}:\n      py: {a}\n      rs: {b}", k + 1);
                }
            }
            if p.files != r.files {
                println!("    written files differ: {:?} vs {:?}", p.files, r.files);
            }
            if !p.stderr.trim().is_empty() || !r.stderr.trim().is_empty() {
                println!("    stderr py: {}\n    stderr rs: {}", p.stderr.trim(), r.stderr.trim());
            }
        }
    }
    println!("{:<40} {:>9} {:>9} {:>9} {:>9}  (cases; {n} differ)", format!("{name} runtime"), cases, cases, pms, rms);
    n
}
