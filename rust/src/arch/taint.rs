//! The taint half of kind = "path" (arch._Taint and its helpers): which
//! names in a function hold the source's value at a line, a call's
//! arguments as written, whether a text carries the value.
//!
//! Offsets are byte offsets into the function's blanked code and raw text,
//! which share their layout (`scan::blank` keeps bytes in place), so a span
//! found in one slices the other.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::graph::infer::{esc, re, rxd};
use crate::graph::Graph;
use crate::model::{Call, Func};
use crate::pyre::{strip, Rx};
use crate::rules::security::{self, anchored, guarded, Sinks, TaintCtx};

/// The sanitizers of `unless`: a call whose name holds one, and a method of that name.
pub struct Clean {
    pub call: Arc<Rx>,
    pub method: Arc<Rx>,
}

/// What a taint check reads with: the source, the sanitizers, the request-reading helpers.
pub struct Ctx {
    pub source: Arc<Rx>,
    pub clean: Option<Clean>,
    pub readers: Option<Arc<Rx>>,
}

/// `len(indent.expandtabs(4))`
pub(crate) fn width(indent: &str) -> usize {
    let mut col = 0;
    for ch in indent.chars() {
        if ch == '\t' {
            col += 4 - col % 4;
        } else {
            col += 1;
        }
    }
    col
}

/// (the leading whitespace, the rest): Python's `lstrip()`
fn split_lead(t: &str) -> (&str, &str) {
    let body = t.trim_start();
    (&t[..t.len() - body.len()], body)
}

/// `s[a:b]` on byte offsets, clamped to the text and its characters.
pub(crate) fn slice(s: &str, a: usize, b: usize) -> &str {
    let b = crate::pyre::floor_char(s, b.min(s.len()));
    let a = crate::pyre::floor_char(s, a.min(b));
    &s[a..b]
}

/// f's code (comments and strings blanked, or `raw`, at the same offsets); a
/// file's `<top>`: its lines outside functions (arch._own_code).
pub fn own_code(g: &Graph, f: &Func, raw: bool) -> String {
    let Some(m) = g.module(&f.module) else { return String::new() };
    let src = if raw { &m.raw } else { &m.code };
    let (a, b) = (f.start.saturating_sub(1).min(src.len()), f.end.min(src.len()));
    let lines: &[String] = if a < b { &src[a..b] } else { &[] };
    if f.kind != "top" {
        return lines.join("\n");
    }
    // (blanked, not dropped: line numbers hold)
    let mut inner: HashSet<usize> = HashSet::new();
    for mn in g.by_file.get(&f.file).map(|v| v.as_slice()).unwrap_or(&[]) {
        for h in g.modules[mn].funcs.iter().filter(|h| h.kind != "top") {
            inner.extend(h.start..=h.end);
        }
    }
    lines
        .iter()
        .enumerate()
        .map(|(i, ln)| if inner.contains(&(i + f.start)) { "" } else { ln.as_str() })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where the bracket open before offset k closes (arch._close).
pub(crate) fn close(code: &str, mut k: usize) -> usize {
    let b = code.as_bytes();
    let mut d: i64 = 1;
    while k < b.len() && d != 0 {
        d += b"([{".contains(&b[k]) as i64;
        d -= b")]}".contains(&b[k]) as i64;
        k += 1;
    }
    k.saturating_sub(1)
}

/// `name` (`$x`, `@x` or `x`) occurs in text as a whole name (arch._names).
pub fn names(name: &str, text: &str) -> bool {
    if !text.contains(name) {
        return false;
    }
    if name.starts_with('$') {
        // (PHP: `"a".$x` concatenates)
        return rxd(&format!(r"(?<![\w$@]){}(?!\w)", esc(name))).is_match(text);
    }
    // not an attribute of something else (`os.path`, `job.name`), but a spread (`...args`)
    rxd(&format!(r"(?<![\w$@])(?<![^.]\.)(?<!^\.){}(?!\w)", esc(name))).is_match(text)
}

/// text calls a request-reading helper bare or on a type
/// (`WebUtils.getRequestBody(req)`), not as any value's method (arch._reads).
pub fn reads(readers: Option<&Rx>, text: &str) -> bool {
    let Some(rd) = readers else { return false };
    rd.find_iter(text).iter().any(|m| {
        let q = m.s(1);
        q.is_empty() || q.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
    })
}

fn function_literal() -> &'static Rx {
    re!(r"\s*(?:async\s+)?(?:function\b|(?:\([^()]*\)|[\w$]+)\s*=>|lambda\b|func\s*\(|proc\b|->)")
}

/// The text carries the value: the source or a tainted name, outside the
/// sanitizer calls in it; a sanitizing method (`params[:id].to_i`) cleans the
/// whole expression; a callback is code, not the value (arch._dirty).
pub fn dirty(arg: &str, cx: &Ctx, tainted: &HashSet<String>) -> bool {
    if function_literal().match_start(arg).is_some() {
        return false;
    }
    // Flask / Werkzeug: `request.args.get("page", 1, type=int)` is an int; `",".join("?" for _ in ids)` holds
    // only its literal
    let arg = re!(r"[\w.]+\.get\(\s*[^()]*\btype\s*=\s*(?:int|float|bool)\b[^()]*\)").sub_with(arg, |_| " ".to_string());
    let arg = re!(r#"\.join\(\s*(?:"[^"]*"|'[^']*')\s+for\s+[\w, ]+\s+in\s+[^()]*(?:\([^()]*\)[^()]*)*\)"#)
        .sub_with(&arg, |_| ".join()".to_string());
    // (a length is a number: `["%s"] * len(exact)`)
    let mut arg = re!(r"\blen\s*\((?:[^()]|\([^()]*\))*\)").sub_with(&arg, |_| "0".to_string());
    if let Some(cl) = &cx.clean {
        let mp = cl.method.pattern();
        if rxd(&format!(r"{mp}\s*(?:\([^()]*\))?\s*$")).is_match(strip(&arg)) {
            return false; // `params[:id].to_i`: the whole value
        }
        // a sanitized part (`#{params[:image].to_i}`) is clean; the rest still counts
        arg = rxd(&format!(r#"[\w@$:.\[\]'"]+?{mp}(?:\([^()]*\))?"#)).sub_with(&arg, |_| " ".to_string());
        let spans: Vec<(usize, usize)> = cl.call.find_iter(&arg).iter().map(|m| (m.start(), m.end())).collect();
        for (s, e) in spans.into_iter().rev() {
            let b = arg.as_bytes();
            let (mut d, mut j): (i64, usize) = (0, e - 1);
            while j < b.len() {
                d += b"([{".contains(&b[j]) as i64;
                d -= b")]}".contains(&b[j]) as i64;
                if d == 0 {
                    break;
                }
                j += 1;
            }
            let tail = if j + 1 <= arg.len() { arg[crate::pyre::floor_char(&arg, j + 1)..].to_string() } else { String::new() };
            arg = format!("{}{}", &arg[..s], tail);
        }
    }
    // the words inside a string are no names (`f"""FROM t c ON c.id"""` holds no `c`): only what it
    // interpolates (the source still reads its keys: `session["backup"]`)
    if cx.source.is_match(&arg) {
        return true;
    }
    if tainted.is_empty() {
        return false;
    }
    let words = security::code_parts(&arg);
    tainted.iter().any(|t| names(t, &words))
}

/// The call's arguments as written (raw text, split at top-level commas) (arch._call_args).
pub fn call_args(g: &Graph, f: &Func, c: &Call) -> Vec<String> {
    let Some(m) = g.module(&f.module) else { return vec![] };
    if !(0 < c.line && c.line <= m.raw.len()) {
        return vec![];
    }
    // bounds from the blanked code (a `,` or `)` in a string is no boundary), text from the raw lines
    let hi = |v: &[String]| v[(c.line - 1).min(v.len())..(c.line + 60).min(v.len())].join("\n");
    let mut text = hi(&m.code);
    let mut raw = hi(&m.raw);
    // (the occurrence that is called: `ProcessBuilder pb = new ProcessBuilder(cmd)` declares the type first)
    let eol = text.find('\n').unwrap_or(text.len());
    let name = esc(&c.name);
    let nm = rxd(&format!(r"(?<![\w$]){name}(?!\w)\s*(?=\()"))
        .search_range(&text, 0, eol)
        .map(|m| m.end())
        .or_else(|| rxd(&format!(r"(?<![\w$]){name}(?!\w)\s*")).search(&text).map(|m| m.end()));
    let Some(mut k) = nm else { return vec![] };
    if text.as_bytes().get(k) != Some(&b'(') {
        if !f.file.ends_with(".rb") {
            return vec![];
        }
        // Ruby without parentheses: `redirect_to :login`, `system "cp #{x}" if y`
        let end = text[k..].find('\n').map(|x| x + k).unwrap_or(text.len());
        let rest = text[k..end].to_string();
        let rest = re!(r"\s+(?:if|unless|while|until|do)\b|\s*\{\s*\|").split(&rest)[0].to_string();
        let cut = crate::pyre::floor_char(&text, k - 1);
        text = format!("{}({rest})", &text[..cut]);
        raw = format!("{}({})", &raw[..crate::pyre::floor_char(&raw, k - 1)], slice(&raw, k, k + rest.len()));
        k -= 1;
    }
    let b = text.as_bytes();
    let (mut d, mut j): (i64, usize) = (0, k);
    while j < b.len() {
        d += b"([{".contains(&b[j]) as i64;
        d -= b")]}".contains(&b[j]) as i64;
        if d == 0 {
            break;
        }
        j += 1;
    }
    if j >= b.len() {
        return vec![];
    }
    let (mut args, mut start, mut dd): (Vec<String>, usize, i64) = (Vec::new(), k + 1, 0);
    for i in k + 1..j {
        dd += b"([{".contains(&b[i]) as i64;
        dd -= b")]}".contains(&b[i]) as i64;
        if b[i] == b',' && dd == 0 {
            args.push(slice(&raw, start, i).to_string());
            start = i + 1;
        }
    }
    args.push(slice(&raw, start, j).to_string());
    args.into_iter().filter(|a| !strip(a).is_empty()).collect()
}

fn shells() -> &'static Rx {
    re!(r#"^\s*[fbru]*["'`](?:/bin/|/usr/bin/)?(?:sh|bash|zsh|cmd(?:\.exe)?|powershell|pwsh)["'`]"#)
}

/// a constant string (not Ruby's "#{x}", PHP's "$x", a JS `${x}` or a Python f-string)
fn literal() -> &'static Rx {
    re!(r#"^\s*(?:[bru]*"(?:[^"\\$#]|\\.|#(?!\{)|\$(?![A-Za-z_{]))*"|[bru]*'(?:[^'\\]|\\.)*'|`[^`$]*`)\s*$"#)
}

/// A sink call's arguments carry the value. A constant first argument (a
/// parameterized query, a fixed or same-site redirect) is clean, unless it
/// names a shell (arch._carries).
pub fn carries(g: &Graph, f: &Func, c: &Call, cx: &Ctx, tainted: &HashSet<String>, sink: bool) -> bool {
    let args = call_args(g, f, c);
    let m = g.module(&f.module);
    let line = m.filter(|m| 0 < c.line && c.line <= m.raw.len()).map(|m| m.raw[c.line - 1].as_str()).unwrap_or("");
    // the receiver: `params[:class].classify.constantize`, `user_input.to_sym.send`
    if sink {
        if let Some(rv) = rxd(&format!(r#"([\w@$:.\[\]'"()]+?)\s*\.\s*{}\b"#, esc(&c.name))).search(line) {
            if dirty(rv.s(1), cx, tainted) {
                return true;
            }
        }
    }
    if args.is_empty() {
        return false;
    }
    let first = strip(&args[0]);
    if sink {
        if let (Some(cm), Some(m)) = (re!(r"([A-Za-z_$][\w$]*)\(\s*\)").fullmatch(first), m) {
            // `find(searchCriteria())`: a closure of this function's, built from its tainted names
            let want = cm.s(1);
            for mn in g.by_file.get(&f.file).map(|v| v.as_slice()).unwrap_or(&[]) {
                for h in &g.modules[mn].funcs {
                    if h.name == want && h.file == f.file && f.start < h.start && h.start <= f.end && h.id != f.id {
                        let body = m.raw[h.start.min(m.raw.len())..h.end.min(m.raw.len()).max(h.start.min(m.raw.len()))].join("\n");
                        if tainted.iter().any(|t| names(t, &body)) || cx.source.is_match(&body) {
                            return true;
                        }
                    }
                }
            }
        }
        if f.file.ends_with(".rb") && re!(r":?\w+\s*(?::(?!:)|=>)").match_start(first).is_some() {
            return false; // Rails: `where(id: params[:id])`, a bound value
        }
        if shells().match_start(first).is_none()
            && (literal().match_start(first).is_some()
                || re!(r#"^[fbru]*["'`]/(?!/)"#).match_start(first).is_some() && c.name.to_lowercase().contains("redirect"))
        {
            return false;
        }
        const JS: &[&str] = &[".js", ".mjs", ".cjs", ".ts", ".mts", ".jsx", ".tsx"];
        if re!(r"find\w*|update\w+|delete\w+|remove|count\w*|aggregate").fullmatch(&c.name).is_some()
            && JS.iter().any(|e| f.file.ends_with(e))
            && !first.starts_with('{')
            && !first.starts_with('[')
        {
            return false; // a MongoDB query is an object (`{ user: id }`); `rows.find(r => ...)` is an array's
        }
        if (c.name == "sendFile" || c.name == "download") && args[1..].iter().any(|a| re!(r"\broot\s*:").is_match(a)) {
            return false; // Express: a path under `{ root }` can't leave it
        }
    }
    args.iter().any(|a| dirty(a, cx, tainted))
}

/// The annotation / attribute lines just above g (`@GetMapping("/x")`, `[HttpPost]`) (arch._annotations).
pub fn annotations(g: &Graph, h: &Func) -> String {
    let m = g.module(&h.module);
    let mut out: Vec<&str> = Vec::new();
    let lo = (h.start as i64 - 9).max(-1);
    let mut ln = h.start as i64 - 2;
    while ln > lo {
        let t = m.filter(|m| (ln as usize) < m.raw.len()).map(|m| strip(&m.raw[ln as usize])).unwrap_or("");
        if !(t.starts_with('@') || t.starts_with('[')) {
            break;
        }
        out.push(t);
        ln -= 1;
    }
    out.reverse();
    out.join("\n")
}

/// g's parameters as (name, its declaration as written), in order (arch._param_decls).
pub fn param_decls(g: &Graph, h: &Func) -> Vec<(String, String)> {
    let Some(m) = g.module(&h.module) else { return vec![] };
    let (a, b) = ((h.start.saturating_sub(1)).min(m.code.len()), h.end.min(h.start + 12).min(m.code.len()));
    let head = if a < b { m.code[a..b].join("\n") } else { String::new() };
    let from = head.find(h.name.as_str()).unwrap_or(0);
    let Some(k) = head[from..].find('(').map(|x| x + from) else { return vec![] };
    let hb = head.as_bytes();
    let (mut d, mut j): (i64, usize) = (0, k);
    while j < hb.len() {
        d += b"([{<".contains(&hb[j]) as i64;
        d -= b")]}>".contains(&hb[j]) as i64;
        if d == 0 {
            break;
        }
        j += 1;
    }
    let ext = super::ext(&h.file);
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut dd: i64 = 0;
    for ch in slice(&head, k + 1, j).chars() {
        dd += "([{<".contains(ch) as i64;
        dd -= ")]}>".contains(ch) as i64;
        if ch == ',' && dd == 0 {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    parts.push(cur);
    const DROP: &[&str] = &[
        "self", "cls", "this", "final", "const", "mut", "ref", "out", "in", "readonly", "public", "private", "protected",
        "val", "var",
    ];
    const FIRST: &[&str] = &[".go", ".py", ".pyw", ".js", ".ts", ".tsx", ".jsx", ".mjs", ".rb", ".kt", ".rs", ".swift", ".ex", ".exs"];
    let mut out = Vec::new();
    for p in parts {
        let decl = strip(&p).to_string();
        let mut p = re!(r"@[\w.]+(?:\([^()]*\))?|\[\w+(?:\([^()]*\))?\]")
            .sub_with(p.split('=').next().unwrap_or(""), |_| " ".to_string());
        if ext != ".go" && p.replace("::", "").contains(':') {
            p = p.replace("::", "").split(':').next().unwrap_or("").to_string();
        }
        let all: Vec<&str> = re!(r"[$@]?[A-Za-z_]\w*").findall(&p);
        let mut ns: Vec<&str> = match all.split_last() {
            Some((last, rest)) => rest.iter().copied().filter(|n| *n != "params").chain([*last]).collect(),
            None => vec![],
        };
        ns.retain(|n| !DROP.contains(n));
        if ns.is_empty() {
            continue;
        }
        let name = match ns.iter().find(|n| n.starts_with('$')) {
            Some(x) => *x,
            None if FIRST.contains(&ext) => ns[0],
            None => ns[ns.len() - 1],
        };
        out.push((name.trim_start_matches(['*', '&']).to_string(), decl));
    }
    out
}

/// The parameters of g that call c (in f) passes a tainted value to (arch._passed).
pub fn passed(g: &Graph, f: &Func, c: &Call, params: &[String], cx: &Ctx, tainted: &HashSet<String>) -> HashSet<String> {
    let mut got = HashSet::new();
    let mut pos = 0;
    for a in call_args(g, f, c) {
        let kw = re!(r"\s*([A-Za-z_]\w*)\s*[=:](?!=)").match_start(&a).map(|m| m.s(1).to_string());
        let dt = dirty(&a, cx, tainted);
        if let Some(k) = kw.filter(|k| params.contains(k)) {
            if dt {
                got.insert(k);
            }
            continue;
        }
        if dt && pos < params.len() {
            got.insert(params[pos].clone());
        }
        pos += 1;
    }
    got
}

/// Where the statement whose value starts at `val` ends: before the next
/// line that starts at its indentation or less (arch._statement_end).
fn statement_end(code: &str, val: usize, w: usize) -> usize {
    let mut k = code.get(val..).and_then(|t| t.find('\n')).map(|x| x + val);
    while let Some(kk) = k {
        let nxt = code[kk + 1..].find('\n').map(|x| x + kk + 1);
        let line = &code[kk + 1..nxt.unwrap_or(code.len())];
        let (lead, body) = split_lead(line);
        if !body.is_empty() && width(lead) <= w && !")]}'\"`".contains(body.chars().next().unwrap()) {
            return kk + 1;
        }
        k = nxt;
    }
    code.len()
}

/// One assignment step: (line, name, dirty, indent width).
type Step = (usize, String, bool, usize);

/// Which names in a function hold the source's value at a line: the
/// parameters it was passed a tainted value in, then each assignment in
/// source order (`x = request.args[...]` taints x, `x = escape(x)` cleans it
/// again: the last assignment before the line decides) (arch._Taint).
pub struct Taint {
    steps: Vec<Step>,
    /// (line, name) -> the value's raw text, for a plain assignment (security's verdicts read it)
    values: HashMap<(usize, String), String>,
    /// name -> the lines a value was added to it at (`buf.write(x)`, `xs.append(q)`)
    adds: HashMap<String, HashSet<usize>>,
    given: Vec<String>,
    start: usize,
    lines: Vec<String>,
}

impl Taint {
    pub fn new(g: &Graph, f: &Func, cx: &Ctx, given: &[String], kinds: Option<&Sinks>) -> Taint {
        let code = own_code(g, f, false);
        let raw = own_code(g, f, true); // (f"dig {domain}": the raw text has the name)
        // (no interpolation: `"... where username = ?"` holds no value named username)
        let no_interp = f.file.ends_with(".java") || f.file.ends_with(".go");
        let mut t = Taint {
            steps: Vec::new(),
            values: HashMap::new(),
            adds: HashMap::new(),
            given: given.to_vec(),
            start: f.start,
            lines: code.split('\n').map(String::from).collect(),
        };
        // (offset, indent, names, the value's offset)
        let mut found: Vec<(usize, String, Vec<String>, usize)> = Vec::new();
        // `x = `, `$x = ` (PHP), `@x = ` (Ruby), `const x = ` / `String x = ` (declared), `x := ` (Go)
        for am in re!(r"(?m)^([ \t]*)(?:(?:let|const|var|val|my|final|auto|[\w<>\[\],.?]+)[ \t]+)*([$@]?[A-Za-z_]\w*)[ \t]*(?::[^=\n]+)?:?=(?!=)\s*(.+)$")
            .find_iter(&code)
        {
            found.push((am.start(), am.s(1).into(), vec![am.s(2).into()], am.span(3).unwrap().0));
        }
        // destructuring: `const { a, b: c } = req.body`, `[x, y] = ...`, Python's `a, b = ...`
        for am in re!(r"(?m)^([ \t]*)(?:(?:const|let|var)[ \t]+)?(\{[^{}=;]*\}|\[[^\[\]=;]*\]|[A-Za-z_]\w*(?:[ \t]*,[ \t]*[A-Za-z_]\w*)+)\s*=(?!=)\s*(.+)$")
            .find_iter(&code)
        {
            let inner = am.s(2).trim_matches(['{', '}', '[', ']']);
            let ns: Vec<String> = inner
                .split(',')
                .filter_map(|part| {
                    let seg = part.rsplit(':').next().unwrap_or("");
                    let seg = seg.split('=').next().unwrap_or("");
                    re!(r"[$\w]+").findall(seg).last().map(|s| s.to_string())
                })
                .collect();
            found.push((am.start(), am.s(1).into(), ns, am.span(3).unwrap().0));
        }
        // in a condition: `if ((cmd = req.getParameter("c")) != null)`, `while ((line = r.readLine()) != null)`
        let mut conds: HashMap<usize, usize> = HashMap::new(); // value offset -> where it ends, when not at the statement's end
        for am in re!(r"\(\s*([A-Za-z_]\w*)[ \t]*=(?!=)\s*").find_iter(&code) {
            let bol = code[..am.start()].rfind('\n').map(|x| x + 1).unwrap_or(0);
            let indent = re!(r"[ \t]*").match_at(&code, bol).map(|m| m.group0().to_string()).unwrap_or_default();
            found.push((am.start(), indent, vec![am.s(1).into()], am.end()));
            conds.insert(am.end(), close(&code, am.end()));
        }
        // only ever taint: a value put into an object (`sb.append(id)`, `doc.Load(Request.Body)`, `xs.push(q)`)
        // and a loop variable over a tainted collection (`for (T x : xs)`, `foreach (var x in xs)`, `for x in xs:`)
        let mut adds: HashSet<usize> = HashSet::new();
        let mut muts: HashSet<usize> = HashSet::new(); // (the calls of those, not the loops)
        for am in re!(r"(?m)^([ \t]*)([$@]?[A-Za-z_]\w*)\.(?:append|add|addAll|put|push|extend|insert|concat|write|Append|AppendFormat|AppendLine|Add|AddRange|Insert|Load|LoadXml|Write)[ \t]*\(")
            .find_iter(&code)
        {
            found.push((am.start(), am.s(1).into(), vec![am.s(2).into()], am.end()));
            adds.insert(am.end());
            muts.insert(am.end());
            conds.insert(am.end(), close(&code, am.end()));
        }
        for am in re!(r"(?m)^([ \t]*)(?:[\w}]+[ \t]+)?for(?:each)?[ \t]*\([ \t]*(?:(?:const|let|var|final)[ \t]+)?(?:[\w<>\[\],.?]+[ \t]+)?([A-Za-z_]\w*)[ \t]+(?:in|of|:)[ \t]+")
            .find_iter(&code)
        {
            found.push((am.start(), am.s(1).into(), vec![am.s(2).into()], am.end()));
            adds.insert(am.end());
            conds.insert(am.end(), close(&code, am.end()));
        }
        for am in re!(r"(?m)^([ \t]*)(?:async[ \t]+)?for[ \t]+([A-Za-z_]\w*(?:[ \t]*,[ \t]*[A-Za-z_]\w*)*)[ \t]+in[ \t]+(.+?):?[ \t]*$")
            .find_iter(&code)
        {
            let (s3, e3) = am.span(3).unwrap();
            // (`for table, rows in data.items()`: each name holds part of it)
            let ns: Vec<String> = re!(r"[A-Za-z_]\w*").findall(am.s(2)).into_iter().map(String::from).collect();
            found.push((am.start(), am.s(1).into(), ns, s3));
            adds.insert(s3);
            conds.insert(s3, e3);
        }
        // a containment check that turns the value away: `if (!p.startsWith(base)) return`,
        // `if not p.startswith(base): raise`, `unless p.start_with?(base)`: p is clean from there on
        let mut guards: HashSet<usize> = HashSet::new();
        for am in re!(r"(?m)^([ \t]*)(?:\}?[ \t]*(?:else[ \t]+)?if[ \t]*\(?[ \t]*(?:!|not[ \t]+)[ \t]*\(?|unless[ \t]+|.*\bunless[ \t]+)([A-Za-z_]\w*)\.(?:startsWith|startswith|start_with\?|StartsWith|HasPrefix)[ \t]*\(")
            .find_iter(&code)
        {
            found.push((am.start(), am.s(1).into(), vec![am.s(2).into()], am.end()));
            guards.insert(am.end());
        }
        if crate::model::is_py(&f.file) {
            // Python's validators: `if not NAME.fullmatch(x): abort(400)`, `if x not in ALLOWED: ...`,
            // `if urlsplit(x).netloc != "": x = url_for(...)` clean x after the `if`; the same check
            // without `not` (`if x.isdigit():`) cleans it inside the block
            for am in re!(r"(?m)^([ \t]*)(?:el)?if[ \t]+(.+?):(?=[ \t]*$|[ \t]+(?:return|raise|abort|continue|break)\b)")
                .find_iter(&code)
            {
                let (s2, e2) = am.span(2).unwrap();
                let cond = slice(&raw, s2, e2); // (the raw text: `re.match(r"^[a-z]+$", x)` shows its pattern)
                let anchored = g.module(&f.module).map(anchored).unwrap_or_default();
                for (k, (name, inside)) in guarded(cond, &anchored).into_iter().enumerate() {
                    let indent = format!("{}{}", am.s(1), if inside { " " } else { "" });
                    found.push((am.start() + k, indent, vec![name], s2 + k));
                    guards.insert(s2 + k);
                }
            }
        }
        found.sort();
        for (start, indent, ns, val) in found {
            let w = width(&indent);
            let ln = f.start + code[..start].matches('\n').count();
            if guards.contains(&val) {
                t.steps.extend(ns.into_iter().map(|n| (ln, n, false, w)));
                continue;
            }
            let end = match conds.get(&val) {
                Some(&e) if e != 0 => e,
                _ => statement_end(&code, val, w),
            };
            let rhs = slice(if no_interp { &code } else { &raw }, val, end);
            let value = slice(&raw, val, end);
            let now = t.at(ln);
            let mut dt = dirty(rhs, cx, &now)
                || (reads(cx.readers.as_deref(), rhs)
                    && !cx.clean.as_ref().map(|c| c.call.is_match(rhs) || c.method.is_match(rhs)).unwrap_or(false));
            if dt && !adds.contains(&val) && !conds.contains_key(&val) && kinds.map(|k| k.lookup(value)).unwrap_or(false) {
                dt = false; // `user = User.query.filter_by(name=q).first()`: the database's row, not the value
            }
            if adds.contains(&val) && !dt {
                continue; // (adding a clean value cleans nothing)
            }
            if muts.contains(&val) {
                if let Some(n) = ns.first() {
                    t.adds.entry(n.clone()).or_default().insert(ln);
                }
            }
            if !adds.contains(&val) && ns.len() == 1 {
                t.values.insert((ln, ns[0].clone()), strip(value).to_string());
            }
            t.steps.extend(ns.into_iter().map(|n| (ln, n, dt, w)));
        }
        t
    }

    /// Line b is still in the block line a opened at `indent` (no line between dedents below it).
    fn inside(&self, a: usize, b: usize, indent: usize) -> bool {
        let lo = (a + 1).saturating_sub(self.start).min(self.lines.len());
        let hi = (b + 1).saturating_sub(self.start).min(self.lines.len());
        for t in self.lines.get(lo..hi.max(lo)).unwrap_or(&[]) {
            let (lead, body) = split_lead(t);
            if !strip(t).is_empty() && width(lead) < indent && !body.starts_with([')', ']', '}']) {
                return false;
            }
        }
        true
    }

    /// (the value's text, its line) of name's latest assignment before line; None when the
    /// latest step is no plain assignment (a parameter, a loop, a destructuring).
    pub fn rhs(&self, name: &str, line: usize) -> Option<(String, usize)> {
        let last = self.steps.iter().filter(|s| s.1 == name && s.0 < line).map(|s| s.0).max()?;
        self.values.get(&(last, name.to_string())).map(|v| (v.clone(), last))
    }

    /// (the value's text, its line) of name's latest plain assignment before line, past the calls
    /// that only add to it (`buf.write(x)`); None when a parameter, a loop or a destructuring came
    /// after it, or there is none (arch._Taint.assigned).
    pub fn assigned(&self, name: &str, line: usize) -> Option<(String, usize)> {
        let mut lns: Vec<usize> = self.steps.iter().filter(|s| s.1 == name && s.0 < line).map(|s| s.0).collect();
        lns.sort_unstable();
        lns.dedup();
        for ln in lns.into_iter().rev() {
            if let Some(v) = self.values.get(&(ln, name.to_string())) {
                return Some((v.clone(), ln));
            }
            if !self.adds.get(name).is_some_and(|a| a.contains(&ln)) {
                return None;
            }
        }
        None
    }

    /// The names that hold the value just before `line`.
    pub fn at(&self, line: usize) -> HashSet<String> {
        let mut state: HashMap<&str, usize> = self.given.iter().map(|n| (n.as_str(), 0)).collect();
        for (ln, name, dt, indent) in &self.steps {
            if *ln >= line {
                break;
            }
            if *dt {
                state.entry(name.as_str()).or_insert(*indent);
            } else if let Some(&at) = state.get(name.as_str()) {
                // a clean value: from here on, or (deeper, in one branch) for the rest of that branch
                if *indent <= at || self.inside(*ln, line, *indent) {
                    state.remove(name.as_str());
                }
            }
        }
        state.into_keys().map(String::from).collect()
    }

    /// Every tainted name call c uses was assigned from a sink already
    /// reported (`st = con.prepareStatement(sql)`, then `st.executeQuery()`):
    /// the same injection, reported once.
    pub fn downstream(&self, g: &Graph, f: &Func, c: &Call, cx: &Ctx, sunk: &HashSet<usize>) -> bool {
        let m = g.module(&f.module);
        let line = m
            .map(|m| if c.line <= m.code.len() { py_index(&m.code, c.line as i64 - 1) } else { "" })
            .unwrap_or("");
        let used: Vec<String> = self.at(c.line).into_iter().filter(|t| names(t, line)).collect();
        // (a call that reads the source itself is a new injection: `r = fetch(req.query.url)`)
        let reads_src = m.map(|m| cx.source.is_match(py_index(&m.raw, c.line as i64 - 1))).unwrap_or(false);
        if used.is_empty() || sunk.is_empty() || reads_src {
            return false;
        }
        for t in &used {
            let ln = self.steps.iter().filter(|s| s.1 == *t && s.0 < c.line).map(|s| s.0).max();
            let Some(ln) = ln.filter(|l| sunk.contains(l)) else { return false };
            // (`file = params[:type].constantize.new(path)` brings in a second value: path still counts)
            let Some(m) = m else { return false };
            let code_ln = py_index(&m.code, ln as i64 - 1);
            let rhs = code_ln.split_once('=').map(|x| x.1).unwrap_or(code_ln);
            let n = self.at(ln).iter().filter(|u| names(u, rhs)).count()
                + cx.source.is_match(py_index(&m.raw, ln as i64 - 1)) as usize;
            if n > 1 {
                return false;
            }
        }
        true
    }
}

/// `xs[i]` with Python's negative index (`xs[-1]`); "" past either end.
fn py_index(xs: &[String], i: i64) -> &str {
    let k = if i < 0 { xs.len() as i64 + i } else { i };
    if k >= 0 && (k as usize) < xs.len() {
        &xs[k as usize]
    } else {
        ""
    }
}

/// What the security verdicts ask of the taint at one line (arch._Ctx).
pub struct At<'a> {
    pub g: &'a Graph,
    pub f: &'a Func,
    pub taint: &'a Taint,
    pub cx: &'a Ctx,
    pub line: usize,
}

impl TaintCtx for At<'_> {
    fn dirty(&self, text: &str) -> bool {
        // (`redirect(raw_next())`: a helper that returns the request's value)
        dirty(text, self.cx, &self.taint.at(self.line))
            || (reads(self.cx.readers.as_deref(), text)
                && !self.cx.clean.as_ref().is_some_and(|c| c.call.is_match(text) || c.method.is_match(text)))
    }

    fn rhs(&self, name: &str) -> Option<(String, Box<dyn TaintCtx + '_>)> {
        let (v, ln) = self.taint.rhs(name, self.line)?;
        Some((v, Box::new(At { g: self.g, f: self.f, taint: self.taint, cx: self.cx, line: ln })))
    }

    fn assigned(&self, name: &str) -> Option<(String, Box<dyn TaintCtx + '_>)> {
        let (v, ln) = self.taint.assigned(name, self.line)?;
        Some((v, Box::new(At { g: self.g, f: self.f, taint: self.taint, cx: self.cx, line: ln })))
    }

    /// The functions a call named `name` on this line resolves to exactly (arch._Ctx.callees).
    fn callees(&self, name: &str) -> Vec<String> {
        self.g
            .edges
            .get(&self.f.id)
            .map(|es| {
                es.iter()
                    .filter(|(d, ln, ex)| *ex && *ln == self.line && self.g.func(d).is_some_and(|h| h.name == name))
                    .map(|(d, _, _)| d.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What every exact caller passes for parameter `name` (raw texts; a caller's local is read as
    /// the value it was last assigned there), None when one passes nothing we can read (arch._Ctx.passed).
    fn passed(&self, name: &str) -> Option<Vec<String>> {
        let (g, f) = (self.g, self.f);
        let params: Vec<String> = param_decls(g, f).into_iter().map(|x| x.0).collect();
        let k = params.iter().position(|p| p == name)?;
        let kw_rx = rxd(&format!(r"\s*{}\s*[=:](?!=)", esc(name)));
        let mut out = Vec::new();
        for (caller, ln, exact) in g.redges.get(&f.id).map(|v| v.as_slice()).unwrap_or(&[]) {
            let Some(h) = g.func(caller).filter(|_| *exact) else { return None };
            for c in h.calls.iter().filter(|c| c.line == *ln && c.name == f.name) {
                let args = call_args(g, h, c);
                let kw: Vec<&str> =
                    args.iter().filter(|a| kw_rx.match_start(a).is_some()).map(|a| a.split_once('=').map(|x| x.1).unwrap_or("")).collect();
                let pos: Vec<&String> = args.iter().filter(|a| crate::rules::security::kwarg(a).is_none()).collect();
                let got = if let Some(v) = kw.first() {
                    v.to_string()
                } else if k < pos.len() {
                    pos[k].clone()
                } else {
                    return None;
                };
                out.push(self.in_caller(h, strip(&got), *ln));
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

impl At<'_> {
    /// A caller's local read as the value it was last assigned before `line` (`listUrl` for
    /// `const listUrl = c ? '/a' : '/b'`), else the text as passed (arch._Ctx._in_caller).
    fn in_caller(&self, h: &Func, arg: &str, line: usize) -> String {
        if re!(r"[A-Za-z_$][\w$]*").fullmatch(arg).is_none() {
            return arg.to_string();
        }
        let t = Taint::new(self.g, h, self.cx, &[], None);
        t.rhs(arg, line).map(|x| x.0).unwrap_or_else(|| arg.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::names;

    // arch._names: an attribute is no name of its own, a spread and PHP's `.$x` are
    #[test]
    fn names_skip_attributes() {
        assert!(names("x", "a x b"));
        assert!(!names("path", "os.path.join(p)"));
        assert!(!names("path", ".path"));
        assert!(names("args", "f(...args)"));
        assert!(names("$x", "\"a\".$x"));
        assert!(!names("x", "xy"));
    }
}
