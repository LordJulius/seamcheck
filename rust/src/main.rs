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
    "run", "fuzz", "tighten", "graph", "search", "port-status", "port-dump", "verify",
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
    q: seamcheck::queries::cli::Opts,
    html: Option<String>,
    level: String,
    via: String,
    strict: bool,
    precise: bool,
    no_cache: bool,
    echo: bool,
    log: Option<String>,
    timeout: f64,
    seeds: String,
    jobs: i64,
    hook: bool,
    force: bool,
    ids: bool,
    apply: bool,
}

fn usage_error(msg: &str) -> ! {
    eprintln!("usage: seamcheck [-h] [--version] {{{}}} ...", COMMANDS.join(","));
    eprintln!("seamcheck: error: {msg}");
    std::process::exit(2);
}

fn parse(argv: Vec<String>) -> Args {
    let mut a =
        Args { root: ".".into(), format: "text".into(), level: "dir".into(), via: "import".into(), seeds: "1-4".into(), jobs: 4, ..Default::default() };
    let mut i = 0;
    let mut positional: Vec<String> = Vec::new();
    let value = |i: &mut usize, name: &str| -> String {
        *i += 1;
        argv.get(*i).cloned().unwrap_or_else(|| usage_error(&format!("argument {name}: expected one argument")))
    };
    // argparse fills `args` (nargs="*") from the first run of positionals
    // that has any; a positional in a later run is unrecognized
    let mut stray: Vec<String> = Vec::new();
    while i < argv.len() {
        let x = argv[i].clone();
        let is_pos = !(x.starts_with('-') && x.len() > 1);
        if !is_pos && positional.len() >= 2 && x != "--" {
            stray.push(String::new()); // (a marker: the args run is closed)
        }
        if is_pos && !stray.is_empty() {
            stray.push(x);
            i += 1;
            continue;
        }
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
            "--level" => a.level = val(&mut i),
            "--via" => a.via = val(&mut i),
            "--fuzzy" => a.q.fuzzy = true,
            "--wide" => a.q.wide = true,
            "--code" => a.q.code = true,
            "--html" => a.html = Some(val(&mut i)),
            "--depth" | "--limit" => {
                let v = val(&mut i);
                let n: i64 = v.parse().unwrap_or_else(|_| usage_error(&format!("argument {flag}: invalid int value: '{v}'")));
                if flag == "--depth" { a.q.depth = n } else { a.q.limit = n }
            }
            "--ids" => a.ids = true,
            "--apply" => a.apply = true,
            "--strict" => a.strict = true,
            "--echo" => a.echo = true,
            "--log" => a.log = Some(val(&mut i)),
            "--seeds" => a.seeds = val(&mut i),
            "--timeout" => {
                let v = val(&mut i);
                a.timeout = v.trim().parse().unwrap_or_else(|_| usage_error(&format!("argument --timeout: invalid float value: '{v}'")))
            }
            "--jobs" => {
                let v = val(&mut i);
                a.jobs = v.trim().parse().unwrap_or_else(|_| usage_error(&format!("argument --jobs: invalid int value: '{v}'")))
            }
            "--precise" => a.precise = true,
            "--no-cache" => a.no_cache = true,
            "--exact" => {}
            "--hook" => a.hook = true,
            "--force" => a.force = true,
            _ if x.starts_with('-') && x.len() > 1 => usage_error(&format!("unrecognized arguments: {x}")),
            _ => positional.push(x),
        }
        i += 1;
    }
    if positional.is_empty() {
        usage_error("the following arguments are required: cmd");
    }
    let stray: Vec<String> = stray.into_iter().filter(|s| !s.is_empty()).collect();
    if !stray.is_empty() {
        usage_error(&format!("unrecognized arguments: {}", stray.join(" ")));
    }
    a.cmd = positional.remove(0);
    if !COMMANDS.contains(&a.cmd.as_str()) {
        usage_error(&format!("argument cmd: invalid choice: '{}'", a.cmd));
    }
    a.args = positional;
    a.q.args = a.args.clone();
    a.q.json = a.format == "json";
    a.q.since = a.since.clone();
    a.q.staged = a.staged;
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // `verify` has its own flags (docs/verify.md); the Python CLI hands it here unchanged.
    if argv.first().map(String::as_str) == Some("verify") {
        std::process::exit(seamcheck::verify::main(&argv[1..]));
    }
    let a = parse(argv);
    if a.cmd == "init" {
        std::process::exit(seamcheck::init::main(&seamcheck::init::abspath(&a.root), a.hook, a.force));
    }
    let root = if a.config.is_empty() {
        find_root(&a.root)
    } else {
        std::path::absolute(&a.root).unwrap_or_else(|_| PathBuf::from(&a.root))
    };
    let t0 = Instant::now();
    let code = match a.cmd.as_str() {
        "check" => check(&a, &root, t0),
        "gate" => seamcheck::tidy::cli::gate(&tidy_opts(&a), &root, t0),
        "tidy" | "tests" | "test-only" | "unused" | "dups" | "split" | "hotspots" | "remove" | "refs" => {
            seamcheck::tidy::cli::main(&tidy_opts(&a), &root, t0)
        }
        "port-status" => port_status(&a, &root),
        "port-dump" => port_dump(&a, &root),
        "matrix" => matrix(&a, &root),
        "tighten" => tighten(&a, &root),
        "callers" | "callees" | "path" | "find" | "context" | "search" | "impact" | "dead" | "stats" => query(&a, &root, t0),
        "map" => map(&a, &root),
        "serve" => seamcheck::serve::main(&root, &a.config),
        "graph" => graph(&a, &root),
        "trace" | "run" | "fuzz" => seamcheck::runtime::cmd::main(
            &seamcheck::runtime::cmd::Opts {
                cmd: a.cmd.clone(),
                args: a.args.clone(),
                config: a.config.clone(),
                json: a.format == "json",
                strict: a.strict,
                log: a.log.clone(),
                echo: a.echo,
                timeout: a.timeout,
                seeds: a.seeds.clone(),
                jobs: a.jobs,
            },
            &root,
        ),
        "deps" => seamcheck::deps::cli(&root, &a.config, &a.args, a.all, a.format == "json", t0),
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

/// The query commands (queries::cli), over one loaded graph.
fn query(a: &Args, root: &Path, t0: Instant) -> i32 {
    let p = Project::load(root, &a.config);
    let precise = a.precise || seamcheck::graph::precise::enabled(&p);
    let cache = (!a.no_cache).then(seamcheck::graph::precise::cache_dir);
    let g = seamcheck::graph::Graph::load_precise(&p, true, precise, cache.as_deref());
    let q = seamcheck::queries::Q::new(&p, &g);
    let r = if a.cmd == "stats" { q.cmd_stats(&a.q, Some(t0.elapsed().as_millis())) } else { q.command(&a.cmd, &a.q) };
    print!("{}", r.out);
    eprint!("{}", r.err);
    r.code
}

/// `map --html out.html`: the interactive map (report.rs).
fn map(a: &Args, root: &Path) -> i32 {
    let p = Project::load(root, &a.config);
    let g = p.graph();
    if let Some(out) = &a.html {
        let only: Option<Vec<String>> = a.only.as_ref().map(|x| x.split(',').map(String::from).collect());
        let skip: Option<Vec<String>> = a.skip.as_ref().map(|x| x.split(',').map(String::from).collect());
        let findings = run_rules(&p, only.as_deref(), skip.as_deref());
        if let Err(e) = seamcheck::report::write_html(&p, &g, &findings, Path::new(out)) {
            eprintln!("seamcheck map: {out}: {e}");
            return 1;
        }
    }
    println!(
        "map: {} modules, {} functions -> {}",
        g.modules.len(),
        g.funcs.len(),
        a.html.as_deref().unwrap_or("(use --html out.html)")
    );
    0
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
    // (as Python's check does: the graph's parser errors and bridge notes are findings too)
    let g = p.graph();
    let mut findings = run_rules(&p, only.as_deref(), skip.as_deref());
    if a.errors_only {
        findings.retain(|f| f.severity == "error");
    }
    if let Some(out) = &a.html {
        if let Err(e) = seamcheck::report::write_html(&p, &p.graph(), &findings, Path::new(out)) {
            eprintln!("seamcheck check: {out}: {e}");
        }
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
    let nfuncs = g.funcs.len();
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

fn tidy_opts(a: &Args) -> seamcheck::tidy::cli::Opts<'_> {
    seamcheck::tidy::cli::Opts {
        cmd: &a.cmd,
        args: &a.args,
        config: &a.config,
        format: &a.format,
        since: a.since.as_deref(),
        staged: a.staged,
        limit: a.q.limit.max(0) as usize,
        ids: a.ids,
        apply: a.apply,
    }
}

fn port_status(a: &Args, root: &Path) -> i32 {
    let p = Project::load(root, &a.config);
    let d = deferred_rules(&p);
    if a.format == "json" {
        let v: Vec<Value> = d.iter().map(|(id, why)| json!({"id": id, "why": why})).collect();
        println!("{}", dumps(&Value::Array(v)));
    } else {
        let n = p.config.list("rule").len();
        println!("{} of {} rules run; deferred:", n - d.len(), n);
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
            let precise = a.precise || seamcheck::graph::precise::enabled(&p);
            let cache = (!a.no_cache).then(seamcheck::graph::precise::cache_dir);
            let g = seamcheck::graph::Graph::load_precise(&p, std::env::var_os("SEAMCHECK_BRIDGES").is_some(), precise, cache.as_deref());
            if let Some(st) = &g.precise {
                eprintln!("{}", seamcheck::graph::precise::summary(st));
            }
            for (caller, callee, line, exact) in g.edge_rows() {
                if g.func(&caller).map(|f| want(&f.file)).unwrap_or(false) {
                    println!("{}", json!([caller, callee, line, exact]));
                }
            }
        }
        Some("arities") => {
            // port-dump arities MODULE NAME: the argument counts a module's declarations of NAME take
            let g = seamcheck::graph::Graph::load(&p);
            let (m, n) = (a.args.get(1).cloned().unwrap_or_default(), a.args.get(2).cloned().unwrap_or_default());
            if let Some(md) = g.modules.get(&m) {
                println!("{:?}", seamcheck::graph::infer::arities(md, &n));
            }
        }
        Some("chains") => {
            // (a helper that panics answers "error", as Python's raising one does)
            std::panic::set_hook(Box::new(|_| {}));
            let todo: Vec<&String> = p
                .files
                .iter()
                .filter(|r| want(r) && seamcheck::project::has_adapter(r) && !matches!(ext_of(r), ".ex" | ".exs"))
                .collect();
            use rayon::prelude::*;
            let out: Vec<String> = todo
                .par_iter()
                .map(|rel| {
                    let raw = p.text(rel);
                    let code = seamcheck::scan::blank_text(&raw, seamcheck::project::scan_family(rel), false);
                    seamcheck::lang::probe::probe(rel, &code).to_string()
                })
                .collect();
            for line in out {
                println!("{line}");
            }
        }
        Some("queries") => {
            // port-dump queries FILE: each line a JSON argv (`["callers", "save", "--depth", "1"]`),
            // run through the CLI's own parsing and printing over one loaded graph (parity --queries)
            let g = seamcheck::graph::Graph::load(&p);
            let q = seamcheck::queries::Q::new(&p, &g);
            let list = std::fs::read_to_string(a.args.get(1).map(|s| s.as_str()).unwrap_or("-")).unwrap_or_default();
            for line in list.lines().filter(|l| !l.trim().is_empty()) {
                let argv: Vec<String> = serde_json::from_str(line).unwrap_or_default();
                let qa = parse(argv.clone());
                let r = q.command(&qa.cmd, &qa.q);
                println!("{}", json!({"argv": argv, "code": r.code, "out": r.out}));
            }
        }
        _ => usage_error("port-dump: parse | edges | chains | queries"),
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

/// `seamcheck graph [GLOB...] [--level file|dir] [--via import|call|any]
/// [--format dot|mermaid|json]`: the dependency graph, cycles marked.
fn graph(a: &Args, root: &Path) -> i32 {
    for (flag, v, ok) in [("--level", &a.level, &["file", "dir"][..]), ("--via", &a.via, &["import", "call", "any"][..])] {
        if !ok.contains(&v.as_str()) {
            let choices: Vec<String> = ok.iter().map(|c| format!("'{c}'")).collect();
            usage_error(&format!("argument {flag}: invalid choice: '{v}' (choose from {})", choices.join(", ")));
        }
    }
    let p = Project::load(root, &a.config);
    let g = seamcheck::arch::graph(&p, &a.level, &a.via, &a.args);
    if matches!(a.format.as_str(), "dot" | "mermaid" | "json") {
        println!("{}", seamcheck::arch::render(&g, &a.format));
        return 0;
    }
    let cyc = seamcheck::arch::in_cycles(&g);
    for (src, ds) in &g {
        println!("{src}{}", if cyc.contains(src) { "  (in a cycle)" } else { "" });
        for (dst, w) in ds {
            println!("  -> {dst}{}", if a.level == "dir" { format!("  ({w} files)") } else { String::new() });
        }
    }
    0
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
