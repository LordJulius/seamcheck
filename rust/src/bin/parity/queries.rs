//! `parity --queries` / `--serve`: the query commands and the MCP server,
//! Python against Rust, over a spread of the project's own functions.
//!
//! --queries: a list of command lines (callers, callees, path, find,
//! context, search, impact, dead, stats; text and --json) picked from the
//! project's functions, run by both CLIs over one loaded project each
//! (Python: `__main__._main` with `core.load` handing back the loaded
//! project, PY_QUERIES; Rust: `port-dump queries`). Exit code and stdout
//! compare per command line, timings masked.
//!
//! --serve: the same picks as tools/call requests (plus initialize, ping,
//! tools/list, check and the error replies) piped into `seamcheck serve` on
//! both sides; each reply compares as JSON, `load_ms` set aside.

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use super::{run, Opts, Ran};

/// How many functions each run picks (env PARITY_PICKS, default 25).
fn picks() -> usize {
    std::env::var("PARITY_PICKS").ok().and_then(|v| v.parse().ok()).unwrap_or(25)
}

const PY_QUERIES: &str = r#"
import contextlib, io, json, sys
from seamcheck import core
from seamcheck import __main__ as cli
root, todo = sys.argv[1], sys.argv[2]
loaded = core.load(root, [])
core.load = lambda *a, **k: loaded
for line in open(todo):
    argv = json.loads(line)
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(io.StringIO()):
        try:
            code = cli._main(argv)
        except SystemExit as e:
            code = e.code if isinstance(e.code, int) else 2
        except Exception as e:
            code = -1
            print(f"EXC {type(e).__name__}: {e}")
    print(json.dumps({"argv": argv, "code": code, "out": buf.getvalue()}))
"#;

fn esc(name: &str) -> String {
    name.chars().map(|c| if r".^$*+?()[]{}|\".contains(c) { format!("\\{c}") } else { c.to_string() }).collect()
}

/// The functions to ask about: every k-th real function by id, and for each
/// one it reaches (a `path` target) and one far away (a chain that may not exist).
struct Pick {
    id: String,
    name: String,
    file: String,
    start: usize,
    short_mod: String,
    near: Option<String>,
    far: String,
}

fn pick(o: &Opts, repo: &Path) -> (Vec<Pick>, Vec<String>) {
    let p = seamcheck::project::Project::load(repo, &[]);
    let g = seamcheck::graph::Graph::load(&p);
    let mut ids: Vec<&String> = g.funcs.keys().filter(|id| g.func(id).map(|f| f.kind != "top").unwrap_or(false)).collect();
    ids.sort();
    let n = picks().min(ids.len());
    let step = if n == 0 { 1 } else { ids.len() / n };
    let chosen: Vec<&String> = (0..n).map(|i| ids[i * step + step / 2]).collect();
    let out = chosen
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let f = g.func(id).unwrap();
            let reach = g.reach(id, 3, false, true);
            let near = reach.iter().filter(|(_, d)| **d >= 2).map(|(k, _)| k.clone()).next().or_else(|| reach.keys().nth(1).cloned());
            let short_mod = f.module.rsplit(['.', '/']).next().unwrap_or(&f.module).to_string();
            Pick {
                id: id.to_string(),
                name: f.name.clone(),
                file: f.file.clone(),
                start: f.start,
                short_mod,
                near,
                far: chosen[(i + n / 2) % n].to_string(),
            }
        })
        .collect();
    let _ = o;
    (out, seamcheck::rules::deferred_rules(&p).into_iter().map(|(id, _)| id).collect())
}

fn argvs(ps: &[Pick]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<&str>> = vec![vec!["dead"], vec!["stats"], vec!["stats", "--json"], vec!["search", "TODO", "--limit", "50"]];
    let owned: Vec<Vec<String>> = ps
        .iter()
        .flat_map(|x| {
            let mut v: Vec<Vec<String>> = Vec::new();
            let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
            let top = x.file.split('/').next().unwrap_or("").to_string();
            v.push(s(&["callers", &x.id]));
            v.push(s(&["callers", &x.name, "--depth", "1"]));
            v.push(s(&["callers", &x.id, "--wide"]));
            v.push(s(&["callers", &x.id, "--fuzzy", "--depth", "3"]));
            v.push(s(&["callees", &x.id]));
            v.push(s(&["callees", &x.id, "--depth", "3", "--fuzzy"]));
            v.push(s(&["callees", &format!("{}.{}", x.short_mod, x.name)]));
            v.push(s(&["find", &x.name]));
            v.push(s(&["find", &format!("{}:{}", x.file, x.start)]));
            v.push(s(&["find", &format!("{top}/**:{}", x.name)]));
            v.push(s(&["context", &x.id]));
            v.push(s(&["context", &x.id, "--json"]));
            v.push(s(&["impact", &x.id]));
            v.push(s(&["impact", &x.id, "--json"]));
            v.push(s(&["impact", &x.file]));
            v.push(s(&["impact", &x.id, "--fuzzy", "--depth", "2", "--limit", "8"]));
            if let Some(n) = &x.near {
                v.push(s(&["path", &x.id, n]));
            }
            v.push(s(&["path", &x.id, &x.far]));
            v.push(s(&["path", &x.id, &x.far, "--fuzzy"]));
            v.push(s(&["search", &format!(r"\b{}\(", esc(&x.name)), "--json"]));
            v.push(s(&["search", &esc(&x.name), &format!("{top}/**"), "--code", "--limit", "30"]));
            v
        })
        .collect();
    let mut all: Vec<Vec<String>> = out.drain(..).map(|a| a.iter().map(|x| x.to_string()).collect()).collect();
    all.extend(owned);
    all
}

fn mask(s: &str) -> String {
    regex::Regex::new(r"\d+ ms\b").unwrap().replace_all(s, "N ms").into_owned()
}

/// `impact` from several functions: Python orders areas of one size by a
/// set's hash order (per process), so the areas compare as a set of blocks.
fn areas_unordered(out: &str) -> String {
    let multi = out.lines().any(|l| l.starts_with("change to ") && !l.starts_with("change to 1 functions"));
    if !multi {
        return out.to_string();
    }
    let mut head: Vec<&str> = Vec::new();
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for l in out.lines() {
        if l.starts_with("  ") && !l.starts_with("    ") {
            blocks.push(vec![l]);
        } else if let Some(b) = blocks.last_mut() {
            b.push(l);
        } else {
            head.push(l);
        }
    }
    blocks.sort();
    let mut v: Vec<&str> = head;
    v.extend(blocks.into_iter().flatten());
    v.join("\n") + "\n"
}

/// `context`: Python sorts the tests (a set) by file and line only, so two
/// tests at one line come in hash order; each test block compares sorted.
fn tests_unordered(out: &str) -> String {
    let mut v: Vec<String> = Vec::new();
    let mut block: Vec<String> = Vec::new();
    let mut inside = false;
    for l in out.lines() {
        if inside && l.starts_with("   ") {
            block.push(l.to_string());
            continue;
        }
        block.sort();
        v.append(&mut block);
        inside = l.starts_with("-- tests reaching it");
        v.push(l.to_string());
    }
    block.sort();
    v.append(&mut block);
    v.join("\n") + "\n"
}

fn lines_of(r: &Ran) -> Vec<Value> {
    r.out.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Where two outputs first differ, a few lines around it.
fn show_diff(a: &str, b: &str) -> String {
    let (x, y): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let k = x.iter().zip(&y).position(|(p, q)| p != q).unwrap_or(x.len().min(y.len()));
    let around = |v: &[&str]| v[k.min(v.len())..(k + 3).min(v.len())].join("\n      ");
    format!("line {} ({} vs {} lines)\n    py: {}\n    rs: {}", k + 1, x.len(), y.len(), around(&x), around(&y))
}

pub fn query_parity(o: &Opts, repo: &Path, name: &str) -> usize {
    let repo = &std::path::absolute(repo).unwrap_or_else(|_| repo.to_path_buf());
    let (ps, _) = pick(o, repo);
    let list = argvs(&ps);
    let tmp = std::env::temp_dir().join(format!("parity-queries-{}-{name}.jsonl", std::process::id()));
    std::fs::write(&tmp, list.iter().map(|a| json!(a).to_string() + "\n").collect::<String>()).unwrap();
    let pr = run(Command::new("python3").args(["-c", PY_QUERIES]).arg(repo).arg(&tmp).current_dir(repo).env("PYTHONPATH", &o.python));
    let rr = run(Command::new(&o.rust).args(["port-dump", "queries"]).arg(&tmp).current_dir(repo).env("SEAMCHECK_RS_QUIET", "1"));
    let _ = std::fs::remove_file(&tmp);
    let (pv, rv) = (lines_of(&pr), lines_of(&rr));
    if pv.len() != list.len() {
        eprintln!("parity: python answered {} of {} queries; stderr:\n{}", pv.len(), list.len(), pr.err);
    }
    let mut n = 0;
    let mut lines = 0;
    for (i, argv) in list.iter().enumerate() {
        let (p, r) = (pv.get(i), rv.get(i));
        let text = |v: &Value| {
            let t = mask(v["out"].as_str().unwrap_or(""));
            match argv[0].as_str() {
                "impact" => areas_unordered(&t),
                "context" if !argv.iter().any(|a| a == "--json") => tests_unordered(&t),
                "context" => {
                    let mut v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    sort_tests(&mut v);
                    serde_json::to_string_pretty(&v).unwrap_or(t)
                }
                _ => t,
            }
        };
        let (pc, po) = p.map(|v| (v["code"].clone(), text(v))).unwrap_or((Value::Null, String::new()));
        let (rc, ro) = r.map(|v| (v["code"].clone(), text(v))).unwrap_or((Value::Null, String::new()));
        lines += po.lines().count();
        if pc != rc || po != ro {
            n += 1;
            if n <= super::max_show() {
                println!("  {} : code {pc} vs {rc}; {}", argv.join(" "), show_diff(&po, &ro));
            }
        }
    }
    println!(
        "{:<40} {:>9} {:>9} {:>9} {:>9}  ({} queries, {} output lines, {} differ)",
        format!("{name} queries"),
        pv.len(),
        rv.len(),
        pr.ms,
        rr.ms,
        list.len(),
        lines,
        n
    );
    n
}

fn requests(ps: &[Pick], deferred: &[String]) -> Vec<String> {
    let mut msgs: Vec<Value> = vec![
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "parity", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": "p", "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": "l", "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": "r", "method": "resources/list"}),
        json!({"jsonrpc": "2.0", "id": "u", "method": "tools/call", "params": {"name": "nosuch"}}),
        json!({"jsonrpc": "2.0", "id": "m", "method": "tools/call", "params": {"name": "path", "arguments": {"to": "x", "bad": 1}}}),
        json!({"jsonrpc": "2.0", "id": "n", "method": "tools/call", "params": {"name": "callers", "arguments": {"name": "no_such_function_anywhere"}}}),
        json!({"jsonrpc": "2.0", "id": "i", "method": "tools/call", "params": {"name": "impact", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": "s", "method": "tools/call", "params": {"name": "stats"}}),
        json!({"jsonrpc": "2.0", "id": "c", "method": "tools/call", "params": {"name": "check", "arguments": {"skip": deferred}}}),
        json!({"jsonrpc": "2.0", "id": "ce", "method": "tools/call", "params": {"name": "check", "arguments": {"skip": deferred, "errors_only": true}}}),
        json!({"jsonrpc": "2.0", "id": "un", "method": "tools/call", "params": {"name": "unused"}}),
        json!({"jsonrpc": "2.0", "id": "uh", "method": "tools/call", "params": {"name": "unused", "arguments": {"since": "HEAD"}}}),
        json!({"jsonrpc": "2.0", "id": "t", "method": "tools/call", "params": {"name": "tests"}}),
        json!({"jsonrpc": "2.0", "id": "g", "method": "tools/call", "params": {"name": "gate"}}),
        json!([1, 2]),
    ];
    let mut k = 0;
    let mut call = |msgs: &mut Vec<Value>, tool: &str, args: Value| {
        k += 1;
        msgs.push(json!({"jsonrpc": "2.0", "id": k, "method": "tools/call", "params": {"name": tool, "arguments": args}}));
    };
    for x in ps {
        call(&mut msgs, "callers", json!({"name": x.id}));
        call(&mut msgs, "callers", json!({"name": x.name, "depth": 1, "wide": true}));
        call(&mut msgs, "callees", json!({"name": x.id, "depth": 3, "fuzzy": true}));
        call(&mut msgs, "find", json!({"name": x.name}));
        call(&mut msgs, "context", json!({"name": x.id}));
        call(&mut msgs, "path", json!({"from": x.id, "to": x.near.clone().unwrap_or(x.far.clone())}));
        call(&mut msgs, "path", json!({"from": x.id, "to": x.far}));
        call(&mut msgs, "impact", json!({"targets": [x.id]}));
        call(&mut msgs, "impact", json!({"targets": [x.file], "depth": 2, "fuzzy": true}));
        call(&mut msgs, "search", json!({"pattern": format!(r"\b{}\(", esc(&x.name)), "limit": 20, "code": true}));
        call(&mut msgs, "refs", json!({"name": x.name}));
    }
    let mut out: Vec<String> = msgs.iter().map(|m| m.to_string()).collect();
    out.insert(3, "{not json".into());
    out
}

fn serve(cmd: &mut Command, input: &str) -> Ran {
    use std::io::Write;
    let t = std::time::Instant::now();
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("serve");
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_string();
    let w = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let o = child.wait_with_output().unwrap();
    let _ = w.join();
    Ran {
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
        ms: t.elapsed().as_millis(),
    }
}

fn sort_tests(v: &mut Value) {
    for c in v.as_array_mut().into_iter().flatten() {
        if let Some(t) = c.get_mut("tests").and_then(|t| t.as_array_mut()) {
            t.sort_by_key(|x| (x["file"].as_str().unwrap_or("").to_string(), x["line"].as_u64(), x["id"].as_str().unwrap_or("").to_string()));
        }
    }
}

/// A reply with its tool's JSON text parsed (and `load_ms` set aside).
fn norm(mut v: Value) -> Value {
    if let Some(text) = v.pointer("/result/content/0/text").and_then(|t| t.as_str()) {
        let mut inner: Value = serde_json::from_str(text).unwrap_or(Value::String(text.into()));
        if let Some(m) = inner.as_object_mut() {
            m.remove("load_ms");
        }
        sort_tests(&mut inner);
        // a bad regex is reported by both, each in its own engine's words (PORT.md, known differences)
        for f in inner.as_array_mut().into_iter().flatten() {
            if let Some(msg) = f.get("message").and_then(|m| m.as_str()) {
                if let Some(k) = msg.find(": ").filter(|_| msg.starts_with("bad pattern in rule ")) {
                    f["message"] = Value::String(msg[..k].to_string());
                }
            }
        }
        v["result"]["content"][0]["text"] = inner;
    }
    v
}

pub fn serve_parity(o: &Opts, repo: &Path, name: &str) -> usize {
    let (ps, deferred) = pick(o, repo);
    let reqs = requests(&ps, &deferred);
    let input = reqs.join("\n") + "\n";
    let pr = serve(Command::new("python3").args(["-m", "seamcheck", "serve"]).current_dir(repo).env("PYTHONPATH", &o.python), &input);
    let rr = serve(Command::new(&o.rust).arg("serve").current_dir(repo).env("SEAMCHECK_RS_QUIET", "1"), &input);
    let (pv, rv) = (lines_of(&pr), lines_of(&rr));
    let mut n = 0;
    let mut seen: HashSet<String> = HashSet::new();
    for i in 0..pv.len().max(rv.len()) {
        let (p, r) = (pv.get(i).cloned().map(norm), rv.get(i).cloned().map(norm));
        if p != r {
            n += 1;
            let id = p.as_ref().or(r.as_ref()).map(|v| v["id"].to_string()).unwrap_or_default();
            if n <= super::max_show() && seen.insert(id.clone()) {
                let d = match (&p, &r) {
                    (Some(a), Some(b)) => super::first_diff(a, b, String::new()).unwrap_or_default(),
                    _ => "missing reply".into(),
                };
                println!("  serve reply {i} (id {id}): {}", seamcheck::pyre::take_chars(&d, 400));
            }
        }
    }
    if pv.is_empty() {
        eprintln!("parity: python serve printed nothing; stderr:\n{}", pr.err);
    }
    println!(
        "{:<40} {:>9} {:>9} {:>9} {:>9}  ({} requests, {} differ)",
        format!("{name} serve"),
        pv.len(),
        rv.len(),
        pr.ms,
        rr.ms,
        reqs.len(),
        n
    );
    n
}
