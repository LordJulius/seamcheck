//! package.json (deps._npm): the four dependency tables, bins and peers of
//! the installed packages, scripts, tools with their own config file,
//! imports in code and styles, tsconfig `types`.

use std::collections::HashSet;
use std::sync::OnceLock;

use indexmap::IndexMap;
use serde_json::Value;

use super::fsx::{self, dirname, join, line_of};
use super::{lit, Ctx, Dep, Where};
use crate::pyre::Rx;

fn pkg_of(spec: &str) -> String {
    if spec.starts_with("node:") {
        return String::new();
    }
    let parts: Vec<&str> = spec.split('/').collect();
    if spec.starts_with('@') {
        parts[..parts.len().min(2)].join("/")
    } else {
        parts[0].to_string()
    }
}

/// The names a JSON value lists the way Python iterates it (an object's
/// keys, an array's strings).
fn names(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

pub fn read(ctx: &Ctx, mrel: &str) -> Vec<Dep> {
    static WORD: OnceLock<Rx> = OnceLock::new();
    static TOOLRC: OnceLock<Rx> = OnceLock::new();
    static SPEC: OnceLock<Rx> = OnceLock::new();
    static TYPES: OnceLock<Rx> = OnceLock::new();
    static QUOTED: OnceLock<Rx> = OnceLock::new();
    let mdir = dirname(mrel);
    let text = ctx.read(mrel);
    let Ok(pkg) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    let empty = serde_json::Map::new();
    let pkg = pkg.as_object().unwrap_or(&empty);
    let find_line = |needle: &str| line_of(&text, text.find(needle).unwrap_or(0));
    let mut deps: IndexMap<String, Dep> = IndexMap::new();
    for (sect, scope) in [
        ("dependencies", "runtime"),
        ("devDependencies", "dev"),
        ("optionalDependencies", "runtime"),
        ("peerDependencies", "runtime"),
    ] {
        for name in names(pkg.get(sect)) {
            let line = find_line(&format!("\"{name}\""));
            deps.entry(name.clone()).or_insert_with(|| Dep::new("npm", &name, mrel, line, scope));
        }
    }
    let nm = match ctx.installed(mdir, "node_modules") {
        Some(d) => d,
        None => {
            for dep in deps.values_mut() {
                dep.uninstalled = true;
            }
            fsx::abs_join(ctx.root, &join(mdir, "node_modules"))
        }
    };
    let mut bins: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let all: Vec<String> = deps.keys().cloned().collect();
    for name in &all {
        let info: Option<Value> = std::fs::read(std::path::Path::new(&join(&join(&nm, name), "package.json")))
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| serde_json::from_str(&t).ok());
        if info.is_none() {
            deps[name].guessed = true;
        }
        let info = info.unwrap_or(Value::Null);
        let bin = info.get("bin");
        let bn: Vec<String> = match bin {
            Some(Value::String(b)) if !b.is_empty() => vec![name.rsplit('/').next().unwrap_or(name).to_string()],
            Some(Value::Object(o)) => o.keys().cloned().collect(),
            _ => Vec::new(),
        };
        for b in bn.into_iter().filter(|b| !b.is_empty()) {
            bins.insert(b, name.clone());
        }
        for peer in names(info.get("peerDependencies")) {
            if peer != *name {
                if let Some(d) = deps.get_mut(&peer) {
                    d.needed_by.push(name.clone());
                }
            }
        }
    }
    let dep_for = |word: &str, deps: &IndexMap<String, Dep>| -> Option<String> {
        bins.get(word).cloned().or_else(|| deps.contains_key(word).then(|| word.to_string()))
    };
    if let Some(Value::Object(scripts)) = pkg.get("scripts") {
        for (key, cmd) in scripts {
            let Some(cmd) = cmd.as_str() else { continue };
            for word in lit(&WORD, r"[\w@/.-]+").findall(cmd) {
                if let Some(dn) = dep_for(word, &deps) {
                    let line = find_line(&format!("\"{key}\""));
                    deps[&dn].config.push((mrel.to_string(), line));
                }
            }
        }
    }
    // a tool with its own config file (stylelint.config.js, .prettierrc) is set up
    let toolrc = lit(&TOOLRC, r"\.?([\w-]+?)(?:\.config\.[cm]?[jt]s|rc(?:\.\w+)?)$");
    for (f, _, _) in fsx::scandir(&ctx.root.join(mdir)) {
        if let Some(mt) = toolrc.match_start(&f) {
            if let Some(dn) = dep_for(mt.s(1), &deps) {
                deps[&dn].add(&join(mdir, &f), 1, Some(Where::Config));
            }
        }
    }
    let exts = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".vue", ".svelte", ".css", ".scss", ".html"];
    let spec = lit(
        &SPEC,
        r#"(?:\bfrom\s*|\bimport\s*\(?\s*|\brequire\s*\(\s*|@(?:import|plugin|use|source|reference)\s+(?:url\()?)["']([^"'./][^"']*)["']"#,
    );
    for rel in ctx.walk(mdir, &exts, &["package.json"]) {
        let raw = ctx.read(&rel);
        let mut seen: HashSet<String> = HashSet::new();
        for r in spec.find_iter(&raw) {
            let dn = pkg_of(r.s(1));
            let types = if dn.is_empty() {
                String::new()
            } else {
                format!("@types/{}", dn.trim_start_matches('@').replace('/', "__"))
            };
            for cand in [dn, types] {
                if deps.contains_key(&cand) && seen.insert(cand.clone()) {
                    deps[&cand].add(&rel, line_of(&raw, r.start()), None);
                }
            }
        }
    }
    for path in fsx::glob(&join(&fsx::abs_join(ctx.root, mdir), "tsconfig*.json")) {
        let raw = fsx::read(std::path::Path::new(&path));
        let rel = fsx::relpath(&path, &ctx.root.to_string_lossy());
        for t in lit(&TYPES, r#""types"\s*:\s*\[([^\]]*)\]"#).findall(&raw) {
            for n in lit(&QUOTED, r#""([^"]+)""#).findall(t) {
                for cand in [n.to_string(), format!("@types/{n}")] {
                    if let Some(d) = deps.get_mut(&cand) {
                        d.add(&rel, 1, Some(Where::Config));
                    }
                }
            }
        }
    }
    if deps.get("@types/node").is_some_and(|d| d.code.is_empty()) && deps.values().any(|d| !d.code.is_empty()) {
        let d = &mut deps["@types/node"];
        let line = d.line;
        d.config.push((mrel.to_string(), line)); // the toolchain's own types
    }
    deps.into_values().collect()
}
