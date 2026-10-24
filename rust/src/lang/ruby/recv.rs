//! Receiver chains and plain expressions (lang_ruby.receiver / expression).

use super::blocks::match_paren;
use super::pats::*;
use super::util::*;

pub type IsLocal<'a> = &'a dyn Fn(&str, usize) -> bool;

fn ident_back(code: &str, i: usize) -> usize {
    let mut k = i;
    while k > 0 {
        match prev_chr(code, k) {
            Some(c) if isalnum(c) || c == '_' => k -= c.len_utf8(),
            _ => break,
        }
    }
    k
}

fn back_ws(code: &str, i: usize) -> usize {
    let t = code.as_bytes();
    let mut i = i;
    while i > 0 && matches!(t[i - 1], b' ' | b'\t' | b'\r' | b'\n') {
        i -= 1;
    }
    i
}

fn match_back(code: &str, i: usize, open: u8) -> Option<usize> {
    let t = code.as_bytes();
    let mut depth = 0i64;
    let mut k = i as isize - 1;
    while k >= 0 {
        let ch = t[k as usize];
        if matches!(ch, b')' | b']' | b'}') {
            depth += 1;
        } else if matches!(ch, b'(' | b'[' | b'{') {
            depth -= 1;
            if depth == 0 {
                return if ch == open { Some(k as usize) } else { None };
            }
        }
        k -= 1;
    }
    None
}

/// The receiver segment that ends at i: ("name" | "@x" | "A::B" | "x()" |
/// "[]" | "(...)", where it starts); ("", i) when there is none.
fn segment_back(code: &str, i: usize) -> (String, usize) {
    if i == 0 {
        return (String::new(), i);
    }
    let ch = prev_chr(code, i).unwrap_or('\0');
    if matches!(ch, '"' | '\'' | '`') {
        return ("String".into(), i - 1);
    }
    if ch == '/' {
        return ("Regexp".into(), i - 1);
    }
    if ch == ')' {
        let Some(k) = match_back(code, i, b'(') else { return ("(...)".into(), i) };
        let j = k;
        let s = ident_back(code, j);
        if s > 0 && byte_at(code, s - 1) == b'%' && j - s <= 1 {
            return (pct_type(sub(code, s, j)).unwrap_or("String").into(), s - 1);
        }
        if s == j {
            return ("(...)".into(), k);
        }
        return (format!("{}()", sub(code, s, j)), s);
    }
    if ch == ']' {
        let Some(k) = match_back(code, i, b'[') else { return ("(...)".into(), i) };
        if k > 1 && byte_at(code, k - 2) == b'%' {
            if let Some(c) = prev_chr(code, k) {
                if let Some(t) = pct_type(&c.to_string()) {
                    return (t.into(), k - 2);
                }
            }
        }
        let before = if k == 0 { None } else { prev_chr(code, k) };
        return match before {
            Some(c) if isalnum(c) || "_)]?!".contains(c) => ("[]".into(), k),
            _ => ("Array".into(), k),
        };
    }
    if matches!(ch, '?' | '!') && i >= 2 && prev_chr(code, i - 1).map(|c| isalnum(c) || c == '_').unwrap_or(false) {
        let s = ident_back(code, i - 1);
        return (format!("{}()", sub(code, s, i)), s);
    }
    if isalnum(ch) || ch == '_' {
        let mut s = ident_back(code, i);
        let mut name = sub(code, s, i).to_string();
        let first = name.chars().next().unwrap_or('\0');
        if first.is_ascii_digit() || (!first.is_ascii() && first.is_numeric()) {
            return ("Integer".into(), s);
        }
        if let Some(l) = literal_word(&name) {
            return (l.into(), s);
        }
        if s >= 1 && byte_at(code, s - 1) == b':' && !(s >= 2 && byte_at(code, s - 2) == b':') {
            return ("Symbol".into(), s - 1);
        }
        if s >= 2 && sub(code, s - 2, s) == "@@" {
            return (format!("@@{name}"), s - 2);
        }
        if s >= 1 && byte_at(code, s - 1) == b'@' {
            return (format!("@{name}"), s - 1);
        }
        if s >= 1 && byte_at(code, s - 1) == b'$' {
            return (format!("${name}"), s - 1);
        }
        if upper(first) {
            while s >= 2 && sub(code, s - 2, s) == "::" {
                let t = ident_back(code, s - 2);
                if t < s - 2 && chr_at(code, t).map(upper).unwrap_or(false) {
                    name = format!("{}::{}", sub(code, t, s - 2), name);
                    s = t;
                } else {
                    if t == s - 2 {
                        s -= 2;
                    }
                    break;
                }
            }
        }
        return (name, s);
    }
    ("(...)".into(), i)
}

/// The chain a method call at pos is made on: `@site.config.fetch` ->
/// ("@site", "config()").
pub fn receiver(code: &str, pos: usize, is_local: IsLocal) -> Vec<String> {
    let mut segs: Vec<String> = Vec::new();
    let mut i = back_ws(code, pos);
    while segs.len() < 8 {
        if i >= 1 && byte_at(code, i - 1) == b'.' && !(i >= 2 && byte_at(code, i - 2) == b'.') {
            i -= 1;
            if i >= 1 && byte_at(code, i - 1) == b'&' {
                i -= 1;
            }
        } else if i >= 2 && sub(code, i - 2, i) == "::" {
            i -= 2;
        } else {
            break;
        }
        i = back_ws(code, i);
        let (mut seg, mut s) = segment_back(code, i);
        if seg.is_empty() {
            break;
        }
        if seg == "[]" {
            segs.push("[]".into());
            let r = segment_back(code, s);
            seg = r.0;
            s = r.1;
            if seg.is_empty() || seg == "(...)" {
                segs.push("(...)".into());
                break;
            }
        }
        if seg == "class" && s > 0 && byte_at(code, s - 1) == b'.' {
            seg = "class()".into();
        }
        if is_keyword(seg.trim_end_matches(['(', ')'])) && seg != "self" && seg != "class()" {
            segs.push("(...)".into());
            break;
        }
        let wild = seg == "(...)";
        segs.push(seg);
        i = s;
        if wild {
            break;
        }
    }
    segs.reverse();
    normalize(segs, is_local, pos)
}

pub fn normalize(segs: Vec<String>, is_local: IsLocal, pos: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, s0) in segs.into_iter().enumerate() {
        let mut s = s0;
        if (first_lower(&s) || s.starts_with('_')) && s != "self" {
            let bare = !s.ends_with("()");
            if bare && (k > 0 || !is_local(&s, pos)) {
                s.push_str("()");
            }
        }
        if s == "class()" && !out.is_empty() {
            continue;
        }
        if is_same(&s) && !out.is_empty() {
            continue;
        }
        out.push(s);
    }
    out
}

/// A literal starting at i (contents blanked): its class and its end.
fn literal(code: &str, i: usize) -> Option<(&'static str, usize)> {
    let p = pats();
    let ch = chr_at(code, i)?;
    if matches!(ch, '"' | '\'' | '`' | '/') {
        let e = find(code, &ch.to_string(), i + 1)?;
        return Some((if ch == '/' { "Regexp" } else { "String" }, e + 1));
    }
    if ch == '[' || ch == '{' {
        return Some((if ch == '[' { "Array" } else { "Hash" }, match_paren(code, i) + 1));
    }
    if ch.is_ascii_digit() || (!ch.is_ascii() && ch.is_numeric()) {
        let m = p.number.at(code, i)?;
        return Some((if m.get(1).is_some() { "Float" } else { "Integer" }, m.end()));
    }
    if ch == ':' && chr_at(code, i + 1).map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false) {
        let m = p.sym_lit.at(code, i)?;
        return Some(("Symbol", m.end()));
    }
    if let Some(m) = p.pct_lit.at(code, i) {
        let open = m.s(2);
        let close = match open {
            "(" => ")",
            "[" => "]",
            "{" => "}",
            "<" => ">",
            o => o,
        };
        let e = if close == open || close == ">" { find(code, close, m.end()) } else { Some(match_paren(code, m.end() - 1)) };
        return match e {
            Some(e) if e > 0 => Some((pct_type(m.s(1)).unwrap_or("String"), e + 1)),
            _ => None,
        };
    }
    None
}

/// The chain `x = <expr>` builds when <expr> is a plain one: `Foo.new(a)`
/// -> ("Foo", "new()"). Empty when it is anything else.
pub fn expression(code: &str, pos: usize, is_local: IsLocal) -> Vec<String> {
    use std::sync::OnceLock;
    static R: OnceLock<[crate::pyre::Rx; 3]> = OnceLock::new();
    let rxs = R.get_or_init(|| {
        [
            crate::pyre::Rx::lit(r"(?:if|unless|rescue|do|while|until|then)\b"),
            crate::pyre::Rx::lit(r#"[\w@:"'\[{(*&$-]|\\$"#),
            crate::pyre::Rx::lit(r"(?:and|or|\?)\b"),
        ]
    });
    let p = pats();
    let n = code.len();
    let mut i = pos;
    while i < n && matches!(byte_at(code, i), b' ' | b'\t') {
        i += 1;
    }
    let mut segs: Vec<String> = Vec::new();
    let tok: String;
    if let Some((t, e)) = literal(code, i) {
        tok = t.to_string();
        i = e;
    } else {
        let Some(m) = p.expr_head.at(code, i) else { return Vec::new() };
        let g = m.s(1);
        let mut t = if g.starts_with("::") { g.trim_start_matches(':').to_string() } else { g.to_string() };
        if let Some(l) = literal_word(&t) {
            t = l.to_string();
        } else if is_keyword(&t) && t != "self" {
            return Vec::new();
        }
        i = m.end();
        if i < n && byte_at(code, i) == b'(' {
            i = match_paren(code, i) + 1;
            if !t.ends_with("()") {
                t.push_str("()");
            }
        } else if i < n && byte_at(code, i) == b'[' && first_upper(&t) {
            segs.push(t);
            i = match_paren(code, i) + 1;
            t = "[]()".to_string();
        }
        tok = t;
    }
    segs.push(tok);
    while segs.len() < 8 {
        let last = segs.last().unwrap().clone();
        if i < n && byte_at(code, i) == b'[' && last != "[]" && last != "[]()" {
            let close = match_paren(code, i);
            if sub(code, i, close).contains('\n') {
                return Vec::new();
            }
            segs.push("[]".into());
            i = close + 1;
            continue;
        }
        let mut j = i;
        while j < n && matches!(byte_at(code, j), b' ' | b'\t') {
            j += 1;
        }
        let mut nl = j;
        while nl < n && matches!(byte_at(code, nl), b' ' | b'\t' | b'\r' | b'\n') {
            nl += 1;
        }
        if starts_at(code, nl, "&.") || (starts_at(code, nl, ".") && !starts_at(code, nl, "..")) {
            i = nl + if byte_at(code, nl) == b'&' { 2 } else { 1 };
            let Some(mm) = p.expr_name.at(code, i) else { return Vec::new() };
            i = mm.end();
            let name = mm.s(1).to_string();
            if i < n && byte_at(code, i) == b'(' {
                i = match_paren(code, i) + 1;
            }
            let mut k = i;
            while k < n && matches!(byte_at(code, k), b' ' | b'\t') {
                k += 1;
            }
            if k < n && byte_at(code, k) == b'{' {
                i = match_paren(code, k) + 1;
            }
            segs.push(format!("{name}()"));
            continue;
        }
        let eol = find(code, "\n", j).unwrap_or(n);
        let rest = rstrip(sub(code, j, eol));
        let first = rest.chars().next();
        if rest.is_empty() || first.map(|c| ";)],}".contains(c)).unwrap_or(false) || rxs[0].match_start(rest).is_some() {
            break;
        }
        if last.ends_with("()") || first_lower(&last) {
            // `Foo.new a, b` / `render x`: arguments without parentheses
            if j > i
                && rxs[1].match_start(rest).is_some()
                && rxs[2].match_start(rest).is_none()
                && !(rest.starts_with("-=") || rest.starts_with("--") || rest.starts_with("- "))
            {
                break;
            }
        }
        return Vec::new();
    }
    normalize(segs, is_local, pos)
}
