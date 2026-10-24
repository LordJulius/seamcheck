//! The brace adapter's per-function helpers: lambda parameters, framework
//! parameters, JS reassignments, module-level calls, class initializers,
//! C macros that define functions, QML component types (lang_other's
//! _bind_lambdas, _bind_closure, _bind_param, _pack_params,
//! _js_reassignments, _field_initializer, _class_initializers, _top_calls,
//! _macro_defined, _qml_types).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use indexmap::IndexMap;

use super::pats::{call_rx, call_skip, not_func};
use super::util::{count_nl, esc, join, lstrip, rb, re, rstrip, rw, rxd, sl, strip};
use super::Idx;
use crate::lang::assign::assignments;
use crate::lang::chains::{expression, receiver, Opts};
use crate::lang::lambdas::{brace_lambda, lambda_args, matching, split_top};
use crate::model::{Call, Func, Module};
use crate::pyre::Rx;

// --- framework parameters (_pack_params) -------------------------------------------

struct PackSig {
    pid: String,
    names: Vec<String>,
    rx: Rx,
    call_rx: Option<Rx>,
}

fn pack_sigs() -> &'static [PackSig] {
    static S: OnceLock<Vec<PackSig>> = OnceLock::new();
    S.get_or_init(|| {
        let mut out = Vec::new();
        for name in crate::config::available_packs() {
            let Some((_, text)) = crate::config::PACKS.iter().find(|p| p.0 == name) else { continue };
            let Ok(v) = toml::from_str::<toml::Value>(text) else { continue };
            let Some(params) = v.get("param").and_then(|x| x.as_array()) else { continue };
            for entry in params {
                let names: Vec<String> = entry
                    .get("names")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                if names.is_empty() {
                    continue;
                }
                let id = entry.get("id").and_then(|x| x.as_str()).unwrap_or("");
                let joined: Vec<String> = names.iter().map(|n| esc(n)).collect();
                let lst = format!(
                    r"\(\s*(?:[A-Za-z_$][\w$]*\s*,\s*)?{}{}",
                    joined.join(r"\s*,\s*"),
                    if names.len() > 1 { r"\s*(?:,[^()]*)?\)" } else { r"\s*\)" }
                );
                let fnp = format!(r"(?:\bfunction\b[\s\w$]*{lst}|(?:\basync\s*)?{lst}\s*(?::[^=;]+?)?=>)");
                let calls: Vec<String> = entry
                    .get("in_call")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(esc)).collect())
                    .unwrap_or_default();
                let call_rx = (!calls.is_empty())
                    .then(|| Rx::lit(&format!(r"\.(?:{})\s*(?:<[^()]*?>)?\s*\(", calls.join("|"))));
                out.push(PackSig { pid: format!("{name}/{id}"), names, rx: Rx::lit(&fnp), call_rx });
            }
        }
        out
    })
}

/// Parameters a framework hands this function or a callback in it (`_pack_params`).
pub fn pack_params(f: &mut Func) {
    for s in pack_sigs() {
        if !f.code.contains(s.names[0].as_str()) {
            continue;
        }
        let hit = match &s.call_rx {
            None => s.rx.search(&f.code).is_some(),
            Some(crx) => {
                let b = f.code.as_bytes();
                let mut hit = false;
                for cm in crx.find_iter(&f.code) {
                    let (mut depth, mut k) = (1i64, cm.end());
                    while k < b.len() && depth != 0 {
                        depth += match b[k] {
                            b'(' => 1,
                            b')' => -1,
                            _ => 0,
                        };
                        k += 1;
                    }
                    if s.rx.search_range(&f.code, cm.end(), k).is_some() {
                        hit = true;
                        break;
                    }
                }
                hit
            }
        };
        if hit {
            for n in &s.names {
                f.bound.entry(n.clone()).or_insert(format!("pack:{}:{n}", s.pid));
            }
        }
    }
}

// --- lambda parameters --------------------------------------------------------------

/// A lambda parameter for the whole function and on the lambda's own lines (`_bind_param`).
/// `sizes`: "name@line" -> the extent of the lambda bound there (Python's _LAM_SIZE).
#[allow(clippy::too_many_arguments)]
fn bind_param(
    f: &mut Func,
    sizes: &mut HashMap<String, isize>,
    nm: &str,
    spec: &str,
    body: &str,
    a: isize,
    b: isize,
    first_line: isize,
) {
    match f.bound.get(nm) {
        None => {
            f.bound.insert(nm.to_string(), spec.to_string());
        }
        Some(held) if held.starts_with("lam:") && held != spec => {
            f.bound.insert(nm.to_string(), "lam:?".into());
        }
        Some(held) if !(held.starts_with("lam:") || held.starts_with("expr:")) => return,
        _ => {}
    }
    if first_line < 0 || b < a {
        return;
    }
    let (a, b) = (a as usize, b as usize);
    let l0 = first_line as usize + count_nl(body, 0, a) + 1;
    let l1 = l0 + count_nl(body, a, b);
    for ln in l0..=l1 {
        let key = format!("{nm}@{ln}");
        let take = match f.bound.get(&key) {
            None => true,
            Some(old) => old.starts_with("lam:") && ((b - a) as isize) < *sizes.get(&key).unwrap_or(&(1 << 30)),
        };
        if take {
            f.bound.insert(key.clone(), spec.to_string()); // (the innermost lambda over a line wins)
            sizes.insert(key, (b - a) as isize);
        }
    }
}

/// `run(x, b => b.go())`: `b` is what run's parameter there hands its callback (`_bind_lambdas`).
#[allow(clippy::too_many_arguments)]
pub fn bind_lambdas(
    f: &mut Func,
    sizes: &mut HashMap<String, isize>,
    fam: &str,
    body: &str,
    rbody: &str,
    open: usize,
    name: &str,
    chain: &[String],
    first_line: isize,
    targs: i64,
) {
    let bb = body.as_bytes();
    if open >= bb.len() || bb[open] != b'(' || name.contains('\x1e') {
        return;
    }
    let close = matching(body, open);
    if close < 0 {
        return;
    }
    let close = close as usize;
    let args = split_top(&body[open + 1..close], ',');
    let mut found = lambda_args(fam, &args);
    let mut spans: Vec<(isize, isize)> = Vec::new();
    let mut at = open + 1;
    for a in &args {
        spans.push((at as isize, (at + a.len()) as isize));
        at += a.len() + 1;
    }
    if fam == "kotlin" || fam == "swift" {
        // a trailing lambda: `run(x) { b -> ... }`
        let mut k = close + 1;
        while k < bb.len() && (bb[k] == b' ' || bb[k] == b'\t') {
            k += 1;
        }
        if bb.get(k) == Some(&b'{') {
            found.push((args.len(), brace_lambda(fam, rw(body, rbody, k, 200))));
            spans.push((k as isize, matching(body, k)));
        }
    }
    for (argi, names) in found {
        let (a, b) = spans.get(argi).copied().unwrap_or((0, -1));
        for (pi, nm) in names.iter().enumerate() {
            if nm.is_empty() || nm == "_" {
                continue;
            }
            let spec = format!("lam:{argi}/{pi}/{}/{targs}\x1e{name}\x1e{}", names.len(), join(chain));
            bind_param(f, sizes, nm, &spec, body, a, b, first_line);
        }
    }
}

/// `xs.map { x -> ... }`: the closure's parameters (`_bind_closure`).
#[allow(clippy::too_many_arguments)]
pub fn bind_closure(
    f: &mut Func,
    sizes: &mut HashMap<String, isize>,
    fam: &str,
    body: &str,
    rbody: &str,
    brace: usize,
    name: &str,
    chain: &[String],
    first_line: isize,
) {
    let names = brace_lambda(fam, rw(body, rbody, brace, 400));
    let end = matching(body, brace);
    for (pi, nm) in names.iter().enumerate() {
        if nm.is_empty() || nm == "_" {
            continue;
        }
        let spec = format!("lam:0/{pi}/{}/-1\x1e{name}\x1e{}", names.len(), join(chain));
        bind_param(f, sizes, nm, &spec, body, brace as isize, end, first_line);
    }
}

// --- JS reassignments (_js_reassignments) ------------------------------------------

pub fn js_reassignments(f: &mut Func, lines: &[String], sa: usize, sb: usize, o: Opts) {
    let mut decl: HashMap<String, usize> = HashMap::new();
    for i in sa..=sb {
        let ln = lines[i].as_str();
        let ind = ln.len() - lstrip(ln).len();
        if let Some(dm) = re!(r"\s*(?:let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=;]+)?=").match_start(ln) {
            decl.entry(dm.s(1).to_string()).or_insert(ind);
            continue;
        }
        let Some(rm) = re!(r"\s*(?:[\w$.]+\s*=\s*)?([A-Za-z_$][\w$]*)\s*=(?![=>])\s*").match_start(ln) else { continue };
        if decl.get(rm.s(1)) != Some(&ind) {
            continue;
        }
        let ch = expression(ln, rm.end(), 40, o);
        if !ch.is_empty() && rstrip(&ln[rm.end()..]).trim_end_matches(';').ends_with(')') && ch[0] != rm.s(1) {
            f.bound.insert(format!("{}\0from{}", rm.s(1), i + 1), format!("expr:{}", join(&ch)));
        }
    }
}

// --- module-level calls -------------------------------------------------------------

/// A call in a field's initializer, not an expression-bodied member (`_field_initializer`).
/// `c.col` is still a byte column here.
pub fn field_initializer(lines: &[String], c: &Call) -> bool {
    let mut i = c.line - 1;
    while i > 0 && !super::util::ends_any(rstrip(&lines[i - 1]), &[";", "{", "}", "]"]) && c.line - i < 12 {
        i -= 1;
    }
    let mut parts: Vec<&str> = lines[i..c.line - 1].iter().map(|s| s.as_str()).collect();
    let last = &lines[c.line - 1];
    parts.push(sl(last, 0, c.col.max(0) as usize));
    let stmt = parts.join("\n");
    match re!(r"(?<![=!<>+\-*/%&|^])=(?![=>])").search(&stmt) {
        None => false,
        Some(em) => {
            !stmt[..em.start()].contains("=>") && re!(r"\b(?:get|set|init)\s*=>|\)\s*=>").search(&stmt).is_none()
        }
    }
}

/// Java / C# / Kotlin: a call in a class body outside its methods is that class's `<init>` (`_class_initializers`).
/// `mods`: every module of the file, the file's first.
pub fn class_initializers(top: &mut Func, mods: &mut IndexMap<String, Module>, code: &str, idx: &Idx, rel: &str) {
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    for m_ in mods.values() {
        let Some(d0) = m_.decls.first() else { continue };
        let pat = rxd(&format!(r"\b(?:class|interface|object|record|struct|enum)\s+{}\b[^{{;]*\{{", esc(d0)));
        let Some(dm) = pat.search(code) else { continue };
        let close = matching(code, dm.end() - 1);
        if close < 0 {
            continue;
        }
        spans.push((idx.of(dm.start()) + 1, idx.of(close as usize) + 1, m_.name.clone()));
    }
    if spans.is_empty() {
        return;
    }
    let mut inits: IndexMap<String, Func> = IndexMap::new();
    let mut keep = Vec::new();
    for c in std::mem::take(&mut top.calls) {
        let around: Vec<&(usize, usize, String)> = spans.iter().filter(|sp| sp.0 <= c.line && c.line <= sp.1).collect();
        if around.is_empty() {
            keep.push(c);
            continue;
        }
        let mut best = around[0];
        for sp in &around[1..] {
            if sp.1 - sp.0 < best.1 - best.0 {
                best = sp;
            }
        }
        let g = inits
            .entry(best.2.clone())
            .or_insert_with(|| Func::new(&best.2, "<init>", rel, best.0, best.1, String::new(), "top"));
        g.calls.push(c);
    }
    top.calls = keep;
    for (mname, g) in inits {
        if let Some(m_) = mods.get_mut(&mname) {
            m_.funcs.push(g);
        }
    }
}

/// JS / TS / PHP / Java / C#: the calls a file makes outside its functions (`_top_calls`).
#[allow(clippy::too_many_arguments)]
pub fn top_calls(top: &mut Func, mods: &IndexMap<String, Module>, code: &str, lines: &[String], idx: &Idx, tk: &str, fam: &str, o: Opts) {
    let mut covered: HashSet<usize> = HashSet::new();
    for m_ in mods.values() {
        for g in &m_.funcs {
            for i in g.start.saturating_sub(1)..g.end {
                covered.insert(i);
            }
        }
    }
    for tm in re!(r"(?m)^[ \t]*(?:export\s+)?(?:declare\s+(?:global|module|namespace)\b[^{\n]*|(?:declare\s+)?interface\s+[^{\n]*|(?:export\s+)?(?:declare\s+)?type\s+\w+[^=\n]*=\s*)\{")
        .find_iter(code)
    {
        let a = idx.of(tm.start());
        let close = matching(code, tm.end() - 1);
        let b = if close > 0 { idx.of(close as usize) } else { a };
        for i in a..=b {
            covered.insert(i);
        }
    }
    let keep: Vec<bool> = (0..lines.len()).map(|i| !covered.contains(&i)).collect();
    let text: String =
        lines.iter().enumerate().map(|(i, ln)| if keep[i] { ln.as_str() } else { "" }).collect::<Vec<_>>().join("\n");
    for (nm, held) in assignments(tk, &text, o) {
        top.bound.entry(nm).or_insert(held);
    }
    let sig = re!(r"^\s*(?:(?:public|private|protected|static|readonly|abstract|declare|export|async|get|set)\s+)*[\w$]+\??\s*(?:<[^>]*>)?\s*\([^)]*\)\s*:\s*[^=;{]+;\s*$");
    let mut off = 0usize;
    for (i, ln) in lines.iter().enumerate() {
        if keep[i] && sig.match_start(ln).is_none() {
            for m in call_rx().find_iter(ln) {
                let nm = m.s(2);
                let s2 = m.span(2).unwrap().0;
                if re!(r"\bfunction\s*\*?\s*$").search(&ln[..s2]).is_some() {
                    continue; // `function name(`: a declaration
                }
                if fam == "js"
                    && rxd(&format!(r"\s*(?:(?:async|static|get|set)\s+)*\*?\s*{}\s*\([^()]*\)\s*\{{", esc(nm)))
                        .match_start(ln)
                        .is_some()
                    && matches!(strip(&ln[..s2]), "" | "async" | "static" | "get" | "set" | "*")
                {
                    continue; // `set(headerValue) {` in an object literal: a method's definition
                }
                let chain = receiver(&text, off + s2, 40, o);
                let before = rstrip(&ln[..s2]);
                let mut q = if !m.s(1).is_empty() {
                    m.s(1).to_string()
                } else if (before.ends_with('.') || before.ends_with("->")) && !before.ends_with("...") {
                    ".".into()
                } else {
                    String::new()
                };
                if !chain.is_empty() && !q.is_empty() && q != "." {
                    let last = chain.last().unwrap();
                    q = if re!(r"^[\w$]+$").is_match(last) { last.clone() } else { ".".into() };
                }
                if call_skip(nm) && (q.is_empty() || (not_func(nm) && nm != "new")) {
                    continue;
                }
                let mut c = Call::new(nm, &q, i + 1);
                c.chain = if q.is_empty() { Vec::new() } else { chain };
                c.col = s2 as i64;
                top.calls.push(c);
            }
        }
        off += (if keep[i] { ln.len() } else { 0 }) + 1;
    }
    if fam == "js" {
        // `validate<[\n  number,\n]>(...)`: a call whose type arguments go on over lines
        let tidx = Idx::new(&text);
        for gm in re!(r"(?<![\w$.])([A-Za-z_$][\w$]*)[ \t]*<(?=[^;()]*\n)[^;()=]*>[ \t]*\(").find_iter(&text) {
            let s1 = gm.span(1).unwrap().0;
            let ln0 = tidx.of(s1);
            if keep[ln0] && !call_skip(gm.s(1)) && gm.group0().contains('\n') {
                let mut c = Call::new(gm.s(1), "", ln0 + 1);
                c.col = (s1 - tidx.starts[ln0]) as i64;
                top.calls.push(c);
            }
        }
    }
}

// --- C macros that define functions (_macro_defined) --------------------------------

type Found = (usize, usize, String, usize, isize);

pub fn macro_defined(code: &str, text: &str, idx: &Idx, found: Vec<Found>) -> Vec<Found> {
    // name -> (params, body, start, end)
    let mut defs: IndexMap<String, (Vec<String>, String, usize, usize)> = IndexMap::new();
    for m in re!(r"(?m)^[ \t]*#[ \t]*define[ \t]+([A-Za-z_]\w*)\(([^)]*)\)((?:[^\n]*\\\n)*[^\n]*)").find_iter(text) {
        let params: Vec<String> = m.s(2).split(',').map(|p| strip(p).to_string()).collect();
        defs.insert(m.s(1).to_string(), (params, m.s(3).replace("\\\n", " "), m.start(), m.end()));
    }
    if defs.is_empty() {
        return found;
    }
    let spans: Vec<(usize, usize, HashSet<String>)> =
        defs.values().map(|(p, _, s, e)| (*s, *e, p.iter().cloned().collect())).collect();
    let found: Vec<Found> = found
        .into_iter()
        .filter(|f| !spans.iter().any(|(s, e, ps)| *s <= f.3 && f.3 < *e && ps.contains(&f.2)))
        .collect();

    fn named(defs: &IndexMap<String, (Vec<String>, String, usize, usize)>, name: &str, depth: usize) -> Vec<usize> {
        let (params, body, _, _) = &defs[name];
        let mut out = Vec::new();
        for (i, p) in params.iter().enumerate() {
            if rxd(&format!(r"[\w&*>][ \t]+(?:\w+::)*{}[ \t]*\([^()]*\)[\w \t]*\{{", esc(p))).search(body).is_some() {
                out.push(i);
            }
        }
        if depth < 3 {
            for m in re!(r"\b([A-Za-z_]\w*)[ \t]*\(([^()]*)\)").find_iter(body) {
                if defs.contains_key(m.s(1)) && m.s(1) != name {
                    let args: Vec<&str> = m.s(2).split(',').map(strip).collect();
                    for j in named(defs, m.s(1), depth + 1) {
                        if j < args.len() {
                            if let Some(k) = params.iter().position(|p| p == args[j]) {
                                out.push(k);
                            }
                        }
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    let makers: IndexMap<String, Vec<usize>> = defs
        .keys()
        .filter_map(|n| {
            let ix = named(&defs, n, 0);
            (!ix.is_empty()).then(|| (n.clone(), ix))
        })
        .collect();
    let mut out = found;
    if makers.is_empty() {
        return out;
    }
    let alt: Vec<String> = makers.keys().map(|k| esc(k)).collect();
    let pat = rxd(&format!(r"(?m)^[ \t]*({})[ \t]*\(", alt.join("|")));
    for m in pat.find_iter(code) {
        if spans.iter().any(|(s, e, _)| *s <= m.start() && m.start() < *e) {
            continue;
        }
        let close = matching(code, m.end() - 1);
        if close < 0 {
            continue;
        }
        let close = close as usize;
        let mut args: Vec<(String, usize)> = Vec::new();
        let mut pos = m.end();
        for part in sl(code, m.end(), close).split(',') {
            let lead = part.len() - lstrip(part).len();
            args.push((strip(part).to_string(), pos + lead));
            pos += part.len() + 1;
        }
        for &i in &makers[m.s(1)] {
            if i < args.len() && re!(r"[A-Za-z_]\w*").fullmatch(&args[i].0).is_some() {
                out.push((idx.of(args[i].1), idx.of(close), args[i].0.clone(), args[i].1, -1));
            }
        }
    }
    out
}

// --- QML component types (_qml_types) -----------------------------------------------

const QML_BASIC: &[(&str, &str)] = &[
    ("url", "QUrl"),
    ("string", "QString"),
    ("date", "QDate"),
    ("color", "QColor"),
    ("point", "QPointF"),
    ("rect", "QRectF"),
    ("size", "QSizeF"),
    ("font", "QFont"),
    ("real", "double"),
    ("int", "int"),
    ("bool", "bool"),
];

pub fn qml_types(m_: &mut Module, code: &str, text: &str, o: Opts) {
    m_.kind = "class".into();
    let base = super::util::basename(&m_.file);
    let stem = &base[..base.len().saturating_sub(4)];
    let name = if super::util::upper1(stem) { stem.to_string() } else { String::new() };
    if !name.is_empty() && !m_.decls.contains(&name) {
        m_.decls.push(name.clone());
    }
    let root = re!(r"(?m)^[ \t]*([A-Z][\w.]*)\s*\{").search(code);
    if let Some(r) = &root {
        m_.bases = vec![r.s(1).to_string()];
    }
    let b = code.as_bytes();
    for m in re!(r"(?m)(?:^|[;{])[ \t]*id[ \t]*:[ \t]*([A-Za-z_]\w*)").find_iter(code) {
        let (mut depth, mut k) = (0i64, m.span(1).unwrap().0);
        while k > 0 {
            k -= 1;
            if b[k] == b'}' {
                depth += 1;
            } else if b[k] == b'{' {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
        }
        let Some(tm) = re!(r"([A-Z][\w.]*)\s*$").search(rb(code, text, k, 120)) else { continue };
        let is_root = root.as_ref().map(|r| super::util::find_from(code, "{", r.start()) == k as isize).unwrap_or(false);
        if is_root {
            // the root: this component
            let v = if name.is_empty() { format!("mod:{}", m_.name) } else { format!("type:{name}") };
            m_.bound.insert(m.s(1).to_string(), v);
        } else {
            m_.bound.entry(m.s(1).to_string()).or_insert(format!("type:{}", tm.s(1)));
        }
    }
    let mut generic: HashSet<String> = HashSet::new();
    for m in re!(r"\b(?:required[ \t]+|readonly[ \t]+|default[ \t]+)*property[ \t]+([A-Z][\w.]*)[ \t]+([a-z_]\w*)")
        .find_iter(code)
    {
        if !matches!(m.s(1), "QtObject" | "QObject" | "Item" | "Component") {
            m_.bound.entry(m.s(2).to_string()).or_insert(format!("type:{}", m.s(1)));
        } else {
            generic.insert(m.s(2).to_string());
        }
    }
    for m in re!(r"\bproperty[ \t]+(url|string|date|color|point|rect|size|font|real|int|bool)[ \t]+([a-z_]\w*)")
        .find_iter(code)
    {
        let t = QML_BASIC.iter().find(|x| x.0 == m.s(1)).map(|x| x.1).unwrap_or("");
        m_.bound.entry(m.s(2).to_string()).or_insert(format!("type:{t}"));
    }
    for m in re!(r"\b(?:property[ \t]+[\w.<>]+|signal)[ \t]+([a-z_]\w*)").find_iter(code) {
        m_.bound.entry(m.s(1).to_string()).or_insert("qmlvar:".into());
    }
    for m in re!(r"\bproperty[ \t]+(?:var|variant)[ \t]+([a-z_]\w*)").find_iter(code) {
        generic.insert(m.s(1).to_string());
    }
    let unset = |m_: &Module, k: &str| m_.bound.get(k).map(|v| v == "qmlvar:").unwrap_or(true);
    for m in re!(r"\bproperty[ \t]+(?:var|variant|QtObject|QObject)[ \t]+([a-z_]\w*)[ \t]*:").find_iter(code) {
        let mut ch = expression(code, m.end(), 40, o); // `property var iface: Factory.create(id)`
        if ch.is_empty() {
            // `cond ? Factory.create(id) : null`: the one branch that holds an object
            let line_end = super::util::find_from(code, "\n", m.end());
            let end = if line_end > 0 { line_end as usize } else { code.len() };
            if let Some(tm) =
                re!(r"[^?\n]*\?\s*([^:\n]+?)\s*:\s*(?:null|undefined)\s*$").match_start(sl(code, m.end(), end))
            {
                ch = expression(&format!("{}\n", tm.s(1)), 0, 40, o);
            }
        }
        if !ch.is_empty() && unset(m_, m.s(1)) {
            m_.bound.insert(m.s(1).to_string(), format!("expr:{}", join(&ch)));
        }
    }
    for m in re!(r"(?m)(?:^|[;{])[ \t]*([a-z_]\w*)[ \t]*=(?!=)").find_iter(code) {
        if generic.contains(m.s(1)) && unset(m_, m.s(1)) {
            // `clipboard = Factory.create(id)`: what it holds
            let ch = expression(code, m.end(), 40, o);
            if !ch.is_empty() {
                m_.bound.insert(m.s(1).to_string(), format!("expr:{}", join(&ch)));
            }
        }
    }
    let raw_text = m_.raw.join("\n");
    let src = if !m_.raw.is_empty() && !raw_text.is_empty() { raw_text.as_str() } else { code };
    for m in re!(r"(?m)^[ \t]*import[ \t]+[\w.]+(?:[ \t]+[\d.]+)?[ \t]+as[ \t]+([A-Z]\w*)").find_iter(src) {
        m_.bound.insert(m.s(1).to_string(), "qmlns:".into()); // `KDEConnect.Foo`: a type of that QML module
    }
}
