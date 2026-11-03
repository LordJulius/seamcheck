//! kind = "path" (arch.path): no call chain from `from` (function specs) may
//! reach a call matching `to` (regexes over `qual.name` as written:
//! `os\.system`, `\.execute$`, `^eval$`) within `depth` exact calls, unless
//! it passes a function named in `unless` (a sanitizer, an auth check). One
//! finding per entry point and sink, with the chain.
//!
//! With `source` (a regex: `request\.args`), it is a taint check: the entry
//! points are the functions that read the source (tests aside), and a sink
//! counts only when its arguments carry the value: the source itself, a name
//! assigned from it (or from such a name), or, below the entry, a parameter
//! the caller passed a tainted argument to. A value through a call named like
//! an `unless` entry is clean, and so is a constant first argument (a
//! parameterized query, a fixed redirect), unless it names a shell. Each sink
//! is reported once, by its shortest chain.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

use super::taint::{self, own_code, At, Clean, Ctx, Taint};
use crate::config::{id, s, str_list, truthy, Rule};
use crate::graph::infer::{esc, re, rxd};
use crate::graph::Graph;
use crate::model::Func;
use crate::project::{Finding, Project};
use crate::model::Call;
use crate::rules::security::{self, Sinks, TaintCtx};
use crate::rules::{compile, need, Res};

fn patterns(r: &Rule, k: &str) -> Res<Vec<Arc<crate::pyre::Rx>>> {
    need(r, k)?;
    str_list(r, k).unwrap_or_default().iter().map(|p| compile(p)).collect()
}

/// `int(r.get("depth", 6))`
fn depth(r: &Rule) -> usize {
    match r.get("depth") {
        Some(serde_json::Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(6).max(0) as usize,
        Some(serde_json::Value::String(t)) => t.trim().parse::<i64>().unwrap_or(6).max(0) as usize,
        Some(serde_json::Value::Bool(b)) => *b as usize,
        _ => 6,
    }
}

/// The sanitizer patterns of `unless` (a call whose name holds an entry,
/// `mysqli_real_escape_string(`; a short one, `int`, only as the whole name;
/// or a method of that name, `params[:id].to_i`).
fn clean_of(unless: &[String]) -> Res<Option<Clean>> {
    if unless.is_empty() {
        return Ok(None);
    }
    let names: Vec<String> = unless
        .iter()
        .map(|u| if u.chars().count() > 3 { format!(r"\w*{}\w*", esc(u)) } else { format!(r"(?<![\w.]){}", esc(u)) })
        .collect();
    let call = compile(&format!(r"(?:{})\s*\(", names.join("|")))?;
    let method = compile(&format!(r"\.(?:{})\b", unless.iter().map(|u| esc(u)).collect::<Vec<_>>().join("|")))?;
    Ok(Some(Clean { call, method }))
}

pub fn path(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let sinks = patterns(r, "to")?;
    let source = match s(r, "source") {
        Some(x) if !x.is_empty() => Some(compile(x)?),
        _ => None,
    };
    let mut unless: Vec<String> = Vec::new();
    let mut names = str_list(r, "unless").unwrap_or_default();
    if source.is_some() {
        // the project's own clean-returning functions ([security] sanitizers)
        let sec = serde_json::json!({"security": serde_json::Value::Object(p.config.table("security").clone())});
        names.extend(security::sanitizers(&sec));
    }
    for u in names {
        if !unless.contains(&u) {
            unless.push(u);
        }
    }
    let mut clean = clean_of(&unless)?;
    let mut kinds = Sinks::new(r).map_err(crate::rules::RuleErr::BadPattern)?;
    let depth = depth(r);
    let fuzzy = truthy(r.get("fuzzy"));
    need(r, "from")?;
    let g = p.graph();
    let mut starts: Vec<&Func> = Vec::new();
    for spec in str_list(r, "from").unwrap_or_default() {
        starts.extend(g.find(&spec));
    }
    // entry id -> the parameters the framework fills from the request
    let mut bound: HashMap<String, Vec<String>> = HashMap::new();
    let mut cx: Option<Ctx> = None;
    if let Some(src) = &source {
        // a handler the framework passes request values to: `@GetMapping` + `String cmd`, `[HttpPost]` + `[FromBody] T x`
        let entry = s(r, "source_entry").filter(|x| !x.is_empty()).map(compile).transpose()?;
        let param = s(r, "source_param").filter(|x| !x.is_empty()).map(compile).transpose()?;
        if let (Some(entry), Some(param)) = (&entry, &param) {
            for f in &starts {
                if f.kind != "top" && entry.is_match(&taint::annotations(&g, f)) {
                    let mut got: Vec<String> =
                        taint::param_decls(&g, f).into_iter().filter(|(_, d)| param.is_match(d)).map(|(n, _)| n).collect();
                    got.sort();
                    got.dedup();
                    if !got.is_empty() {
                        bound.insert(f.id.clone(), got);
                    }
                }
            }
        }
        // a helper that reads the request and returns it: `getCookie(request, "x")`, `WebUtils.getRequestBody(request)`
        let mut rd: HashSet<&str> = g
            .all_funcs()
            .filter(|f| {
                f.kind != "top"
                    && re!(r"[A-Za-z_]\w*$").match_start(&f.name).is_some()
                    && !crate::gate::is_test(p, &f.file)
                    && re!(r"\breturn\b").is_match(&own_code(&g, f, false))
                    && src.is_match(&own_code(&g, f, true))
            })
            .map(|f| f.name.as_str())
            .collect();
        let not_param = truthy(r.get("source_not_param"));
        // (raw text: `"#{params[:id]}"` reads the source inside a string)
        let direct: Vec<&Func> = starts
            .iter()
            .copied()
            .filter(|f| !crate::gate::is_test(p, &f.file) && (bound.contains_key(&f.id) || reads_source(&g, f, src, not_param)))
            .collect();
        if rd.is_empty() && direct.is_empty() {
            return Ok(Vec::new()); // nothing reads the source
        }
        // what each function returns, by name: a sanitized value (its own sanitizer call, url_for, or such a
        // function's result), an in-memory buffer (security.returns)
        if kinds.any() {
            // (kept per load and sanitizer pattern, as security.returns does)
            let call = clean.as_ref().map(|c| c.call.clone());
            let key = format!("security.returns\0{}", call.as_ref().map(|c| c.pattern()).unwrap_or(""));
            let got = p.memo(&key, || security::returns(&g, &|h: &Func| own_code(&g, h, true), call.as_deref()));
            let (done, buffers) = (got.0.clone(), got.1.clone());
            kinds.buffers = buffers;
            if !done.is_empty() {
                let mut names: Vec<&String> = done.iter().collect();
                names.sort();
                let extra = format!(r"(?<![\w$])(?:{})\s*\(", names.iter().map(|n| esc(n)).collect::<Vec<_>>().join("|"));
                clean = Some(match clean.take() {
                    Some(c) => Clean { call: compile(&format!("(?:{})|(?:{extra})", c.call.pattern()))?, method: c.method },
                    None => Clean { call: compile(&extra)?, method: compile(r"(?!x)x")? },
                });
                rd.retain(|n| !done.contains(*n));
            }
        }
        let mut rd: Vec<&str> = rd.into_iter().collect();
        rd.sort();
        let readers = if rd.is_empty() {
            None
        } else {
            Some(rxd(&format!(r"(?<![\w$])((?:[A-Za-z_]\w*\.)?)(?:{})\s*\(", rd.join("|"))))
        };
        let seen: HashSet<&str> = direct.iter().map(|f| f.id.as_str()).collect();
        let more: Vec<&Func> = starts
            .iter()
            .copied()
            .filter(|f| {
                !seen.contains(f.id.as_str())
                    && !crate::gate::is_test(p, &f.file)
                    && taint::reads(readers.as_deref(), &own_code(&g, f, false))
            })
            .collect();
        starts = direct.into_iter().chain(more).collect();
        let c = Ctx { source: src.clone(), clean: clean.take(), readers };
        if let Some(scope) = s(r, "source_scope").filter(|x| !x.is_empty()) {
            // (the web request: not a desktop tool's QNetworkRequest named `request`)
            let keep = security::scoped(&g, &*compile(scope)?, depth);
            starts.retain(|f| keep.contains(&f.id));
        }
        cx = Some(c);
    }
    let mut uniq: BTreeMap<&str, &Func> = BTreeMap::new();
    for f in starts {
        uniq.insert(f.id.as_str(), f);
    }
    let rid = id(r);
    let mut out = Vec::new();
    let mut best: IndexMap<(String, usize), (usize, Finding)> = IndexMap::new();
    for s0 in uniq.into_values() {
        if unless.contains(&s0.name) {
            continue;
        }
        let mut prev: HashMap<String, Option<String>> = HashMap::from([(s0.id.clone(), None)]);
        // (taint) the parameters each function was passed a tainted value in
        let mut given: HashMap<String, Vec<String>> =
            HashMap::from([(s0.id.clone(), bound.get(&s0.id).cloned().unwrap_or_default())]);
        let mut frontier = vec![s0.id.clone()];
        let mut hits: BTreeMap<(String, usize), (String, String, String, String)> = BTreeMap::new();
        for _ in 0..depth + 1 {
            let mut nxt = Vec::new();
            for fid in &frontier {
                let Some(f) = g.func(fid) else { continue };
                let tn = cx.as_ref().map(|c| Taint::new(&g, f, c, &given[fid], Some(&kinds)));
                for c in &f.calls {
                    let text = format!(
                        "{}{}",
                        if !c.qual.is_empty() && c.qual != "." && c.qual != "self" {
                            format!("{}.", c.qual)
                        } else if c.qual == "." {
                            ".".to_string()
                        } else {
                            String::new()
                        },
                        c.name
                    );
                    if hits.contains_key(&(f.file.clone(), c.line)) {
                        continue;
                    }
                    let Some((sev, note)) = sink(&g, f, c, &text, &sinks, &kinds, cx.as_ref().zip(tn.as_ref())) else {
                        continue;
                    };
                    if let (Some(cx), Some(tn)) = (&cx, &tn) {
                        let sunk: HashSet<usize> = hits.keys().filter(|(fl, _)| *fl == f.file).map(|(_, ln)| *ln).collect();
                        if tn.downstream(&g, f, c, cx, &sunk) {
                            continue;
                        }
                    }
                    hits.insert((f.file.clone(), c.line), (fid.clone(), text, sev, note));
                }
                for (dst, ln, exact) in g.edges.get(fid).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if !(*exact || fuzzy) {
                        continue;
                    }
                    let Some(h) = g.func(dst) else { continue };
                    if unless.contains(&h.name) || (prev.contains_key(dst) && cx.is_none()) {
                        continue;
                    }
                    if let (Some(cx), Some(tn)) = (&cx, &tn) {
                        let params: Vec<String> = taint::param_decls(&g, h).into_iter().map(|x| x.0).collect();
                        let mut got: HashSet<String> = HashSet::new();
                        for c in f.calls.iter().filter(|c| c.line == *ln && c.name == h.name) {
                            got.extend(taint::passed(&g, f, c, &params, cx, &tn.at(c.line)));
                        }
                        let had = given.get(dst);
                        if got.is_empty() || (prev.contains_key(dst) && had.is_some_and(|h| got.iter().all(|x| h.contains(x)))) {
                            continue; // nothing (new) tainted goes in
                        }
                        if prev.contains_key(dst) {
                            // another call passes it another tainted parameter: walk it again with both
                            got.extend(had.into_iter().flatten().cloned());
                            let mut got: Vec<String> = got.into_iter().collect();
                            got.sort();
                            given.insert(dst.clone(), got);
                            nxt.push(dst.clone());
                            continue;
                        }
                        let mut got: Vec<String> = got.into_iter().collect();
                        got.sort();
                        given.insert(dst.clone(), got);
                    }
                    prev.insert(dst.clone(), Some(fid.clone()));
                    nxt.push(dst.clone());
                }
            }
            frontier = nxt;
        }
        for ((file, line), (fid, text, sev, note)) in hits {
            let mut chain: Vec<&str> = Vec::new();
            let mut k = Some(fid.clone());
            while let Some(x) = k {
                chain.push(g.func(&x).map(|f| f.name.as_str()).unwrap_or(""));
                k = prev.get(&x).cloned().flatten();
            }
            chain.reverse();
            let msg = format!(
                "{}{}",
                s(r, "message").map(String::from).unwrap_or_else(|| format!("reaches {text}(){note} at {file}:{line}: ")),
                chain.join(" -> ")
            );
            let fd = Finding::new(rid, &sev, &s0.file, s0.start, &s0.name, msg);
            if cx.is_none() {
                out.push(fd);
            } else {
                let key = (file, line);
                match best.get_mut(&key) {
                    Some(b) if chain.len() >= b.0 => {}
                    Some(b) => *b = (chain.len(), fd),
                    None => {
                        best.insert(key, (chain.len(), fd));
                    }
                }
            }
        }
    }
    let mut rest: Vec<Finding> = best.into_values().map(|x| x.1).collect();
    rest.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    out.extend(rest);
    Ok(out)
}

/// f reads the source; with `not_param`, not when the name it starts with is
/// one of f's own parameters (a fastlane action's `def self.run(params)` is
/// not Rails' params) (arch._reads_source).
fn reads_source(g: &Graph, f: &Func, source: &crate::pyre::Rx, not_param: bool) -> bool {
    let text = own_code(g, f, true);
    if !not_param {
        return source.is_match(&text);
    }
    let head = g
        .module(&f.module)
        .filter(|m| f.start <= m.code.len() && f.start >= 1)
        .map(|m| m.code[f.start - 1].as_str())
        .unwrap_or("");
    // (the def line's own: `def show` has none)
    let own: HashSet<String> = match re!(r"\bdef\s+[\w.]+[?!=]?\s*\(?([^)\n#]*)").search(head) {
        Some(sig) => {
            let cut = re!(r"=.*?(,|$)").sub(r"\1", sig.s(1));
            re!(r"[A-Za-z_]\w*").findall(&cut).into_iter().map(String::from).collect()
        }
        None => HashSet::new(),
    };
    source.find_iter(&text).iter().any(|m| {
        let lead = re!(r"[$@]?\w*").match_start(m.group0()).map(|x| x.group0()).unwrap_or("");
        !own.contains(lead)
    })
}

/// (severity, note) when call c is a sink the value reaches the way it does
/// harm, else None. A categorised sink (`sql_sinks`, `url_sinks`, ...) reads
/// only the argument it uses (rules::security); Python's ORM session is no
/// HTTP client, a project subclass of one is (arch._sink).
fn sink(
    g: &Graph,
    f: &Func,
    c: &Call,
    text: &str,
    sinks: &[Arc<crate::pyre::Rx>],
    kinds: &Sinks,
    taint: Option<(&Ctx, &Taint)>,
) -> Option<(String, String)> {
    let sev = kinds.severity.clone();
    let hit = sinks.iter().any(|p| p.is_match(text));
    if !hit && !(!kinds.url.is_empty() && f.file.ends_with(".py") && kinds.url.iter().any(|p| p.is_match(&format!("session.{}", c.name)))) {
        return None;
    }
    let at = taint.map(|(cx, tn)| At { g, f, taint: tn, cx, line: c.line });
    let mut recv = None;
    if !hit || kinds.kind(text) == Some("url") {
        let line = g.module(&f.module).filter(|m| 0 < c.line && c.line <= m.raw.len()).map(|m| m.raw[c.line - 1].as_str()).unwrap_or("");
        recv = security::receiver(g, f, c, line, at.as_ref().map(|a| a as &dyn TaintCtx));
    }
    let mut text = text.to_string();
    if !hit {
        if recv != Some("http") {
            return None;
        }
        text = format!("session.{}", c.name); // `api.get(url)` with `class Api(requests.Session)`
        if !sinks.iter().any(|p| p.is_match(&text)) {
            return None;
        }
    }
    let kind = kinds.kind(&text);
    if recv == Some("orm") && kind == Some("url") {
        return None; // `db.session.get(User, pk)`: a row by its key, no request
    }
    let Some((cx, tn)) = taint else { return Some((sev, String::new())) };
    let Some(kind) = kind else {
        return taint::carries(g, f, c, cx, &tn.at(c.line), true).then(|| (sev, String::new()));
    };
    let args = taint::call_args(g, f, c);
    if kind == "url" {
        let first = args.iter().find(|a| security::kwarg(a).is_none()).map(|a| a.as_str()).unwrap_or("");
        if re!(r"\s*[A-Z]\w*\s*").fullmatch(first).is_some()
            && g.short.contains_key(crate::pyre::strip(first))
            && c.name == "get"
        {
            return None; // `session.get(Post, pk)`: a model class, so an ORM session
        }
    }
    kinds.verdict(kind, &args, at.as_ref().unwrap())
}
