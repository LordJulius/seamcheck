//! A self-contained HTML map (report.py, no network): modules as nodes
//! (sized by lines, red when they have findings), calls between them as
//! edges; click a module for its functions, who calls them and what they
//! call; the rules panel lists every contract and what broke it.
//!
//! The page (`report_page.html`) is report.py's `_PAGE` byte for byte; the
//! data it is handed is the same JSON.

use std::collections::BTreeSet;
use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{json, Value};

use crate::config::{id, s};
use crate::graph::Graph;
use crate::output::dumps_line;
use crate::project::{Finding, Project};

const PAGE: &str = include_str!("report_page.html");

/// Python's `html.escape(s)` (quotes too).
pub fn html_escape(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// The data the page draws (report.write_html's `data`).
pub fn data(p: &Project, g: &Graph, findings: &[Finding]) -> Value {
    let mut mods: Vec<&crate::model::Module> = g.modules.values().collect();
    mods.sort_by(|a, b| a.file.cmp(&b.file));
    let idx: HashMap<&str, usize> = mods.iter().enumerate().map(|(i, m)| (m.name.as_str(), i)).collect();
    let mut medges: IndexMap<(usize, usize), usize> = IndexMap::new();
    for (caller, lst) in &g.edges {
        let Some(cf) = g.func(caller) else { continue };
        for (callee, _, _) in lst {
            let Some(tf) = g.func(callee) else { continue };
            if tf.module != cf.module {
                if let (Some(&a), Some(&b)) = (idx.get(cf.module.as_str()), idx.get(tf.module.as_str())) {
                    *medges.entry((a, b)).or_insert(0) += 1;
                }
            }
        }
    }
    let mut fcount: HashMap<&str, usize> = HashMap::new();
    for f in findings {
        *fcount.entry(f.file.as_str()).or_insert(0) += 1;
    }
    let ids = |v: Option<&Vec<crate::graph::Edge>>| -> Vec<String> {
        v.map(|v| v.iter().map(|e| e.0.clone()).collect::<BTreeSet<_>>().into_iter().collect()).unwrap_or_default()
    };
    let nodes: Vec<Value> = mods
        .iter()
        .map(|m| {
            let funcs: Vec<Value> = m
                .funcs
                .iter()
                .filter(|f| f.kind != "top")
                .map(|f| json!({"n": f.name, "l": f.start, "out": ids(g.edges.get(&f.id)), "in": ids(g.redges.get(&f.id))}))
                .collect();
            let dir = m.file.rsplit_once('/').map(|x| x.0).unwrap_or(".");
            json!({"id": m.name, "file": m.file, "lines": m.lines, "dir": dir,
                   "bad": fcount.get(m.file.as_str()).copied().unwrap_or(0), "funcs": funcs})
        })
        .collect();
    let name = p
        .config
        .table("project")
        .get("name")
        .cloned()
        .unwrap_or_else(|| Value::String(p.root.to_string_lossy().into_owned()));
    let rules: Vec<Value> = p
        .config
        .rules()
        .map(|r| {
            json!({"id": id(r), "kind": s(r, "kind").unwrap_or("forbid"), "why": s(r, "why").unwrap_or(""),
                   "sev": s(r, "severity").unwrap_or("error")})
        })
        .collect();
    json!({
        "name": name,
        "nodes": nodes,
        "edges": medges.iter().map(|((a, b), n)| json!([a, b, n])).collect::<Vec<_>>(),
        "findings": findings.iter().map(|f| json!({"rule": f.rule, "severity": f.severity, "file": f.file, "line": f.line,
                                                    "func": f.func, "message": f.message, "snippet": f.snippet})).collect::<Vec<_>>(),
        "rules": rules,
    })
}

/// The whole page (report.write_html, less the write).
pub fn html(p: &Project, g: &Graph, findings: &[Finding]) -> String {
    let d = data(p, g, findings);
    let title = match &d["name"] {
        Value::String(s) => s.clone(),
        other => crate::config::py_str(other),
    };
    PAGE.replace("__TITLE__", &html_escape(&title)).replace("__DATA__", &dumps_line(&d))
}

pub fn write_html(p: &Project, g: &Graph, findings: &[Finding], out_path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(out_path, html(p, g, findings))
}
