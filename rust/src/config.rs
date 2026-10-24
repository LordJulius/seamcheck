//! Config: the target project's own files, merged (config.py).
//!
//! `seamcheck.toml` at the root, every `seamcheck.d/*.toml` beside it, and any
//! `--config FILE` (a file or a folder of .toml), merged in that order. Tables
//! merge keys (include / exclude / packs add up); `[[rule]]`-style lists
//! concatenate, each entry remembering its file (`_file`). Packs join as
//! `<pack>/<id>`; a project rule with a pack rule's id overrides its keys.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

use crate::glob::close_match;

pub const LISTS: [&str; 4] = ["rule", "invariant", "contract", "bridge"];
pub const TABLES: [&str; 6] = ["project", "runtime", "deps", "tidy", "gate", "precise"];
const ADDITIVE: [&str; 3] = ["include", "exclude", "packs"];
const DEFAULT_PACKS: [&str; 1] = ["security"];
const SEVERITIES: [&str; 3] = ["error", "warn", "info"];

pub const RULE_KEYS: &[&str] = &[
    "id", "kind", "pattern", "in", "allow", "why", "severity", "if", "then", "scope", "window", "text", "defined",
    "used", "check", "from", "when", "to", "depth", "unless", "message", "mode", "report", "skip", "strict", "ignore",
    "package", "path", "func", "ordered", "source", "source_entry", "source_param", "source_not_param", "views",
    "extra_severity", "match", "fallback", "normalize", "rewrite", "run", "timeout", "sep", "wildcard", "lint",
    "options", "gate", "bar", "controls", "menus", "repo", "files", "prefix", "fix", "tighten", "via", "layers",
    "level", "allow_to", "fuzzy",
];
const INVARIANT_KEYS: &[&str] = &["id", "each", "where", "when", "assert", "for", "key", "why", "severity"];
const CONTRACT_KEYS: &[&str] = &["id", "act", "when", "assert", "why", "severity"];
const BRIDGE_KEYS: &[&str] = &["id", "from", "to", "match", "sep", "wildcard", "normalize", "why"];

fn keys_for(kind: &str) -> &'static [&'static str] {
    match kind {
        "rule" => RULE_KEYS,
        "invariant" => INVARIANT_KEYS,
        "contract" => CONTRACT_KEYS,
        _ => BRIDGE_KEYS,
    }
}

/// The packs seamcheck ships, compiled in (seamcheck/packs/*.toml).
pub const PACKS: &[(&str, &str)] = &[
    ("compose", include_str!("../../seamcheck/packs/compose.toml")),
    ("express", include_str!("../../seamcheck/packs/express.toml")),
    ("hono", include_str!("../../seamcheck/packs/hono.toml")),
    ("phoenix", include_str!("../../seamcheck/packs/phoenix.toml")),
    ("qml", include_str!("../../seamcheck/packs/qml.toml")),
    ("react", include_str!("../../seamcheck/packs/react.toml")),
    ("security", include_str!("../../seamcheck/packs/security.toml")),
];

pub fn available_packs() -> Vec<String> {
    let mut v: Vec<String> = PACKS.iter().map(|p| p.0.to_string()).collect();
    v.sort();
    v
}

fn pack_text(name: &str) -> Option<&'static str> {
    PACKS.iter().find(|p| p.0 == name).map(|p| p.1)
}

pub type Rule = Map<String, Value>;

#[derive(Default, Debug, Clone)]
pub struct Config {
    /// The merged config as one JSON object (tables + lists).
    pub v: Map<String, Value>,
}

impl Config {
    pub fn table(&self, t: &str) -> &Map<String, Value> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        self.v.get(t).and_then(|x| x.as_object()).unwrap_or_else(|| EMPTY.get_or_init(Map::new))
    }
    pub fn list(&self, k: &str) -> &[Value] {
        self.v.get(k).and_then(|x| x.as_array()).map(|a| a.as_slice()).unwrap_or(&[])
    }
    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.list("rule").iter().filter_map(|r| r.as_object())
    }
}

pub fn toml_to_json(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect()),
    }
}

fn parse(text: &str) -> Result<Map<String, Value>, String> {
    let t: toml::Table = toml::from_str(text).map_err(|e| e.message().to_string())?;
    match toml_to_json(toml::Value::Table(t)) {
        Value::Object(m) => Ok(m),
        _ => unreachable!(),
    }
}

fn sorted_tomls(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn files_for(root: &Path, extra: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let main = root.join("seamcheck.toml");
    if main.exists() {
        out.push(main);
    }
    out.extend(sorted_tomls(&root.join("seamcheck.d")));
    for p in extra {
        let p = std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p));
        if p.is_dir() {
            out.extend(sorted_tomls(&p));
        } else if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn rel_name(path: &Path, root: &Path) -> String {
    let ps = path.to_string_lossy();
    let rs = root.to_string_lossy();
    if ps.starts_with(rs.as_ref()) {
        match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => ps.to_string(),
        }
    } else {
        ps.to_string()
    }
}

/// The merged config and its problems (bad files, duplicate ids, unknown keys).
pub fn load(root: &Path, extra: &[String]) -> (Config, Vec<String>) {
    let mut merged: Map<String, Value> = Map::new();
    for k in LISTS {
        merged.insert(k.to_string(), Value::Array(vec![]));
    }
    for t in TABLES {
        merged.insert(t.to_string(), Value::Object(Map::new()));
    }
    let mut problems = Vec::new();
    let mut seen: Vec<((String, String), String)> = Vec::new();
    let mut listed: Vec<(String, String)> = Vec::new();
    let paths = files_for(root, extra);
    if paths.is_empty() {
        for name in DEFAULT_PACKS {
            listed.push((name.to_string(), "(no seamcheck.toml: built-in default)".to_string()));
        }
    }
    for path in &paths {
        let rel = rel_name(path, root);
        let cfg = match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
            Ok(c) => c,
            Err(e) => {
                problems.push(format!("{rel}: {e}"));
                continue;
            }
        };
        for t in TABLES {
            if let Some(Value::Object(tab)) = cfg.get(t) {
                let dst = merged.get_mut(t).unwrap().as_object_mut().unwrap();
                for (k, v) in tab {
                    if ADDITIVE.contains(&k.as_str()) && dst.contains_key(k) {
                        let mut cur = dst.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default();
                        if let Some(a) = v.as_array() {
                            cur.extend(a.iter().cloned());
                        }
                        dst.insert(k.clone(), Value::Array(cur));
                    } else {
                        dst.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        for kind in LISTS {
            if let Some(Value::Array(entries)) = cfg.get(kind) {
                for entry in entries {
                    let Some(e) = entry.as_object() else { continue };
                    let rid = e.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    check(kind, e, &rel, &mut problems);
                    let mut e = e.clone();
                    e.insert("_file".into(), Value::String(rel.clone()));
                    if let Some((_, f)) = seen.iter().find(|(k, _)| k.0 == kind && k.1 == rid) {
                        problems.push(format!("{rel}: {kind} id {} already defined in {f}", crate::pyre::repr(&rid)));
                        continue;
                    }
                    seen.push(((kind.to_string(), rid), rel.clone()));
                    merged.get_mut(kind).unwrap().as_array_mut().unwrap().push(Value::Object(e));
                }
            }
        }
        if let Some(Value::Array(packs)) = cfg.get("project").and_then(|p| p.get("packs")) {
            for name in packs.iter().filter_map(|x| x.as_str()) {
                if !listed.iter().any(|(n, _)| n == name) {
                    listed.push((name.to_string(), rel.clone()));
                }
            }
        }
    }
    let packs_ok = match merged["project"].get("packs") {
        None => true,
        Some(v) => v.is_array(),
    };
    if !packs_ok {
        problems.push("[project] packs: a list of pack names, e.g. packs = [\"phoenix\"]".to_string());
    } else if !listed.is_empty() {
        apply_packs(&mut merged, &listed, &mut problems);
    }
    (Config { v: merged }, problems)
}

fn check(kind: &str, entry: &Map<String, Value>, rel: &str, problems: &mut Vec<String>) {
    let rid = entry.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let keys = keys_for(kind);
    for k in entry.keys() {
        if !keys.contains(&k.as_str()) {
            let near = close_match(k, keys);
            problems.push(format!(
                "{rel}: {kind} {}: unknown key {}{}",
                crate::pyre::repr(rid),
                crate::pyre::repr(k),
                near.map(|n| format!(" (did you mean {}?)", crate::pyre::repr(&n))).unwrap_or_default()
            ));
        }
    }
    let sev = entry.get("severity").map(py_str).unwrap_or_else(|| "error".into());
    if !SEVERITIES.contains(&sev.as_str()) {
        problems.push(format!(
            "{rel}: {kind} {}: severity {} (use error, info, warn)",
            crate::pyre::repr(rid),
            crate::pyre::repr(&sev)
        ));
    }
    if kind == "rule" {
        let g = entry.get("gate").map(py_str).unwrap_or_else(|| "all".into());
        if !["all", "changed", "touched", "off"].contains(&g.as_str()) {
            problems.push(format!(
                "{rel}: rule {}: gate {} (use all, changed, touched or off)",
                crate::pyre::repr(rid),
                crate::pyre::repr(&g)
            ));
        }
    }
    if rid.is_empty() {
        problems.push(format!("{rel}: a {kind} without an id"));
    }
}

/// A value as Python's str() would print a TOML scalar.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

fn apply_packs(merged: &mut Map<String, Value>, listed: &[(String, String)], problems: &mut Vec<String>) {
    let known = available_packs();
    let mut base: Vec<(String, Rule)> = Vec::new();
    for (name, rel) in listed {
        let Some(text) = pack_text(name).filter(|_| known.contains(name)) else {
            let near = close_match(name, &known);
            problems.push(format!(
                "{rel}: unknown pack {}{}; known: {}",
                crate::pyre::repr(name),
                near.map(|n| format!(" (did you mean {}?)", crate::pyre::repr(&n))).unwrap_or_default(),
                known.join(", ")
            ));
            continue;
        };
        let pack = parse(text).unwrap_or_default();
        if let Some(Value::Array(rules)) = pack.get("rule") {
            for entry in rules.iter().filter_map(|x| x.as_object()) {
                check("rule", entry, &format!("pack {name}"), problems);
                let rid = format!("{name}/{}", entry.get("id").and_then(|x| x.as_str()).unwrap_or(""));
                let mut e = entry.clone();
                e.insert("id".into(), Value::String(rid.clone()));
                e.insert("_file".into(), Value::String(rel.clone()));
                if let Some(slot) = base.iter_mut().find(|(k, _)| *k == rid) {
                    slot.1 = e;
                } else {
                    base.push((rid, e));
                }
            }
        }
    }
    let mut own = Vec::new();
    let rules = merged.get("rule").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    for entry in rules {
        let Some(e) = entry.as_object() else { continue };
        let id = e.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let prefix = if id.contains('/') { id.split('/').next().unwrap().to_string() } else { String::new() };
        if let Some(slot) = base.iter_mut().find(|(k, _)| *k == id) {
            for (k, v) in e {
                slot.1.insert(k.clone(), v.clone());
            }
        } else if known.contains(&prefix) {
            let file = e.get("_file").and_then(|x| x.as_str()).unwrap_or("");
            let rest = id.splitn(2, '/').nth(1).unwrap_or("");
            let msg = if listed.iter().any(|(n, _)| *n == prefix) {
                format!("pack {prefix} has no rule {}", crate::pyre::repr(rest))
            } else {
                format!("overrides pack {prefix}, which [project] packs doesn't list")
            };
            problems.push(format!("{file}: rule {}: {msg}", crate::pyre::repr(&id)));
        } else {
            own.push(Value::Object(e.clone()));
        }
    }
    let mut all: Vec<Value> = base.into_iter().map(|(_, e)| Value::Object(e)).collect();
    all.extend(own);
    merged.insert("rule".into(), Value::Array(all));
}

// --- reading a rule's keys the way the Python code does ------------------------------

pub fn s<'a>(r: &'a Rule, k: &str) -> Option<&'a str> {
    r.get(k).and_then(|v| v.as_str())
}

pub fn s_or<'a>(r: &'a Rule, k: &str, d: &'a str) -> &'a str {
    s(r, k).unwrap_or(d)
}

/// A key that may be one string or a list of strings.
pub fn str_list(r: &Map<String, Value>, k: &str) -> Option<Vec<String>> {
    match r.get(k)? {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(a) => Some(a.iter().map(|x| x.as_str().map(String::from).unwrap_or_else(|| py_str(x))).collect()),
        _ => None,
    }
}

/// A key that is a list (missing: the default).
pub fn list_or(r: &Map<String, Value>, k: &str, d: &[&str]) -> Vec<String> {
    match r.get(k) {
        Some(Value::Array(a)) => a.iter().map(|x| x.as_str().map(String::from).unwrap_or_else(|| py_str(x))).collect(),
        Some(Value::String(s)) => s.chars().map(|c| c.to_string()).collect(), // Python iterates a str
        _ => d.iter().map(|x| x.to_string()).collect(),
    }
}

pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
    }
}

pub fn id(r: &Rule) -> &str {
    s(r, "id").unwrap_or("")
}

pub fn file_of(r: &Rule) -> String {
    s(r, "_file").unwrap_or("seamcheck.toml").to_string()
}

pub fn severity(r: &Rule) -> String {
    r.get("severity").map(py_str).unwrap_or_else(|| "error".into())
}
