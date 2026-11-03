//! Cargo.toml, pyproject.toml / requirements*.txt and go.mod (deps._cargo,
//! deps._python, deps._gomod).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use indexmap::IndexMap;
use serde_json::Value;

use super::fsx::{self, basename, dirname, glob, home, join, line_of};
use super::{dyn_rx, escape, find_after, has_word, lit, py_word, Ctx, Dep, Where};
use crate::pyre::{strip, Rx};

fn toml_json(text: &str) -> Option<Value> {
    text.parse::<toml::Table>().ok().map(|t| crate::config::toml_to_json(toml::Value::Table(t)))
}

/// The (package name, `[lib] name`) of a Cargo.toml, None when unreadable.
fn cargo_names(
    path: &str,
    cache: &mut HashMap<String, Option<(Option<String>, String)>>,
) -> Option<(Option<String>, String)> {
    cache
        .entry(path.to_string())
        .or_insert_with(|| {
            let t = String::from_utf8(std::fs::read(path).ok()?).ok()?;
            let man = toml_json(&t)?;
            let name = man.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()).map(String::from);
            let lib =
                man.get("lib").and_then(|l| l.get("name")).and_then(|n| n.as_str()).unwrap_or("").replace('-', "_");
            Some((name, lib))
        })
        .clone()
}

/// A crate's `[lib] name` when it differs (rustdesk -> librustdesk), from its
/// source: a path dependency, or the cargo registry / git checkouts (`_cargo_lib_name`).
fn cargo_lib_name(
    crate_: &str,
    path: Option<&str>,
    cache: &mut HashMap<String, Option<(Option<String>, String)>>,
) -> String {
    let home = std::env::var("CARGO_HOME").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| join(&home(), ".cargo"));
    let mut cands: Vec<String> = path.map(|p| vec![join(p, "Cargo.toml")]).unwrap_or_default();
    cands.extend(glob(&join(&join(&join(&home, "registry/src/*"), &format!("{crate_}-[0-9]*")), "Cargo.toml")));
    cands.extend(glob(&join(&home, "git/checkouts/*/*/Cargo.toml")));
    for c in cands {
        if let Some((Some(name), lib)) = cargo_names(&c, cache) {
            if name == crate_ {
                return lib;
            }
        }
    }
    String::new()
}

fn table_names(v: Option<&Value>) -> Vec<(String, Value)> {
    match v {
        Some(Value::Object(o)) => o.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => Vec::new(),
    }
}

pub fn cargo(ctx: &Ctx, mrel: &str) -> Vec<Dep> {
    static USE: OnceLock<Rx> = OnceLock::new();
    let mdir = dirname(mrel);
    let text = ctx.read(mrel);
    let Some(man) = toml_json(&text) else { return Vec::new() };
    let mut tables = vec![
        (table_names(man.get("dependencies")), "runtime"),
        (table_names(man.get("dev-dependencies")), "test"),
        (table_names(man.get("build-dependencies")), "tool"),
    ];
    if let Some(Value::Object(t)) = man.get("target") {
        for v in t.values() {
            tables.push((table_names(v.get("dependencies")), "runtime"));
        }
    }
    let mut deps: IndexMap<String, Dep> = IndexMap::new();
    let mut crate_of: HashMap<String, String> = HashMap::new();
    let mut cache = HashMap::new();
    for (tbl, scope) in &tables {
        for (name, spec) in tbl {
            let e = escape(name);
            let pos =
                dyn_rx(&format!(r"(?m)^\s*{e}\s*=|\[[\w.-]*dependencies\.{e}\]")).search(&text).map(|m| m.start());
            let line = pos.map(|p| line_of(&text, p)).unwrap_or(1);
            deps.entry(name.clone()).or_insert_with(|| Dep::new("cargo", name, mrel, line, scope));
            crate_of.insert(name.replace('-', "_"), name.clone());
            let real = spec.get("package").map(crate::config::py_str).unwrap_or_else(|| name.clone());
            let path = spec
                .get("path")
                .filter(|p| crate::config::truthy(Some(*p)))
                .map(|p| join(&fsx::abs_join(ctx.root, mdir), &crate::config::py_str(p)));
            let lib = cargo_lib_name(&real, path.as_deref(), &mut cache);
            if !lib.is_empty() {
                crate_of.insert(lib, name.clone());
            }
        }
    }
    static UNGUARDED: OnceLock<Rx> = OnceLock::new();
    // (?<![\w:]) guards the first alternative only
    let used = lit(&USE, r"(\w+)\s*::|\bextern\s+crate\s+(\w+)|\buse\s+(\w+)\s*;");
    let rescue = lit(&UNGUARDED, r"\bextern\s+crate\s+(\w+)|\buse\s+(\w+)\s*;");
    for rel in ctx.walk(mdir, &[".rs"], &["Cargo.toml"]) {
        let code = crate::scan::blank_text(&ctx.read(&rel), "rust", false);
        let mut seen: HashSet<String> = HashSet::new();
        for r in find_after(used, &code, |c| py_word(c) || c == ':', |c| c.get(1).is_some(), Some(rescue)) {
            let w = if r.ngroups() == 3 { [r.s(1), r.s(2), r.s(3)] } else { [r.s(1), r.s(2), ""] };
            let w = w.into_iter().find(|x| !x.is_empty()).unwrap_or("");
            if let Some(dn) = crate_of.get(w) {
                if seen.insert(dn.clone()) {
                    deps[dn].add(&rel, line_of(&code, r.start()), None);
                }
            }
        }
    }
    deps.into_values().collect()
}

const PY_ALIAS: &[(&str, &[&str])] = &[
    ("pyyaml", &["yaml"]),
    ("beautifulsoup4", &["bs4"]),
    ("pillow", &["PIL"]),
    ("scikit-learn", &["sklearn"]),
    ("opencv-python", &["cv2"]),
    ("opencv-python-headless", &["cv2"]),
    ("python-dateutil", &["dateutil"]),
    ("protobuf", &["google.protobuf"]),
    ("attrs", &["attr", "attrs"]),
    ("python-dotenv", &["dotenv"]),
    ("typing-extensions", &["typing_extensions"]),
    ("pyjwt", &["jwt"]),
    ("msgpack-python", &["msgpack"]),
    ("stashapp-tools", &["stashapi"]),
];

fn norm(n: &str) -> String {
    static SEP: OnceLock<Rx> = OnceLock::new();
    lit(&SEP, r"[-_.]+").sub("-", n).to_lowercase()
}

/// The interpreter's import path (`sys.path`), as `python3` on PATH has it.
fn sys_path() -> Vec<String> {
    std::process::Command::new("python3")
        .args(["-c", "import sys; print('\\n'.join(sys.path))"])
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default()
}

/// Installed distributions -> the modules they provide (dist-info `top_level.txt`).
fn py_top_levels() -> &'static HashMap<String, Vec<String>> {
    static TOPS: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    TOPS.get_or_init(|| {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        let mut paths = sys_path();
        paths.extend(glob(".venv/lib/python*/site-packages"));
        for sp in paths {
            for di in glob(&join(&sp, "*.dist-info")) {
                let b = basename(&di);
                let name = norm(b.rsplit_once('-').map(|x| x.0).unwrap_or(b));
                let tl = join(&di, "top_level.txt");
                if std::path::Path::new(&tl).exists() {
                    out.entry(name).or_insert_with(|| {
                        fsx::read(std::path::Path::new(&tl)).split_whitespace().map(String::from).collect()
                    });
                }
            }
        }
        out
    })
}

fn strs(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

pub fn python(ctx: &Ctx, mrel: &str) -> Vec<Dep> {
    static DEVTEST: OnceLock<Rx> = OnceLock::new();
    static NAME: OnceLock<Rx> = OnceLock::new();
    static IMPORT: OnceLock<Rx> = OnceLock::new();
    let mdir = dirname(mrel);
    let text = ctx.read(mrel);
    let mut reqs: Vec<(String, &str, usize)> = Vec::new();
    if mrel.ends_with(".toml") {
        let Some(man) = toml_json(&text) else { return Vec::new() };
        let proj = man.get("project");
        for s in strs(proj.and_then(|p| p.get("dependencies"))) {
            reqs.push((s, "runtime", 0));
        }
        if let Some(Value::Object(o)) = proj.and_then(|p| p.get("optional-dependencies")) {
            for grp in o.values() {
                reqs.extend(strs(Some(grp)).into_iter().map(|s| (s, "dev", 0)));
            }
        }
        if let Some(Value::Object(o)) = man.get("dependency-groups") {
            for grp in o.values() {
                reqs.extend(strs(Some(grp)).into_iter().map(|s| (s, "dev", 0)));
            }
        }
        let poetry = man.get("tool").and_then(|t| t.get("poetry"));
        for (n, _) in table_names(poetry.and_then(|p| p.get("dependencies"))) {
            if n != "python" {
                reqs.push((n, "runtime", 0));
            }
        }
        for (n, _) in table_names(poetry.and_then(|p| p.get("dev-dependencies"))) {
            reqs.push((n, "dev", 0));
        }
    } else {
        let scope = if lit(&DEVTEST, r"dev|test").is_match(basename(mrel)) { "dev" } else { "runtime" };
        for (i, ln) in text.split('\n').enumerate() {
            let ln = strip(ln.split('#').next().unwrap_or(""));
            if !ln.is_empty() && !ln.starts_with('-') {
                reqs.push((ln.to_string(), scope, i + 1));
            }
        }
    }
    let tops = py_top_levels();
    let mut deps: IndexMap<String, Dep> = IndexMap::new();
    let mut mod_of: HashMap<String, String> = HashMap::new();
    for (spec, scope, line) in reqs {
        let Some(m) = lit(&NAME, r"[A-Za-z0-9_.-]+").match_start(strip(&spec)) else { continue };
        let name = m.group0().to_string();
        let line = if line == 0 { line_of(&text, text.find(&name).unwrap_or(0)) } else { line };
        let key = norm(&name);
        let d = deps.entry(key.clone()).or_insert_with(|| Dep::new("python", &name, mrel, line, scope));
        let mut mods: Vec<String> = tops
            .get(&key)
            .filter(|m| !m.is_empty())
            .cloned()
            .or_else(|| PY_ALIAS.iter().find(|a| a.0 == key).map(|a| a.1.iter().map(|s| s.to_string()).collect()))
            .unwrap_or_default();
        if mods.is_empty() {
            d.guessed = true;
            mods = vec![name.replace('-', "_").to_lowercase()];
        }
        for mo in mods {
            mod_of.insert(mo, key.clone());
        }
    }
    let import = lit(&IMPORT, r"(?m)^\s*(?:from\s+([\w.]+)\s+import|import\s+([\w., ]+))");
    for rel in ctx.walk(mdir, &[".py", ".pyw"], &["pyproject.toml", "setup.py"]) {
        let code = crate::scan::blank_text(&ctx.read(&rel), "python", false);
        let mut seen: HashSet<String> = HashSet::new();
        for r in import.find_iter(&code) {
            let mos: Vec<String> = match r.get(1) {
                Some(g) => vec![g.to_string()],
                None => r.s(2).split(',').map(|x| strip(x).split(' ').next().unwrap_or("").to_string()).collect(),
            };
            for mo in mos {
                let parts: Vec<&str> = mo.split('.').collect();
                for k in (1..=parts.len()).rev() {
                    if let Some(dn) = mod_of.get(&parts[..k].join(".")) {
                        if seen.insert(dn.clone()) {
                            deps[dn].add(&rel, line_of(&code, r.start()), None);
                            break;
                        }
                    }
                }
            }
        }
    }
    // tools (pytest, ruff, mypy...): named in config / CI / scripts
    let tools: Vec<std::sync::Arc<Rx>> =
        deps.values().map(|d| dyn_rx(&format!(r"\[tool\.{}\b", escape(&d.name)))).collect();
    let exts = [".toml", ".ini", ".cfg", ".yml", ".yaml", ".sh", "Makefile", "justfile"];
    for rel in ctx.walk(mdir, &exts, &["pyproject.toml", "setup.py"]) {
        let raw = ctx.read(&rel);
        for d in deps.values_mut() {
            if d.code.is_empty() && rel != mrel && has_word(&raw, &d.name) {
                d.add(&rel, 1, Some(Where::Config));
            }
        }
        if rel == mrel {
            for (d, tool) in deps.values_mut().zip(&tools) {
                if tool.is_match(&raw) {
                    d.add(&rel, 1, Some(Where::Config));
                }
            }
        }
    }
    deps.into_values().collect()
}

pub fn gomod(ctx: &Ctx, mrel: &str) -> Vec<Dep> {
    static REQ: OnceLock<Rx> = OnceLock::new();
    static IMPORT: OnceLock<Rx> = OnceLock::new();
    let mdir = dirname(mrel);
    let text = ctx.read(mrel);
    let mut deps: IndexMap<String, Dep> = IndexMap::new();
    for m in
        lit(&REQ, r"(?m)^\s*(?:require\s+)?([\w.-]+\.[\w./-]+)\s+v[\w.+-]+(\s*//\s*indirect)?\s*$").find_iter(&text)
    {
        if m.get(2).is_none_or(|g| g.is_empty()) {
            let name = m.s(1).to_string();
            deps.insert(name.clone(), Dep::new("go", &name, mrel, line_of(&text, m.start()), "runtime"));
        }
    }
    for rel in ctx.walk(mdir, &[".go"], &["go.mod"]) {
        let raw = ctx.read(&rel);
        for r in lit(&IMPORT, r#""([\w.-]+\.[\w./-]+)""#).find_iter(&raw) {
            let path = r.s(1);
            for (name, d) in deps.iter_mut() {
                if path == name || path.starts_with(&format!("{name}/")) {
                    d.add(&rel, line_of(&raw, r.start()), None);
                }
            }
        }
    }
    deps.into_values().collect()
}
