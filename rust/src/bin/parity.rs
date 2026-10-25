//! Parity harness: run the Python seamcheck and this port on the same
//! projects and diff their findings.
//!
//!   parity [--python DIR] [--rust BIN] [--gate] [--staged] [--fixtures DIR]
//!          [--parse] [--edges] [--untyped] [--ext .ex,.exs] [--check] [REPO...]
//!
//! For each REPO (and each sub-directory of --fixtures): `check --format json`
//! from both (Python with `--skip` for the rules `port-status` defers), and
//! with --gate also `gate --format json` (Python's graph-only gate findings
//! set aside). Findings compare on (rule, severity, file, line, message,
//! snippet), plus `func` where phase 1 knows the file's functions. Prints the
//! differences and both wall times; exits 1 on any difference.
//!
//! --parse: each file's modules as its adapter gives them (every model
//! field but the text), compared file by file; --edges: the call graph's
//! edges (caller, callee, line, exact) from callers in those files, bridges
//! left out. Both for the extensions in --ext (default: every ported one).
//! The Python side runs inline (`python3 -c`, PY_PARSE / PY_EDGES). An edge
//! Python draws into a language not ported yet is listed apart and is no
//! failure: that language's lane closes it. --chains: the shared text
//! helpers (lang::chains, lang::lambdas) on every call site, assignment,
//! declared type and bracket of the files with those extensions (default:
//! every non-Elixir adapter's), Python answering the same sites on the same
//! blanked code (PY_CHAINS, fed Rust's lang::probe output). With any of the
//! three the `check` comparison runs only when --check is given too.

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
    parse: bool,
    edges: bool,
    chains: bool,
    untyped: bool,
    check: bool,
    ext: Vec<String>,
    repos: Vec<PathBuf>,
}

fn opts() -> Opts {
    let exe = std::env::current_exe().unwrap();
    let mut o = Opts {
        python: Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf(),
        rust: exe.with_file_name("seamcheck"),
        gate: false,
        staged: false,
        parse: false,
        edges: false,
        chains: false,
        untyped: false,
        check: false,
        ext: seamcheck::lang::ported_exts().iter().map(|s| s.to_string()).collect(),
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
            "--parse" => o.parse = true,
            "--edges" => o.edges = true,
            "--chains" => o.chains = true,
            "--untyped" => o.untyped = true,
            "--check" => o.check = true,
            "--ext" => {
                i += 1;
                o.ext = args[i].split(',').map(String::from).collect();
            }
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
                println!(
                    "parity [--python DIR] [--rust BIN] [--gate] [--staged] [--fixtures DIR] \
                     [--parse] [--edges] [--untyped] [--ext .ex,.exs] [--check] [REPO...]"
                );
                std::process::exit(0);
            }
            other => o.repos.push(PathBuf::from(other)),
        }
        i += 1;
    }
    if !o.parse && !o.edges && !o.chains {
        o.check = true;
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
    seamcheck::lang::ported(file) || !seamcheck::project::has_adapter(file)
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

/// Python's side of --parse: argv root, exts; one JSON object per file.
const PY_PARSE: &str = r#"
import sys, json, os, multiprocessing
from seamcheck import core, config as config_mod
root = os.path.abspath(sys.argv[1]); exts = tuple(sys.argv[2].split(","))
config, _ = config_mod.load(root, [])
pc = config.get("project", {})
inc = pc.get("include", ["**/*"])
files = core.list_files(root, inc, core.default_exclude(inc) + pc.get("exclude", []))
res = core._res_map(files)
todo = [(root, r, res) for r in files if os.path.splitext(r)[1] in exts and core.adapter_for(r)]
out = core._parse_parallel(todo, multiprocessing.get_context("fork")) if len(todo) > 64 else dict(core._parse_one(t) for t in todo)
def fd(f):
    return {"name": f.name, "kind": f.kind, "start": f.start, "end": f.end,
            "calls": [[c.name, c.qual, c.line, list(c.chain), c.col, c.targs] for c in f.calls],
            "refs": sorted(f.refs), "bound": f.bound, "recv": f.recv, "returns": f.returns, "targs": f.targs,
            "ptypes": f.ptypes, "relem": f.relem, "ns": f.ns}
def md(m):
    return {"name": m.name, "lines": m.lines, "parent": m.parent, "aliases": m.aliases, "decls": m.decls,
            "package": m.package, "uses": m.uses, "imported": {k: list(v) for k, v in m.imported.items()},
            "bound": m.bound, "using": m.using, "owner": m.owner, "bases": m.bases, "kind": m.kind,
            "methods": m.methods, "reexports": m.reexports, "funcs": [fd(f) for f in m.funcs]}
for _, rel, _ in todo:
    mods = out.get(rel, [])
    errs = [m[1] for m in mods if isinstance(m, tuple)]
    print(json.dumps({"file": rel, "error": errs[0]} if errs else {"file": rel, "modules": [md(m) for m in mods]}))
"#;

/// Python's side of --edges: the linked graph without bridges (and without
/// reading or writing a cached graph); [caller, callee, line, exact, callee's file].
const PY_EDGES: &str = r#"
import sys, json, os
from seamcheck import core, fingerprint as fp
root = os.path.abspath(sys.argv[1]); exts = tuple(sys.argv[2].split(","))
core._apply_bridges = lambda p: None
if os.environ.get("SEAMCHECK_UNTYPED"):
    # no type inference and no parameter flow: the infer lane's, not yet in Rust
    from seamcheck import infer
    infer.Types.chain = lambda self, *a, **k: None
    core._param_flow = lambda *a, **k: None
    core._param_receivers = lambda *a, **k: None
_load = fp.load
fp.load = lambda name, default: default if "-graph" in name else _load(name, default)
fp.save = lambda *a, **k: None
fp.prune = lambda *a, **k: None
p = core.load(root)
for fid, es in p.edges.items():
    f = p.funcs.get(fid)
    if f is None or os.path.splitext(f.file)[1] not in exts:
        continue
    for t, line, ex in es:
        g = p.funcs.get(t)
        print(json.dumps([fid, t, line, bool(ex), g.file if g else ""]))
"#;

/// Python's side of --chains: reads lang::probe's objects on stdin and
/// answers the same sites on the same code.
const PY_CHAINS: &str = r#"
import sys, json
from seamcheck import chains, lambdas, lang_other
TKS = ["jvm", "rust", "kotlin", "js", "swift"]
ASSIGN_TKS = ["js", "kotlin", "swift", "rust", "go", "jvm", "cpp", "php", "py"]
FAMS = ["js", "kotlin", "swift", "rust"]
def g(f):
    try:
        return f()
    except Exception:
        return "error"
for line in sys.stdin:
    d = json.loads(line)
    chains.CALL_TARGS, chains.TRAILING_CLOSURES = d["opts"]
    code = d["code"]
    out = {
        "file": d["file"],
        "recv": [[p, g(lambda: list(chains.receiver(code, p)))] for p, _ in d["recv"]],
        "expr": [[p, g(lambda: list(chains.expression(code, p)))] for p, _ in d["expr"]],
        "types": [[t, chains.type_name(t), chains.elem_type(t), chains.generic_name(t), lambdas.lambda_params(t),
                   lambdas.generic_param(t)] for t, *_ in d["types"]],
        "parens": [[s, [lambdas.param_types(tk, s) for tk in TKS],
                    [g(lambda: [[i, n] for i, n in lambdas.lambda_args(f, lambdas.split_top(s))]) for f in FAMS]]
                   for s, *_ in d["parens"]],
        "braces": [[s, g(lambda: lambdas.brace_lambda("kotlin", s)), g(lambda: lambdas.brace_lambda("swift", s))]
                   for s, *_ in d["braces"]],
        "assign": [g(lambda: [list(x) for x in lang_other._assignments(tk, code)]) for tk in ASSIGN_TKS],
    }
    print(json.dumps(out))
"#;

fn chain_parity(o: &Opts, repo: &Path, name: &str) -> usize {
    let exts = if o.ext.iter().all(|e| e == ".ex" || e == ".exs") {
        ".js,.jsx,.mjs,.cjs,.ts,.tsx,.mts,.qml,.rs,.kt,.kts,.java,.cs,.go,.c,.cpp,.cc,.cxx,.hpp,.hh,.h,.swift,.php,.py,.gd,.rb,.rake,.ru".to_string()
    } else {
        o.ext.join(",")
    };
    let rr = rs(o, repo, &["port-dump", "chains", "--ext", &exts]);
    let t = Instant::now();
    let mut child = Command::new("python3")
        .args(["-c", PY_CHAINS])
        .current_dir(repo)
        .env("PYTHONPATH", &o.python)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("python3");
    {
        use std::io::Write;
        let mut sin = child.stdin.take().unwrap();
        let input = rr.out.clone();
        std::thread::spawn(move || {
            let _ = sin.write_all(input.as_bytes());
        });
    }
    let po = child.wait_with_output().expect("python3");
    let pr = Ran {
        out: String::from_utf8_lossy(&po.stdout).into_owned(),
        err: String::from_utf8_lossy(&po.stderr).into_owned(),
        ms: t.elapsed().as_millis(),
    };
    let by_file = |v: Vec<Value>| -> BTreeMap<String, Value> {
        v.into_iter().map(|x| (x.get("file").and_then(|f| f.as_str()).unwrap_or("").to_string(), x)).collect()
    };
    let (pm, rm) = (by_file(json_lines(&pr, "python chains")), by_file(json_lines(&rr, "rust chains")));
    let (mut n, mut sites) = (0, 0);
    for (f, rv) in &rm {
        let Some(pv) = pm.get(f) else {
            n += 1;
            println!("  chains only rust: {f}");
            continue;
        };
        for part in ["recv", "expr", "types", "parens", "braces", "assign"] {
            let (a, b) = (pv[part].as_array().cloned().unwrap_or_default(), rv[part].as_array().cloned().unwrap_or_default());
            sites += b.len();
            for (x, y) in a.iter().zip(&b) {
                if x != y {
                    n += 1;
                    if n <= 40 {
                        let shorten = |v: &Value| {
                            let s = v.to_string();
                            if s.len() > 300 { format!("{}...", &s[..s.floor_char_boundary(300)]) } else { s }
                        };
                        println!("  chains {f} {part}: python {} | rust {}", shorten(x), shorten(y));
                    }
                }
            }
            if a.len() != b.len() {
                n += 1;
                println!("  chains {f} {part}: {} sites vs {}", a.len(), b.len());
            }
        }
    }
    println!(
        "{:<40} {:>9} {:>9} {:>9} {:>9}  ({} files, {} sites)",
        format!("{name} chains"),
        pm.len(),
        rm.len(),
        pr.ms,
        rr.ms,
        rm.len(),
        sites
    );
    n
}

fn py_inline(o: &Opts, repo: &Path, script: &str, exts: &str) -> Ran {
    run(Command::new("python3")
        .args(["-c", script])
        .arg(repo)
        .arg(exts)
        .current_dir(repo)
        .env("PYTHONPATH", &o.python)
        .env("SEAMCHECK_UNTYPED", if o.untyped { "1" } else { "" }))
}

fn json_lines(r: &Ran, who: &str) -> Vec<Value> {
    let out: Vec<Value> = r.out.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    if out.is_empty() && !r.err.trim().is_empty() {
        eprintln!("parity: {who} printed nothing; stderr:\n{}", r.err);
    }
    out
}

/// Where two JSON values first differ, as a path.
fn first_diff(a: &Value, b: &Value, path: String) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in x {
                match y.get(k) {
                    Some(w) => {
                        if let Some(d) = first_diff(v, w, format!("{path}.{k}")) {
                            return Some(d);
                        }
                    }
                    None => return Some(format!("{path}.{k}: only python")),
                }
            }
            y.keys().find(|k| !x.contains_key(*k)).map(|k| format!("{path}.{k}: only rust"))
        }
        (Value::Array(x), Value::Array(y)) => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                let label = v.get("name").and_then(|n| n.as_str()).map(|n| format!("[{n}]")).unwrap_or(format!("[{i}]"));
                if let Some(d) = first_diff(v, w, format!("{path}{label}")) {
                    return Some(d);
                }
            }
            (x.len() != y.len()).then(|| format!("{path}: {} items vs {}", x.len(), y.len()))
        }
        _ => (a != b).then(|| format!("{path}: {a} vs {b}")),
    }
}

fn parse_parity(o: &Opts, repo: &Path, name: &str) -> usize {
    let exts = o.ext.join(",");
    let pr = py_inline(o, repo, PY_PARSE, &exts);
    let rr = rs(o, repo, &["port-dump", "parse", "--ext", &exts]);
    let by_file = |v: Vec<Value>| -> BTreeMap<String, Value> {
        v.into_iter().map(|x| (x.get("file").and_then(|f| f.as_str()).unwrap_or("").to_string(), x)).collect()
    };
    let (pm, rm) = (by_file(json_lines(&pr, "python parse")), by_file(json_lines(&rr, "rust parse")));
    let mut n = 0;
    for (f, pv) in &pm {
        match rm.get(f) {
            None => {
                n += 1;
                println!("  parse only python: {f}");
            }
            Some(rv) => {
                if let Some(d) = first_diff(pv, rv, String::new()) {
                    n += 1;
                    if n <= 40 {
                        println!("  parse {f}: {d}");
                    }
                }
            }
        }
    }
    for f in rm.keys().filter(|f| !pm.contains_key(*f)) {
        n += 1;
        println!("  parse only rust: {f}");
    }
    let funcs = |m: &BTreeMap<String, Value>| -> usize {
        m.values()
            .filter_map(|v| v.get("modules").and_then(|x| x.as_array()))
            .flatten()
            .filter_map(|m| m.get("funcs").and_then(|x| x.as_array()).map(|a| a.len()))
            .sum()
    };
    println!(
        "{:<40} {:>9} {:>9} {:>9} {:>9}  ({} files, {} differ)",
        format!("{name} parse"),
        funcs(&pm),
        funcs(&rm),
        pr.ms,
        rr.ms,
        pm.len(),
        n
    );
    n
}

fn edge_parity(o: &Opts, repo: &Path, name: &str) -> usize {
    let exts = o.ext.join(",");
    let pr = py_inline(o, repo, PY_EDGES, &exts);
    let rr = rs(o, repo, &["port-dump", "edges", "--ext", &exts]);
    type E = (String, String, u64, bool);
    let row = |v: &Value| -> E {
        (
            v[0].as_str().unwrap_or("").into(),
            v[1].as_str().unwrap_or("").into(),
            v[2].as_u64().unwrap_or(0),
            v[3].as_bool().unwrap_or(false),
        )
    };
    let pv = json_lines(&pr, "python edges");
    let rv = json_lines(&rr, "rust edges");
    let mut pb: BTreeMap<E, usize> = BTreeMap::new();
    let mut into: BTreeMap<E, String> = BTreeMap::new();
    for v in &pv {
        let e = row(v);
        into.insert(e.clone(), v[4].as_str().unwrap_or("").to_string());
        *pb.entry(e).or_insert(0) += 1;
    }
    let mut rb: BTreeMap<E, usize> = BTreeMap::new();
    for v in &rv {
        *rb.entry(row(v)).or_insert(0) += 1;
    }
    let (mut n, mut unported) = (0, 0);
    let show = |e: &E| format!("{} -> {} :{}{}", e.0, e.1, e.2, if e.3 { "" } else { " (guess)" });
    for (e, c) in &pb {
        let rc = rb.get(e).copied().unwrap_or(0);
        if *c > rc {
            let callee_file = into.get(e).map(|s| s.as_str()).unwrap_or("");
            if !callee_file.is_empty() && !seamcheck::lang::ported(callee_file) {
                unported += c - rc;
                println!("  edge into an unported language ({callee_file}): {}", show(e));
                continue;
            }
            n += c - rc;
            if n <= 60 {
                println!("  edge only python ({}x): {}", c - rc, show(e));
            }
        }
    }
    for (e, c) in &rb {
        let pc = pb.get(e).copied().unwrap_or(0);
        if *c > pc {
            n += c - pc;
            if n <= 60 {
                println!("  edge only rust   ({}x): {}", c - pc, show(e));
            }
        }
    }
    let exact = |b: &BTreeMap<E, usize>| b.iter().filter(|(e, _)| e.3).map(|(_, c)| c).sum::<usize>();
    println!(
        "{:<40} {:>9} {:>9} {:>9} {:>9}  ({} / {} exact; {} into unported languages)",
        format!("{name} edges"),
        pv.len(),
        rv.len(),
        pr.ms,
        rr.ms,
        exact(&pb),
        exact(&rb),
        unported
    );
    n
}

fn main() {
    let o = opts();
    let mut total = 0;
    println!("{:<40} {:>9} {:>9} {:>9} {:>9}", "project", "py n", "rs n", "py ms", "rs ms");
    for repo in &o.repos {
        let name = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let before = total;
        if o.parse {
            total += parse_parity(&o, repo, &name);
        }
        if o.edges {
            total += edge_parity(&o, repo, &name);
        }
        if o.chains {
            total += chain_parity(&o, repo, &name);
        }
        if !o.check {
            if total == before {
                println!("  identical");
            }
            continue;
        }
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
