//! Architecture rules over the file dependency graph (arch.py): what ArchUnit
//! checks in Java and dependency-cruiser in JS / TS, for every language
//! seamcheck reads. A file depends on another when it imports it or (exactly)
//! calls into it (`via = "import" | "call" | "any"`, default "any").
//!
//! Kinds `depend` (nothing in `from` may depend on `to`), `layers` (each
//! layer depends only on the layers after it), `cycles` (no cycle among
//! files, or folders with `level = "dir"`), the `graph` command's graph and
//! its rendering; `path` (call chains into sinks, and taint) is `path.rs`.

pub mod path;
mod taint;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};

use crate::config::{id, list_or, s, severity, Rule};
use crate::glob::{match_any, Globs};
use crate::graph::infer::{arities, Types};
use crate::graph::Graph;
use crate::model::Module;
use crate::project::{Finding, Project};
use crate::rules::{need, Res, RuleErr};

/// src file -> {dst file: (line, "import" | "call")}: the first line that makes the dependency.
pub type FileDeps = IndexMap<String, IndexMap<String, (usize, &'static str)>>;

/// `os.path.basename`
pub(crate) fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// `os.path.dirname`
pub(crate) fn dirname(p: &str) -> &str {
    p.rsplit_once('/').map(|x| x.0).unwrap_or("")
}

/// `os.path.splitext(p)[0]` of a base name (leading dots are no extension).
pub(crate) fn stem(base: &str) -> &str {
    let lead = base.len() - base.trim_start_matches('.').len();
    match base[lead..].rfind('.') {
        Some(k) => &base[..lead + k],
        None => base,
    }
}

/// `os.path.splitext(p)[1]`
pub(crate) fn ext(p: &str) -> &str {
    let b = basename(p);
    &b[stem(b).len()..]
}

/// The file dependency graph, once per run and `via` (arch.file_deps).
pub fn file_deps(p: &Project, via: &str) -> Arc<FileDeps> {
    p.memo(&format!("\0filedeps\0{via}"), || {
        let g = p.graph();
        build(&g, p, via)
    })
}

fn build(g: &Graph, p: &Project, via: &str) -> FileDeps {
    let mut out: FileDeps = IndexMap::new();
    if via == "import" || via == "any" {
        let mut pkg: HashMap<(String, String), String> = HashMap::new();
        for m in g.modules.values() {
            if !m.owner.is_empty() {
                continue;
            }
            let mut targets: Vec<String> = m.aliases.values().cloned().collect();
            for (k, (w, n)) in &m.imported {
                if !m.type_imports.contains(k) {
                    targets.push(in_package(g, &mut pkg, w, n));
                }
            }
            targets.extend(m.uses.iter().cloned());
            targets.extend(m.reexports.iter().cloned());
            let mut seen: HashSet<&str> = HashSet::new();
            for t in &targets {
                if t.is_empty() || !seen.insert(t.as_str()) {
                    continue;
                }
                let Some(tm) = g.modules.get(t) else { continue };
                if tm.file == m.file {
                    continue;
                }
                // (every module of a file has the file's lines: the line is the file's either way)
                out.entry(m.file.clone()).or_default().entry(tm.file.clone()).or_insert_with(|| (import_line(m, tm), "import"));
            }
        }
    }
    if via == "call" || via == "any" {
        let types = Types::new(g, p);
        let mut dispatched: HashMap<String, bool> = HashMap::new();
        // g overrides / implements a method its supertype declares: a call reaches it
        // through that type (NodeTraversor's `visitor.head()` names NodeVisitor, not
        // every visitor), so the caller depends on the supertype, not on g's file
        let mut through_dispatch = |gf: &crate::model::Func| -> bool {
            if let Some(&d) = dispatched.get(&gf.id) {
                return d;
            }
            let anc = types.ancestors(&gf.module);
            let d = anc.iter().skip(1).any(|a| {
                let am = &g.modules[a];
                am.file != gf.file && (g.funcs.contains_key(&format!("{a}.{}", gf.name)) || !arities(am, &gf.name).is_empty())
            });
            dispatched.insert(gf.id.clone(), d);
            d
        };
        for (src, es) in &g.edges {
            let Some(f) = g.func(src) else { continue };
            for (dst, ln, exact) in es {
                let Some(gf) = g.func(dst) else { continue };
                if !exact || gf.file == f.file || through_dispatch(gf) {
                    continue;
                }
                // (Python's setdefault: the first call edge to a file stays)
                out.entry(f.file.clone()).or_default().entry(gf.file.clone()).or_insert((*ln, "call"));
            }
        }
    }
    out
}

/// `import org.jsoup.nodes.Document` names package org.jsoup.nodes: the module of its Document.
fn in_package(g: &Graph, cache: &mut HashMap<(String, String), String>, where_: &str, name: &str) -> String {
    let Some(pk) = where_.strip_prefix("pkg:") else { return where_.to_string() };
    let key = (where_.to_string(), name.to_string());
    if let Some(h) = cache.get(&key) {
        return h.clone();
    }
    let cands: &[String] = g.packages.get(pk).map(|v| v.as_slice()).unwrap_or(&[]);
    let hit = cands
        .iter()
        .find(|mn| stem(basename(&g.modules[*mn].file)) == name)
        .or_else(|| cands.iter().find(|mn| g.modules[*mn].decls.iter().any(|d| d == name)))
        .cloned()
        .unwrap_or_default();
    cache.insert(key, hit.clone());
    hit
}

/// The line of m that imports tm: the first import-like line naming its file stem.
fn import_line(m: &Module, tm: &Module) -> usize {
    let mut st = stem(basename(&tm.file));
    if matches!(st, "index" | "__init__" | "mod" | "lib") {
        let d = basename(dirname(&tm.file));
        if !d.is_empty() {
            st = d;
        }
    }
    let pat = crate::graph::infer::rxd(&format!(r"\b{}\b", crate::graph::infer::esc(st)));
    const HEADS: &[&str] =
        &["import", "from", "use ", "using", "#include", "require", "alias", "include", "const", "let", "var"];
    for (i, ln) in m.raw.iter().take(400).enumerate() {
        let t = ln.trim_start();
        if HEADS.iter().any(|h| t.starts_with(h)) && pat.is_match(ln) {
            return i + 1;
        }
    }
    1
}

fn via_of(r: &Rule, d: &str) -> String {
    r.get("via").map(crate::config::py_str).unwrap_or_else(|| d.to_string())
}

fn sorted_deps(deps: &FileDeps) -> Vec<(&String, Vec<(&String, &(usize, &'static str))>)> {
    let mut v: Vec<_> = deps
        .iter()
        .map(|(s, ds)| {
            let mut d: Vec<_> = ds.iter().collect();
            d.sort_by(|a, b| a.0.cmp(b.0));
            (s, d)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v
}

fn globs(r: &Rule, k: &str) -> Res<Vec<String>> {
    need(r, k)?;
    Ok(list_or(r, k, &[]))
}

/// kind = "depend": nothing in `from` may depend on `to` (arch.depend).
pub fn depend(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let deps = file_deps(p, &via_of(r, "any"));
    let (from, to) = (Globs::new(&globs(r, "from")?), globs(r, "to")?);
    let (allow, allow_to) = (Globs::new(&list_or(r, "allow", &[])), Globs::new(&list_or(r, "allow_to", &[])));
    let to = Globs::new(&to);
    let mut out = Vec::new();
    for (src, ds) in sorted_deps(&deps) {
        if !from.any(src) || allow.any(src) {
            continue;
        }
        for (dst, (ln, how)) in ds {
            if to.any(dst) && !allow_to.any(dst) {
                let msg = s(r, "message").map(String::from).unwrap_or_else(|| format!("depends on {dst} ({how})"));
                out.push(Finding::new(id(r), &severity(r), src, *ln, "", msg));
            }
        }
    }
    Ok(out)
}

/// kind = "layers": each layer depends only on the layers after it (arch.layers).
pub fn layers(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let ls: Vec<&serde_json::Map<String, Value>> = match need(r, "layers")? {
        Value::Array(a) => a.iter().filter_map(|x| x.as_object()).collect(),
        _ => return Err(RuleErr::Key("layers".into())),
    };
    let mut ins = Vec::new();
    let mut names = Vec::new();
    for l in &ls {
        ins.push(Globs::new(&globs(l, "in")?));
        names.push(l.get("name").map(crate::config::py_str).ok_or_else(|| RuleErr::Key("name".into()))?);
    }
    let deps = file_deps(p, &via_of(r, "any"));
    let layer_of = |path: &str| ins.iter().position(|g| g.any(path));
    let allow = Globs::new(&list_or(r, "allow", &[]));
    let mut out = Vec::new();
    for (src, ds) in sorted_deps(&deps) {
        let Some(a) = layer_of(src) else { continue };
        if allow.any(src) {
            continue;
        }
        for (dst, (ln, how)) in ds {
            if let Some(b) = layer_of(dst).filter(|b| *b < a) {
                let msg = s(r, "message").map(String::from).unwrap_or_else(|| {
                    format!("layer {} depends on the layer above it, {}: {dst} ({how})", names[a], names[b])
                });
                out.push(Finding::new(id(r), &severity(r), src, *ln, "", msg));
            }
        }
    }
    Ok(out)
}

/// kind = "cycles": strongly connected components of the dependency graph
/// within `in`, one finding per cycle, at the first file of a shortest loop
/// through it (arch.cycles).
pub fn cycles(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let level = s(r, "level").unwrap_or("file");
    let deps = file_deps(p, &via_of(r, "import"));
    let scope = Globs::new(&list_or(r, "in", &["**/*"]));
    let skip = Globs::new(&list_or(r, "skip", &[]));
    let node = |path: &str| -> String {
        if level == "dir" {
            let d = dirname(path);
            if d.is_empty() { ".".into() } else { d.to_string() }
        } else {
            path.to_string()
        }
    };
    let mut graph: IndexMap<String, IndexMap<String, (String, usize)>> = IndexMap::new();
    for (src, ds) in deps.iter() {
        if !scope.any(src) || skip.any(src) {
            continue;
        }
        for (dst, (ln, _)) in ds {
            if !scope.any(dst) || skip.any(dst) {
                continue;
            }
            let (a, b) = (node(src), node(dst));
            if a != b {
                graph.entry(a).or_default().entry(b).or_insert((src.clone(), *ln));
            }
        }
    }
    let adj: BTreeMap<String, BTreeSet<String>> =
        graph.iter().map(|(a, ds)| (a.clone(), ds.keys().cloned().collect())).collect();
    let mut out = Vec::new();
    for comp in sccs(&adj) {
        if comp.len() < 2 {
            continue;
        }
        let lp = shortest_loop(&adj, &comp);
        let first = &graph[&lp[0]][&lp[1]];
        let msg = s(r, "message").map(String::from).unwrap_or_else(|| {
            let mut chain = lp.clone();
            chain.push(lp[0].clone());
            format!(
                "dependency cycle ({} {}): {}",
                comp.len(),
                if level == "dir" { "folders" } else { "files" },
                chain.join(" -> ")
            )
        });
        out.push(Finding::new(id(r), &severity(r), &first.0, first.1, "", msg));
    }
    Ok(out)
}

/// Tarjan, iterative, nodes and successors in sorted order (arch._sccs).
pub fn sccs(graph: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    let mut nodes: BTreeSet<&String> = graph.keys().collect();
    for ds in graph.values() {
        nodes.extend(ds.iter());
    }
    let empty = BTreeSet::new();
    let succ = |v: &str| -> Vec<&String> { graph.get(v).unwrap_or(&empty).iter().collect() };
    let (mut index, mut low): (HashMap<&String, usize>, HashMap<&String, usize>) = Default::default();
    let mut on: HashSet<&String> = HashSet::new();
    let mut stack: Vec<&String> = Vec::new();
    let mut out = Vec::new();
    let mut counter = 0;
    for root in nodes {
        if index.contains_key(root) {
            continue;
        }
        let mut work: Vec<(&String, Vec<&String>, usize)> = vec![(root, succ(root), 0)];
        index.insert(root, counter);
        low.insert(root, counter);
        counter += 1;
        stack.push(root);
        on.insert(root);
        while let Some(top) = work.last_mut() {
            let v = top.0;
            if top.2 < top.1.len() {
                let nxt = top.1[top.2];
                top.2 += 1;
                if !index.contains_key(nxt) {
                    index.insert(nxt, counter);
                    low.insert(nxt, counter);
                    counter += 1;
                    stack.push(nxt);
                    on.insert(nxt);
                    work.push((nxt, succ(nxt), 0));
                } else if on.contains(nxt) {
                    let l = low[v].min(index[nxt]);
                    low.insert(v, l);
                }
                continue;
            }
            work.pop();
            if let Some(w) = work.last() {
                let l = low[w.0].min(low[v]);
                low.insert(w.0, l);
            }
            if low[v] == index[v] {
                let mut comp = Vec::new();
                loop {
                    let w = stack.pop().unwrap();
                    on.remove(w);
                    comp.push(w.clone());
                    if w == v {
                        break;
                    }
                }
                comp.sort();
                out.push(comp);
            }
        }
    }
    out
}

/// A shortest cycle through the component's first node (BFS back to it).
fn shortest_loop(graph: &BTreeMap<String, BTreeSet<String>>, comp: &[String]) -> Vec<String> {
    let members: HashSet<&String> = comp.iter().collect();
    let start = &comp[0];
    let mut prev: HashMap<&String, Option<&String>> = HashMap::from([(start, None)]);
    let mut queue: VecDeque<&String> = VecDeque::from([start]);
    while let Some(v) = queue.pop_front() {
        for w in graph.get(v).into_iter().flatten() {
            if !members.contains(w) {
                continue;
            }
            if w == start {
                let mut path = vec![v.clone()];
                let mut cur = prev[v];
                while let Some(c) = cur {
                    path.push(c.clone());
                    cur = prev[c];
                }
                path.reverse();
                return path;
            }
            if !prev.contains_key(w) {
                prev.insert(w, Some(v));
                queue.push_back(w);
            }
        }
    }
    comp.to_vec()
}

/// The dependency graph at `level` ("file" | "dir"), edges weighted by how
/// many file pairs make them (arch.graph).
pub fn graph(p: &Project, level: &str, via: &str, only: &[String]) -> BTreeMap<String, BTreeMap<String, usize>> {
    let deps = file_deps(p, via);
    let mut out: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let dir = |x: &str| -> String {
        let d = dirname(x);
        if d.is_empty() { ".".into() } else { d.to_string() }
    };
    for (src, ds) in deps.iter() {
        if !only.is_empty() && !match_any(src, only) {
            continue;
        }
        for dst in ds.keys() {
            if !only.is_empty() && !match_any(dst, only) {
                continue;
            }
            let (a, b) = if level == "dir" { (dir(src), dir(dst)) } else { (src.clone(), dst.clone()) };
            if a != b {
                *out.entry(a).or_default().entry(b).or_insert(0) += 1;
            }
        }
    }
    out
}

/// The nodes on some cycle of a weighted graph.
pub fn in_cycles(g: &BTreeMap<String, BTreeMap<String, usize>>) -> HashSet<String> {
    let adj: BTreeMap<String, BTreeSet<String>> = g.iter().map(|(a, ds)| (a.clone(), ds.keys().cloned().collect())).collect();
    sccs(&adj).into_iter().filter(|c| c.len() > 1).flatten().collect()
}

/// dot (Graphviz), mermaid, or json; nodes in a dependency cycle drawn red (arch.render).
pub fn render(g: &BTreeMap<String, BTreeMap<String, usize>>, fmt: &str) -> String {
    let cyc = in_cycles(g);
    let mut nodes: BTreeSet<&String> = g.keys().collect();
    for ds in g.values() {
        nodes.extend(ds.keys());
    }
    if fmt == "json" {
        let v = json!({
            "nodes": nodes.iter().map(|n| json!({"id": n, "cycle": cyc.contains(*n)})).collect::<Vec<_>>(),
            "edges": g.iter().flat_map(|(a, ds)| ds.iter().map(move |(b, w)| json!({"from": a, "to": b, "weight": w})))
                .collect::<Vec<_>>(),
        });
        return crate::output::dumps(&v);
    }
    let ids: HashMap<&String, String> = nodes.iter().enumerate().map(|(k, n)| (*n, format!("n{k}"))).collect();
    let mut lines: Vec<String> = Vec::new();
    if fmt == "mermaid" {
        lines.push("graph LR".into());
        lines.extend(nodes.iter().map(|n| format!("  {}[\"{n}\"]", ids[*n])));
        for (a, ds) in g {
            lines.extend(ds.keys().map(|b| format!("  {} --> {}", ids[a], ids[b])));
        }
        lines.extend(nodes.iter().filter(|n| cyc.contains(**n)).map(|n| format!("  style {} stroke:#c00", ids[*n])));
        return lines.join("\n");
    }
    lines.push("digraph deps {".into());
    lines.push("  rankdir=LR; node [shape=box, fontsize=10];".into());
    lines.extend(nodes.iter().map(|n| format!("  \"{n}\"{};", if cyc.contains(*n) { " [color=red]" } else { "" })));
    for (a, ds) in g {
        lines.extend(ds.keys().map(|b| {
            format!("  \"{a}\" -> \"{b}\"{};", if cyc.contains(a) && cyc.contains(b) { " [color=red]" } else { "" })
        }));
    }
    lines.push("}".into());
    lines.join("\n")
}
