//! Parity for whole commands: run one seamcheck command with the Python
//! package and with this port in each repo and compare what they print
//! (stdout, byte for byte, with `(N ms)` timings set aside) and their exit
//! codes.
//!
//!   parity_cmd [--python DIR] [--rust BIN] [--copy DIR] REPO... -- ARGS...
//!
//! `--copy DIR`: each side runs in its own fresh copy of the repo under DIR
//! (`cp -a --reflink=auto`), never in the repo itself, and the files the
//! command writes (seamcheck.toml, the pre-commit hook) are compared too:
//! for `init`. Paths in the output name the copy, the same for both sides.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

struct Side {
    out: String,
    code: i32,
    files: Vec<(String, Option<Vec<u8>>)>,
    ms: u128,
}

const WRITTEN: &[&str] = &["seamcheck.toml", ".git/hooks/pre-commit"];

fn fresh(src: &Path, dst: &Path) {
    let _ = std::fs::remove_dir_all(dst);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    let ok = Command::new("cp")
        .args(["-a", "--reflink=auto"])
        .arg(src)
        .arg(dst)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "cp {src:?} {dst:?}");
}

fn timing_free(s: &str) -> String {
    let re = regex::Regex::new(r"\(\d+ ms\)").unwrap();
    re.replace_all(s, "(N ms)").into_owned()
}

fn run(mut cmd: Command, dir: &Path, copy: bool) -> Side {
    let t = Instant::now();
    let o = cmd.current_dir(dir).output().unwrap_or_else(|e| panic!("can't run {cmd:?}: {e}"));
    let ms = t.elapsed().as_millis();
    let files = if copy {
        WRITTEN.iter().map(|f| (f.to_string(), std::fs::read(dir.join(f)).ok())).collect()
    } else {
        Vec::new()
    };
    Side { out: timing_free(&String::from_utf8_lossy(&o.stdout)), code: o.status.code().unwrap_or(-1), files, ms }
}

fn first_diff(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}:\n    python: {x}\n    rust:   {y}", i + 1);
        }
    }
    format!("lengths differ: python {} lines, rust {} lines", a.lines().count(), b.lines().count())
}

fn main() {
    let exe = std::env::current_exe().unwrap();
    let mut python = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut rust = exe.with_file_name("seamcheck");
    let mut copy: Option<PathBuf> = None;
    let mut repos: Vec<PathBuf> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--python" => {
                i += 1;
                python = PathBuf::from(&argv[i]);
            }
            "--rust" => {
                i += 1;
                rust = PathBuf::from(&argv[i]);
            }
            "--copy" => {
                i += 1;
                copy = Some(PathBuf::from(&argv[i]));
            }
            "--" => {
                args = argv[i + 1..].to_vec();
                break;
            }
            other => repos.push(PathBuf::from(other)),
        }
        i += 1;
    }
    if repos.is_empty() || args.is_empty() {
        eprintln!("parity_cmd [--python DIR] [--rust BIN] [--copy DIR] REPO... -- ARGS...");
        std::process::exit(2);
    }
    let mut bad = 0;
    let (mut tp, mut tr) = (0u128, 0u128);
    for repo in &repos {
        let repo = std::path::absolute(repo).unwrap();
        let name = repo.file_name().unwrap().to_string_lossy().into_owned();
        let dir = copy.as_ref().map(|c| c.join(&name)).unwrap_or_else(|| repo.clone());
        if copy.is_some() {
            fresh(&repo, &dir);
        }
        let mut pc = Command::new("python3");
        pc.args(["-m", "seamcheck"]).args(&args).env("PYTHONPATH", &python);
        let p = run(pc, &dir, copy.is_some());
        if copy.is_some() {
            fresh(&repo, &dir);
        }
        let mut rc = Command::new(&rust);
        rc.args(&args);
        let r = run(rc, &dir, copy.is_some());
        if copy.is_some() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        tp += p.ms;
        tr += r.ms;
        let mut why: Vec<String> = Vec::new();
        if p.code != r.code {
            why.push(format!("exit python {} rust {}", p.code, r.code));
        }
        if p.out != r.out {
            why.push(format!("stdout differs at {}", first_diff(&p.out, &r.out)));
        }
        for ((f, a), (_, b)) in p.files.iter().zip(&r.files) {
            if a != b {
                let (a, b) = (a.as_deref().map(String::from_utf8_lossy), b.as_deref().map(String::from_utf8_lossy));
                why.push(format!(
                    "{f} differs: {}",
                    first_diff(a.as_deref().unwrap_or("<none>"), b.as_deref().unwrap_or("<none>"))
                ));
            }
        }
        if why.is_empty() {
            println!("same  {name}  (python {} ms, rust {} ms)", p.ms, r.ms);
        } else {
            bad += 1;
            println!("DIFF  {name}  (python {} ms, rust {} ms)", p.ms, r.ms);
            for w in why {
                println!("  {w}");
            }
        }
    }
    println!("\n{} repos, {} differ; python {} ms, rust {} ms", repos.len(), bad, tp, tr);
    std::process::exit((bad > 0) as i32);
}
