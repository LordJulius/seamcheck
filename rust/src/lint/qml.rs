//! QML seams (lint_qml.py): what a QML file names must be what the engine
//! finds, on every Qt build.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value};

use super::{line, opt_list, rxs, Hits};
use crate::project::{read_text, Project};
use crate::rules::{compile, RuleErr};

type Res<T> = Result<T, RuleErr>;

fn qt_types() -> &'static HashSet<String> {
    static T: OnceLock<HashSet<String>> = OnceLock::new();
    T.get_or_init(|| {
        include_str!("../../../seamcheck/qt_types.txt")
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    })
}

fn stem_of(path: &str) -> &str {
    let b = path.rsplit('/').next().unwrap_or(path);
    match b.rfind('.') {
        Some(0) | None => b,
        Some(k) => &b[..k],
    }
}

fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(k) => &p[..k],
        None => "",
    }
}

/// os.path.normpath(os.path.join(base, tok))
fn norm_join(base: &str, tok: &str) -> String {
    let joined = if tok.starts_with('/') || base.is_empty() { tok.to_string() } else { format!("{base}/{tok}") };
    let abs = joined.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().map(|l| *l != "..").unwrap_or(false) {
                    parts.pop();
                } else if !abs {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let s = parts.join("/");
    if abs {
        format!("/{s}")
    } else if s.is_empty() {
        ".".into()
    } else {
        s
    }
}

pub struct Modules {
    pub qml: Vec<String>,
    pub module_of: HashMap<String, String>,
    pub singletons: HashMap<String, BTreeSet<String>>,
    pub uris: HashSet<String>,
    pub types: HashSet<String>,
}

fn build(p: &Project) -> Modules {
    let root = &p.root;
    let mut excl: Vec<String> = crate::walk::DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect();
    excl.extend(p.exclude());
    let read = |rel: &str| read_text(&p.path(rel)).unwrap_or_default();
    let qml = crate::walk::list_files(root, &["**/*.qml".into(), "**/*.js".into(), "**/*.mjs".into()], &excl);
    let mut m = Modules {
        types: qml.iter().filter(|r| r.ends_with(".qml")).map(|r| stem_of(r).to_string()).collect(),
        qml,
        module_of: HashMap::new(),
        singletons: HashMap::new(),
        uris: HashSet::new(),
    };
    for cm in crate::walk::list_files(root, &["**/CMakeLists.txt".into(), "**/*.cmake".into()], &excl) {
        let text = read(&cm);
        if !text.contains("qt_add_qml_module") && !text.contains("QT_QML_SINGLETON_TYPE") {
            continue;
        }
        let base = dirname(&cm);
        let mut single: HashSet<String> = HashSet::new();
        for sm in rxs(100, r"(?s)set_source_files_properties\s*\(([^)]*?)PROPERTIES[^)]*QT_QML_SINGLETON_TYPE\s+TRUE").find_iter(&text) {
            for tok in sm.s(1).split_whitespace() {
                let t = tok.trim_matches('"');
                if t.ends_with(".qml") {
                    single.insert(stem_of(t).to_string());
                }
            }
        }
        for mm in rxs(101, r"(?s)qt_add_qml_module\s*\(([^)]*)\)").find_iter(&text) {
            let body = mm.s(1);
            let Some(uri) = rxs(102, r"\bURI\s+([\w.]+)").search(body) else { continue };
            let uri = uri.s(1).to_string();
            m.uris.insert(uri.clone());
            let mut names: HashSet<String> = HashSet::new();
            for tok in body.split_whitespace() {
                let tok = tok.trim_matches('"');
                if tok.ends_with(".qml") || tok.ends_with(".js") || tok.ends_with(".mjs") {
                    names.insert(stem_of(tok).to_string());
                    if !tok.contains('$') {
                        m.module_of.insert(norm_join(base, tok), uri.clone());
                    }
                }
            }
            let e = m.singletons.entry(uri).or_default();
            for s in single.intersection(&names) {
                e.insert(s.clone());
            }
        }
    }
    for qd in crate::walk::list_files(root, &["**/qmldir".into()], &excl) {
        let text = read(&qd);
        let Some(md) = rxs(103, r"(?m)^module\s+([\w.]+)").search(&text) else { continue };
        let uri = md.s(1).to_string();
        let base = dirname(&qd).to_string();
        m.uris.insert(uri.clone());
        for ln in crate::rules::command::splitlines(&text) {
            let parts: Vec<&str> = ln.split_whitespace().collect();
            if parts.len() >= 3 && (parts.last().unwrap().ends_with(".qml") || parts.last().unwrap().ends_with(".js")) {
                if parts[0] == "singleton" && parts.len() >= 4 {
                    m.singletons.entry(uri.clone()).or_default().insert(parts[1].to_string());
                }
                m.module_of.entry(norm_join(&base, parts.last().unwrap())).or_insert_with(|| uri.clone());
            }
        }
    }
    let qml = m.qml.clone();
    for rel in qml {
        if rel.ends_with(".qml") {
            if let Some(uri) = m.module_of.get(&rel).cloned() {
                if rxs(104, r"(?m)^\s*pragma\s+Singleton\b").is_match(&read(&rel)) {
                    m.singletons.entry(uri).or_default().insert(stem_of(&rel).to_string());
                }
            }
        }
    }
    let srcs = crate::walk::list_files(root, &["**/*.h".into(), "**/*.hpp".into(), "**/*.cpp".into(), "**/*.cc".into()], &excl);
    for src in srcs {
        let text = read(&src);
        if text.contains("QML_") || text.contains("qmlRegister") {
            for (id, pat) in [
                (108, r"(?s)\b(?:class|struct)\s+(?:\w+\s+)?(\w+)[^;{]*\{(?:(?!\b(?:class|struct)\s+\w+[^;{]*\{).)*?\bQML_(?:ELEMENT|SINGLETON|ANONYMOUS)\b"),
                (109, r"\bQML_NAMED_ELEMENT\s*\(\s*(\w+)\s*\)"),
                (110, r#"\bqmlRegister\w*\s*(?:<[^>]*>)?\s*\([^;]*?"([A-Z]\w*)"\s*[,)]"#),
            ] {
                for w in rxs(id, pat).findall(&text) {
                    m.types.insert(w.to_string());
                }
            }
        }
    }
    m
}

fn modules(p: &Project) -> Arc<Modules> {
    super::shared(p, "qml-modules", || build(p))
}

fn imports(raw: &str) -> Vec<String> {
    rxs(105, r#"(?m)^\s*import\s+("[^"]+"|[\w.]+)(?:\s+[\d.]+)?(?:\s+as\s+(\w+))?"#)
        .find_iter(raw)
        .into_iter()
        .map(|m| m.s(1).to_string())
        .collect()
}

pub fn qml_unknown_type(rel: &str, raw: &str, code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !rel.ends_with(".qml") {
        return Ok(vec![]);
    }
    let mods = modules(p);
    let inline: HashSet<String> =
        rxs(106, r"(?m)^\s*component\s+([A-Z]\w*)\s*:").findall(raw).into_iter().map(String::from).collect();
    let extra: HashSet<String> = opt_list(o, "types", &[]).into_iter().collect();
    for imp in imports(raw) {
        if imp.starts_with('"') || imp.starts_with("Qt") || imp.starts_with("QML") || mods.uris.contains(&imp) {
            continue;
        }
        return Ok(vec![]);
    }
    let known = |n: &str| qt_types().contains(n) || mods.types.contains(n) || inline.contains(n) || extra.contains(n);
    let mut out = Vec::new();
    for m in rxs(107, r"(?m)^\s*(?:[a-z_]\w*(?:\.\w+)*\s*:\s*)?([A-Z]\w*)(?:\s+on\s+[\w.]+)?\s*\{").find_iter(code) {
        let name = m.s(1);
        if !known(name) {
            out.push((
                line(code, m.span(1).unwrap().0),
                format!(
                    "{name} is not a type any import, project file or C++ class defines: the page fails to load (Sheet {{ }} in FoodBookDive.qml left the desk's Food Book blank)"
                ),
            ));
        }
    }
    Ok(out)
}

pub fn qml_singleton_import(rel: &str, raw: &str, code: &str, _o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !rel.ends_with(".qml") {
        return Ok(vec![]);
    }
    let mods = modules(p);
    let Some(uri) = mods.module_of.get(rel) else { return Ok(vec![]) };
    let own = stem_of(rel);
    let mut used: Vec<&String> = Vec::new();
    if let Some(ss) = mods.singletons.get(uri) {
        for s in ss {
            if s != own && compile(&format!(r"\b{s}\.\w"))?.is_match(code) {
                used.push(s);
            }
        }
    }
    let imported: HashSet<String> = imports(raw).into_iter().collect();
    if used.is_empty() || imported.contains(uri) {
        return Ok(vec![]);
    }
    let first = used.iter().filter_map(|s| code.find(&format!("{s}."))).min().unwrap_or(0);
    let names: Vec<&str> = used.iter().map(|s| s.as_str()).collect();
    Ok(vec![(
        line(code, first),
        format!(
            "uses {} (singleton{} of {uri}) without `import {uri}`: which one it gets depends on the Qt build (6.11.2 read the plain Tokens.qml beside it, an empty object, and the window went black)",
            names.join(", "),
            if used.len() > 1 { "s" } else { "" }
        ),
    )])
}

pub fn qml_js_shadows_type(rel: &str, _raw: &str, _code: &str, _o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !(rel.ends_with(".js") || rel.ends_with(".mjs")) {
        return Ok(vec![]);
    }
    let mods = modules(p);
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let stem = match base.rfind('.') {
        Some(k) if k > 0 => &rel[..rel.len() - (base.len() - k)],
        _ => rel,
    };
    let twin = format!("{stem}.qml");
    if mods.qml.contains(&twin)
        && (mods.module_of.get(rel) == mods.module_of.get(&twin) || dirname(rel) == dirname(&twin))
    {
        let tb = twin.rsplit('/').next().unwrap_or(&twin);
        let sb = stem.rsplit('/').next().unwrap_or(stem);
        return Ok(vec![(
            1,
            format!(
                "{base} shares its name with {tb}: the module lists both under one name and the library can hide the type (HubMove.js hid the HubMove singleton). Rename the library ({sb}Rules.js)"
            ),
        )]);
    }
    Ok(vec![])
}
