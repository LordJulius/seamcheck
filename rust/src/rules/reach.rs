//! The rule kinds and options that follow calls: kind = "reach"
//! (core._reach) and `together`'s `depth` (a `then` may match in what the
//! function calls, N exact calls deep).

use std::sync::Arc;

use serde_json::Value;

use super::{allowed, compile, need, Res};
use crate::config::{id, s, severity, str_list, truthy, Rule};
use crate::project::{Finding, Project};
use crate::pyre::Rx;

/// Does this rule read the call graph? (`run_rules` loads it first, outside its threads.)
pub fn needs_graph(r: &Rule) -> bool {
    match s(r, "kind").unwrap_or("forbid") {
        "reach" | "depend" | "layers" | "cycles" | "path" | "deps" => true,
        "together" => truthy(r.get("depth")),
        "lint" => matches!(s(r, "lint"), Some("auth-in-background" | "ets-owner")),
        _ => false,
    }
}

/// `int(v)` of a TOML value (a number, or a string of one).
pub(crate) fn int_of(v: Option<&Value>, d: i64) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(d),
        Some(Value::String(t)) => t.trim().parse().unwrap_or(d),
        Some(Value::Bool(b)) => *b as i64,
        _ => d,
    }
}

/// kind = "reach": every function picked by `from` (specs; optionally only
/// those whose code matches `when`) must reach one of `to` (function names)
/// within `depth` calls ("all" mode: every one of them).
/// The picked functions are deduplicated by id, in order (core._reach).
pub fn reach(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let when = match s(r, "when") {
        Some(w) if !w.is_empty() => Some(compile(w)?),
        _ => None,
    };
    need(r, "to")?;
    let targets = str_list(r, "to").unwrap_or_default();
    let depth = int_of(r.get("depth"), 4).max(0) as usize;
    let all = s(r, "mode").map(|m| m != "any").unwrap_or(false);
    need(r, "from")?;
    let g = p.graph();
    let mut seen = std::collections::HashSet::new();
    let mut picked = Vec::new();
    for spec in str_list(r, "from").unwrap_or_default() {
        for f in g.find(&spec) {
            if seen.insert(f.id.clone()) {
                picked.push(f);
            }
        }
    }
    let allow = crate::config::list_or(r, "allow", &[]);
    let mut out = Vec::new();
    for f in picked {
        if when.as_ref().map(|w| !w.is_match(&f.code)).unwrap_or(false) {
            continue;
        }
        if allowed(p, &f.file, Some(&f.span()), &allow) {
            continue;
        }
        let dist = g.reach(&f.id, depth, false, truthy(r.get("strict")));
        let names: std::collections::HashSet<&str> =
            dist.keys().filter(|k| **k != f.id).filter_map(|k| g.func(k)).map(|h| h.name.as_str()).collect();
        let hit: Vec<&String> = targets.iter().filter(|t| names.contains(t.as_str()) || f.refs.contains(*t)).collect();
        let ok = if all { hit.len() == targets.len() } else { !hit.is_empty() };
        if !ok {
            let miss: Vec<&str> = targets.iter().filter(|t| !hit.contains(t)).map(|t| t.as_str()).collect();
            let msg = s(r, "message")
                .map(String::from)
                .unwrap_or_else(|| format!("doesn't reach {} within {depth} calls", miss.join(" / ")));
            out.push(Finding::new(id(r), &severity(r), &f.file, f.start, &f.name, msg));
        }
    }
    Ok(out)
}

/// `together` with `depth`: the `then`s still missing after the code of what
/// the function reaches (exact calls only).
pub fn missing_after_calls(p: &Project, r: &Rule, rel: &str, func: &crate::model::Span, mut missing: Vec<Arc<Rx>>) -> Vec<Arc<Rx>> {
    let g = p.graph();
    // (the graph's own id for the function: a second module of one name is `name@file`)
    let fid = g
        .by_file
        .get(rel)
        .into_iter()
        .flatten()
        .flat_map(|mn| g.modules[mn].funcs.iter())
        .find(|f| f.name == func.name && f.start == func.start && f.end == func.end && f.kind == func.kind)
        .map(|f| f.id.clone())
        .unwrap_or_else(|| format!("{}.{}", func.module, func.name));
    let depth = int_of(r.get("depth"), 0).max(0) as usize;
    for cid in g.reach(&fid, depth, false, true).keys() {
        if *cid == fid {
            continue;
        }
        if let Some(callee) = g.func(cid) {
            missing.retain(|t| !t.is_match(&callee.code));
        }
    }
    missing
}
