//! Parity harness: run the Python seamcheck and this port on the same
//! projects and diff their findings.
//!
//!   parity [--python DIR] [--rust BIN] [--gate] [--staged] [--fixtures DIR] [REPO...]
//!
//! For each REPO (and each sub-directory of --fixtures): `check --format json`
//! from both (Python with `--skip` for the rules `port-status` defers), and
//! with --gate also `gate --format json` (Python's graph-only gate findings
//! set aside). Findings compare on (rule, severity, file, line, message,
//! snippet), plus `func` where phase 1 knows the file's functions. Prints the
//! differences and both wall times; exits 1 on any difference.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::Value;

struct Opts {
    python: PathBuf,
    rust: PathBuf,
    gate: bool,
    staged: bool,
    repos: Vec<PathBuf>,
}

fn opts() -> Opts {
    let exe = std::env::current_exe().unwrap();
    let mut o = Opts {
        python: Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf(),
        rust: exe.with_file_name("seamcheck"),
        gate: false,
        staged: false,
        repos: vec![],
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--python" => {
                i += 1;
                o.python = PathBuf::from(&args[i]);
            }
            "--rust" => {
                i += 1;
                o.rust = PathBuf::from(&args[i]);
            }
            "--gate" => o.gate = true,
            "--staged" => o.staged = true,
            "--fixtures" => {
                i += 1;
                let mut subs: Vec<PathBuf> = std::fs::read_dir(&args[i])
                    .expect("fixtures dir")
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                subs.sort();
                o.repos.extend(subs);
            }
            "-h" | "--help" => {
                println!("parity [--python DIR] [--rust BIN] [--gate] [--staged] [--fixtures DIR] [REPO...]");
                std::process::exit(0);
            }
            other => o.repos.push(PathBuf::from(other)),
        }
        i += 1;
    }
    if o.repos.is_empty() {
        eprintln!("parity: no repo given (REPO... or --fixtures DIR)");
        std::process::exit(2);
    }
    o
}

struct Ran {
    out: String,
    err: String,
    ms: u128,
}

fn run(cmd: &mut Command) -> Ran {
    let t = Instant::now();
    let o = cmd.output().unwrap_or_else(|e| panic!("can't run {cmd:?}: {e}"));
    Ran {
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
        ms: t.elapsed().as_millis(),
    }
}

fn py(o: &Opts, repo: &Path, args: &[&str]) -> Ran {
    run(Command::new("python3").args(["-m", "seamcheck"]).args(args).current_dir(repo).env("PYTHONPATH", &o.python))
}

fn rs(o: &Opts, repo: &Path, args: &[&str]) -> Ran {
    run(Command::new(&o.rust).args(args).current_dir(repo).env("SEAMCHECK_RS_QUIET", "1"))
}

fn parse(r: &Ran, who: &str) -> Value {
    serde_json::from_str(&r.out).unwrap_or_else(|e| {
        eprintln!("parity: {who} printed no JSON ({e}); stderr:\n{}", r.err);
        Value::Null
    })
}

fn func_known(file: &str) -> bool {
    seamcheck::project::is_elixir(file) || !seamcheck::project::has_adapter(file)
}

type Key = (String, String, String, u64, String, String, String);

fn key(f: &Value, with_snippet: bool) -> Key {
    let s = |k: &str| f.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let file = s("file");
    // a bad regex is reported by both, in each engine's own words
    let mut message = s("message");
    if let Some(k) = message.find(": ").filter(|_| message.starts_with("bad pattern in rule ")) {
        message.truncate(k + 2);
        message.push_str("<the regex engine's error>");
    }
    (
        if f.get("kind").is_some() { s("kind") } else { String::new() },
        s("rule"),
        s("severity"),
        f.get("line").and_then(|x| x.as_u64()).unwrap_or(0),
        format!("{file}\u{0}{}", if func_known(&file) { s("func") } else { String::new() }),
        message,
        if with_snippet { s("snippet") } else { String::new() },
    )
}

fn bag(v: &[Value], with_snippet: bool) -> BTreeMap<Key, usize> {
    let mut m = BTreeMap::new();
    for f in v {
        *m.entry(key(f, with_snippet)).or_insert(0) += 1;
    }
    m
}

/// Python findings phase 1 sets aside: bridges (built with the graph) and,
/// in a gate, the graph-only kinds and the rules phase 1 defers.
fn phase1(f: &Value, deferred: &[String]) -> bool {
    let s = |k: &str| f.get(k).and_then(|x| x.as_str()).unwrap_or("");
    if s("rule") == "config" && s("message").contains(": bridge ") {
        return false;
    }
    if matches!(s("kind"), "unused" | "test_only" | "dups" | "tests" | "exactness") {
        return false;
    }
    !deferred.iter().any(|d| d == s("rule"))
}

fn diff(label: &str, p: &[Value], r: &[Value], with_snippet: bool) -> usize {
    let (bp, br) = (bag(p, with_snippet), bag(r, with_snippet));
    let mut n = 0;
    for (k, c) in &bp {
        let rc = br.get(k).copied().unwrap_or(0);
        if *c > rc {
            n += c - rc;
            println!("  {label} only python ({}x): {}", c - rc, show(k));
        }
    }
    for (k, c) in &br {
        let pc = bp.get(k).copied().unwrap_or(0);
        if *c > pc {
            n += c - pc;
            println!("  {label} only rust   ({}x): {}", c - pc, show(k));
        }
    }
    n
}

fn show(k: &Key) -> String {
    let (file, func) = k.4.split_once('\u{0}').unwrap_or((&k.4, ""));
    format!(
        "{}[{}] {} {}:{}{}  {}{}",
        if k.0.is_empty() { String::new() } else { format!("{} ", k.0) },
        k.2,
        k.1,
        file,
        k.3,
        if func.is_empty() { String::new() } else { format!(" ({func})") },
        k.5,
        if k.6.is_empty() { String::new() } else { format!("  | {}", k.6) }
    )
}

fn main() {
    let o = opts();
    let mut total = 0;
    println!("{:<40} {:>9} {:>9} {:>9} {:>9}", "project", "py n", "rs n", "py ms", "rs ms");
    for repo in &o.repos {
        let name = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let status = rs(&o, repo, &["port-status", "--format", "json"]);
        let deferred: Vec<String> = parse(&status, "rust port-status")
            .as_array()
            .map(|a| a.iter().filter_map(|d| d.get("id").and_then(|x| x.as_str()).map(String::from)).collect())
            .unwrap_or_default();
        let mut pargs = vec!["check", "--format", "json"];
        let skip = deferred.join(",");
        if !deferred.is_empty() {
            pargs.extend(["--skip", skip.as_str()]);
        }
        let pc = py(&o, repo, &pargs);
        let rc = rs(&o, repo, &["check", "--format", "json"]);
        let pv: Vec<Value> = parse(&pc, "python check")
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|f| phase1(f, &[]))
            .collect();
        let rv: Vec<Value> = parse(&rc, "rust check").as_array().cloned().unwrap_or_default();
        let before = total;
        println!(
            "{:<40} {:>9} {:>9} {:>9} {:>9}  ({} rules deferred)",
            format!("{name} check"),
            pv.len(),
            rv.len(),
            pc.ms,
            rc.ms,
            deferred.len()
        );
        total += diff("check", &pv, &rv, true);
        if o.gate {
            let mut gargs = vec!["gate", "--format", "json"];
            if o.staged {
                gargs.push("--staged");
            }
            let pg = py(&o, repo, &gargs);
            let rg = rs(&o, repo, &gargs);
            let pj = parse(&pg, "python gate");
            let rj = parse(&rg, "rust gate");
            let pf: Vec<Value> = pj
                .get("findings")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|f| phase1(f, &deferred))
                .collect();
            let rf: Vec<Value> = rj.get("findings").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            println!("{:<40} {:>9} {:>9} {:>9} {:>9}", format!("{name} gate"), pf.len(), rf.len(), pg.ms, rg.ms);
            total += diff("gate", &pf, &rf, false);
            if pj.get("changed_files") != rj.get("changed_files") {
                println!("  gate changed_files differ: {:?} vs {:?}", pj.get("changed_files"), rj.get("changed_files"));
                total += 1;
            }
        }
        if total == before {
            println!("  identical");
        }
    }
    println!("parity: {total} differences");
    std::process::exit((total > 0) as i32);
}
