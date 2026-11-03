//! God modules (tidy.split, tidy.hotspots): how a big file could split, and
//! which files are biggest.

use std::collections::{BTreeSet, HashMap, HashSet};

use indexmap::IndexMap;
use serde_json::{json, Value};

use super::Tidy;
use crate::model::Func;
use crate::pyre::Rx;

const STOP: &[&str] = &[
    "get", "set", "is", "has", "on", "to", "from", "with", "for", "and", "the", "of", "a", "in", "do", "make", "build",
    "run", "update", "load", "open", "close", "handle", "apply", "by", "fetch", "default", "new", "init", "render",
    "view", "state", "data", "item", "items", "value", "values", "list", "map", "text", "label", "id", "ok", "maybe",
    "to_", "put",
];

fn words(name: &str) -> Vec<String> {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    let rx = R.get_or_init(|| Rx::lit(r"[A-Z]?[a-z]+|[A-Z]+(?![a-z])"));
    rx.findall(&name.replace('_', " "))
        .into_iter()
        .filter(|w| !STOP.contains(&w.to_lowercase().as_str()) && w.chars().count() > 2)
        .map(|w| w.to_lowercase())
        .collect()
}

fn size(f: &Func) -> usize {
    f.end + 1 - f.start
}

/// A big file's features (configured `domains`, else the leading words of
/// function names) with line counts, and its call clusters once widely
/// shared helpers are set aside. One giant cluster means the file is knotted.
pub fn split(t: &Tidy, rel: &str) -> Value {
    let g = t.g;
    let fs: Vec<&Func> = g
        .by_file
        .get(rel)
        .map(|ms| ms.iter().filter_map(|n| g.modules.get(n)).flat_map(|m| m.funcs.iter()).filter(|f| f.kind != "top").collect())
        .unwrap_or_default();
    let domains: Vec<(String, Option<Rx>)> = super::cfg(t.p)
        .get("domains")
        .and_then(|v| v.as_object())
        .map(|o| o.iter().map(|(k, v)| (k.clone(), Rx::new(&crate::config::py_str(v)).ok())).collect())
        .unwrap_or_default();
    let mut feats: IndexMap<String, usize> = IndexMap::new();
    let mut feat_fns: HashMap<String, Vec<String>> = HashMap::new();
    for f in &fs {
        let n = f.name.to_lowercase();
        let d = domains.iter().find(|(_, pat)| pat.as_ref().is_some_and(|p| p.search(&n).is_some())).map(|(k, _)| k.clone());
        let d = d.unwrap_or_else(|| {
            let ws = words(&f.name);
            if !ws.is_empty() && domains.is_empty() {
                ws[0].clone()
            } else {
                "other".into()
            }
        });
        *feats.entry(d.clone()).or_insert(0) += size(f);
        feat_fns.entry(d).or_default().push(f.name.clone());
    }
    let ids: HashSet<&str> = fs.iter().map(|f| f.id.as_str()).collect();
    let mut adj: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    let mut indeg: IndexMap<&str, usize> = IndexMap::new();
    for f in &fs {
        for (to, _, _) in g.edges.get(&f.id).map(|v| v.as_slice()).unwrap_or(&[]) {
            if ids.contains(to.as_str()) && *to != f.id {
                adj.entry(&f.id).or_default().insert(to);
                adj.entry(to).or_default().insert(&f.id);
                *indeg.entry(to).or_insert(0) += 1;
            }
        }
    }
    let cap = (6.0f64).max(fs.len() as f64 * 0.04);
    let hubs: HashSet<&str> = indeg.iter().filter(|(_, c)| **c as f64 > cap).map(|(k, _)| *k).collect();
    let mut fmap: HashMap<&str, &Func> = HashMap::new();
    for f in &fs {
        fmap.insert(&f.id, f);
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut comps: Vec<Vec<&str>> = Vec::new();
    for f in &fs {
        if seen.contains(f.id.as_str()) || hubs.contains(f.id.as_str()) {
            continue;
        }
        let mut stack = vec![f.id.as_str()];
        let mut comp = Vec::new();
        seen.insert(&f.id);
        while let Some(u) = stack.pop() {
            comp.push(u);
            for v in adj.get(u).into_iter().flatten() {
                if !hubs.contains(v) && seen.insert(v) {
                    stack.push(v);
                }
            }
        }
        comps.push(comp);
    }
    let csize = |c: &[&str]| c.iter().map(|i| size(fmap[i])).sum::<usize>();
    comps.sort_by_key(|c| std::cmp::Reverse(csize(c)));
    let total = fs.iter().map(|f| size(f)).sum::<usize>().max(1);
    let mut helpers: Vec<&str> = hubs.iter().map(|h| fmap[h].name.as_str()).collect();
    helpers.sort();
    let mut fe: Vec<(&String, &usize)> = feats.iter().collect();
    fe.sort_by_key(|(_, v)| std::cmp::Reverse(**v));
    json!({
        "file": rel, "functions": fs.len(), "lines": total,
        "shared_helpers": helpers,
        "features": fe.iter().map(|(k, v)| json!({"name": k, "lines": v,
                     "examples": feat_fns[*k].iter().take(5).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "clusters": comps.iter().filter(|c| c.len() > 1).take(10).map(|c| json!({"functions": c.len(), "lines": csize(c),
                     "examples": c.iter().take(5).map(|i| fmap[i].name.clone()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "knotted": !comps.is_empty() && csize(&comps[0]) as f64 > total as f64 * 0.6,
    })
}

#[derive(Debug, Clone)]
pub struct Hot {
    pub file: String,
    pub lines: usize,
    pub functions: usize,
    pub caller_files: usize,
}

impl Hot {
    pub fn json(&self) -> Value {
        json!({"file": self.file, "lines": self.lines, "functions": self.functions, "caller_files": self.caller_files})
    }
}

/// The biggest files (lines in functions), with function count and how many
/// other files call into them: where splits pay off most.
pub fn hotspots(t: &Tidy, limit: usize) -> Vec<Hot> {
    let g = t.g;
    let mut per: IndexMap<&str, (HashSet<usize>, usize, HashSet<&str>)> = IndexMap::new();
    for f in t.funcs() {
        if f.kind == "top" || t.is_test(&f.file) {
            continue;
        }
        let p = per.entry(&f.file).or_default();
        p.0.extend(f.start..=f.end); // (a nested function's lines count once)
        p.1 += 1;
        for (c, _, ex) in g.redges.get(&f.id).map(|v| v.as_slice()).unwrap_or(&[]) {
            if let Some(cf) = g.func(c) {
                if *ex && cf.file != f.file {
                    p.2.insert(&cf.file);
                }
            }
        }
    }
    let mut rows: Vec<Hot> = per
        .into_iter()
        .map(|(k, v)| Hot { file: k.to_string(), lines: v.0.len(), functions: v.1, caller_files: v.2.len() })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.lines));
    rows.truncate(limit);
    rows
}
