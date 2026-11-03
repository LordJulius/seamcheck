//! Which tests test something (tidy.test_audit): per test function, how many
//! product functions it reaches (exact edges), whether it reads a product
//! source file as text (a grep test), its suite. The graph stops at a process
//! boundary and at the router; a test that runs a repo script or requests a
//! routed path reaches what's behind them.

use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

use indexmap::IndexMap;
use rayon::prelude::*;
use serde_json::{json, Value};

use super::{dirname, join, normpath, Tidy};
use crate::config::list_or;
use crate::model::Func;
use crate::pyre::Rx;

#[derive(Debug, Clone)]
pub struct TestRow {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub suite: String,
    pub reach: usize,
    pub reads_source: Vec<String>,
    pub runs: Vec<String>,
}

impl TestRow {
    pub fn json(&self) -> Value {
        json!({"id": self.id, "file": self.file, "line": self.line, "suite": self.suite, "reach": self.reach,
               "reads_source": self.reads_source, "runs": self.runs})
    }
}

macro_rules! rx {
    ($pat:expr) => {{
        static R: OnceLock<Rx> = OnceLock::new();
        R.get_or_init(|| Rx::lit($pat))
    }};
}

const DEPTH: usize = 5;

/// A Phoenix route: (verb, path pattern, Module.action).
type Route = (String, regex::Regex, String);

/// Every test function (or just the ids in `only`), audited.
pub fn test_audit(t: &Tidy, only: Option<&HashSet<String>>) -> Vec<TestRow> {
    let g = t.g;
    let product = |rel: &str| t.p.file_set.contains(rel) && !t.is_test(rel);
    // product types by short name (a Kotlin enum, a struct): a test that names
    // one exercises it even with no call edge (an enum's entries aren't functions)
    let types: HashSet<&str> = g
        .short
        .iter()
        .filter(|(_, mods)| mods.iter().any(|n| g.modules.get(n).is_some_and(|m| product(&m.file))))
        .map(|(k, _)| k.as_str())
        .collect();
    let suites = crate::tidy::cfg(t.p).get("suites").and_then(|v| v.as_object());
    let routes: OnceLock<Vec<Route>> = OnceLock::new();
    let tests: Vec<&Func> = t
        .funcs()
        .filter(|f| f.kind == "test" && only.map_or(true, |o| o.contains(&f.id)))
        .filter(|f| !(f.file.ends_with(".ex") || f.file.ends_with(".exs")) || f.name.starts_with("test ") || f.name.starts_with("property "))
        .collect();
    tests
        .par_iter()
        .map(|tf| {
            let reach = g.reach(&tf.id, DEPTH, false, true);
            let in_product = |fid: &str| g.funcs.contains_key(fid) && g.func(fid).is_some_and(|f| product(&f.file));
            let mut prod = reach.keys().filter(|fid| in_product(fid)).count();
            let raw = t.raw_lines(tf).join("\n");
            if prod == 0 && !types.is_empty() {
                let named: HashSet<&str> =
                    rx!(r"\b[A-Z]\w+\b").findall(&raw).into_iter().filter(|w| types.contains(w)).collect();
                prod = named.len();
            }
            if prod == 0 {
                let routed = self::routed(t, tf, &reach, routes.get_or_init(|| self::routes(t)));
                let mut hit: HashSet<String> = routed.iter().cloned().collect();
                for r in &routed {
                    hit.extend(g.reach(r, DEPTH, false, true).into_keys().filter(|fid| in_product(fid)));
                }
                prod = hit.len();
            }
            let named = repo_paths(t, &tf.file, rx!(r#"["'`]([\w./-]+\.\w+)["'`]"#).findall(&raw));
            let mut reads: Vec<String> = named.iter().filter(|p| product(p)).cloned().collect();
            // a test that reads a repo file no language parses (compose.yml, a
            // firewall table, a unit) checks that file: it is the product there
            let mut configs: Vec<String> =
                named.iter().filter(|p| !t.p.file_set.contains(*p) && !t.is_test(p)).cloned().collect();
            reads.sort();
            configs.sort();
            let suite = suites
                .and_then(|s| s.iter().find(|(_, globs)| crate::glob::match_any(&tf.file, &globs_of(globs))))
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| tf.file.split('/').next().unwrap_or("").to_string());
            let runs = if prod == 0 { runs_scripts(t, tf, &reach) } else { vec![] };
            reads.retain(|r| !runs.contains(r)); // running it isn't grepping it
            configs.retain(|c| !runs.contains(c));
            let reach_n = if prod > 0 {
                prod
            } else if !runs.is_empty() {
                runs.len()
            } else {
                configs.len()
            };
            TestRow {
                id: tf.id.clone(),
                file: tf.file.clone(),
                line: tf.start,
                suite,
                reach: reach_n,
                reads_source: reads,
                runs,
            }
        })
        .collect()
}

fn globs_of(v: &Value) -> Vec<String> {
    let mut m = serde_json::Map::new();
    m.insert("g".into(), v.clone());
    list_or(&m, "g", &[])
}

/// The repo files these path strings name: from the root ("scripts/x.py",
/// "./x"), or from the test's own folder (Path.expand("../../scripts/x.py", __DIR__)).
fn repo_paths(t: &Tidy, test_file: &str, cands: Vec<&str>) -> BTreeSet<String> {
    let here = dirname(test_file);
    let mut out = BTreeSet::new();
    for cand in cands {
        for rel in [normpath(cand), normpath(&join(here, cand))] {
            if !rel.starts_with("..") && !rel.starts_with('/') && (t.p.file_set.contains(&rel) || t.p.path(&rel).is_file()) {
                out.insert(rel);
                break;
            }
        }
    }
    out
}

fn spawn() -> &'static Rx {
    rx!(r"System\.cmd|System\.shell|Port\.open|:os\.cmd|\bsubprocess\.|\bexecFile(?:Sync)?\b|\bspawn(?:Sync)?\s*\(|ProcessBuilder|Runtime\.getRuntime\(\)\.exec|\bexecv?p?\(")
}

/// Repo scripts a test runs as a subprocess (System.cmd("python3",
/// ["scripts/lib/x.py"]), Port.open, subprocess.run, spawn); the spawn may sit
/// in a helper of the test file, the path in a module attribute.
fn runs_scripts(t: &Tidy, tf: &Func, reach: &IndexMap<String, usize>) -> Vec<String> {
    let g = t.g;
    let mut bodies: Vec<String> = Vec::new();
    for fid in reach.keys() {
        if let Some(f) = g.func(fid) {
            if f.file == tf.file && t.module(f).is_some() {
                bodies.push(t.raw_lines(f).join("\n"));
            }
        }
    }
    if !bodies.iter().any(|b| spawn().search(b).is_some()) {
        return vec![];
    }
    let mut text = bodies.join("\n");
    if let Some(m) = t.module(tf) {
        text.push('\n');
        text.push_str(&m.raw.join("\n"));
    }
    let found = rx!(r#"["'`]((?:\./)?[\w.-]+(?:/[\w.-]+)+)["'`]"#).findall(&text);
    repo_paths(t, &tf.file, found).into_iter().filter(|p| !t.is_test(p)).collect()
}

const RESOURCE: &[(&str, &str, &str)] = &[
    ("index", "get", ""),
    ("show", "get", "/:id"),
    ("create", "post", ""),
    ("update", "put", "/:id"),
    ("delete", "delete", "/:id"),
    ("new", "get", "/new"),
    ("edit", "get", "/:id/edit"),
];

/// Phoenix routes from every module that `use`s a router: a scope's path
/// and alias prefix the routes inside it (tidy._routes).
fn routes(t: &Tidy) -> Vec<Route> {
    let opens = rx!(r"\bdo\s*$|\bfn\b.*->\s*$");
    let scope = rx!(r#"^\s*scope\s+"([^"]*)"(?:\s*,\s*([A-Z][\w.]*))?"#);
    let route = rx!(r#"^\s*(get|post|put|patch|delete|head|options|live|resources)\s+"([^"]*)",\s*([A-Z][\w.]*)(?:,\s*:(\w+))?"#);
    let router = rx!(r"use\s+(Phoenix\.Router|\w+,\s*:router)");
    let end = rx!(r"^\s*end\b");
    let mut out = Vec::new();
    for m in t.g.modules.values() {
        if !m.file.ends_with(".ex") || !m.raw.iter().any(|l| router.search(l).is_some()) {
            continue;
        }
        let mut stack: Vec<(String, String)> = Vec::new();
        for (line, code) in m.raw.iter().zip(m.code.iter()) {
            if let Some(sc) = scope.match_start(line) {
                if opens.search(code).is_some() {
                    stack.push((sc.s(1).to_string(), sc.s(2).to_string()));
                    continue;
                }
            }
            if let Some(r) = route.match_start(line) {
                let (verb, path, md, action) = (r.s(1), r.s(2), r.s(3), r.s(4));
                let prefix: String = stack.iter().map(|(p, _)| p.as_str()).collect();
                let alias: Vec<&str> = stack.iter().map(|(_, a)| a.as_str()).filter(|a| !a.is_empty()).collect();
                let full = if alias.is_empty() { md.to_string() } else { format!("{}.{md}", alias.join(".")) };
                let acts: Vec<(&str, &str, &str)> = match verb {
                    "live" => vec![("get", "", "mount")],
                    "resources" => RESOURCE.iter().map(|(a, v, suf)| (*v, *suf, *a)).collect(),
                    _ => vec![(verb, "", action)],
                };
                for (v, suffix, a) in acts {
                    out.push((v.to_string(), route_pattern(&format!("{prefix}{path}{suffix}")), format!("{full}.{a}")));
                }
                if opens.search(code).is_none() {
                    continue;
                }
            }
            if opens.search(code).is_some() {
                stack.push((String::new(), String::new()));
            } else if end.match_start(code).is_some() && !stack.is_empty() {
                stack.pop();
            }
        }
    }
    out
}

/// "/api/car/:id" -> a regex: `:param` is one segment, `*glob` the rest.
fn route_pattern(path: &str) -> regex::Regex {
    let parts: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with(':') {
                "[^/]+".to_string()
            } else if s.starts_with('*') {
                ".*".to_string()
            } else {
                regex::escape(s)
            }
        })
        .collect();
    regex::Regex::new(&format!("^/{}/?$", parts.join("/"))).unwrap()
}

/// Controller actions a test's requests reach through the router
/// (`get(conn, ~p"/api/car")`, `conn |> post("/api/x", body)`).
fn routed(t: &Tidy, tf: &Func, reach: &IndexMap<String, usize>, routes: &[Route]) -> Vec<String> {
    if !tf.file.ends_with(".exs") || routes.is_empty() {
        return vec![];
    }
    let g = t.g;
    let mut lines: Vec<&str> = t.raw_lines(tf).iter().map(|s| s.as_str()).collect();
    for fid in reach.keys() {
        if let Some(f) = g.func(fid) {
            if f.file == tf.file && f.id != tf.id && t.module(f).is_some() {
                lines.extend(t.raw_lines(f).iter().map(|s| s.as_str()));
            }
        }
    }
    let text = lines.join("\n");
    let mut hit: BTreeSet<String> = BTreeSet::new();
    for m in rx!(r#"\b(get|post|put|patch|delete|head|options|live)\(\s*(?:[\w.()]+\s*,\s*)?~?p?"([^"]*)""#).find_iter(&text) {
        let verb = m.s(1);
        let path = m.s(2).split('?').next().unwrap_or("");
        let path = rx!(r"#\{[^}]*\}").sub("x", path);
        for (v, pat, fid) in routes {
            if (v == verb || verb == "live") && pat.is_match(&path) && g.funcs.contains_key(fid) {
                hit.insert(fid.clone());
            }
        }
    }
    hit.into_iter().collect()
}
