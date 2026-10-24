//! Return types, loop variables, Rust match arms and Kotlin bodies
//! (lang_other._returns, _return_text, _own_return, _rs_arms,
//! _loop_bindings, _kotlin_body, _kotlin_expr_end).

use super::util::{ends_any, find_from, join, line_begin, lstrip, prev_char, re, rstrip, sl, starts_any, strip, words};
use crate::lang::chains::{expression, generic_name, type_name, Opts};
use crate::lang::lambdas::matching;

const RET_MODS: &[&str] = &[
    "public", "private", "protected", "internal", "static", "final", "abstract", "virtual", "override", "async",
    "sealed", "extern", "unsafe", "new", "readonly", "synchronized", "native", "default", "inline", "constexpr",
    "explicit", "friend", "volatile", "Q_INVOKABLE", "transient", "strictfp", "partial", "const", "fn", "func", "fun",
    "def", "function", "operator", "infix", "suspend", "tailrec", "open", "export",
];

fn ret_after(tk: &str) -> Option<&'static crate::pyre::Rx> {
    Some(match tk {
        "js" => re!(r"(?s)\)\s*:\s*([^={;]+?)\s*(?:=>|\{|;|$)"),
        "kotlin" => re!(r"(?s)\)\s*:\s*([^={\n]+?)\s*(?:=|\{|\bwhere\b|\n|$)"),
        "rust" => re!(r"(?s)->\s*([^{;]+?)\s*(?:\bwhere\b|\{|;|$)"),
        "swift" => re!(r"(?s)->\s*([^{]+?)\s*(?:\bwhere\b|\{|$)"),
        "php" => re!(r"(?s)\)\s*:\s*\??\s*([\\\w|?]+)"),
        _ => return None,
    })
}

/// Where the parameter list ends (its `)`), 0 when unsure (`_after_params`).
fn after_params(sig: &str) -> usize {
    let Some(k) = sig.find('(') else { return 0 };
    if re!(r"[\s\w<>,:?]*").fullmatch(&sig[..k]).is_none() {
        return 0;
    }
    let close = matching(sig, k);
    if close > 0 {
        close as usize
    } else {
        0
    }
}

fn go_result(sig: &str) -> String {
    let m = re!(r"(?s)\s*(?:\[[^\]]*\])?\s*\((?:[^()]|\([^()]*\))*\)\s*(.*)$").match_start(sig);
    let res = strip(m.map(|m| m.s(1)).unwrap_or(""));
    strip(res.trim_end_matches('{')).to_string()
}

fn jvm_tokens(tk: &str, prefix: &str, strip_attrs: bool) -> (String, Vec<String>) {
    let p = if strip_attrs { re!(r"\[[^\]]*\]|@\w+(?:\([^)]*\))?").sub(" ", prefix) } else { prefix.to_string() };
    let _ = tk;
    let p = rstrip(&re!(r"\b\w+::\s*$").sub("", rstrip(&p))).to_string();
    let toks: Vec<String> = re!(r"[\w.:]+(?:<[^;{}()]*>)?[?*&\[\]]*")
        .find_iter(&p)
        .iter()
        .map(|m| m.group0().to_string())
        .filter(|t| !RET_MODS.contains(&t.as_str()))
        .collect();
    (p, toks)
}

/// The return type's class (`_returns`).
pub fn returns(tk: &str, prefix: &str, sig: &str, name: &str, cls: &str) -> String {
    if !cls.is_empty() && (name == cls || matches!(name, "constructor" | "__construct" | "init")) {
        return "Self".into();
    }
    if tk == "go" {
        let mut res = go_result(sig);
        if let Some(r) = res.strip_prefix('(') {
            let r = strip(r.split(',').next().unwrap_or("")).to_string();
            let w = words(&r);
            res = if w.len() == 2 { w[1].to_string() } else { r.clone() };
        }
        return type_name(res.trim_start_matches('*'));
    }
    if let Some(pat) = ret_after(tk) {
        return pat.search_at(sig, after_params(sig)).map(|m| type_name(m.s(1))).unwrap_or_default();
    }
    if tk == "jvm" || tk == "cpp" {
        let (p, toks) = jvm_tokens(tk, prefix, true);
        if re!(r"\)[?\[\]]*$").search(&p).is_some() {
            return String::new(); // a C# tuple `(bool, object?) TryGet(`: no one class
        }
        return match toks.last() {
            Some(t) if tk == "jvm" => generic_name(t),
            Some(t) => type_name(t),
            None => String::new(),
        };
    }
    String::new()
}

/// The declared return type as written (`_return_text`).
pub fn return_text(tk: &str, prefix: &str, sig: &str) -> String {
    if tk == "go" {
        let res = go_result(sig);
        if let Some(r) = res.strip_prefix('(') {
            if !strip(r).is_empty() {
                let first = strip(r.split(',').next().unwrap_or(""));
                return words(first).last().map(|s| s.to_string()).unwrap_or_else(|| panic!("list index out of range"));
            }
        }
        return res;
    }
    if let Some(pat) = ret_after(tk) {
        return pat.search_at(sig, after_params(sig)).map(|m| strip(m.s(1)).to_string()).unwrap_or_default();
    }
    let pr = rstrip(prefix);
    if tk == "jvm" && pr.ends_with(')') {
        if let Some(o) = pr.rfind('(') {
            if matching(pr, o) == pr.len() as isize - 1 && prefix[o..].contains(',') {
                return pr[o..].to_string(); // C# `(List<X> Variables, List<Y> Expressions) Scratchpad(`: a tuple
            }
        }
    }
    if tk == "jvm" || tk == "cpp" {
        let (_, toks) = jvm_tokens(tk, prefix, tk == "jvm");
        return toks.last().cloned().unwrap_or_default();
    }
    String::new()
}

/// With no declared type, what the body's own last top-level `return <chain>` gives (`_own_return`).
pub fn own_return(code: &str, bpos: usize, o: Opts) -> String {
    let b = code.as_bytes();
    let (mut depth, mut k, n, mut last) = (0i64, bpos, b.len(), String::new());
    while k < n {
        let ch = b[k];
        if ch == b'{' || ch == b'(' || ch == b'[' {
            depth += 1;
        } else if ch == b'}' || ch == b')' || ch == b']' {
            depth -= 1;
            if depth == 0 {
                break;
            }
        } else if depth == 1
            && b[k..].starts_with(b"return")
            && !prev_char(code, k).chars().next().map(|c| c.is_alphanumeric()).unwrap_or(false)
            && matches!(b.get(k + 6), None | Some(b' ') | Some(b'\t'))
        {
            let ch_ = expression(code, k + 6, 40, o);
            last = if ch_.is_empty() { String::new() } else { format!("expr:{}", join(&ch_)) };
        }
        k += 1;
    }
    last
}

/// (name, 1-based line, "variant:<Enum::>Variant") for each line an arm binds (`_rs_arms`).
pub fn rs_arms(body: &str, rbody: &str, first_line: usize) -> Vec<(String, usize, String)> {
    let mut starts = vec![0usize];
    let parts: Vec<&str> = body.split('\n').collect();
    for ln in &parts[..parts.len() - 1] {
        starts.push(starts.last().unwrap() + ln.len() + 1);
    }
    let line_at = |pos: usize| -> usize { starts.partition_point(|&s| s <= pos) - 1 };
    let b = body.as_bytes();
    let mut out = Vec::new();
    for m in re!(r"(?:\b([A-Z]\w*)::)?\b([A-Z]\w*)\s*\(\s*(?:ref\s+)?(?:mut\s+)?([a-z_]\w*)\s*\)\s*(=>|=(?![=>]))")
        .find_iter(body)
    {
        if matches!(m.s(2), "Some" | "Ok" | "Err" | "Box" | "Rc" | "Arc" | "Cow" | "Vec") {
            continue;
        }
        let mut k = m.end() as isize;
        while (k as usize) < b.len() && matches!(b[k as usize], b' ' | b'\t' | b'\n') {
            k += 1;
        }
        if m.s(4) == "=" {
            k = find_from(body, "{", k as usize); // `if let V(x) = expr {`: the block after
        }
        if k < 0 || k as usize >= b.len() {
            continue;
        }
        let k = k as usize;
        let end: isize = if b[k] == b'{' {
            matching(body, k)
        } else {
            // `V(x) => x.ino(),`: to the arm's `,` (or the match's `}`)
            let (mut d, mut end, mut steps) = (0i64, k, 0usize);
            while end < b.len() && steps < 2000 {
                let ch = b[end];
                if ch == b'(' || ch == b'[' || ch == b'{' {
                    d += 1;
                } else if ch == b')' || ch == b']' || ch == b'}' {
                    if d == 0 {
                        break;
                    }
                    d -= 1;
                } else if ch == b',' && d == 0 {
                    break;
                }
                if super::util::lead(rbody, end) {
                    steps += 1;
                }
                end += 1;
            }
            end as isize
        };
        if end < 0 {
            continue;
        }
        let spec = if m.s(1).is_empty() {
            format!("variant:{}", m.s(2))
        } else {
            format!("variant:{}::{}", m.s(1), m.s(2))
        };
        for ln in line_at(m.start())..=line_at(end as usize) {
            out.push((m.s(3).to_string(), first_line + ln + 1, spec.clone()));
        }
    }
    out
}

const ITER_ADAPTERS: &[&str] = &[
    "iter()", "iter_mut()", "into_iter()", "drain()", "asSequence()", "asIterable()", "toList()", "toArray()",
    "entries()", "values()", "lazy",
];

fn loop_rx(tk: &str) -> Option<&'static crate::pyre::Rx> {
    Some(match tk {
        "rust" => re!(r"\bfor\s+(?:&\s*)?(?:mut\s+)?([a-z_]\w*)\s+in\s+(?:&\s*(?:mut\s+)?)?"),
        "js" => re!(r"\bfor\s*\(\s*(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s+of\s+"),
        "kotlin" => re!(r"\bfor\s*\(\s*([a-z_]\w*)\s+in\s+"),
        "swift" => re!(r"\bfor\s+(?:case\s+let\s+|var\s+)?([a-z_]\w*)\s+in\s+"),
        "go" => re!(r"\bfor\s+[a-z_]\w*\s*,\s*([a-z_]\w*)\s*:=\s*range\s+"),
        "jvm" => re!(r"\bfor(?:each)?\s*\(\s*(?:final\s+)?var\s+([a-z_]\w*)\s*(?::|\bin\b)\s*"),
        "cpp" => re!(r"\bfor\s*\(\s*(?:const\s+)?auto\s*[&*]*\s*([a-z_]\w*)\s*:\s*"),
        _ => return None,
    })
}

/// (loop variable, "elem:<chain>") for each for-loop over a chain (`_loop_bindings`).
pub fn loop_bindings(tk: &str, text: &str, o: Opts) -> Vec<(String, String)> {
    let Some(pat) = loop_rx(tk) else { return Vec::new() };
    let mut out = Vec::new();
    for m in pat.find_iter(text) {
        let mut ch: Vec<String> = if matches!(tk, "rust" | "swift" | "go") {
            // `for m in xs.iter() {`: the chain ends at the body's brace
            let brace = find_from(text, "{", m.end());
            if brace > 0 {
                expression(&format!("{}\n", &text[..brace as usize]), m.end(), 40, o)
            } else {
                Vec::new()
            }
        } else {
            expression(text, m.end(), 40, o)
        };
        if ch.is_empty() && matches!(tk, "jvm" | "cpp" | "kotlin" | "js") {
            // (`for (x in xs)`: the chain ends at the `)`)
            let close = find_from(text, ")", m.end());
            if close > 0 {
                ch = expression(&format!("{}\n", &text[..close as usize]), m.end(), 40, o);
            }
        }
        while ch.last().map(|s| ITER_ADAPTERS.contains(&s.as_str())).unwrap_or(false) {
            ch.pop();
        }
        if !ch.is_empty() {
            out.push((m.s(1).to_string(), format!("elem:{}", join(&ch))));
        }
    }
    out
}

const KT_CONT_END: &[&str] =
    &["=", "+", "-", "*", "/", "%", "&&", "||", "?:", "(", "[", ",", "->", ".", "?.", "!", "<", ">", ".."];
const KT_CONT_START: &[&str] = &[
    ".", "?.", "?:", "&&", "||", "+", "-", "*", "/", "%", "->", ")", "]", "}", "..", "as ", "as?", "is ", "!is ", "in ",
    "!in ", "else",
];

/// A Kotlin function's body from just after its `(` (`_kotlin_body`):
/// (Some(block `{`), None), (None, Some(end of `= expr`)), or neither.
pub fn kotlin_body(code: &str, text: &str, after_paren: usize, n: usize) -> (Option<usize>, Option<usize>) {
    let b = code.as_bytes();
    let (mut k, mut depth) = (after_paren, 1i64);
    while k < n && depth > 0 {
        let ch = b[k];
        if ch == b'(' || ch == b'[' || ch == b'{' {
            depth += 1;
        } else if ch == b')' || ch == b']' || ch == b'}' {
            depth -= 1;
        }
        k += 1;
    }
    depth = 0;
    while k < n {
        let ch = b[k];
        if ch == b'(' || ch == b'[' {
            depth += 1;
        } else if ch == b')' || ch == b']' {
            depth -= 1;
        } else if depth <= 0 && ch == b'{' {
            return (Some(k), None);
        } else if depth <= 0
            && ch == b'='
            && !matches!(b.get(k + 1), Some(b'=') | Some(b'>'))
            && !(k > 0 && matches!(b[k - 1], b'!' | b'<' | b'>' | b'='))
        {
            return (None, Some(kotlin_expr_end(code, k + 1, n)));
        } else if depth <= 0 && ch == b'\n' {
            let rest = lstrip(super::util::rw(code, text, k + 1, 199));
            if !starts_any(rest, &["{", "=", ":", "where"]) && !ends_any(rstrip(&code[..k]), &[":", ","]) {
                return (None, None); // abstract / interface declaration
            }
        }
        k += 1;
    }
    (None, None)
}

/// The end of an expression body (`_kotlin_expr_end`).
pub fn kotlin_expr_end(code: &str, mut k: usize, n: usize) -> usize {
    let b = code.as_bytes();
    let mut depth = 0i64;
    while k < n {
        let ch = b[k];
        if ch == b'(' || ch == b'[' || ch == b'{' {
            depth += 1;
        } else if ch == b')' || ch == b']' || ch == b'}' {
            depth -= 1;
            if depth < 0 {
                return k - 1; // closed an enclosing scope: the expression ended before it
            }
        } else if ch == b'\n' && depth <= 0 {
            let here = rstrip(sl(code, line_begin(code, k), k));
            let nxt_end = code[k + 1..].find('\n').map(|e| k + 1 + e).unwrap_or(n);
            let nxt = lstrip(sl(code, k + 1, nxt_end));
            if here.is_empty() || !(ends_any(here, KT_CONT_END) || starts_any(nxt, KT_CONT_START)) {
                return k;
            }
        }
        k += 1;
    }
    n - 1
}
