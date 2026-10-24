//! `kind = "fanout"` and the harvest it runs on (core.harvest_all,
//! view_tokens, _handles, _fanout, matrix): one producer, many consumers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value};
use sha1::{Digest, Sha1};

use super::indexmap_like::Ordered;
use super::{compile, need, RuleErr, Res};
use crate::config::{file_of, id, list_or, s, severity, str_list, Rule};
use crate::glob::{close_match, fnmatchcase, Globs};
use crate::project::{Finding, Mode, Project};
use crate::pyre::{repr, Rx};

const HARVEST_KEYS: &[&str] =
    &["in", "skip", "func", "within", "pattern", "values", "text", "run", "rewrite", "timeout", "join", "token", "inputs"];
const VIEW_KEYS: &[&str] = &["match", "normalize", "fallback", "except", "ignore", "sep", "wildcard", "unhandled"];

/// One harvested occurrence: (token, file, line, the match: its text, start,
/// end and the whole text it was found in).
#[derive(Clone)]
pub struct Occ {
    pub token: String,
    pub file: String,
    pub line: usize,
    pub m: Option<(Arc<String>, usize, usize)>,
}

fn pairs(v: Option<&Value>) -> Vec<(String, String)> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_array())
            .map(|p| {
                (
                    p.first().and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    p.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string(),
                )
            })
            .collect(),
        _ => vec![],
    }
}

fn token_of(spec: &Map<String, Value>, m: &crate::pyre::Caps) -> String {
    if let Some(t) = spec.get("token") {
        let i = t.as_u64().unwrap_or(0) as usize;
        return m.get(i).unwrap_or("").to_string();
    }
    let groups: Vec<&str> = m.groups().into_iter().flatten().filter(|g| !g.is_empty()).collect();
    if let (Some(j), false) = (s(spec, "join"), groups.is_empty()) {
        return groups.join(j);
    }
    groups.first().map(|g| g.to_string()).unwrap_or_else(|| m.group0().to_string())
}

static RUN_CACHE: OnceLock<Mutex<HashMap<(String, String), Arc<String>>>> = OnceLock::new();

fn cache_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join(".cache/seamcheck-rs")
}

fn main_checkout(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let top = git(&["rev-parse", "--show-toplevel"])?;
    let c = std::path::Path::new(&common);
    if c.file_name().map(|n| n == ".git").unwrap_or(false) {
        let cand = c.parent()?.to_path_buf();
        let rc = std::fs::canonicalize(&cand).ok()?;
        let rt = std::fs::canonicalize(&top).ok()?;
        if !top.is_empty() && rc != rt {
            return Some(cand);
        }
    }
    None
}

/// A command's output; with `inputs`, cached on disk until those files change.
fn run_output(p: &Project, cmd: &str, spec: &Map<String, Value>) -> Res<String> {
    let inputs = str_list(spec, "inputs").filter(|v| !v.is_empty());
    let (timeout, _) = super::command::py_num(spec.get("timeout"), 300.0);
    let mut path = None;
    if let Some(inputs) = &inputs {
        let files = crate::walk::list_files(&p.root, inputs, &[]);
        let mut h = Sha1::new();
        h.update(cmd.as_bytes());
        for f in &files {
            h.update(b"\0");
            h.update(f.as_bytes());
            h.update(b"\0");
            h.update(std::fs::read(p.path(f)).unwrap_or_default());
        }
        let d: String = h.finalize().iter().take(10).map(|b| format!("{b:02x}")).collect();
        let pth = cache_dir().join(format!("run-{d}.txt"));
        if let Ok(t) = std::fs::read_to_string(&pth) {
            return Ok(t);
        }
        path = Some(pth);
    }
    let mut res = super::command::run_shell(cmd, &p.root, timeout)
        .ok_or_else(|| RuleErr::Env(format!("run {} timed out", repr(cmd))))?;
    if res.code != 0 {
        let main = main_checkout(&p.root);
        let same = match (&main, &inputs) {
            (Some(m), Some(inputs)) => crate::walk::list_files(&p.root, inputs, &[])
                .iter()
                .all(|rel| std::fs::read(p.path(rel)).ok() == std::fs::read(m.join(rel)).ok()),
            _ => false,
        };
        if same {
            if let Some(r2) = super::command::run_shell(cmd, main.as_ref().unwrap(), timeout) {
                res = r2;
            }
        }
        if res.code != 0 {
            let err = res.stderr.trim();
            let tail: String = {
                let c: Vec<char> = err.chars().collect();
                c[c.len().saturating_sub(300)..].iter().collect()
            };
            let hint = if same {
                ""
            } else if main.is_some() {
                " (in a worktree: install its deps, or its inputs differ from the main checkout's)"
            } else {
                ""
            };
            return Err(RuleErr::Env(format!("run {} exited {}: {tail}{hint}", repr(cmd), res.code)));
        }
    }
    if let Some(pth) = path {
        let _ = std::fs::create_dir_all(cache_dir());
        let _ = std::fs::write(pth, &res.stdout);
    }
    Ok(res.stdout)
}

pub fn harvest_all(p: &Project, spec: &Map<String, Value>) -> Res<Vec<Occ>> {
    let rewrite: Vec<(Arc<Rx>, String)> =
        pairs(spec.get("rewrite")).into_iter().map(|(a, b)| Ok((compile(&a)?, b))).collect::<Res<Vec<_>>>()?;
    let mut found: Vec<Occ> = Vec::new();
    let keep = |found: &mut Vec<Occ>, key: String, file: &str, line: usize, m: Option<(Arc<String>, usize, usize)>| {
        let mut key = key;
        for (pat, rep) in &rewrite {
            key = pat.sub(rep, &key);
        }
        if !key.is_empty() {
            found.push(Occ { token: key, file: file.to_string(), line, m });
        }
    };
    let vfile = s(spec, "_file").unwrap_or("seamcheck.toml").to_string();
    for v in list_or(spec, "values", &[]) {
        keep(&mut found, v, &vfile, 0, None);
    }
    let Some(pv) = spec.get("pattern") else { return Ok(found) };
    let pat_list: Vec<String> = match pv {
        Value::Array(a) => a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect(),
        v => vec![v.as_str().unwrap_or("").to_string()],
    };
    if let Some(cmd) = s(spec, "run").filter(|c| !c.is_empty()) {
        let key = (cmd.to_string(), p.root.to_string_lossy().to_string());
        let cache = RUN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let hit = cache.lock().unwrap().get(&key).cloned();
        let out = match hit {
            Some(o) => o,
            None => {
                let o = Arc::new(run_output(p, cmd, spec)?);
                cache.lock().unwrap().insert(key, o.clone());
                o
            }
        };
        for pat in &pat_list {
            let rx = compile(pat)?;
            for m in rx.find_iter(&out) {
                let line = out[..m.start()].matches('\n').count() + 1;
                let tok = token_of(spec, &m);
                keep(&mut found, tok, &format!("$ {cmd}"), line, Some((out.clone(), m.start(), m.end())));
            }
        }
        return Ok(found);
    }
    let pats: Vec<Arc<Rx>> = pat_list.iter().map(|x| compile(x)).collect::<Res<Vec<_>>>()?;
    let within = match s(spec, "within") {
        Some(w) if !w.is_empty() => Some(compile(w)?),
        _ => None,
    };
    let funcs = str_list(spec, "func");
    let gi = Globs::new(&list_or(spec, "in", &["**/*"]));
    let gs = Globs::new(&list_or(spec, "skip", &[]));
    let mode_s = s(spec, "text").unwrap_or("nocomment");
    let files: Vec<&String> = p.files.iter().filter(|f| gi.any(f) && !gs.any(f)).collect();
    for rel in files {
        let lines = super::text_mode(p, rel, mode_s, Mode::parse(mode_s));
        let text = Arc::new(lines.join("\n"));
        let mut starts = vec![0usize];
        for ln in &lines[..lines.len().saturating_sub(1)] {
            starts.push(starts.last().unwrap() + ln.len() + 1);
        }
        let mut regions: Vec<(usize, usize)> = vec![(0, text.len())];
        if let Some(fs) = &funcs {
            let mods = p.modules(rel);
            if mods.is_empty() {
                return Err(RuleErr::Key(format!(
                    "func = [{}] on {rel}, which seamcheck can't parse into functions: use within",
                    fs.iter().map(|f| repr(f)).collect::<Vec<_>>().join(", ")
                )));
            }
            regions = mods
                .iter()
                .flat_map(|m| m.funcs.iter())
                .filter(|f| f.kind != "top" && fs.iter().any(|g| fnmatchcase(&f.name, g)))
                .map(|f| (starts[f.start - 1], if f.end < starts.len() { starts[f.end] } else { text.len() }))
                .collect();
        }
        if let Some(w) = &within {
            let mut nr = Vec::new();
            for (a, b) in &regions {
                for m in w.find_iter_range(&text, *a, *b) {
                    if w.groups() > 0 {
                        if let Some(sp) = m.span(1) {
                            nr.push(sp);
                            continue;
                        }
                    }
                    nr.push((m.start(), m.end()));
                }
            }
            regions = nr;
        }
        for (a, b) in regions {
            for pat in &pats {
                for m in pat.find_iter_range(&text, a, b) {
                    let line = starts.partition_point(|&x| x <= m.start());
                    let tok = token_of(spec, &m);
                    keep(&mut found, tok, rel, line, Some((text.clone(), m.start(), m.end())));
                }
            }
        }
    }
    Ok(found)
}

pub fn harvest(p: &Project, spec: &Map<String, Value>) -> Res<Ordered> {
    let mut out = Ordered::default();
    for o in harvest_all(p, spec)? {
        out.set_default(&o.token, (o.file, o.line));
    }
    Ok(out)
}

pub fn view_tokens(p: &Project, spec: &Map<String, Value>) -> Res<Ordered> {
    let Some(un) = s(spec, "unhandled").filter(|u| !u.is_empty()) else { return harvest(p, spec) };
    let dead = compile(un)?;
    let mut occ: Vec<Occ> = harvest_all(p, spec)?.into_iter().filter(|o| o.m.is_some()).collect();
    occ.sort_by(|a, b| (a.file.as_str(), a.m.as_ref().unwrap().1).cmp(&(b.file.as_str(), b.m.as_ref().unwrap().1)));
    static CUT: OnceLock<Rx> = OnceLock::new();
    static FALL: OnceLock<Rx> = OnceLock::new();
    let cut = CUT.get_or_init(|| Rx::lit(r"(?m)\bdefault\s*:|\belse\s*(?:->|\{|$)|^\s*_\s*->"));
    let fall = FALL.get_or_init(|| Rx::lit(r"[\s,|:]*(?:case\s*)?"));
    let mut bodies: Vec<String> = Vec::new();
    for (i, o) in occ.iter().enumerate() {
        let (text, _s, e) = o.m.as_ref().unwrap();
        let nxt = if i + 1 < occ.len() && occ[i + 1].file == o.file {
            occ[i + 1].m.as_ref().unwrap().1
        } else {
            crate::pyre::floor_char(text, (e + 400).min(text.len()))
        };
        let body = if nxt >= *e { &text[*e..nxt] } else { "" };
        let body = match cut.search(body) {
            Some(c) => &body[..c.start()],
            None => body,
        };
        bodies.push(body.to_string());
    }
    if bodies.len() >= 2 {
        for i in (0..bodies.len() - 1).rev() {
            if fall.fullmatch(&bodies[i]).is_some() && occ[i + 1].file == occ[i].file {
                bodies[i] = bodies[i + 1].clone();
            }
        }
    }
    let mut out = Ordered::default();
    for (o, body) in occ.iter().zip(bodies.iter()) {
        if !dead.is_match(body) {
            out.set_default(&o.token, (o.file.clone(), o.line));
        }
    }
    Ok(out)
}

/// Does a view's harvested set cover `token`?
pub fn handles(spec: &Map<String, Value>, got: &[&str], token: &str) -> Res<bool> {
    let mut token = token.to_string();
    for (pat, rep) in pairs(spec.get("normalize")) {
        token = compile(&pat)?.sub(&rep, &token);
    }
    let how = s(spec, "match").unwrap_or("exact");
    Ok(match how {
        "exact" => got.contains(&token.as_str()),
        "contains" => got.iter().any(|g| token.contains(g)),
        "prefix" => got.iter().any(|g| token.starts_with(g)),
        "regex" => {
            for g in got {
                if compile(g)?.fullmatch(&token).is_some() {
                    return Ok(true);
                }
            }
            false
        }
        "segments" => {
            let sep = s(spec, "sep").unwrap_or("/");
            let wild = s(spec, "wildcard").unwrap_or(":p");
            let mut ts: Vec<&str> = token.split(sep).collect();
            let rest = ts.last() == Some(&"**");
            if rest {
                ts.pop();
            }
            got.iter().any(|g| {
                let mut gs: Vec<&str> = g.split(sep).collect();
                if rest {
                    if gs.len() >= ts.len() {
                        gs.truncate(ts.len());
                    } else {
                        return false;
                    }
                }
                gs.len() == ts.len() && ts.iter().zip(gs.iter()).all(|(a, b)| a == b || *a == wild || *b == wild)
            })
        }
        _ => {
            return Err(RuleErr::Key(format!(
                "match = {} (exact | contains | prefix | regex | segments)",
                repr(how)
            )))
        }
    })
}

fn covers_some(spec: &Map<String, Value>, g: &str, produced: &Ordered) -> Res<bool> {
    for k in produced.keys() {
        if handles(spec, &[g], k)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn except(spec: &Map<String, Value>) -> Vec<String> {
    match spec.get("except") {
        Some(Value::Array(a)) => a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect(),
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        _ => vec![],
    }
}

fn with_file(spec: &Map<String, Value>, r: &Rule) -> Map<String, Value> {
    let mut m = spec.clone();
    m.insert("_file".into(), r.get("_file").cloned().unwrap_or(Value::Null));
    m
}

pub fn fanout(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let rid = id(r);
    let rfile = file_of(r);
    let mut out = Vec::new();
    let source = need(r, "source")?.as_object().ok_or_else(|| RuleErr::Key("source".into()))?;
    let views = need(r, "views")?.as_object().ok_or_else(|| RuleErr::Key("views".into()))?;
    let mut sides: Vec<(String, &Map<String, Value>)> = vec![("source".into(), source)];
    for (v, sp) in views {
        if let Some(sp) = sp.as_object() {
            sides.push((format!("views.{v}"), sp));
        }
    }
    for (where_, spec) in &sides {
        let mut allowed: Vec<&str> = HARVEST_KEYS.to_vec();
        if where_ != "source" {
            allowed.extend(VIEW_KEYS);
        }
        for k in spec.keys() {
            if !allowed.contains(&k.as_str()) {
                let near = close_match(k, &allowed);
                out.push(Finding::new(
                    rid,
                    "error",
                    &rfile,
                    0,
                    "",
                    format!(
                        "{where_}: unknown key {}{}",
                        repr(k),
                        near.map(|n| format!(" (did you mean {}?)", repr(&n))).unwrap_or_default()
                    ),
                ));
            }
        }
    }
    let src = harvest(p, &with_file(source, r))?;
    let checks = list_or(r, "check", &["missing"]);
    if src.is_empty() {
        return Ok(vec![Finding::new(rid, "error", &rfile, 0, "", "source produced no tokens (pattern or path wrong?)".into())]);
    }
    let message = s(r, "message").filter(|m| !m.is_empty());
    for (view, spec) in views {
        let Some(spec) = spec.as_object() else { continue };
        let got = view_tokens(p, &with_file(spec, r))?;
        let has_values = spec.get("values").map(|v| crate::config::truthy(Some(v))).unwrap_or(false);
        if got.is_empty() && !has_values {
            out.push(Finding::new(rid, "error", &rfile, 0, "", format!("view {view} handles nothing (pattern or path wrong?)")));
            continue;
        }
        let gotv: Vec<&str> = got.keys().map(|k| k.as_str()).collect();
        let exc = except(spec);
        let fb = s(spec, "fallback").filter(|f| !f.is_empty());
        if checks.iter().any(|c| c == "missing") {
            let mut ks: Vec<&String> = Vec::new();
            for k in src.keys() {
                if !exc.contains(k) && !handles(spec, &gotv, k)? {
                    ks.push(k);
                }
            }
            ks.sort();
            for k in ks {
                let (rel, ln) = src.get(k).unwrap();
                let sev = if fb.is_some() { "warn".to_string() } else { severity(r) };
                out.push(Finding::new(
                    rid,
                    &sev,
                    rel,
                    *ln,
                    "",
                    format!(
                        "{view} does not handle {}{}{}",
                        repr(k),
                        fb.map(|f| format!(" (falls back to {f})")).unwrap_or_default(),
                        message.map(|m| format!(": {m}")).unwrap_or_default()
                    ),
                ));
            }
        }
        if checks.iter().any(|c| c == "extra") {
            let ignore = list_or(spec, "ignore", &[]);
            let mut ks: Vec<&String> = Vec::new();
            for g in got.keys() {
                if !ignore.contains(g) && !covers_some(spec, g, &src)? {
                    ks.push(g);
                }
            }
            ks.sort();
            let xsev = r.get("extra_severity").map(crate::config::py_str).unwrap_or_else(|| "warn".into());
            for k in ks {
                let (rel, ln) = got.get(k).unwrap();
                out.push(Finding::new(
                    rid,
                    &xsev,
                    rel,
                    *ln,
                    "",
                    format!("{view} handles {}, which the source never produces", repr(k)),
                ));
            }
        }
        let mut ex_sorted = exc.clone();
        ex_sorted.sort();
        ex_sorted.dedup();
        for k in ex_sorted {
            if handles(spec, &gotv, &k)? {
                if let Some((rel, ln)) = got.get(&k) {
                    out.push(Finding::new(
                        rid,
                        "warn",
                        rel,
                        *ln,
                        "",
                        format!("{view} lists {} under except but handles it now: drop the exception", repr(&k)),
                    ));
                } else {
                    // Python: got[k] raises KeyError for a token handled only through match/normalize
                    return Err(RuleErr::Key(k));
                }
            }
        }
    }
    Ok(out)
}

/// (tokens, views, cell) for a fanout rule: cell[(token, view)] is "ok",
/// "except", "fallback", "missing", "extra" (handled, never produced) or "".
pub fn matrix(p: &Project, r: &Rule) -> Res<(Vec<String>, Vec<String>, HashMap<(String, String), &'static str>)> {
    let source = need(r, "source")?.as_object().ok_or_else(|| RuleErr::Key("source".into()))?;
    let views = need(r, "views")?.as_object().ok_or_else(|| RuleErr::Key("views".into()))?;
    let src = harvest(p, &with_file(source, r))?;
    let names: Vec<String> = views.keys().cloned().collect();
    let mut got: HashMap<String, Vec<String>> = HashMap::new();
    let mut exc: HashMap<String, Vec<String>> = HashMap::new();
    for v in &names {
        let spec = views[v].as_object().ok_or_else(|| RuleErr::Key(v.clone()))?;
        let ignore = list_or(spec, "ignore", &[]);
        got.insert(v.clone(), view_tokens(p, &with_file(spec, r))?.keys().filter(|k| !ignore.contains(k)).cloned().collect());
        exc.insert(v.clone(), except(spec));
    }
    let mut extra: Vec<String> = Vec::new();
    for v in &names {
        let spec = views[v].as_object().unwrap();
        for g in &got[v] {
            if !covers_some(spec, g, &src)? && !extra.contains(g) {
                extra.push(g.clone());
            }
        }
    }
    let mut cell = HashMap::new();
    let mut all: Vec<String> = src.keys().cloned().collect();
    for e in &extra {
        if !src.contains(e) && !all.contains(e) {
            all.push(e.clone());
        }
    }
    for k in &all {
        for v in &names {
            let spec = views[v].as_object().unwrap();
            let gv: Vec<&str> = got[v].iter().map(|s| s.as_str()).collect();
            let c = if src.contains(k) {
                if handles(spec, &gv, k)? {
                    "ok"
                } else if exc[v].contains(k) {
                    "except"
                } else if s(spec, "fallback").map(|f| !f.is_empty()).unwrap_or(false) {
                    "fallback"
                } else {
                    "missing"
                }
            } else if got[v].contains(k) {
                "extra"
            } else {
                ""
            };
            cell.insert((k.clone(), v.clone()), c);
        }
    }
    all.sort_by(|a, b| (!src.contains(a), a).cmp(&(!src.contains(b), b)));
    Ok((all, names, cell))
}
