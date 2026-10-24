//! Running the rules (core.run_rules and the rule kinds that need no call
//! graph: forbid, require, together, coverage, fanout, command, lint,
//! essential). Kinds and options that need the graph are reported by
//! [`deferred`] and left to phase 2.

pub mod command;
pub mod fanout;

use rayon::prelude::*;
use serde_json::{Map, Value};

use crate::config::{self, file_of, id, list_or, s, severity, str_list, Rule};
use crate::glob::{close_match, fnmatchcase, glob_re, match_any, Globs};
use crate::project::{has_adapter, Finding, Mode, Project};
use crate::pyre::{repr, rx, strip, take_chars, Rx};
use crate::walk::{list_files, DEFAULT_EXCLUDE};

#[derive(Debug, Clone)]
pub enum RuleErr {
    /// a rule's own regex is bad: name the rule, run the rest
    BadPattern(String),
    /// the rule's helper command can't run here (warn, vouches neither way)
    Env(String),
    /// a key the rule needs (Python's KeyError, printed with repr)
    Key(String),
    /// needs what phase 1 doesn't have (the call graph, another adapter)
    Deferred(String),
}

pub type Res<T> = Result<T, RuleErr>;

pub fn compile(pat: &str) -> Res<std::sync::Arc<Rx>> {
    rx(pat).map_err(RuleErr::BadPattern)
}

pub fn need<'a>(r: &'a Map<String, Value>, k: &str) -> Res<&'a Value> {
    r.get(k).ok_or_else(|| RuleErr::Key(k.to_string()))
}

pub fn need_str<'a>(r: &'a Map<String, Value>, k: &str) -> Res<&'a str> {
    match need(r, k)? {
        Value::String(s) => Ok(s),
        _ => Err(RuleErr::Key(k.to_string())),
    }
}

/// `--only` / `--skip` name a rule by its id or a pack by its name.
pub fn picked(rid: &str, ids: Option<&[String]>) -> bool {
    match ids {
        Some(ids) if !ids.is_empty() => {
            ids.iter().any(|i| rid == i || rid.starts_with(&format!("{}/", i.trim_end_matches('/'))))
        }
        _ => false,
    }
}

pub fn allowed(p: &Project, rel: &str, func: Option<&crate::elixir::Func>, allow: &[String]) -> bool {
    for a in allow {
        let (pg, fg) = match a.find(':') {
            Some(k) => (&a[..k], &a[k + 1..]),
            None => (a.as_str(), ""),
        };
        if glob_re(pg).is_match(rel) && (fg.is_empty() || func.map(|f| fnmatchcase(&f.name, fg)).unwrap_or(false)) {
            return true;
        }
    }
    let _ = p;
    false
}

/// A `seamcheck: allow [rule]` comment on the line or the one above.
pub fn suppressed(raw: &[String], line: usize, rule: &str) -> bool {
    static RX: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    static SPLIT: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    let rxa = RX.get_or_init(|| Rx::lit(r"seamcheck:\s*allow\b\s*([\w\-, ]*)"));
    let split = SPLIT.get_or_init(|| Rx::lit(r"[,\s]+"));
    for ln in [line as isize - 1, line as isize - 2] {
        if ln >= 0 && (ln as usize) < raw.len() {
            let t = &raw[ln as usize];
            if !t.contains("seamcheck") {
                continue;
            }
            if let Some(m) = rxa.search(t) {
                let g = m.s(1);
                if strip(g).is_empty() || split.split(g).iter().any(|w| *w == rule) {
                    return true;
                }
            }
        }
    }
    false
}

pub fn snippet(raw: &[String], line: usize) -> String {
    if line >= 1 && line <= raw.len() {
        take_chars(strip(&raw[line - 1]), 160).to_string()
    } else {
        String::new()
    }
}

/// core._scope_files
pub fn scope_files(p: &Project, r: &Rule) -> Vec<String> {
    let inn = list_or(r, "in", &["**/*"]);
    let skip = list_or(r, "skip", &[]);
    let pool: Vec<String> = if config::truthy(r.get("repo")) {
        let mut ex: Vec<String> = DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect();
        ex.extend(p.exclude());
        list_files(&p.root, &inn, &ex)
    } else {
        p.files.clone()
    };
    let gi = Globs::new(&inn);
    let gs = Globs::new(&skip);
    pool.into_iter().filter(|f| gi.any(f) && !gs.any(f)).collect()
}

/// Why phase 1 can't run this rule exactly as Python does (None: it can).
pub fn deferred(p: &Project, r: &Rule) -> Option<String> {
    let kind = s(r, "kind").unwrap_or("forbid");
    match kind {
        "reach" | "depend" | "layers" | "cycles" | "path" => return Some(format!("kind {kind} walks the call graph")),
        "deps" => return Some("kind deps reads the dependency analysis".into()),
        "lint" => {
            let name = s(r, "lint").unwrap_or("");
            if let Some(l) = crate::lint::get(name) {
                if l.f.is_none() {
                    return Some(format!("lint {name} is not ported yet"));
                }
            }
        }
        "essential" if crate::lint::essential_ported() == false => {
            return Some("kind essential is not ported yet".into())
        }
        _ => {}
    }
    if kind == "together" && r.get("depth").is_some() {
        return Some("together with depth follows calls".into());
    }
    // functions of a language phase 1 doesn't parse yet
    let wants_funcs = match kind {
        "together" => !matches!(s(r, "scope"), Some("file") | Some("near")) || r.get("func").is_some(),
        "require" => r.get("func").is_some(),
        _ => false,
    } || str_list(r, "allow").unwrap_or_default().iter().any(|a| a.contains(':'));
    if wants_funcs && matches!(kind, "together" | "require" | "forbid" | "lint") {
        let files = if kind == "require" {
            list_or(r, "in", &[]).into_iter().flat_map(|spec| expand_spec(p, &spec)).collect()
        } else {
            scope_files(p, r)
        };
        if let Some(f) = files.iter().find(|f| !p.funcs_known(f) && p.file_set.contains(*f)) {
            return Some(format!("needs the functions of {f} (its adapter is phase 2)"));
        }
    }
    if kind == "fanout" {
        let mut specs: Vec<&Map<String, Value>> = Vec::new();
        if let Some(Value::Object(src)) = r.get("source") {
            specs.push(src);
        }
        if let Some(Value::Object(views)) = r.get("views") {
            specs.extend(views.values().filter_map(|v| v.as_object()));
        }
        for sp in specs {
            if sp.get("func").is_some() {
                let inn = list_or(sp, "in", &["**/*"]);
                let skip = list_or(sp, "skip", &[]);
                if let Some(f) =
                    p.files.iter().find(|f| match_any(f, &inn) && !match_any(f, &skip) && !p.funcs_known(f) && has_adapter(f))
                {
                    return Some(format!("harvests functions of {f} (its adapter is phase 2)"));
                }
            }
        }
    }
    None
}

fn expand_spec(p: &Project, spec: &str) -> Vec<String> {
    if spec.contains(['*', '?', '[']) {
        p.files.iter().filter(|f| match_any(f, &[spec])).cloned().collect()
    } else {
        vec![spec.to_string()]
    }
}

/// The ids of rules phase 1 leaves to phase 2, with the reason.
pub fn deferred_rules(p: &Project) -> Vec<(String, String)> {
    p.config.rules().filter_map(|r| deferred(p, r).map(|why| (id(r).to_string(), why))).collect()
}

pub fn run_rules(p: &Project, only: Option<&[String]>, skip: Option<&[String]>) -> Vec<Finding> {
    let mut out: Vec<Finding> = p
        .config_problems
        .iter()
        .map(|pr| Finding::new("config", "error", pr.split(':').next().unwrap_or(""), 0, "", pr.clone()))
        .collect();
    let rules: Vec<&Rule> = p
        .config
        .rules()
        .filter(|r| {
            let rid = id(r);
            !(only.map(|o| !o.is_empty()).unwrap_or(false) && !picked(rid, only)) && !picked(rid, skip)
        })
        .collect();
    let timing = std::env::var_os("SEAMCHECK_RS_TIMING").is_some();
    let one = |r: &Rule| {
        let t = std::time::Instant::now();
        let f = run_one(p, r);
        if timing {
            eprintln!("timing {:>7} ms  {}", t.elapsed().as_millis(), id(r));
        }
        f
    };
    // command rules wait on other processes: each gets its own thread (as
    // Python's pool does), so they don't hold rayon's workers while the rest
    // run; results are collected in rule order
    let is_cmd = |r: &&Rule| s(r, "kind") == Some("command");
    let per: Vec<Vec<Finding>> = std::thread::scope(|sc| {
        let cmds: Vec<(usize, std::thread::ScopedJoinHandle<Vec<Finding>>)> = rules
            .iter()
            .enumerate()
            .filter(|(_, r)| is_cmd(r))
            .map(|(k, r)| (k, sc.spawn(move || one(r))))
            .collect();
        let mut rest: Vec<Option<Vec<Finding>>> =
            rules.par_iter().map(|r| if is_cmd(r) { None } else { Some(one(r)) }).collect();
        for (k, h) in cmds {
            rest[k] = Some(h.join().unwrap_or_default());
        }
        rest.into_iter().map(|x| x.unwrap_or_default()).collect()
    });
    for f in per {
        out.extend(f);
    }
    sort_findings(&mut out);
    out
}

pub fn sort_findings(out: &mut [Finding]) {
    let order = |s: &str| match s {
        "error" => 0,
        "warn" => 1,
        "info" => 2,
        _ => 3,
    };
    out.sort_by(|a, b| {
        (order(&a.severity), &a.rule, &a.file, a.line).cmp(&(order(&b.severity), &b.rule, &b.file, b.line))
    });
}

pub fn run_one(p: &Project, r: &Rule) -> Vec<Finding> {
    let rid = id(r);
    let kind = s(r, "kind").unwrap_or("forbid");
    if let Some(why) = deferred(p, r) {
        if std::env::var_os("SEAMCHECK_RS_QUIET").is_none() {
            eprintln!("seamcheck-rs: rule {rid} deferred: {why}");
        }
        return vec![];
    }
    let res = match kind {
        "forbid" => forbid(p, r),
        "require" => require(p, r),
        "together" => together(p, r),
        "coverage" => coverage(p, r),
        "fanout" => fanout::fanout(p, r),
        "command" => command::command(p, r),
        "lint" => lint(p, r),
        "essential" => essential(p, r),
        _ => {
            return vec![Finding::new(rid, "error", &file_of(r), 0, "", format!("unknown rule kind {}", repr(kind)))];
        }
    };
    match res {
        Ok(f) => f,
        Err(RuleErr::BadPattern(e)) => {
            vec![Finding::new(rid, "error", &file_of(r), 0, "", format!("bad pattern in rule {rid}: {e}"))]
        }
        Err(RuleErr::Env(e)) => {
            vec![Finding::new(rid, "warn", &file_of(r), 0, "", format!("rule {rid} not checked here: {e}"))]
        }
        Err(RuleErr::Key(k)) => vec![Finding::new(rid, "error", &file_of(r), 0, "", format!("rule {rid} needs {}", repr(&k)))],
        Err(RuleErr::Deferred(why)) => {
            eprintln!("seamcheck-rs: rule {rid} deferred: {why}");
            vec![]
        }
    }
}

// --- forbid ---------------------------------------------------------------------

fn forbid(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let pat = compile(need_str(r, "pattern")?)?;
    let mode = Mode::parse(s(r, "text").unwrap_or("code"));
    let mut files = scope_files(p, r);
    if let Some(ch) = &p.gate_changed {
        files.retain(|f| ch.contains(f));
    }
    let allow = list_or(r, "allow", &[]);
    let rid = id(r);
    let sev = severity(r);
    let whole = s(r, "scope") == Some("file");
    let per: Vec<Vec<Finding>> = files
        .par_iter()
        .map(|rel| {
            let lines = text_mode(p, rel, s(r, "text").unwrap_or("code"), mode);
            let raw = p.text_for(rel, Mode::Raw);
            let hits: Vec<usize> = if whole {
                let text = lines.join("\n");
                let mut h: Vec<usize> =
                    pat.find_iter(&text).iter().map(|m| text[..m.start()].matches('\n').count()).collect();
                h.sort();
                h.dedup();
                h
            } else {
                lines.iter().enumerate().filter(|(_, ln)| pat.is_match(ln)).map(|(i, _)| i).collect()
            };
            let mut out = Vec::new();
            for i in hits {
                let func = p.func_at(rel, i + 1);
                if allowed(p, rel, func.as_ref(), &allow) || suppressed(&raw, i + 1, rid) {
                    continue;
                }
                let msg = s(r, "message").map(String::from).unwrap_or_else(|| format!("forbidden: {}", pat.pattern()));
                out.push(
                    Finding::new(rid, &sev, rel, i + 1, func.as_ref().map(|f| f.name.as_str()).unwrap_or(""), msg)
                        .snip(if i < raw.len() { take_chars(strip(&raw[i]), 160).to_string() } else { String::new() }),
                );
            }
            out
        })
        .collect();
    Ok(per.into_iter().flatten().collect())
}

/// `_text_for` with the mode string as given: anything but raw / nocomment /
/// code blanks strings too (Python's fall-through).
pub fn text_mode(p: &Project, rel: &str, mode_s: &str, mode: Mode) -> std::sync::Arc<Vec<String>> {
    if matches!(mode_s, "raw" | "nocomment" | "code") {
        p.text_for(rel, mode)
    } else {
        // an unknown mode: blanked like code, but never the module's code
        let raw = p.text(rel);
        std::sync::Arc::new(
            crate::scan::blank_text(&raw, crate::project::scan_family(rel), false).split('\n').map(String::from).collect(),
        )
    }
}

// --- require --------------------------------------------------------------------

fn require(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let pats: Vec<std::sync::Arc<Rx>> = match need(r, "pattern")? {
        Value::Array(a) => a
            .iter()
            .map(|x| compile(&format!("(?m){}", x.as_str().unwrap_or(""))))
            .collect::<Res<Vec<_>>>()?,
        v => vec![compile(&format!("(?m){}", v.as_str().unwrap_or("")))?],
    };
    let mode_s = s(r, "text").unwrap_or("nocomment");
    let mode = Mode::parse(mode_s);
    let funcs = str_list(r, "func");
    let sev = severity(r);
    let rid = id(r);
    let ordered = config::truthy(r.get("ordered"));
    let mut out = Vec::new();
    let mut miss = |rel: &str, line: usize, fname: &str, msg: String| {
        let m = if msg.contains("missing") { s(r, "message").map(String::from).unwrap_or(msg) } else { msg };
        out.push(Finding::new(rid, &sev, rel, line, fname, m));
    };
    let inn = match need(r, "in")? {
        Value::Array(a) => a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>(),
        Value::String(s) => s.chars().map(|c| c.to_string()).collect(),
        _ => vec![],
    };
    for spec in &inn {
        let files = if spec.contains(['*', '?', '[']) {
            let f: Vec<String> = p.files.iter().filter(|x| match_any(x, &[spec])).cloned().collect();
            if f.is_empty() {
                miss(spec, 0, "", format!("no file matches {spec}"));
            }
            f
        } else {
            vec![spec.clone()]
        };
        for rel in files {
            if !p.path(&rel).is_file() {
                miss(&rel, 0, "", format!("required file is gone: {rel}"));
                continue;
            }
            let lines = text_mode(p, &rel, mode_s, mode);
            let mut units: Vec<(Option<String>, usize, usize)> = vec![(None, 1, lines.len())];
            if let Some(fs) = &funcs {
                units = p
                    .modules(&rel)
                    .iter()
                    .flat_map(|m| m.funcs.iter())
                    .filter(|f| f.kind != "top" && fs.iter().any(|g| fnmatchcase(&f.name, g)))
                    .map(|f| (Some(f.name.clone()), f.start, f.end))
                    .collect();
                if units.is_empty() {
                    miss(&rel, 0, "", format!("no function {} in {rel}", fs.join(", ")));
                }
            }
            for (fname, a, b) in units {
                let body = lines[(a - 1).min(lines.len())..b.min(lines.len())].join("\n");
                let mut pos = 0;
                for pt in &pats {
                    match pt.search_at(&body, pos) {
                        None => {
                            let pat_src = pt.pattern().strip_prefix("(?m)").unwrap_or(pt.pattern());
                            miss(
                                &rel,
                                a,
                                fname.as_deref().unwrap_or(""),
                                format!("missing{}: {}", if ordered && pos > 0 { " (in order)" } else { "" }, pat_src),
                            );
                            break;
                        }
                        Some(m) => {
                            if ordered {
                                pos = m.end();
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

// --- together -------------------------------------------------------------------

fn together(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let cond = compile(need_str(r, "if")?)?;
    let thens: Vec<std::sync::Arc<Rx>> = match need(r, "then")? {
        Value::Array(a) => a.iter().map(|x| compile(x.as_str().unwrap_or(""))).collect::<Res<Vec<_>>>()?,
        v => vec![compile(v.as_str().unwrap_or(""))?],
    };
    let unless = match s(r, "unless") {
        Some(u) if !u.is_empty() => Some(compile(u)?),
        _ => None,
    };
    let scope = s(r, "scope").unwrap_or("func");
    let mode_s = s(r, "text").unwrap_or("code");
    let mode = Mode::parse(mode_s);
    let allow = list_or(r, "allow", &[]);
    let funcs = str_list(r, "func");
    let rid = id(r);
    let sev = severity(r);
    let files = scope_files(p, r);
    let per: Vec<Vec<Finding>> = files
        .par_iter()
        .map(|rel| {
            let mut out = Vec::new();
            let mods = p.modules(rel);
            let raw = p.text_for(rel, Mode::Raw);
            let lines = text_mode(p, rel, mode_s, mode);
            if scope == "near" {
                let (before, after) = window(r);
                for (i, ln) in lines.iter().enumerate() {
                    if !cond.is_match(ln) {
                        continue;
                    }
                    let func = p.func_at(rel, i + 1);
                    let lo = i.saturating_sub(before);
                    let hi = (i + after + 1).min(lines.len());
                    let win = lines[lo..hi].join("\n");
                    if unless.as_ref().map(|u| u.is_match(&win)).unwrap_or(false)
                        || allowed(p, rel, func.as_ref(), &allow)
                        || suppressed(&raw, i + 1, rid)
                    {
                        continue;
                    }
                    let missing: Vec<&str> = thens.iter().filter(|t| !t.is_match(&win)).map(|t| t.pattern()).collect();
                    if !missing.is_empty() {
                        let msg = s(r, "message")
                            .map(String::from)
                            .unwrap_or_else(|| format!("missing nearby: {}", missing.join(", ")));
                        out.push(
                            Finding::new(rid, &sev, rel, i + 1, func.as_ref().map(|f| f.name.as_str()).unwrap_or(""), msg)
                                .snip(if i < raw.len() { take_chars(strip(&raw[i]), 160).to_string() } else { String::new() }),
                        );
                    }
                }
                return out;
            }
            let mut units: Vec<(Option<crate::elixir::Func>, usize, usize)> = Vec::new();
            if scope == "file" || mods.is_empty() {
                units.push((None, 1, lines.len()));
            } else {
                for m in mods.iter() {
                    for f in &m.funcs {
                        if f.kind != "top" && funcs.as_ref().map(|fs| fs.iter().any(|g| fnmatchcase(&f.name, g))).unwrap_or(true) {
                            units.push((Some(f.clone()), f.start, f.end));
                        }
                    }
                }
            }
            for (func, a, b) in units {
                let body = lines[(a - 1).min(lines.len())..b.min(lines.len())].join("\n");
                let Some(m) = cond.search(&body) else { continue };
                if unless.as_ref().map(|u| u.is_match(&body)).unwrap_or(false) {
                    continue;
                }
                let line = a + body[..m.start()].matches('\n').count();
                if allowed(p, rel, func.as_ref(), &allow) || suppressed(&raw, line, rid) {
                    continue;
                }
                let missing: Vec<&str> = thens.iter().filter(|t| !t.is_match(&body)).map(|t| t.pattern()).collect();
                if !missing.is_empty() {
                    let msg =
                        s(r, "message").map(String::from).unwrap_or_else(|| format!("missing: {}", missing.join(", ")));
                    out.push(
                        Finding::new(rid, &sev, rel, line, func.as_ref().map(|f| f.name.as_str()).unwrap_or(""), msg)
                            .snip(if line >= 1 && line - 1 < raw.len() {
                                take_chars(strip(&raw[line - 1]), 160).to_string()
                            } else {
                                String::new()
                            }),
                    );
                }
            }
            out
        })
        .collect();
    Ok(per.into_iter().flatten().collect())
}

fn window(r: &Rule) -> (usize, usize) {
    match r.get("window") {
        Some(Value::Array(a)) if !a.is_empty() => {
            let n = |v: Option<&Value>| v.and_then(|x| x.as_i64()).unwrap_or(0).max(0) as usize;
            (n(a.first()), n(a.get(1)))
        }
        _ => (0, 6),
    }
}

// --- coverage -------------------------------------------------------------------

fn coverage(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let harvest = |spec: &Map<String, Value>| -> Res<indexmap_like::Ordered> {
        let pat = compile(need_str(spec, "pattern")?)?;
        let mut found = indexmap_like::Ordered::default();
        let inn = list_or(spec, "in", &["**/*"]);
        let g = Globs::new(&inn);
        let mode_s = s(spec, "text").unwrap_or("nocomment");
        for rel in p.files.iter().filter(|f| g.any(f)) {
            for (i, ln) in text_mode(p, rel, mode_s, Mode::parse(mode_s)).iter().enumerate() {
                for m in pat.find_iter(ln) {
                    let key = m.groups().into_iter().flatten().find(|g| !g.is_empty()).unwrap_or(m.group0());
                    found.set_default(key, (rel.clone(), i + 1));
                }
            }
        }
        Ok(found)
    };
    let d = harvest(need(r, "defined")?.as_object().ok_or_else(|| RuleErr::Key("defined".into()))?)?;
    let u = harvest(need(r, "used")?.as_object().ok_or_else(|| RuleErr::Key("used".into()))?)?;
    let ignore = list_or(r, "ignore", &[]);
    let checks = list_or(r, "check", &["unused", "undefined"]);
    let rid = id(r);
    let suffix = s(r, "message").filter(|m| !m.is_empty()).map(|m| format!(": {m}")).unwrap_or_default();
    let mut out = Vec::new();
    if checks.iter().any(|c| c == "unused") {
        let mut ks: Vec<&String> = d.keys().filter(|k| !u.contains(k) && !ignore.contains(k)).collect();
        ks.sort();
        for k in ks {
            let (rel, ln) = d.get(k).unwrap();
            out.push(Finding::new(
                rid,
                &r.get("severity").map(config::py_str).unwrap_or_else(|| "warn".into()),
                rel,
                *ln,
                "",
                format!("{} is defined but never used{suffix}", repr(k)),
            ));
        }
    }
    if checks.iter().any(|c| c == "undefined") {
        let mut ks: Vec<&String> = u.keys().filter(|k| !d.contains(k) && !ignore.contains(k)).collect();
        ks.sort();
        for k in ks {
            let (rel, ln) = u.get(k).unwrap();
            out.push(Finding::new(rid, &severity(r), rel, *ln, "", format!("{} is used but never defined{suffix}", repr(k))));
        }
    }
    Ok(out)
}

/// A tiny insertion-ordered map (Python dict semantics for harvests).
pub mod indexmap_like {
    use std::collections::HashMap;

    #[derive(Default, Clone)]
    pub struct Ordered {
        keys: Vec<String>,
        map: HashMap<String, (String, usize)>,
    }

    impl Ordered {
        pub fn set_default(&mut self, k: &str, v: (String, usize)) {
            if !self.map.contains_key(k) {
                self.keys.push(k.to_string());
                self.map.insert(k.to_string(), v);
            }
        }
        pub fn contains(&self, k: &str) -> bool {
            self.map.contains_key(k)
        }
        pub fn get(&self, k: &str) -> Option<&(String, usize)> {
            self.map.get(k)
        }
        pub fn keys(&self) -> impl Iterator<Item = &String> {
            self.keys.iter()
        }
        pub fn len(&self) -> usize {
            self.keys.len()
        }
        pub fn is_empty(&self) -> bool {
            self.keys.is_empty()
        }
    }
}

// --- lint / essential ------------------------------------------------------------

fn lint(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let name = s(r, "lint").unwrap_or("");
    let rid = id(r);
    let Some(def) = crate::lint::get(name) else {
        let mut known: Vec<&str> = crate::lint::LINTS.iter().map(|l| l.name).collect();
        known.sort();
        let near = close_match(name, &known);
        return Ok(vec![Finding::new(
            rid,
            "error",
            &file_of(r),
            0,
            "",
            format!(
                "unknown lint {}{}; known: {}",
                repr(name),
                near.map(|n| format!(" (did you mean {}?)", repr(&n))).unwrap_or_default(),
                known.join(", ")
            ),
        )]);
    };
    let empty = Map::new();
    let opts = r.get("options").and_then(|o| o.as_object()).unwrap_or(&empty);
    let bad: Vec<&String> = opts.keys().filter(|k| !def.options.contains(&k.as_str())).collect();
    if !bad.is_empty() {
        let mut known: Vec<&str> = def.options.to_vec();
        known.sort();
        let known = if known.is_empty() { "none".to_string() } else { known.join(", ") };
        return Ok(vec![Finding::new(
            rid,
            "error",
            &file_of(r),
            0,
            "",
            format!(
                "lint {name}: unknown option(s) {} (known: {known})",
                bad.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ),
        )]);
    }
    let Some(f) = def.f else { return Err(RuleErr::Deferred(format!("lint {name} is not ported yet"))) };
    let scope = match r.get("in") {
        Some(_) => list_or(r, "in", &[]),
        None => def.default_in.iter().map(|s| s.to_string()).collect(),
    };
    let pool: Vec<String> = if def.repo_wide || config::truthy(r.get("repo")) {
        let mut ex: Vec<String> = DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect();
        ex.extend(p.exclude());
        list_files(&p.root, &scope, &ex)
    } else {
        p.files.clone()
    };
    let gi = Globs::new(&scope);
    let gs = Globs::new(&list_or(r, "skip", &[]));
    let mut files: Vec<String> = pool.into_iter().filter(|f| gi.any(f) && !gs.any(f)).collect();
    if let (Some(ch), true) = (&p.gate_changed, def.per_file) {
        let mut sib: std::collections::HashSet<String> = std::collections::HashSet::new();
        for c in ch {
            if c.ends_with("package.json") {
                let d = match c.rfind('/') {
                    Some(k) => &c[..k],
                    None => "",
                };
                sib.insert(if d.is_empty() { "package-lock.json".into() } else { format!("{d}/package-lock.json") });
            }
            if let Some(stem) = c.strip_suffix(".service") {
                sib.insert(format!("{stem}.path"));
            }
        }
        files.retain(|f| ch.contains(f) || sib.contains(f));
    }
    let allow = list_or(r, "allow", &[]);
    let sev = severity(r);
    let per: Vec<Res<Vec<Finding>>> = files
        .par_iter()
        .map(|rel| {
            let raw_lines = p.text_for(rel, Mode::Raw);
            let raw_text = raw_lines.join("\n");
            let code = p.joined(rel, Mode::Code);
            let hits = f(rel, &raw_text, &code, opts, p)?;
            let mut out = Vec::new();
            for (line, msg) in hits {
                let func = p.enclosing(rel, line);
                if allowed(p, rel, func.as_ref(), &allow) || suppressed(&raw_lines, line, rid) {
                    continue;
                }
                let m = s(r, "message").map(String::from).unwrap_or(msg);
                out.push(
                    Finding::new(rid, &sev, rel, line, func.as_ref().map(|f| f.name.as_str()).unwrap_or(""), m)
                        .snip(snippet(&raw_lines, line)),
                );
            }
            Ok(out)
        })
        .collect();
    let mut out = Vec::new();
    for x in per {
        out.extend(x?);
    }
    Ok(out)
}

fn essential(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let rid = id(r);
    let sev = severity(r);
    let mut out = Vec::new();
    for (rel, line, msg) in crate::lint::essential(p, r)? {
        let raw: Vec<String> = if line > 0 { p.text(&rel).split('\n').map(String::from).collect() } else { vec![] };
        if line > 0 && suppressed(&raw, line, rid) {
            continue;
        }
        out.push(Finding::new(rid, &sev, &rel, line, "", msg).snip(snippet(&raw, line)));
    }
    Ok(out)
}
