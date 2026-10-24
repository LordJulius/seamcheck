//! Receiver chains and type names, read off blanked code (chains.py).
//!
//! A call's receiver is what it is called on: `self.store.get().save(x)`
//! calls `save` on the chain ["self", "store", "get()"]. The graph walks a
//! chain one step at a time to know which class a call lands in. These
//! helpers only read text; they never guess a type.
//!
//! Positions are byte offsets into `code`. Every character these functions
//! step over or compare is ASCII, so a byte offset lands where Python's
//! character offset does; the few windows Python measures in characters
//! (`code[i-300:i]`) are measured in characters here too. Python's module
//! flags `CALL_TARGS` / `TRAILING_CLOSURES` (set while a C# or a Swift /
//! Kotlin file is parsed) are the `Opts` every function takes.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::pyre::Rx;

/// What the language being parsed changes about chains (chains.CALL_TARGS,
/// chains.TRAILING_CLOSURES).
#[derive(Clone, Copy, Default, Debug)]
pub struct Opts {
    /// C#: a method call's segment carries its type-argument count (`Handle<E>()` -> "Handle`1()")
    pub call_targs: bool,
    /// Swift / Kotlin: a closure after a call is its last argument
    pub trailing_closures: bool,
}

impl Opts {
    /// The options a file's language sets (lang_other._parse_brace).
    pub fn for_file(rel: &str) -> Opts {
        Opts {
            call_targs: rel.ends_with(".cs"),
            trailing_closures: rel.ends_with(".swift") || rel.ends_with(".kt") || rel.ends_with(".kts"),
        }
    }
}

/// Segments that unwrap an Option / Result / nullable: the type inside.
pub const UNWRAP: &[&str] = &[
    "unwrap()", "expect()", "unwrap_or_default()", "unwrap_or()", "unwrap_or_else()", "?", "!!", "!", "get()",
    "orElseThrow()", "requireNotNull()", "checkNotNull()", "value()", "Value", "Result", "await",
];

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [OnceLock<Rx>; 48] = [const { OnceLock::new() }; 48];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// The last `n` characters before byte `i` (Python's `code[max(0, i - n):i]`).
fn win(code: &str, i: usize, n: usize) -> &str {
    let mut start = i;
    let mut k = 0;
    for (p, _) in code[..i].char_indices().rev() {
        if k == n {
            break;
        }
        start = p;
        k += 1;
    }
    &code[start..i]
}

/// Top-level commas in a bracketed text (chains._top_commas).
pub fn top_commas(text: &str) -> usize {
    let (mut d, mut n) = (0i64, 0);
    for ch in text.bytes() {
        d += b"<([{".contains(&ch) as i64;
        d -= b">)]}".contains(&ch) as i64;
        n += (ch == b',' && d == 0) as usize;
    }
    n
}

fn skip_back_ws(c: &[u8], mut i: usize) -> usize {
    while i > 0 && ws(c[i - 1]) {
        i -= 1;
    }
    i
}

/// `c[i-1]` is `close`: the index of its matching `open`, or -1. All bracket
/// kinds nest; a `;` outside them means the statement ended first.
fn match_back(c: &[u8], i: usize, close: u8, open: u8) -> isize {
    let pair = |ch: u8| match ch {
        b')' => b'(',
        b']' => b'[',
        b'}' => b'{',
        _ => b'<',
    };
    let mut stack: Vec<u8> = Vec::new();
    let mut k = i as isize - 1;
    while k >= 0 {
        let ch = c[k as usize];
        if matches!(ch, b')' | b']' | b'}') || (ch == b'>' && close == b'>') {
            stack.push(pair(ch));
        } else if matches!(ch, b'(' | b'[' | b'{') || (ch == b'<' && close == b'>') {
            if stack.last() != Some(&ch) {
                return -1;
            }
            stack.pop();
            if stack.is_empty() {
                return if ch == open { k } else { -1 };
            }
        } else if ch == b';' && stack.len() <= 1 {
            return -1; // left the statement
        }
        k -= 1;
    }
    -1
}

fn ident_back(c: &[u8], i: usize) -> usize {
    let mut k = i;
    while k > 0 && ident(c[k - 1]) {
        k -= 1;
    }
    k
}

/// `code[i-1]` is the `}` of `new Foo<T>(args) { ... }`: "new Foo()", else "".
fn anonymous_class(code: &str, i: usize) -> String {
    let c = code.as_bytes();
    let k = match_back(c, i, b'}', b'{');
    if k < 0 {
        return String::new();
    }
    let j = skip_back_ws(c, k as usize);
    if j == 0 || c[j - 1] != b')' {
        return String::new();
    }
    let k2 = match_back(c, j, b')', b'(');
    if k2 < 0 {
        return String::new();
    }
    let mut j2 = skip_back_ws(c, k2 as usize);
    if j2 > 0 && c[j2 - 1] == b'>' {
        let g = match_back(c, j2, b'>', b'<');
        if g < 0 {
            return String::new();
        }
        j2 = skip_back_ws(c, g as usize);
    }
    let s = ident_back(c, j2);
    if s == j2 || !c[s.saturating_sub(4)..s].ends_with(b"new ") {
        return String::new();
    }
    format!("new {}()", &code[s..j2])
}

/// The chain a call at `pos` (its name's first character) is made on, or
/// empty for a bare call (chains.receiver). Segments: `name`, `name()`,
/// `[]`, `?`, `!!`; `new X()`; a parenthesized expression is "(...)".
pub fn receiver(code: &str, pos: usize, max_segs: usize, o: Opts) -> Vec<String> {
    let c = code.as_bytes();
    let mut segs: Vec<String> = Vec::new();
    let mut i = skip_back_ws(c, pos);
    if i < pos && &c[i.saturating_sub(2)..i] == b"->" && matches!(c[pos - 1], b' ' | b'\t') {
        return Vec::new(); // `x == null -> knownNotNull(d)`: a Kotlin `when` / lambda arrow
    }
    while segs.len() < max_segs {
        let mut op = "";
        for o_ in ["?.", "!.", "->", "::", "."] {
            if i >= o_.len() && &c[i - o_.len()..i] == o_.as_bytes() {
                op = o_;
                break;
            }
        }
        if op.is_empty() {
            break;
        }
        if op == "." && i >= 2 && c[i - 2] == b'.' {
            break; // a range `..` or a spread `...`
        }
        i = skip_back_ws(c, i - op.len());
        if op == "."
            && segs.is_empty()
            && (i == 0
                || b"(,[:={".contains(&c[i - 1])
                || rx(0, r"(?:^|[^\w$])(?:case|return|in|else|try|await|let|var)$").search(win(code, i, 7)).is_some())
        {
            segs.push("<implicit>".into()); // Swift's `.option(x)`: a member of a type the context gives
            break;
        }
        // postfix unwraps: `x!!.`, `x!.`, `x?.`, Rust `x?.`
        while i > 0 && (c[i - 1] == b'!' || c[i - 1] == b'?') {
            segs.push(if c[i - 1] == b'!' { "!!" } else { "?" }.into());
            i = skip_back_ws(c, i - 1);
        }
        if i == 0 {
            break;
        }
        let mut ch = c[i - 1];
        let mut closure_call = false;
        if ch == b'}' && o.trailing_closures {
            // `.uploadProgress(q) { p in ... }.next()`: the closure ends a call
            let k = match_back(c, i, b'}', b'{');
            let j = if k > 0 { skip_back_ws(c, k as usize) } else { 0 };
            if j > 0 && (c[j - 1] == b')' || ident(c[j - 1])) {
                i = j;
                ch = c[i - 1];
                closure_call = ch != b')';
            }
        }
        if ch == b'/'
            && rx(1, r"(?:^|[=(,:!&|?{};\s])/(?:[^/\\\n]|\\.)+/[a-z]*$").search(win(code, i, 300)).is_some()
        {
            segs.push("<literal>".into()); // a regex literal: the runtime's RegExp
            break;
        }
        if ch == b')' {
            let k = match_back(c, i, b')', b'(');
            if k < 0 {
                return Vec::new();
            }
            let k = k as usize;
            let mut j = skip_back_ws(c, k);
            let mut arity = String::new();
            if j > 0 && c[j - 1] == b'>' {
                // `foo<T>(`: a generic call
                let g = match_back(c, j, b'>', b'<');
                if g > 0 {
                    let g = g as usize;
                    arity = format!("`{}", 1 + top_commas(&code[g + 1..j - 1]));
                    j = skip_back_ws(c, g);
                }
            }
            let mut s = ident_back(c, j);
            if s < j
                && ["return", "await", "throw", "yield", "case", "in", "else", "and", "or", "not"].contains(&&code[s..j])
                && !{
                    let b = code[..s].trim_end();
                    b.ends_with('.') || b.ends_with("::") || b.ends_with("->") || b.ends_with('$')
                }
            {
                s = j; // a keyword before a parenthesized expression, no call
            }
            if s == j && j > 1 && c[j - 1] == b'!' && ident(c[j - 2]) && &c[j - 2..j] != b"!!" {
                let ms = ident_back(c, j - 1); // Rust `arg!(...)`: a macro's value starts the chain
                segs.push(format!("{}!()", &code[ms..j - 1]));
                break;
            }
            if s == j {
                // `(expr).x`: a parenthesized expression
                let inner = &code[k + 1..i - 1];
                let cast = rx(2, r"\bas[!?]?\s+([A-Z][\w.]*|`[^`\n]+`)\s*[?]?\s*$").search(inner).or_else(|| {
                    rx(
                        3,
                        r"\s*\(\s*([A-Z][\w.]*|byte|sbyte|short|ushort|int|uint|long|ulong|char|bool|decimal|double|float|string|object)(?:<[^()]*>)?\s*\)\s*[\w$(]",
                    )
                    .match_start(inner)
                });
                let aw = rx(4, r"^\s*(?:await|try[?!]?)\s+([A-Za-z_$][\w$]*)\s*$").match_start(inner);
                let lit = rx(5, r"^\s*&?((?:[A-Za-z_]\w*\.)?[A-Z]\w*)\s*\{")
                    .match_start(inner)
                    .or_else(|| rx(6, r"^\s*new\s+([A-Z][\w.]*)\s*(?:<[^()]*>)?\s*\(").match_start(inner));
                if let (Some(l), None) = (&lit, &cast) {
                    segs.push(format!("new {}()", l.s(1).rsplit('.').next().unwrap_or("")));
                    break;
                }
                if let Some(cm) = &cast {
                    let mut cn = cm.s(1).to_string();
                    if cn.starts_with('`') {
                        cn = rx(7, r"\W").sub("_", cn.trim_matches('`'));
                    }
                    segs.push(format!("<as:{cn}>"));
                } else if let Some(a) = &aw {
                    segs.push(a.s(1).to_string());
                } else if let Some(ich) = {
                    // `(responder ?? self).downloader(...)`: the fallback's type
                    rx(8, r"^[^?()]*(?:\([^()]*\)[^?()]*)*\?\?\s*").match_start(inner).and_then(|nc| {
                        let e = nc.end();
                        if rx(9, r"[\w$.]+(?:\(\))?\s*").fullmatch(&inner[e..]).is_some() {
                            Some(expression(inner, e, 40, o)).filter(|x| !x.is_empty())
                        } else {
                            None
                        }
                    })
                } {
                    segs.extend(ich.into_iter().rev());
                } else if let Some(ich) = rx(10, r"^\s*(?:await|try[?!]?)\s+")
                    .match_start(inner)
                    .map(|awc| expression(inner, awc.end(), 40, o))
                    .filter(|x| !x.is_empty())
                {
                    segs.extend(ich.into_iter().rev()); // `(await conn.QueryAsync<T>(sql)).Single()`
                } else if rx(11, r"^\s*-?[\w.]+\s*(?:\.\.[.<=]|\.\.(?=\s*-?\w))\s*-?[\w.]+\s*$").match_start(inner).is_some() {
                    segs.push("<range>".into()); // `(1...9).contains(x)`
                } else if rx(12, r"[-+*/%|^~]|<<|>>").search(inner).is_some()
                    && rx(13, r#"[A-Za-z_$][\w$]*\s*\(|\?|=|\b(?:as|new|await)\b|['"`]"#).search(inner).is_none()
                {
                    segs.push("<literal>".into()); // arithmetic gives a number
                } else {
                    segs.push("(...)".into());
                }
                break;
            }
            let name = &code[s..j];
            let before = &c[s.saturating_sub(4)..s];
            if before.ends_with(b"new ") || before.ends_with(b"new\t") {
                segs.push(format!("new {name}{arity}()"));
                break;
            }
            segs.push(format!("{name}{}()", if o.call_targs { arity.as_str() } else { "" }));
            i = s;
        } else if ch == b']' {
            let k = match_back(c, i, b']', b'[');
            if k < 0 {
                return Vec::new();
            }
            let j = skip_back_ws(c, k as usize);
            if j > 0 && (ident(c[j - 1]) || c[j - 1] == b')' || c[j - 1] == b']') {
                segs.push("[]".into());
                if ident(c[j - 1]) {
                    let s = ident_back(c, j);
                    segs.push(code[s..j].to_string());
                    i = s;
                } else {
                    segs.push("(...)".into());
                    break;
                }
            } else {
                segs.push("<literal>".into()); // `[1, 2].map(...)`: an array literal
                break;
            }
        } else if ch == b'}' && !anonymous_class(code, i).is_empty() {
            segs.push(anonymous_class(code, i)); // `new TypeToken<List<X>>() {}.getType()`: a TypeToken
            break;
        } else if ch == b'"' || ch == b'\'' || ch == b'`' {
            segs.push("<literal>".into()); // a string's method
            break;
        } else if ident(ch) {
            let s = ident_back(c, i);
            let name = &code[s..i];
            if name.is_empty() || name.as_bytes()[0].is_ascii_digit() {
                return Vec::new();
            }
            if ["await", "return", "new", "throw", "yield", "in", "of", "case", "else", "do"].contains(&name) {
                break;
            }
            if s > 0
                && c[s - 1] == b'/'
                && rx(14, r"[dgimsuyv]+").fullmatch(name).is_some()
                && rx(15, r"(?:^|[=(,:!&|?{};\s])/(?:[^/\\\n]|\\.)+/$").search(win(code, s, 300)).is_some()
            {
                segs.push("<literal>".into()); // `/refused/i.test(m)`: a regex literal with its flags
                break;
            }
            segs.push(if closure_call { format!("{name}()") } else { name.to_string() });
            i = s;
        } else if ch == b'>' && !(i >= 2 && &c[i - 2..i] == b"->") {
            // `Vec::<T>::new`, `Foo<T>::bar`, `Policy<T>.Handle()`
            let g = match_back(c, i, b'>', b'<');
            if g < 0 {
                return Vec::new();
            }
            let mut j = skip_back_ws(c, g as usize);
            if j >= 2 && &c[j - 2..j] == b"::" {
                j = skip_back_ws(c, j - 2); // Rust's turbofish
            }
            let s = ident_back(c, j);
            if s == j || c[s].is_ascii_digit() {
                return Vec::new(); // `a > b.c()`: a comparison
            }
            segs.push(code[s..j].to_string());
            i = s;
        } else {
            segs.push("(...)".into());
            break;
        }
    }
    segs.reverse();
    bare_first(segs, o)
}

/// The index of the bracket closing the one at `c[i]`; -1 when none.
fn close(c: &[u8], i: usize) -> isize {
    let mut depth = 0i64;
    for (k, &ch) in c.iter().enumerate().skip(i) {
        if matches!(ch, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(ch, b')' | b']' | b'}') {
            depth -= 1;
            if depth == 0 {
                return k as isize;
            }
        }
    }
    -1
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xff => 4,
        _ => 1,
    }
}

/// The chain an expression starting at `pos` builds (the right side of
/// `x = <expr>`; chains.expression): `repo.find(id).first()` ->
/// ["repo", "find()", "first()"]; `new Foo(a)` -> ["new Foo()"]; empty when
/// the expression is not a plain chain.
pub fn expression(code: &str, pos: usize, max_segs: usize, o: Opts) -> Vec<String> {
    let c = code.as_bytes();
    let n = c.len();
    let mut i = pos;
    while i < n && ws(c[i]) {
        i += 1;
    }
    for kw in ["await ", "try ", "try! ", "try? ", "yield "] {
        if code[i..].starts_with(kw) {
            i += kw.len();
        }
    }
    // `app = module.exports = express()`: the value is the last one assigned
    while let Some(am) = rx(16, r"[ \t]*[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*[ \t]*=(?![=>])[ \t]*").match_at(code, i) {
        i = am.end();
    }
    while i < n && (c[i] == b' ' || c[i] == b'\t') {
        i += 1;
    }
    if code[i..].starts_with("Object.create(") {
        return expression(code, i + 14, max_segs, o); // an object whose prototype is the argument
    }
    let mut segs: Vec<String> = Vec::new();
    let mut new = false;
    // `if let x = f() {`: that brace opens the block, not a trailing closure
    let line_start = code[..pos].rfind('\n').map(|k| k + 1).unwrap_or(0);
    let cond =
        o.trailing_closures && rx(17, r"\b(?:if|guard|while)\b|^\s*,").search(&code[line_start..pos]).is_some();
    if code[i..].starts_with("new ") {
        new = true;
        i += 4;
    }
    while segs.len() < max_segs {
        while i < n && (c[i] == b' ' || c[i] == b'\t') {
            i += 1;
        }
        let s = i;
        while i < n && ident(c[i]) {
            i += 1;
        }
        if i == s || c[s].is_ascii_digit() {
            return Vec::new();
        }
        let name = code[s..i].to_string();
        let mut j = i;
        while j < n && (c[j] == b' ' || c[j] == b'\t') {
            j += 1;
        }
        if segs.is_empty() && j + 1 < n && c[j] == b'!' && b"([{".contains(&c[j + 1]) {
            // Rust `command!()...`: a macro's value starts the chain
            let (mut depth, mut k) = (0i64, j + 1);
            while k < n {
                if b"([{".contains(&c[k]) {
                    depth += 1;
                } else if b")]}".contains(&c[k]) {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                k += 1;
            }
            segs.push(format!("{name}!()"));
            i = k + 1;
            while i < n && matches!(c[i], b' ' | b'\t' | b'\n') {
                i += 1;
            }
            if i < n && c[i] == b'.' && c.get(i + 1) != Some(&b'.') {
                i += 1;
                continue;
            }
            return segs;
        }
        let mut arity = String::new();
        if j < n && c[j] == b'<' {
            // generic arguments: `make<T>(` / `List<Foo>()` (within 200 characters)
            let (mut depth, mut k, mut seen) = (0i64, j, 0usize);
            while k < n && seen < 200 {
                if c[k] == b'<' {
                    depth += 1;
                } else if c[k] == b'>' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                } else if b";{}\n".contains(&c[k]) {
                    break;
                }
                k += utf8_len(c[k]);
                seen += 1;
            }
            if depth == 0 && k < n && c[k] == b'>' {
                arity = format!("`{}", 1 + top_commas(&code[j + 1..k]));
                j = k + 1;
            }
        }
        let upper = name.as_bytes()[0].is_ascii_uppercase();
        if j < n && (c[j] == b'(' || c[j] == b'{') && !(c[j] == b'{' && !new && !upper) {
            let (mut depth, mut k) = (0i64, j);
            while k < n {
                if b"([{".contains(&c[k]) {
                    depth += 1;
                } else if b")]}".contains(&c[k]) {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                k += 1;
            }
            if c[j] == b'{' {
                segs.push(format!("new {name}()")); // a struct / object literal builds one
            } else if new {
                segs.push(format!("new {name}{arity}()"));
            } else {
                segs.push(format!("{name}{}()", if o.call_targs { arity.as_str() } else { "" }));
            }
            new = false;
            i = k + 1;
        } else {
            if new && j < n && c[j] == b'.' && segs.is_empty() {
                segs.push(name); // `new express.Router(...)`: the path, then the constructor
                i = j + 1;
                continue;
            }
            if new && !(j < n && c[j] == b'.') {
                return Vec::new();
            }
            segs.push(name);
            i = j;
        }
        // what follows: another link, an unwrap, an index, or the end
        loop {
            while i < n && (c[i] == b' ' || c[i] == b'\t') {
                i += 1;
            }
            if i < n && c[i] == b'[' && !segs.is_empty() && segs.last().map(|s| s != "[]").unwrap_or(false) {
                let (mut depth, mut k) = (0i64, i);
                while k < n {
                    if b"([{".contains(&c[k]) {
                        depth += 1;
                    } else if b")]}".contains(&c[k]) {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    } else if (c[k] == b';' || c[k] == b'\n') && depth <= 1 {
                        k = n;
                        break;
                    }
                    k += 1;
                }
                if k >= n {
                    return Vec::new();
                }
                segs.push("[]".into());
                i = k + 1;
                continue;
            }
            if o.trailing_closures
                && i < n
                && c[i] == b'{'
                && segs.last().map(|l| !l.starts_with("new ") && l.as_bytes()[0].is_ascii_lowercase()).unwrap_or(false)
                && !cond
            {
                // `x.response { _ in ... }`: the closure is an argument
                let k = close(c, i);
                if k < 0 {
                    return Vec::new();
                }
                let l = segs.last_mut().unwrap();
                if !l.ends_with("()") {
                    l.push_str("()");
                }
                i = k as usize + 1;
                continue;
            }
            if code[i.min(n)..].starts_with("!!") || (i < n && c[i] == b'!' && (i + 1 >= n || c[i + 1] == b'.')) {
                i += if code[i..].starts_with("!!") { 2 } else { 1 };
                segs.push("!!".into());
            } else if i < n && c[i] == b'?' && (i + 1 >= n || matches!(c[i + 1], b'.' | b';' | b'\n' | b')' | b',')) {
                segs.push("?".into());
                i += 1;
            } else {
                break;
            }
        }
        let mut nl = i;
        while nl < n && ws(c[nl]) {
            nl += 1;
        }
        let rest = &code[nl.min(n)..];
        let next = ["?.", "->", "::", "."].into_iter().find(|o_| rest.starts_with(o_) && !rest.starts_with(".."));
        match next {
            Some(o_) => i = nl + o_.len(),
            None => {
                let end = c.get(i).copied();
                let after = code[i.min(n)..].trim_start_matches([' ', '\t']);
                if cond && (end == Some(b'{') || after.starts_with("else")) {
                    return bare_first(segs, o); // `if let x = f() {`, `guard let x = f() else {`
                }
                if let Some(e) = end {
                    if !b";,)\n\r}]".contains(&e)
                        && !["//", "#", "as ", "?:", "??", "||", "or "].iter().any(|p| after.starts_with(p))
                    {
                        return Vec::new(); // more expression follows: `a.b() + 1`, a ternary...
                    }
                }
                return bare_first(segs, o);
            }
        }
    }
    bare_first(segs, o)
}

/// The chain, with a leading bare call's type-argument count dropped.
fn bare_first(mut segs: Vec<String>, o: Opts) -> Vec<String> {
    if o.call_targs && !segs.is_empty() {
        if let Some(m) = rx(18, r"^([\w$]+)`\d+\(\)$").match_start(&segs[0]) {
            segs[0] = format!("{}()", m.s(1));
        }
    }
    segs
}

fn set(k: usize, items: &'static [&'static str]) -> &'static HashSet<&'static str> {
    static CELLS: [OnceLock<HashSet<&'static str>>; 4] = [const { OnceLock::new() }; 4];
    CELLS[k].get_or_init(|| items.iter().copied().collect())
}

const WRAPPERS: &[&str] = &[
    "Promise", "Task", "ValueTask", "Future", "Box", "Rc", "Arc", "Option", "Optional", "Result", "Deferred", "Lazy",
    "LazyLock", "OnceCell", "OnceLock", "Ref", "RefCell", "Cell", "Mutex", "RwLock", "Pin", "Cow", "Awaitable",
    "Coroutine", "Observable", "Single", "Maybe", "NotRequired", "Required", "Final", "ClassVar", "Annotated",
    "ReadOnly", "Readonly", "WeakReference", "Nullable", "QPointer", "QSharedPointer", "QScopedPointer",
    "QWeakPointer", "unique_ptr", "shared_ptr", "weak_ptr", "optional", "Ptr", "Partial", "NonNullable", "Awaited",
    "DeepReadonly", "Mutable", "ContextManager", "AbstractContextManager", "AsyncContextManager",
    "AbstractAsyncContextManager",
];

const SEQUENCES: &[&str] = &[
    "Vec", "VecDeque", "HashSet", "BTreeSet", "LinkedList", "BinaryHeap", "List", "ArrayList", "Set", "TreeSet",
    "SortedSet", "Array", "ReadonlyArray", "Iterable", "Iterator", "IEnumerable", "ICollection", "IList",
    "IReadOnlyList", "IReadOnlyCollection", "ISet", "Sequence", "MutableList", "MutableSet", "Collection", "Stream",
    "QList", "QVector", "QSet", "QStringList", "vector", "list", "deque", "set", "span", "Span", "ReadOnlySpan",
    "Slice", "IntoIter", "Iter", "ArraySlice", "ContiguousArray", "Deque",
];

const TS_COMPUTED: &[&str] = &[
    "ReturnType", "Parameters", "ConstructorParameters", "Pick", "Omit", "Exclude", "Extract", "ThisParameterType",
    "OmitThisParameter", "ValueOf",
];

const NOT_A_CLASS: &[&str] = &[
    "void", "Unit", "None", "Nothing", "never", "any", "unknown", "object", "Any", "AnyObject", "auto", "var",
    "dynamic", "Self", "self", "this", "static", "mixed", "int", "bool", "string", "float", "double", "char", "long",
    "short", "byte", "usize", "isize", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64", "str",
    "number", "boolean", "symbol", "bigint", "error", "rune", "uint",
];

/// Cut at the first top-level comma (brackets `<([{` nest).
fn first_arg(s: &str) -> &str {
    let mut depth = 0i64;
    for (k, ch) in s.char_indices() {
        depth += "<([{".contains(ch) as i64;
        depth -= ">)]}".contains(ch) as i64;
        if ch == ',' && depth == 0 {
            return &s[..k];
        }
    }
    s
}

/// What iterating a declared type gives (chains.elem_type): `Vec<Match>`,
/// `&[Match]`, `List<Foo>`, `Foo[]`, `[Foo]`, `impl Iterator<Item = &Foo>`
/// -> the element's class; "" when it is no sequence.
pub fn elem_type(text: &str) -> String {
    let mut t = text.trim().to_string();
    for _ in 0..4 {
        t = rx(19, r"^(?:&\s*(?:'\w+\s+)?(?:mut\s+)?|impl\s+|dyn\s+|const\s+|readonly\s+)").sub("", &t).trim().to_string();
        let Some(m) = rx(20, r"(?s)^(?:Box|Rc|Arc|Option|Optional)\s*<(.*)>\s*[?]?$").match_start(&t) else { break };
        t = m.s(1).trim().to_string();
    }
    let t = t.trim_end_matches(['?', '!']).trim();
    if let Some(m) = rx(21, r"^(?:\[\d*\]|map\[[^\]]+\])\s*\*?(.+)$").match_start(t) {
        if !t.starts_with("[]]") {
            return type_name(m.s(1)); // Go `[]*Command`, `map[string]*Flag` (its values)
        }
    }
    if let Some(m) = rx(22, r"^\[([^\[\]:;]+)(?:;[^\]]*)?\]$").match_start(t) {
        return type_name(m.s(1)); // Rust `[Match]` / `[u8; 4]`, Swift `[Foo]`
    }
    if let Some(m) = rx(23, r"^(.+?)\s*\[\s*\]$").match_start(t) {
        return type_name(m.s(1)); // `Foo[]`
    }
    if let Some(m) = rx(24, r"(?s)^(?:[\w.:]+::|[\w.]+\.)?(\w+)\s*<(.*)>$").match_start(t) {
        if let Some(it) = rx(25, r"(?s)^\s*Item\s*=\s*(.+)$").match_start(m.s(2)) {
            return type_name(it.s(1).trim_start_matches('&').trim()); // `Iterator<Item = &Foo>`
        }
        if set(1, SEQUENCES).contains(m.s(1)) {
            return type_name(first_arg(m.s(2)).trim_start_matches('&').trim());
        }
    }
    String::new()
}

/// type_name plus the generic arity when the type itself is generic
/// (`Builder<TResult>` -> "Builder`1"; chains.generic_name).
pub fn generic_name(text: &str) -> String {
    let t = type_name(text);
    let stripped = text.trim();
    let Some(m) = rx(26, r"(?s)^\s*(?:const\s+|readonly\s+)?([\w.:]+)\s*<(.*)>\s*[?*&]*\s*$").match_start(stripped) else {
        return t;
    };
    let last = |s: &str| s.replace("::", ".").rsplit('.').next().unwrap_or("").to_string();
    if t.is_empty() || last(m.s(1)) != last(&t) {
        return t;
    }
    let (mut depth, mut n) = (0i64, 1);
    for ch in m.s(2).chars() {
        depth += "<([{".contains(ch) as i64;
        depth -= ">)]}".contains(ch) as i64;
        n += (ch == ',' && depth == 0) as usize;
    }
    format!("{t}`{n}")
}

/// The class a declared type names (chains.type_name): `Promise<Foo>` ->
/// Foo, `Foo?` -> Foo, `&mut Foo<'a>` -> Foo, `Optional[Foo]` -> Foo,
/// `Foo | None` -> Foo, `pkg.Foo` -> pkg.Foo, `List<Foo>` -> List. "" when
/// it isn't one class.
pub fn type_name(text: &str) -> String {
    let mut t = text.trim().trim_matches(['"', '\'']).to_string();
    for _ in 0..6 {
        loop {
            let prev = t.clone();
            t = rx(
                27,
                r"^(?:const|mut|readonly|final|static|volatile|unowned|weak|inout|ref|out|in|dyn|impl|some|any)\s+",
            )
            .sub("", &t);
            t = t.trim_start_matches(['&', '*', '^']).trim().to_string();
            t = rx(28, r"^'\w+\s+").sub("", &t); // a Rust lifetime
            if prev == t {
                break;
            }
        }
        let inter: Vec<String> = rx(29, r"\s&\s(?![^<\[(]*[>\])])")
            .split(&t)
            .iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if inter.len() > 1 {
            t = inter[0].clone(); // TS `Response & TypedResponse<T>`: a Response
        }
        let parts: Vec<String> = rx(30, r"\|(?![^<\[(]*[>\])])")
            .split(&t)
            .iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty() && !["None", "null", "undefined", "nil", "void"].contains(&p.as_str()))
            .collect();
        if parts.len() > 1 {
            let each: HashSet<String> = parts.iter().map(|p| type_name(p)).collect();
            return if each.len() == 1 { each.into_iter().next().unwrap() } else { String::new() };
        }
        if parts.len() != 1 {
            return String::new();
        }
        t = parts[0].clone();
        if rx(31, r"(?s)^\[.*\][?!]?$").match_start(&t).is_some() {
            return "Array".into(); // Swift's `[Element]` / `[Key: Value]`
        }
        if rx(32, r"\[\s*\]\s*[?!]?$").search(&t).is_some() {
            return "Array".into(); // `Foo[]`: an array, not a Foo
        }
        if let Some(m) = rx(33, r"(?s)^([\w.:$\\]+)\s*[<\[](.*)[>\]]\s*[?!*&]*$").match_start(&t) {
            let g1 = m.s(1).to_string();
            let g2 = m.s(2).to_string();
            let head = g1.rsplit("::").next().unwrap_or("").rsplit('.').next().unwrap_or("").to_string();
            if set(2, TS_COMPUTED).contains(head.as_str()) {
                return String::new(); // a type computed from another, no class
            }
            if head == "InstanceType" {
                t = rx(34, r"^\s*typeof\s+").sub("", &g2).trim().to_string();
                continue;
            }
            if set(0, WRAPPERS).contains(head.as_str()) {
                let mut depth = 0i64;
                let mut cut = g2.len();
                for (k, ch) in g2.char_indices() {
                    if "<[(".contains(ch) {
                        depth += 1;
                    } else if ">])".contains(ch) {
                        depth -= 1;
                    } else if ch == ',' && depth == 0 {
                        cut = k;
                        break;
                    }
                }
                t = g2[..cut].to_string();
                continue;
            }
            t = g1;
        }
        break;
    }
    let t = t.trim_end_matches(['?', '!', '*', '&']).trim();
    let t = t.replace("::", ".").replace('\\', ".");
    let t = t.trim_start_matches('.');
    if rx(35, r"^[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*$").match_start(t).is_none() {
        return String::new();
    }
    if set(3, NOT_A_CLASS).contains(t) {
        return if ["Self", "self", "this", "static"].contains(&t) { "Self".into() } else { String::new() };
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    // chains.py's docstrings, as cases
    #[test]
    fn receivers_and_expressions() {
        let o = Opts::default();
        let code = "self.store.get().save(x)";
        assert_eq!(receiver(code, code.find("save").unwrap(), 40, o), v(&["self", "store", "get()"]));
        assert_eq!(receiver("save(x)", 0, 40, o), Vec::<String>::new());
        assert_eq!(expression("repo.find(id).first()", 0, 40, o), v(&["repo", "find()", "first()"]));
        assert_eq!(expression("new Foo(a)", 0, 40, o), v(&["new Foo()"]));
        assert_eq!(expression("await svc.load()", 0, 40, o), v(&["svc", "load()"]));
        assert_eq!(expression("a.b() + 1", 0, 40, o), Vec::<String>::new());
        let cs = Opts { call_targs: true, trailing_closures: false };
        let code = "x.Handle<E>().Run()";
        assert_eq!(receiver(code, code.find("Run").unwrap(), 40, cs), v(&["x", "Handle`1()"]));
    }

    #[test]
    fn type_names() {
        for (t, want) in [
            ("Promise<Foo>", "Foo"),
            ("Foo?", "Foo"),
            ("&mut Foo<'a>", "Foo"),
            ("Optional[Foo]", "Foo"),
            ("Foo | None", "Foo"),
            ("pkg.Foo", "pkg.Foo"),
            ("List<Foo>", "List"),
            ("Foo[]", "Array"),
            ("int", ""),
            ("Self", "Self"),
        ] {
            assert_eq!(type_name(t), want, "{t}");
        }
        for (t, want) in [("Vec<Match>", "Match"), ("&[Match]", "Match"), ("Foo[]", "Foo"), ("impl Iterator<Item = &Foo>", "Foo")] {
            assert_eq!(elem_type(t), want, "{t}");
        }
        assert_eq!(generic_name("Builder<TResult>"), "Builder`1");
        assert_eq!(generic_name("Map<K, V>"), "Map`2");
    }
}
