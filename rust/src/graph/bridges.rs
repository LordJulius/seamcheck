//! `[[bridge]]` rules (core._apply_bridges): edges no call graph can see,
//! across languages. Each `from` occurrence (a client's `"/api/osa/inbox"`, a
//! hub `push(socket, "ops")`) links its enclosing function to what the
//! matching `to` names: `target` (a function id built from the match's
//! groups, e.g. a route's "{2}.{3}" -> Controller.action) or the function
//! enclosing each `to` occurrence (a client's `.on("ops")` handler). Matching
//! is `match` (exact, segments...) like a fanout view. Direction is data
//! flow: from -> to.

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::graph::Graph;
use crate::project::Project;
use crate::pyre::repr;
use crate::rules::fanout::{handles, harvest_all, HARVEST_KEYS};
use crate::rules::RuleErr;

/// The innermost function around a line (the file's <top> if none).
fn enclosing<'a>(g: &'a Graph, rel: &str, line: usize) -> Option<&'a crate::model::Func> {
    let mut inside: Vec<&crate::model::Func> = Vec::new();
    for mn in g.by_file.get(rel).map(|v| v.as_slice()).unwrap_or(&[]) {
        for f in &g.modules[mn].funcs {
            if f.start <= line && line <= f.end {
                inside.push(f);
            }
        }
    }
    let real: Vec<&&crate::model::Func> = inside.iter().filter(|f| f.kind != "top").collect();
    if !real.is_empty() {
        // max by (start, -end): the last of the greatest key
        let mut best = real[0];
        for f in &real[1..] {
            if (f.start, std::cmp::Reverse(f.end)) >= (best.start, std::cmp::Reverse(best.end)) {
                best = f;
            }
        }
        return Some(best);
    }
    inside.first().copied()
}

/// Python `str.format` with positional `{N}` fields only; None when a field
/// can't be filled (IndexError / KeyError).
fn format_target(target: &str, args: &[String]) -> Option<String> {
    let mut out = String::new();
    let mut it = target.chars().peekable();
    while let Some(c) = it.next() {
        if c == '{' {
            if it.peek() == Some(&'{') {
                it.next();
                out.push('{');
                continue;
            }
            let mut name = String::new();
            for d in it.by_ref() {
                if d == '}' {
                    break;
                }
                name.push(d);
            }
            let idx: usize = name.parse().ok()?;
            out.push_str(args.get(idx)?);
        } else if c == '}' {
            if it.peek() == Some(&'}') {
                it.next();
            }
            out.push('}');
        } else {
            out.push(c);
        }
    }
    Some(out)
}

pub fn apply_bridges(g: &mut Graph, p: &Project) {
    let bridges: Vec<Map<String, Value>> = p.config.list("bridge").iter().filter_map(|b| b.as_object().cloned()).collect();
    for b in bridges {
        let bid = b.get("id").and_then(|x| x.as_str()).unwrap_or("bridge").to_string();
        let file = b.get("_file").and_then(|x| x.as_str()).unwrap_or("seamcheck.toml").to_string();
        let with_file = |side: &str| -> Option<Map<String, Value>> {
            let mut m = b.get(side)?.as_object()?.clone();
            m.insert("_file".into(), b.get("_file").cloned().unwrap_or(Value::Null));
            Some(m)
        };
        let (Some(fs), Some(ts)) = (with_file("from"), with_file("to")) else {
            g.problems.push(format!("{file}: bridge {bid}: {}", repr(if b.get("from").is_none() { "from" } else { "to" })));
            continue;
        };
        let froms = match harvest_all(p, &fs) {
            Ok(x) => x,
            Err(e) => {
                note(g, &file, &bid, e);
                continue;
            }
        };
        let tos = match harvest_all(p, &ts) {
            Ok(x) => x,
            Err(e) => {
                note(g, &file, &bid, e);
                continue;
            }
        };
        for side in ["from", "to"] {
            if let Some(o) = b.get(side).and_then(|x| x.as_object()) {
                if let Some(bad) = o.keys().find(|k| !HARVEST_KEYS.contains(&k.as_str()) && *k != "target") {
                    g.problems.push(format!("{file}: bridge {bid}: {side}: unknown key {}", repr(bad)));
                }
            }
        }
        let target = b.get("to").and_then(|t| t.get("target")).and_then(|t| t.as_str()).filter(|t| !t.is_empty());
        let mut dests: IndexMap<String, Vec<String>> = IndexMap::new();
        for o in &tos {
            if let Some(target) = target {
                let args: Vec<String> = if o.m.is_some() {
                    o.groups.iter().map(|x| x.clone().unwrap_or_else(|| "None".into())).collect()
                } else {
                    vec![o.token.clone()]
                };
                let Some(fid) = format_target(target, &args) else { continue };
                if g.funcs.contains_key(&fid) {
                    dests.entry(o.token.clone()).or_default().push(fid);
                }
            } else if let Some(f) = enclosing(g, &o.file, o.line) {
                dests.entry(o.token.clone()).or_default().push(f.id.clone());
            }
        }
        if dests.is_empty() {
            g.problems.push(format!("{file}: bridge {bid}: `to` found nothing"));
            continue;
        }
        let mut spec: Map<String, Value> = Map::new();
        for k in ["match", "sep", "wildcard", "normalize"] {
            if let Some(v) = b.get(k) {
                spec.insert(k.into(), v.clone());
            }
        }
        let mut exact_hits: IndexMap<String, Vec<String>> = IndexMap::new();
        let mut add: Vec<(String, String, usize)> = Vec::new();
        for o in &froms {
            if !exact_hits.contains_key(&o.token) {
                let mut hs = Vec::new();
                for (t, ds) in &dests {
                    if handles(&spec, &[t.as_str()], &o.token).unwrap_or(false) {
                        hs.extend(ds.iter().cloned());
                    }
                }
                exact_hits.insert(o.token.clone(), hs);
            }
            let hits = &exact_hits[&o.token];
            if hits.is_empty() {
                continue;
            }
            let Some(src) = enclosing(g, &o.file, o.line) else { continue };
            let mut seen: Vec<&String> = Vec::new();
            for dst in hits {
                if seen.contains(&dst) {
                    continue;
                }
                seen.push(dst);
                if *dst == src.id {
                    continue;
                }
                add.push((src.id.clone(), dst.clone(), o.line));
            }
        }
        for (src, dst, line) in add {
            g.edges.entry(src.clone()).or_default().push((dst.clone(), line, true));
            g.redges.entry(dst.clone()).or_default().push((src.clone(), line, true));
            g.bridged.insert((src, dst), bid.clone());
        }
    }
}

fn note(g: &mut Graph, file: &str, bid: &str, e: RuleErr) {
    match e {
        // couldn't run here (a worktree without deps): not a broken bridge
        RuleErr::Env(m) => g.env_notes.push(format!("{file}: bridge {bid} not built here: {m}")),
        RuleErr::BadPattern(m) => g.problems.push(format!("{file}: bridge {bid}: {m}")),
        RuleErr::Key(k) => g.problems.push(format!("{file}: bridge {bid}: {}", repr(&k))),
        RuleErr::Deferred(m) => g.problems.push(format!("{file}: bridge {bid}: {m}")),
    }
}
