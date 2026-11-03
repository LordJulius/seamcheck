//! What a taint `path` rule's sink, source and sanitizers mean (security.py).
//!
//! The path rule (arch.path) finds a sink a request value reaches; this module
//! says what that sink does with which argument: `sql_sinks` (SQL text built
//! from the value), `url_sinks` (the value in a URL's host is the rule's
//! severity, in the path of a fixed host `url_severity`, a same-origin path
//! nothing), `path_sinks` (only the first argument, an in-memory buffer is no
//! path), `shell_sinks` (a fixed program's argument list is `argv_severity`).
//! `lookups`, `source_scope`, Python `if` guards, `[security] sanitizers` and
//! the ORM / HTTP receivers are here too. The functions take the taint through
//! [`TaintCtx`], so the path rule's own taint walk (arch) only has to answer
//! "does this text carry the value at this line" and "what was this name last
//! assigned".

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::Value;

use crate::config::{str_list, Rule};
use crate::graph::Graph;
use crate::model::{Call, Func, Module};
use crate::pyre::Rx;

/// The taint at one line, as the verdicts ask it (arch.py's `_Ctx`).
pub trait TaintCtx {
    /// The text carries the value at this line.
    fn dirty(&self, text: &str) -> bool;
    /// The latest plain assignment of `name` before this line: (its value's text, the taint at
    /// its line); None for a parameter, a loop variable, a destructuring, or no assignment.
    fn rhs(&self, name: &str) -> Option<(String, Box<dyn TaintCtx + '_>)>;
}

macro_rules! rx {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static Rx {
            static R: OnceLock<Rx> = OnceLock::new();
            R.get_or_init(|| Rx::lit(&$pat))
        }
    };
}

const STR: &str = r#"[rRbBuU]{0,2}(?:"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')"#;
rx!(fstr, r#"(?s)^[rRbB]?[fF][rRbB]?(["'])(.*)\1$"#);
rx!(plain, r#"(?s)^[rRbBuU]{0,2}(["'])((?:(?!\1)[^\\]|\\.)*)\1$"#);
rx!(template, r"(?s)^`(.*)`$");
rx!(same_site, r#"(?i)^\s*[fbru]{0,2}["'`]/(?![/\\])"#);
rx!(
    query_object,
    r"(?:[A-Za-z_]\w*\.)*(?:select|insert|update|delete|union|union_all|exists|values|lateral|intersect|except_)\s*\(|[A-Z]\w*\.(?:query|objects|__table__)\b"
);
rx!(text_call, r"(?s)(?:[A-Za-z_]\w*\.)*(?:text|literal_column|SQL)\s*\((.*)\)\s*$");
rx!(buffer, r"(?:io\.)?(?:BytesIO|StringIO)\s*\(|(?:tempfile\.)?(?:SpooledTemporaryFile|TemporaryFile)\s*\(");
rx!(shell_kw, r"^\s*shell\s*=\s*(?!False\b|None\b|0\b)");
rx!(
    program,
    format!(r#"^\s*(?:[rRbBuU]{{0,2}}(?:"[^"$\\{{]*"|'[^'$\\{{]*')|sys\.executable|shutil\.which\(\s*{STR}\s*\))\s*$"#)
);
rx!(orm_rhs, r"^(?:[A-Za-z_]\w*\.)?(?:Session|sessionmaker|scoped_session)\s*\(");
rx!(http_bases, r"^(?:requests\.(?:sessions\.)?Session|httpx\.(?:Async)?Client|aiohttp\.ClientSession)$");
rx!(
    http_import,
    r"^\s*from\s+(?:requests|httpx|aiohttp)(?:\.\w+)*\s+import\s+.*\b(Session|AsyncClient|Client|ClientSession)\b"
);
rx!(http_rhs, r"^\s*(?:requests\.(?:sessions\.)?Session|httpx\.(?:Async)?Client|aiohttp\.ClientSession)\s*\(");
rx!(
    validator,
    concat!(
        r"(?:(?P<n1>[A-Za-z_]\w*)\.(?:isdigit|isnumeric|isdecimal|isalnum|isidentifier)\(\s*\))",
        r"|(?:(?:re\.)?fullmatch\(\s*[^,()]+(?:\([^()]*\))?\s*,\s*(?P<n2>[A-Za-z_]\w*)\s*[,)])",
        r"|(?:(?<![\w.])(?!re\.)[A-Za-z_][\w.]*\.fullmatch\(\s*(?P<n3>[A-Za-z_]\w*)\s*[,)])",
        r"|(?:(?P<n5>[A-Za-z_]\w*)\.startswith\()",
        r"|(?:(?:url_has_allowed_host_and_scheme|is_safe_url|is_safe_redirect)\(\s*(?:url\s*=\s*)?(?P<n4>[A-Za-z_]\w*)\s*[,)])"
    )
);
rx!(netloc, r"\b(?:urlsplit|urlparse)\(\s*([A-Za-z_]\w*)\s*\)\.netloc\b");
rx!(allowed, r"(?<![\w.])([A-Za-z_]\w*)\s+(not\s+)?in\s+(?:[A-Z][A-Z0-9_]*\b|[\[({])");
rx!(not_before, r"\bnot\s+$");
rx!(kwarg_rx, r"(?s)\s*([A-Za-z_]\w*)\s*=(?!=)\s*(.*)$");
rx!(bare_rx, r"[$@]?[A-Za-z_]\w*");
rx!(interp_rx, r"#\{|\$\{");
rx!(relative_lead, r"/(?![/\\])");
rx!(scheme_slashes, r"^(?:[A-Za-z][\w+.-]*:)?//");
rx!(authority_end, r"^(?:[A-Za-z][\w+.-]*:)?//[^/?#]*[/?#]");
rx!(scheme_only, r"^[A-Za-z][\w+.-]*:?/?$");
rx!(path_end, r"^[^/?#]*[/?#]");
rx!(format_call, r#"(?s)^([rRbBuU]{0,2}(["'])(?:(?!\2)[^\\]|\\.)*\2)\s*\.format\s*\((.*)\)$"#);
rx!(format_field, r"\{[^{}]*\}");
rx!(percent_call, r#"(?s)^([rRbBuU]{0,2}(["'])(?:(?!\2)[^\\]|\\.)*\2)\s*%\s*(.+)$"#);
rx!(percent_field, r"%[-+ #0]*\d*(?:\.\d+)?[sdirfx]");
rx!(fstring_spec, r"(?<![=!<>])[!:](?![=])");
const ADJ_LIT: &str = r#"[rRuU]?(?:"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')"#;
rx!(adjacent, format!(r"^((?:{ADJ_LIT}\s*){{2,}})(?=$|\.|\s*[%+])"));
rx!(adj_lit, ADJ_LIT);
rx!(sqla_assign, r"^([A-Za-z_]\w*)\s*(?::[^=]+)?=\s*(?:flask_sqlalchemy\.)?SQLAlchemy\s*\(");
rx!(class_head, r"\s*class\s+\w+\s*\(([^)]*)\)");
rx!(session_root, r"([A-Za-z_]\w*)\.session\b");
rx!(ctor_rx, r"([A-Z]\w*)\s*\(");

const URL_KWS: [&str; 3] = ["url", "uri", "endpoint"];
const PATH_KWS: [&str; 5] = ["path_or_file", "filename_or_fp", "file", "path", "filename"];
const SQL_KWS: [&str; 4] = ["sql", "statement", "query", "stmt"];

/// The names a Python `if` condition checks (security.guarded): (name, inside), inside = only
/// within the block (`if x.isdigit():`), else from the `if` on (`if not x.isdigit(): return`).
pub fn guarded(cond: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for m in validator().find_iter(cond) {
        let Some(name) = ["n1", "n2", "n3", "n5", "n4"].iter().find_map(|n| m.name(n)) else { continue };
        let negated = not_before().is_match(&cond[..m.start()]);
        out.push((name.to_string(), !negated));
    }
    for m in netloc().find_iter(cond) {
        out.push((m.s(1).to_string(), false));
    }
    for m in allowed().find_iter(cond) {
        if !["not", "and", "or", "is", "if"].contains(&m.s(1)) {
            out.push((m.s(1).to_string(), m.get(2).is_none()));
        }
    }
    let mut seen = HashSet::new();
    out.into_iter().filter(|(n, _)| seen.insert(n.clone())).collect()
}

/// `[security] sanitizers` from the merged config: the project's own clean-returning functions.
pub fn sanitizers(config: &Value) -> Vec<String> {
    match config.get("security").and_then(|t| t.get("sanitizers")) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).filter(|x| !x.is_empty()).map(String::from).collect(),
        _ => Vec::new(),
    }
}

/// The function ids a `source_scope` keeps (security.scoped): functions in files whose text
/// matches it, and those they reach by exact calls within `depth`.
pub fn scoped(g: &Graph, scope: &Rx, depth: usize) -> HashSet<String> {
    let inside: HashSet<&String> = g.by_file.keys().filter(|rel| scope.is_match(&file_text(g, rel))).collect();
    let mut reach: HashSet<String> =
        g.funcs.keys().filter(|id| g.func(id).map(|f| inside.contains(&f.file)).unwrap_or(false)).cloned().collect();
    let mut frontier: Vec<String> = g.funcs.keys().filter(|id| reach.contains(*id)).cloned().collect();
    for _ in 0..depth {
        let mut nxt = Vec::new();
        for fid in &frontier {
            for (dst, _ln, exact) in g.edges.get(fid).map(|v| v.as_slice()).unwrap_or(&[]) {
                if *exact && !reach.contains(dst) && g.funcs.contains_key(dst) {
                    reach.insert(dst.clone());
                    nxt.push(dst.clone());
                }
            }
        }
        frontier = nxt;
    }
    reach
}

fn file_text(g: &Graph, rel: &str) -> String {
    g.by_file
        .get(rel)
        .and_then(|ms| ms.first())
        .and_then(|mn| g.modules.get(mn))
        .map(|m| m.raw.join("\n"))
        .unwrap_or_default()
}

/// A rule's categorised sinks (security.Sinks).
pub struct Sinks {
    pub sql: Vec<Rx>,
    pub url: Vec<Rx>,
    pub path: Vec<Rx>,
    pub shell: Vec<Rx>,
    pub lookups: Option<Rx>,
    pub severity: String,
    pub url_severity: String,
    pub argv_severity: String,
}

/// (severity, note) of a sink the value reaches the way it does harm.
pub type Verdict = Option<(String, String)>;

impl Sinks {
    pub fn new(r: &Rule) -> Result<Sinks, String> {
        let pats = |k: &str| -> Result<Vec<Rx>, String> {
            str_list(r, k).unwrap_or_default().iter().map(|p| Rx::new(p)).collect()
        };
        let sv = |k: &str, d: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string();
        let lookups = match r.get("lookups").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => Some(Rx::new(p)?),
            _ => None,
        };
        Ok(Sinks {
            sql: pats("sql_sinks")?,
            url: pats("url_sinks")?,
            path: pats("path_sinks")?,
            shell: pats("shell_sinks")?,
            lookups,
            severity: sv("severity", "error"),
            url_severity: sv("url_severity", "warn"),
            argv_severity: sv("argv_severity", "warn"),
        })
    }

    pub fn kind(&self, text: &str) -> Option<&'static str> {
        [("sql", &self.sql), ("url", &self.url), ("path", &self.path), ("shell", &self.shell)]
            .into_iter()
            .find(|(_, ps)| ps.iter().any(|p| p.is_match(text)))
            .map(|(k, _)| k)
    }

    pub fn any(&self) -> bool {
        !(self.sql.is_empty() && self.url.is_empty() && self.path.is_empty() && self.shell.is_empty())
    }

    /// An assignment from a database lookup holds the database's row, not the value.
    pub fn lookup(&self, rhs: &str) -> bool {
        self.lookups.as_ref().map(|l| l.match_start(rhs.trim()).is_some()).unwrap_or(false)
    }

    pub fn verdict(&self, kind: &str, args: &[String], ctx: &dyn TaintCtx) -> Verdict {
        let pos: Vec<(usize, &String)> = args.iter().enumerate().filter(|(_, a)| kwarg(a).is_none()).collect();
        let mut kws: HashMap<String, String> = HashMap::new();
        for a in args {
            if let Some((k, v)) = kwarg(a) {
                kws.insert(k, v);
            }
        }
        let pick = |names: &[&str]| -> Option<(Option<usize>, String)> {
            names
                .iter()
                .find_map(|k| kws.get(*k).map(|v| (None, v.clone())))
                .or_else(|| pos.first().map(|(i, a)| (Some(*i), (*a).clone())))
        };
        let hit = |sev: &str, note: &str| Some((sev.to_string(), note.to_string()));
        match kind {
            "sql" => pick(&SQL_KWS).filter(|(_, a)| sql_dirty(a, ctx, 0)).and_then(|_| hit(&self.severity, "")),
            "path" => pick(&PATH_KWS).filter(|(_, a)| path_dirty(a, ctx, 0)).and_then(|_| hit(&self.severity, "")),
            "url" => {
                let first = pick(&URL_KWS);
                let how = first.as_ref().map(|(_, a)| url_taint(a, ctx, 0)).unwrap_or("clean");
                if how == "relative" {
                    return None;
                }
                if how == "head" {
                    return hit(&self.severity, "");
                }
                let skip = first.as_ref().and_then(|(i, _)| *i);
                let rest = args.iter().enumerate().any(|(i, a)| Some(i) != skip && ctx.dirty(a));
                if (how == "tail" || rest) && self.url_severity != "off" {
                    return hit(&self.url_severity, " (a fixed host; request data in the path or parameters)");
                }
                None
            }
            "shell" => {
                let shell = args.iter().any(|a| shell_kw().match_start(a).is_some());
                let first = pos.first().map(|(_, a)| (*a).clone()).unwrap_or_else(|| kws.get("args").cloned().unwrap_or_default());
                if !shell && self.argv_severity == "off" {
                    return None;
                }
                let elems = if shell { None } else { argv(&first, ctx, 0) };
                let Some(elems) = elems else {
                    return if !first.is_empty() && ctx.dirty(&first) { hit(&self.severity, "") } else { None };
                };
                if elems.is_empty() {
                    return None;
                }
                if program().match_start(&elems[0]).is_none() && ctx.dirty(&elems[0]) {
                    return hit(&self.severity, "");
                }
                if elems[1..].iter().any(|e| ctx.dirty(e)) && self.argv_severity != "off" {
                    return hit(&self.argv_severity, " (an argument of a fixed program, no shell)");
                }
                None
            }
            _ => None,
        }
    }
}

pub fn kwarg(a: &str) -> Option<(String, String)> {
    kwarg_rx().match_start(a).map(|m| (m.s(1).to_string(), m.s(2).to_string()))
}

fn bare(a: &str) -> Option<&str> {
    let a = a.trim();
    bare_rx().fullmatch(a).map(|_| a)
}

fn literal(a: &str) -> bool {
    let a = a.trim();
    plain().match_start(a).is_some() && !interp_rx().is_match(a)
}

/// The SQL text is built from the value (not a constant, a query object or a bound parameter).
pub fn sql_dirty(a: &str, ctx: &dyn TaintCtx, depth: usize) -> bool {
    let a = a.trim();
    if literal(a) || !ctx.dirty(a) {
        return false;
    }
    if let Some(m) = text_call().match_start(a) {
        let inner = split_args(m.s(1));
        return !inner.is_empty() && sql_dirty(&inner[0], ctx, depth + 1);
    }
    if query_object().match_start(a).is_some() {
        return false;
    }
    if let Some(name) = bare(a) {
        return match ctx.rhs(name) {
            Some((v, sub)) if depth <= 6 => sql_dirty(&v, sub.as_ref(), depth + 1),
            _ => true,
        };
    }
    true
}

/// The path argument carries the value (an in-memory buffer is no path).
pub fn path_dirty(a: &str, ctx: &dyn TaintCtx, depth: usize) -> bool {
    let a = a.trim();
    if buffer().match_start(a).is_some() || !ctx.dirty(a) {
        return false;
    }
    if let Some(name) = bare(a) {
        return match ctx.rhs(name) {
            Some((v, sub)) if depth <= 6 => path_dirty(&v, sub.as_ref(), depth + 1),
            _ => true,
        };
    }
    true
}

/// An argument list's elements (a list or tuple literal, or a name assigned one), else None.
pub fn argv(a: &str, ctx: &dyn TaintCtx, depth: usize) -> Option<Vec<String>> {
    let a = a.trim();
    if (a.starts_with('[') || a.starts_with('(')) && (a.ends_with(']') || a.ends_with(')')) && a.len() >= 2 {
        return Some(split_args(&a[1..a.len() - 1]));
    }
    let name = bare(a)?;
    if depth >= 6 {
        return None;
    }
    let (v, sub) = ctx.rhs(name)?;
    argv(&v, sub.as_ref(), depth + 1)
}

/// Where the value is in a URL: "head" (scheme or host, or all of it), "tail" (the path or query
/// of a fixed host), "relative" (a same-origin path) or "clean".
pub fn url_taint(a: &str, ctx: &dyn TaintCtx, depth: usize) -> &'static str {
    let a = a.trim();
    if same_site().match_start(a).is_some() {
        return "relative";
    }
    if !ctx.dirty(a) {
        return "clean";
    }
    let Some(parts) = parts(a) else {
        let sub = if depth < 6 { bare(a).and_then(|n| ctx.rhs(n)) } else { None };
        return match sub {
            Some((v, c)) => url_taint(&v, c.as_ref(), depth + 1),
            None => "head",
        };
    };
    if let Some((false, lead)) = parts.first() {
        if relative_lead().match_start(lead).is_some() {
            return "relative";
        }
    }
    let mut head: Vec<&String> = Vec::new();
    let mut seen = String::new();
    for (is_expr, text) in &parts {
        if *is_expr {
            head.push(text);
            seen.push('\0');
            continue;
        }
        seen.push_str(text);
        if authority_closed(&seen) {
            break;
        }
    }
    for h in head {
        if !ctx.dirty(h) {
            continue;
        }
        let sub = if depth < 6 { bare(h).and_then(|n| ctx.rhs(n)) } else { None };
        match sub {
            Some((v, c)) if url_taint(&v, c.as_ref(), depth + 1) != "head" => {}
            _ => return "head",
        }
    }
    "tail"
}

fn authority_closed(seen: &str) -> bool {
    if scheme_slashes().match_start(seen).is_some() {
        return authority_end().match_start(seen).is_some();
    }
    if scheme_only().match_start(seen).is_some() {
        return false;
    }
    path_end().match_start(seen).is_some()
}

/// A string expression as (is_expr, text) parts (security._parts): an f-string, a JS template, a
/// `+` concatenation, `"...".format(...)`, `"..." % x`; None for anything else.
pub fn parts(a: &str) -> Option<Vec<(bool, String)>> {
    let a = join_adjacent(a);
    let a = a.as_str();
    if let Some(m) = fstr().match_start(a) {
        if !a.contains(&m.s(1).repeat(3)) {
            return Some(interpolated(m.s(2), "{"));
        }
    }
    if let Some(m) = template().match_start(a) {
        if !m.s(1).contains('`') {
            return Some(interpolated(m.s(1), "${"));
        }
    }
    if let Some(m) = plain().match_start(a) {
        return Some(vec![(false, m.s(2).to_string())]);
    }
    let terms = split_top(a, '+');
    if terms.len() > 1 {
        let mut out = Vec::new();
        for t in terms {
            match parts(t.trim()) {
                Some(sub) => out.extend(sub),
                None => out.push((true, t.trim().to_string())),
            }
        }
        return Some(out);
    }
    if let Some(fm) = format_call().match_start(a) {
        let lit = plain().match_start(fm.s(1)).map(|m| m.s(2).to_string()).unwrap_or_default();
        let vals = split_args(fm.s(3));
        return Some(fill(&lit, format_field(), &vals, fm.s(3)));
    }
    if let Some(pm) = percent_call().match_start(a) {
        let lit = plain().match_start(pm.s(1)).map(|m| m.s(2).to_string()).unwrap_or_default();
        let vals = pm.s(3).trim();
        let list = if vals.starts_with('(') && vals.ends_with(')') && vals.len() >= 2 {
            split_args(&vals[1..vals.len() - 1])
        } else {
            vec![vals.to_string()]
        };
        return Some(fill(&lit, percent_field(), &list, vals));
    }
    None
}

/// A format string's literal pieces and its fields filled by `vals` in order (re.split with the
/// field as a kept group).
fn fill(lit: &str, field: &Rx, vals: &[String], all: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let (mut last, mut k) = (0, 0);
    for m in field.find_iter(lit) {
        if m.start() > last {
            out.push((false, lit[last..m.start()].to_string()));
        }
        out.push((true, vals.get(k).cloned().unwrap_or_else(|| all.to_string())));
        k += 1;
        last = m.end();
    }
    if last < lit.len() {
        out.push((false, lit[last..].to_string()));
    }
    out
}

/// Python's adjacent literals as one: `'https://host' '/x?a={}'.format(v)`.
pub fn join_adjacent(a: &str) -> String {
    let Some(m) = adjacent().match_start(a) else { return a.to_string() };
    let pieces: String = adj_lit()
        .find_iter(m.s(1))
        .iter()
        .map(|x| plain().match_start(x.group0()).map(|p| p.s(2).to_string()).unwrap_or_default())
        .collect();
    format!("\"{}\"{}", pieces.replace('"', "\\\""), &a[m.end()..])
}

fn interpolated(body: &str, opener: &str) -> Vec<(bool, String)> {
    let b: Vec<char> = body.chars().collect();
    let op: Vec<char> = opener.chars().collect();
    let at = |i: usize, s: &[char]| i + s.len() <= b.len() && b[i..i + s.len()] == *s;
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    while i < b.len() {
        if opener == "{" && (at(i, &['{', '{']) || at(i, &['}', '}'])) {
            lit.push(b[i]);
            i += 2;
            continue;
        }
        if at(i, &op) {
            let (mut j, mut d) = (i + op.len(), 1i32);
            while j < b.len() && d != 0 {
                d += (b[j] == '{') as i32;
                d -= (b[j] == '}') as i32;
                j += 1;
            }
            if !lit.is_empty() {
                out.push((false, std::mem::take(&mut lit)));
            }
            let expr: String = b[i + op.len()..j.saturating_sub(1).max(i + op.len())].iter().collect();
            let expr = if opener == "{" {
                fstring_spec().search(&expr).map(|m| expr[..m.start()].to_string()).unwrap_or(expr)
            } else {
                expr
            };
            out.push((true, expr));
            i = j;
            continue;
        }
        lit.push(b[i]);
        i += 1;
    }
    if !lit.is_empty() {
        out.push((false, lit));
    }
    out
}

/// s split at `sep` outside brackets and strings.
pub fn split_top(s: &str, sep: char) -> Vec<String> {
    let c: Vec<char> = s.chars().collect();
    let (mut out, mut cur, mut d, mut q, mut i) = (Vec::new(), String::new(), 0i32, None::<char>, 0);
    while i < c.len() {
        let ch = c[i];
        if let Some(qc) = q {
            cur.push(ch);
            if ch == '\\' && i + 1 < c.len() {
                cur.push(c[i + 1]);
                i += 2;
                continue;
            }
            if ch == qc {
                q = None;
            }
        } else if "\"'`".contains(ch) {
            q = Some(ch);
            cur.push(ch);
        } else if "([{".contains(ch) {
            d += 1;
            cur.push(ch);
        } else if ")]}".contains(ch) {
            d -= 1;
            cur.push(ch);
        } else if ch == sep && d == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
        i += 1;
    }
    out.push(cur);
    out
}

pub fn split_args(s: &str) -> Vec<String> {
    split_top(s, ',').into_iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// Python: what a call is made on (security.receiver): "orm" for an ORM session (`db.session` with
/// `db = SQLAlchemy()`, a name assigned it or `Session(engine)`), "http" for an HTTP client
/// (`requests.Session()`, a project subclass of one, `self` inside one), else None.
pub fn receiver(g: &Graph, f: &Func, c: &Call, line: &str, ctx: Option<&dyn TaintCtx>) -> Option<&'static str> {
    if !f.file.ends_with(".py") {
        return None;
    }
    let pat = Rx::new(&format!(r"([A-Za-z_][\w.]*?)\s*\.\s*{}\s*\(", regex::escape(&c.name))).ok()?;
    let m = pat.search(line)?;
    let parts: Vec<&str> = m.s(1).split('.').collect();
    if parts[0] == "self" && parts.len() == 1 {
        return g.modules.get(&f.module).filter(|own| http_class(g, own, 0)).map(|_| "http");
    }
    if parts.len() >= 2 && parts[parts.len() - 1] == "session" && sqlalchemy_names(g, f).contains(parts[0]) {
        return Some("orm");
    }
    if parts.len() == 1 {
        let (val, _) = ctx?.rhs(parts[0])?;
        let val = val.trim();
        if http_rhs().match_start(val).is_some() {
            return Some("http");
        }
        if let Some(cm) = ctor_rx().match_start(val) {
            let hit = g.short.get(cm.s(1)).map(|ms| {
                ms.iter().any(|mn| g.modules.get(mn).map(|m| http_class(g, m, 0)).unwrap_or(false))
            });
            if hit == Some(true) {
                return Some("http");
            }
        }
        let root = session_root().match_start(val).map(|r| r.s(1).to_string());
        if orm_rhs().match_start(val).is_some() || root.map(|r| sqlalchemy_names(g, f).contains(&r)).unwrap_or(false) {
            return Some("orm");
        }
    }
    None
}

/// A project class that subclasses an HTTP client (its `class X(...)` line as written).
fn http_class(g: &Graph, m: &Module, depth: usize) -> bool {
    if depth > 8 || m.decls.is_empty() {
        return false;
    }
    let Ok(head_rx) = Rx::new(&format!(r"\s*class\s+{}\s*\(", regex::escape(&m.decls[0]))) else { return false };
    let head = m.raw.iter().find(|ln| head_rx.match_start(ln).is_some()).cloned().unwrap_or_default();
    let imported: HashSet<String> =
        m.raw.iter().filter_map(|ln| http_import().match_start(ln).map(|im| im.s(1).to_string())).collect();
    let bases: Vec<String> = class_head()
        .match_start(&head)
        .map(|bm| bm.s(1).split(',').filter(|x| !x.contains('=')).map(|x| x.trim().to_string()).collect())
        .unwrap_or_default();
    for b in bases {
        if http_bases().match_start(&b).is_some() || imported.contains(&b) {
            return true;
        }
        let short = b.rsplit('.').next().unwrap_or(&b);
        for mn in g.short.get(short).map(|v| v.as_slice()).unwrap_or(&[]) {
            if let Some(sub) = g.modules.get(mn) {
                if *mn != m.name && http_class(g, sub, depth + 1) {
                    return true;
                }
            }
        }
    }
    false
}

/// The names a file sees bound to `SQLAlchemy()`: its own, or imported from the file that builds it.
fn sqlalchemy_names(g: &Graph, f: &Func) -> HashSet<String> {
    let mut by_file: HashMap<&str, HashSet<String>> = HashMap::new();
    for (rel, ms) in &g.by_file {
        if !rel.ends_with(".py") {
            continue;
        }
        if let Some(m) = ms.first().and_then(|mn| g.modules.get(mn)) {
            for ln in m.raw.iter() {
                if let Some(am) = sqla_assign().match_start(ln) {
                    by_file.entry(rel.as_str()).or_default().insert(am.s(1).to_string());
                }
            }
        }
    }
    let mut own = by_file.get(f.file.as_str()).cloned().unwrap_or_default();
    let names: HashSet<&String> = by_file.values().flatten().collect();
    if let Some(fm) = g.by_file.get(&f.file).and_then(|ms| ms.first()).and_then(|mn| g.modules.get(mn)) {
        for (local, (_w, n)) in &fm.imported {
            if names.contains(n) {
                own.insert(local.clone());
            }
        }
    }
    own
}

#[cfg(test)]
mod tests {
    //! Ports tests/test_security_precision.py's TextHelpers (the pieces on their own).
    use super::*;

    struct Ctx {
        names: Vec<&'static str>,
        values: HashMap<&'static str, &'static str>,
    }

    impl TaintCtx for Ctx {
        fn dirty(&self, text: &str) -> bool {
            self.names.iter().any(|n| Rx::lit(&format!(r"(?<![\w.]){n}(?!\w)")).is_match(text))
        }
        fn rhs(&self, name: &str) -> Option<(String, Box<dyn TaintCtx + '_>)> {
            let v = self.values.get(name)?;
            Some((v.to_string(), Box::new(Ctx { names: self.names.clone(), values: self.values.clone() })))
        }
    }

    /// test_security_precision.TextHelpers.test_url_taint
    #[test]
    fn url_taint_cases() {
        let c = Ctx { names: vec!["q", "u", "h"], values: HashMap::from([("u", "request.args['u']")]) };
        for (url, want) in [
            ("f'{base}/x?q={q}'", "tail"),
            ("'https://api.example.org/' + q", "tail"),
            ("u", "head"),
            ("'https://' + h + '/x'", "head"),
            ("f'https://{h}/x'", "head"),
            ("`/api?q=${q}`", "relative"),
            ("'/search?q=' + q", "relative"),
            ("'//' + h", "head"),
            ("'https://x.org/{}'.format(q)", "tail"),
            ("'http://%s/x' % h", "head"),
            ("base", "clean"),
            ("'https://api.x.com'\n        '/t?a={}'.format(q)", "tail"),
        ] {
            assert_eq!(want, url_taint(url, &c, 0), "{url}");
        }
    }

    /// test_security_precision.TextHelpers.test_sql_dirty
    #[test]
    fn sql_dirty_cases() {
        let c = Ctx {
            names: vec!["q", "s", "t"],
            values: HashMap::from([("s", "select(User).where(User.n == q)"), ("t", "'SELECT ' + q")]),
        };
        for (sql, want) in [
            ("text('SELECT :q')", false),
            ("text(f'SELECT {q}')", true),
            ("s", false),
            ("t", true),
            ("q", true),
            ("'SELECT 1'", false),
            ("User.query.filter(User.n == q)", false),
        ] {
            assert_eq!(want, sql_dirty(sql, &c, 0), "{sql}");
        }
    }

    /// test_security_precision.TextHelpers.test_guarded
    #[test]
    fn guarded_cases() {
        let g = guarded;
        assert_eq!(vec![("n".to_string(), false)], g("not NAME.fullmatch(n)"));
        assert_eq!(vec![("n".to_string(), true)], g("n.isdigit() and x"));
        assert_eq!(vec![("nxt".to_string(), false)], g("not nxt or urlsplit(nxt).netloc != ''"));
        assert_eq!(vec![("fmt".to_string(), false)], g("fmt not in ALLOWED"));
        assert_eq!(vec![("fmt".to_string(), true)], g("fmt in ('a', 'b')"));
        assert_eq!(vec![("next".to_string(), false)], g("next is None or not next.startswith('/')"));
    }

    /// test_security_precision.{Files, Commands, Outbound}: the verdicts on argument lists
    #[test]
    fn verdicts() {
        let mut r = Rule::new();
        for (k, v) in [("sql_sinks", r"\.execute$"), ("url_sinks", r"^requests\.get$"), ("path_sinks", r"^send_file$"),
                       ("shell_sinks", r"^subprocess\.run$")] {
            r.insert(k.into(), Value::Array(vec![Value::String(v.into())]));
        }
        let s = Sinks::new(&r).unwrap();
        let c = Ctx { names: vec!["q", "fmt", "buf"], values: HashMap::from([("buf", "io.BytesIO(fmt.encode())")]) };
        let a = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(Some("sql"), s.kind("db.session.execute"));
        assert_eq!(None, s.verdict("path", &a(&["buf", "download_name=f'x.{fmt}'"]), &c));
        assert_eq!(None, s.verdict("path", &a(&["BytesIO(fmt.encode())"]), &c));
        assert!(s.verdict("path", &a(&["os.path.join(BASE, q)"]), &c).is_some());
        assert_eq!(Some("warn".to_string()), s.verdict("shell", &a(&["['grep', q, '/srv']"]), &c).map(|v| v.0));
        assert_eq!(Some("error".to_string()), s.verdict("shell", &a(&["f'ping {q}'", "shell=True"]), &c).map(|v| v.0));
        assert_eq!(None, s.verdict("shell", &a(&["['tar', 'czf', os.path.join(BASE, 'a')]"]), &c));
        assert_eq!(Some("warn".to_string()), s.verdict("url", &a(&["f'{base}/x'", "params={'q': q}"]), &c).map(|v| v.0));
        assert_eq!(Some("error".to_string()), s.verdict("url", &a(&["q"]), &c).map(|v| v.0));
        assert_eq!(None, s.verdict("sql", &a(&["text('SELECT :n')", "{'n': q}"]), &c));
    }

    #[test]
    fn sanitizers_are_a_list_of_names() {
        let cfg: Value = serde_json::json!({"security": {"sanitizers": ["coerce_safe_redirect_target", 3]}});
        assert_eq!(vec!["coerce_safe_redirect_target".to_string()], sanitizers(&cfg));
        assert!(sanitizers(&serde_json::json!({"security": {"sanitizers": "x"}})).is_empty());
    }
}
