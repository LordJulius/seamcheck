//! `seamcheck` CLI (the Python __main__.py's surface). Phase 1 runs `check`
//! and `gate` without the call graph; graph commands say they are not
//! ported yet. `seamcheck port-status` lists the rules phase 1 defers.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use seamcheck::config::{id, s};
use seamcheck::output::{dumps, flat, github, sarif};
use seamcheck::project::Project;
use seamcheck::rules::{deferred_rules, picked, run_rules};

const COMMANDS: &[&str] = &[
    "init", "serve", "check", "map", "callers", "callees", "path", "find", "context", "dead", "stats", "deps", "matrix",
    "tidy", "tests", "test-only", "unused", "dups", "split", "hotspots", "remove", "refs", "impact", "gate", "trace",
    "run", "fuzz", "tighten", "graph", "search", "port-status", "port-dump",
];

#[derive(Default)]
struct Args {
    cmd: String,
    args: Vec<String>,
    root: String,
    config: Vec<String>,
    format: String,
    only: Option<String>,
    skip: Option<String>,
    staged: bool,
    since: Option<String>,
    errors_only: bool,
    all: bool,
    ext: Option<String>,
}

fn usage_error(msg: &str) -> ! {
    eprintln!("usage: seamcheck [-h] [--version] {{{}}} ...", COMMANDS.join(","));
    eprintln!("seamcheck: error: {msg}");
    std::process::exit(2);
}

fn parse(argv: Vec<String>) -> Args {
    let mut a = Args { root: ".".into(), format: "text".into(), ..Default::default() };
    let mut i = 0;
    let mut positional: Vec<String> = Vec::new();
    let value = |i: &mut usize, name: &str| -> String {
        *i += 1;
        argv.get(*i).cloned().unwrap_or_else(|| usage_error(&format!("argument {name}: expected one argument")))
    };
    while i < argv.len() {
        let x = argv[i].clone();
        let (flag, inline) = match x.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (x.clone(), None),
        };
        let val = |i: &mut usize| inline.clone().unwrap_or_else(|| value(i, &flag));
        match flag.as_str() {
            "--" => {
                positional.extend(argv[i + 1..].iter().cloned());
                break;
            }
            "--version" => {
                println!("seamcheck {}", seamcheck::VERSION);
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("seamcheck (Rust port, phase 1): check | gate | port-status; see PORT.md");
                std::process::exit(0);
            }
            "-C" => a.root = val(&mut i),
            "--config" => {
                let v = val(&mut i);
                a.config.push(v)
            }
            "--json" => a.format = "json".into(),
            "--format" => a.format = val(&mut i),
            "--only" => a.only = Some(val(&mut i)),
            "--skip" => a.skip = Some(val(&mut i)),
            "--staged" => a.staged = true,
            "--since" => a.since = Some(val(&mut i)),
            "--errors-only" => a.errors_only = true,
            "--all" => a.all = true,
            "--ext" => a.ext = Some(val(&mut i)),
            "--no-cache" | "--exact" | "--precise" | "--fuzzy" | "--wide" | "--ids" | "--apply" | "--code"
            | "--strict" | "--echo" | "--hook" | "--force" => {}
            "--html" | "--depth" | "--log" | "--timeout" | "--seeds" | "--jobs" | "--limit" | "--level" | "--via" => {
                let _ = val(&mut i);
            }
            _ if x.starts_with('-') && x.len() > 1 => usage_error(&format!("unrecognized arguments: {x}")),
            _ => positional.push(x),
        }
        i += 1;
    }
    if positional.is_empty() {
        usage_error("the following arguments are required: cmd");
    }
    a.cmd = positional.remove(0);
    if !COMMANDS.contains(&a.cmd.as_str()) {
        usage_error(&format!("argument cmd: invalid choice: '{}'", a.cmd));
    }
    a.args = positional;
    a
}

fn find_root(start: &str) -> PathBuf {
    let start = std::path::absolute(start).unwrap_or_else(|_| PathBuf::from(start));
    let mut d = start.clone();
    loop {
        if d.join("seamcheck.toml").exists() {
            return d;
        }
        match d.parent() {
            Some(p) => d = p.to_path_buf(),
            None => return start,
        }
    }
}

fn whys(p: &Project) -> std::collections::HashMap<String, (String, String)> {
    p.config
        .rules()
        .map(|r| {
            (id(r).to_string(), (s(r, "why").unwrap_or("").to_string(), s(r, "fix").unwrap_or("").to_string()))
        })
        .collect()
}

fn main() {
    let a = parse(std::env::args().skip(1).collect());
    let root = if a.config.is_empty() {
        find_root(&a.root)
    } else {
        std::path::absolute(&a.root).unwrap_or_else(|_| PathBuf::from(&a.root))
    };
    let t0 = Instant::now();
    let code = match a.cmd.as_str() {
        "check" => check(&a, &root, t0),
        "gate" => gate(&a, &root, t0),
        "port-status" => port_status(&a, &root),
        "port-dump" => port_dump(&a, &root),
        "matrix" => matrix(&a, &root),
        "tighten" => tighten(&a, &root),
        other => {
            eprintln!(
                "seamcheck (rust, phase 1): `{other}` is not ported yet; it needs the call graph (phase 2). \
                 Run the Python seamcheck for it."
            );
            2
        }
    };
    std::process::exit(code);
}

fn check(a: &Args, root: &Path, t0: Instant) -> i32 {
    let p = Project::load(root, &a.config);
    let only: Option<Vec<String>> = a.only.as_ref().map(|x| x.split(',').map(String::from).collect());
    let skip: Option<Vec<String>> = a.skip.as_ref().map(|x| x.split(',').map(String::from).collect());
    let ids: Vec<String> = p.config.rules().map(|r| id(r).to_string()).collect();
    for name in only.iter().flatten().chain(skip.iter().flatten()) {
        if !ids.iter().any(|i| picked(i, Some(std::slice::from_ref(name)))) {
            eprintln!("seamcheck: no rule or pack named {} (rules: {})", seamcheck::pyre::repr(name), ids.join(", "));
        }
    }
    let mut findings = run_rules(&p, only.as_deref(), skip.as_deref());
    if a.errors_only {
        findings.retain(|f| f.severity == "error");
    }
    let w = whys(&p);
    let failed = findings.iter().any(|f| f.severity == "error");
    if a.format != "text" {
        let flatv: Vec<Value> = findings
            .iter()
            .map(|f| {
                let (why, fix) = w.get(&f.rule).cloned().unwrap_or_default();
                flat(f, &why, &fix)
            })
            .collect();
        match a.format.as_str() {
            "json" => println!("{}", dumps(&Value::Array(flatv))),
            "sarif" => {
                let rules: Vec<(String, String, String)> =
                    w.iter().map(|(k, (why, fix))| (k.clone(), why.clone(), fix.clone())).collect();
                println!("{}", sarif(&flatv, &rules));
            }
            _ => println!("{}", github(&flatv)),
        }
        return failed as i32;
    }
    let mut cur: Option<&str> = None;
    for f in &findings {
        if cur != Some(f.rule.as_str()) {
            cur = Some(&f.rule);
            let why = w.get(&f.rule).map(|x| x.0.as_str()).unwrap_or("");
            println!("\n[{}] {}{}", f.severity, f.rule, if why.is_empty() { String::new() } else { format!("  - {why}") });
        }
        let func = if f.func.is_empty() { String::new() } else { format!(" ({})", f.func) };
        println!("  {}:{}{}  {}", f.file, f.line, func, f.message);
        if !f.snippet.is_empty() {
            println!("      {}", f.snippet);
        }
    }
    let errs = findings.iter().filter(|f| f.severity == "error").count();
    // (phase 1 parses Elixir only: the count is of Elixir functions)
    let nfuncs: usize = p.files.iter().filter(|f| seamcheck::lang::ported(f)).map(|f| p.modules(f).iter().map(|m| m.funcs.len()).sum::<usize>()).sum();
    println!(
        "\nseamcheck: {} rules, {} files, {} functions; {} errors, {} warnings  ({} ms)",
        p.config.list("rule").len(),
        p.files.len(),
        nfuncs,
        errs,
        findings.len() - errs,
        t0.elapsed().as_millis()
    );
    failed as i32
}

fn gate(a: &Args, root: &Path, t0: Instant) -> i32 {
    let mut p = Project::load(root, &a.config);
    let r = a.since.clone().unwrap_or_else(|| "HEAD".into());
    let rep = seamcheck::gate::gate(&mut p, &r, a.staged);
    let w = whys(&p);
    let bad = rep.failed();
    if a.format != "text" {
        let mut flatv: Vec<Value> = rep
            .rules
            .iter()
            .map(|f| {
                json!({"kind": "rule", "rule": f.rule, "severity": f.severity, "file": f.file, "line": f.line,
                       "message": f.message, "why": w.get(&f.rule).map(|x| x.0.clone()).unwrap_or_default(), "fix": ""})
            })
            .collect();
        for x in &rep.forbidden {
            flatv.push(json!({"kind": "forbidden", "rule": "forbidden", "severity": "error", "file": x.file,
                              "line": x.line, "message": x.word, "why": "",
                              "fix": "retire the old path instead of naming a second one"}));
        }
        for x in &rep.moved_refs {
            flatv.push(json!({"kind": "moved_refs", "rule": "moved-ref", "severity": "error", "file": x.file,
                              "line": x.line, "message": format!("names {}, which this change split up", x.moved_from),
                              "why": "a check pinned to one file checks less after a split",
                              "fix": "point it at where the code went (or a glob), or `seamcheck: allow moved-ref`"}));
        }
        match a.format.as_str() {
            "json" => println!(
                "{}",
                dumps(&json!({"blocked": bad, "changed_files": rep.changed_files, "findings": flatv,
                              "ms": t0.elapsed().as_millis() as u64}))
            ),
            "sarif" => println!("{}", sarif(&flatv, &[])),
            _ => println!("{}", github(&flatv)),
        }
        return bad as i32;
    }
    let mut shown = std::collections::HashSet::new();
    for f in &rep.rules {
        println!("  rule {}: {}:{}  {}", f.rule, f.file, f.line, f.message);
        let why = w.get(&f.rule).map(|x| x.0.as_str()).unwrap_or("");
        if !why.is_empty() && shown.insert(f.rule.clone()) {
            println!("      why: {why}");
        }
    }
    for x in &rep.forbidden {
        println!("  new code names '{}': {}:{}  (retire the old path instead)", x.word, x.file, x.line);
    }
    for x in &rep.moved_refs {
        println!(
            "  names {}, which this change split up: {}:{}  (point it at the new files or a glob)",
            x.moved_from, x.file, x.line
        );
    }
    println!(
        "seamcheck gate: {} changed files; {}  ({} ms)",
        rep.changed_files.len(),
        if bad { "BLOCKED" } else { "clear" },
        t0.elapsed().as_millis()
    );
    bad as i32
}

fn port_status(a: &Args, root: &Path) -> i32 {
    let p = Project::load(root, &a.config);
    let d = deferred_rules(&p);
    if a.format == "json" {
        let v: Vec<Value> = d.iter().map(|(id, why)| json!({"id": id, "why": why})).collect();
        println!("{}", dumps(&Value::Array(v)));
    } else {
        let n = p.config.list("rule").len();
        println!("{} of {} rules run in phase 1; deferred:", n - d.len(), n);
        for (id, why) in &d {
            println!("  {id}: {why}");
        }
    }
    0
}

/// `port-dump parse|edges [--ext .ex,.exs]`: what `parity --parse` /
/// `--edges` compare, one JSON value per line. parse: each file's modules
/// as its adapter gives them; edges: [caller, callee, line, exact] for the
/// callers in files with those extensions.
fn port_dump(a: &Args, root: &Path) -> i32 {
    use seamcheck::project::ext_of;
    let p = Project::load(root, &a.config);
    let exts: Option<Vec<String>> = a.ext.as_ref().map(|e| e.split(',').map(String::from).collect());
    let want = |rel: &str| exts.as_ref().map(|e| e.iter().any(|x| x == ext_of(rel))).unwrap_or(true);
    let t = Instant::now();
    match a.args.first().map(|s| s.as_str()) {
        Some("parse") => {
            for (rel, got) in seamcheck::graph::parse_all(&p, exts.as_deref()) {
                let v = match got {
                    Ok(mods) => json!({"file": rel, "modules": mods.iter().map(|m| m.dump()).collect::<Vec<_>>()}),
                    Err(e) => json!({"file": rel, "error": e}),
                };
                println!("{v}");
            }
        }
        Some("edges") => {
            let g = seamcheck::graph::Graph::load(&p);
            for (caller, callee, line, exact) in g.edge_rows() {
                if g.func(&caller).map(|f| want(&f.file)).unwrap_or(false) {
                    println!("{}", json!([caller, callee, line, exact]));
                }
            }
        }
        _ => usage_error("port-dump: parse | edges"),
    }
    eprintln!("port-dump: {} ms", t.elapsed().as_millis());
    0
}

fn matrix(a: &Args, root: &Path) -> i32 {
    let p = Project::load(root, &a.config);
    let rules: Vec<&seamcheck::config::Rule> = p
        .config
        .rules()
        .filter(|r| s(r, "kind") == Some("fanout") && (a.args.is_empty() || a.args.iter().any(|x| x == id(r))))
        .collect();
    if rules.is_empty() {
        println!(
            "no fanout rules{}",
            a.args.first().map(|x| format!(" named {}", seamcheck::pyre::repr(x))).unwrap_or_default()
        );
        return 1;
    }
    let mark = |c: &str| match c {
        "ok" => "+",
        "except" => "~",
        "fallback" => "generic",
        "missing" => "MISSING",
        "extra" => "extra",
        _ => ".",
    };
    let mut bad = 0;
    for r in rules {
        let (toks, views, cell) = match seamcheck::rules::fanout::matrix(&p, r) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("seamcheck: rule {}: {e:?}", id(r));
                return 1;
            }
        };
        let w = toks.iter().map(|t| t.chars().count()).chain([8]).max().unwrap();
        let why = s(r, "why").unwrap_or("");
        println!("\n{}{}", id(r), if why.is_empty() { String::new() } else { format!("  - {why}") });
        println!("  {}{}", " ".repeat(w), views.iter().map(|v| format!("{v:>10}")).collect::<String>());
        let checks = seamcheck::config::list_or(r, "check", &["missing"]);
        let show_extra = a.all || checks.iter().any(|c| c == "extra");
        for t in &toks {
            let row: Vec<&str> = views.iter().map(|v| *cell.get(&(t.clone(), v.clone())).unwrap_or(&"")).collect();
            if !show_extra && row.iter().all(|c| *c == "extra" || c.is_empty()) {
                continue;
            }
            bad += row.iter().filter(|c| **c == "missing").count();
            println!("  {t:<w$}{}", row.iter().map(|c| format!("{:>10}", mark(c))).collect::<String>());
        }
    }
    println!(
        "\n+ handled   ~ intentional gap (except)   generic: the view's fallback host   MISSING not handled   extra: handled, never produced"
    );
    (bad > 0) as i32
}

/// `seamcheck tighten`: run each command rule's `tighten` (its own allowlist
/// rewrite, which only ever shrinks a list).
fn tighten(a: &Args, root: &Path) -> i32 {
    let (config, problems) = seamcheck::config::load(root, &a.config);
    for pr in &problems {
        println!("config: {pr}");
    }
    let mut failed = 0;
    for r in config.rules() {
        let Some(cmd) = s(r, "tighten").filter(|c| !c.is_empty()) else { continue };
        let res = seamcheck::rules::command::run_shell(cmd, root, 600.0);
        let (ok, both) = match &res {
            Some(x) => (x.code == 0, format!("{}{}", x.stdout, x.stderr)),
            None => (false, String::new()),
        };
        let lines = seamcheck::rules::command::splitlines(seamcheck::pyre::strip(&both));
        println!("{} {}: {}", if ok { "ok " } else { "FAIL" }, id(r), lines.last().copied().unwrap_or(""));
        failed += (!ok) as i32;
    }
    (failed > 0 || !problems.is_empty()) as i32
}
