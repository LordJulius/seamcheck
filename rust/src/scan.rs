//! Blank comments and string contents out of source text, keeping every
//! newline and byte in place, so regexes only ever match real code.
//! (Python's scan.py keeps characters in place; a blanked character here
//! becomes as many spaces as its UTF-8 bytes, so a byte offset in the code
//! is the same byte offset in the raw text.)
//! Also returns the string literals (line, text).

fn is_alnum(c: char) -> bool {
    c.is_alphanumeric()
}

struct Out {
    wiped: Vec<bool>,
}

impl Out {
    fn wipe(&mut self, t: &[char], a: usize, b: usize) {
        let b = b.min(t.len());
        for k in a..b {
            if t[k] != '\n' {
                self.wiped[k] = true;
            }
        }
    }
    fn render(&self, t: &[char]) -> String {
        let mut s = String::with_capacity(t.len());
        for (k, &c) in t.iter().enumerate() {
            if self.wiped[k] {
                for _ in 0..c.len_utf8() {
                    s.push(' ');
                }
            } else {
                s.push(c);
            }
        }
        s
    }
}

fn find(t: &[char], pat: &str, from: usize) -> Option<usize> {
    let p: Vec<char> = pat.chars().collect();
    if p.is_empty() {
        return Some(from);
    }
    if t.len() < p.len() {
        return None;
    }
    let mut i = from;
    while i + p.len() <= t.len() {
        if t[i..i + p.len()] == p[..] {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn starts(t: &[char], i: usize, pat: &str) -> bool {
    let mut k = i;
    for c in pat.chars() {
        if k >= t.len() || t[k] != c {
            return false;
        }
        k += 1;
    }
    true
}

#[cfg(test)]
fn count_nl(t: &[char], a: usize, b: usize) -> usize {
    t[a.min(t.len())..b.min(t.len())].iter().filter(|&&c| c == '\n').count()
}

fn slice(t: &[char], a: usize, b: usize) -> String {
    let a = a.min(t.len());
    let b = b.min(t.len()).max(a);
    t[a..b].iter().collect()
}

fn sigil_close(op: char) -> Option<char> {
    Some(match op {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        '/' => '/',
        '|' => '|',
        '"' => '"',
        '\'' => '\'',
        _ => return None,
    })
}

/// The character-indexed transcription of scan.py's `blank` (the byte
/// scanner below must agree with it; see the tests).
#[cfg(test)]
fn blank_chars(text: &str, lang: &str, keep_strings: bool) -> (String, Vec<(usize, String)>) {
    let t: Vec<char> = text.chars().collect();
    let n = t.len();
    let mut o = Out { wiped: vec![false; n] };
    let mut strings: Vec<(usize, String)> = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let cfam = matches!(lang, "c" | "rust" | "kotlin" | "js" | "swift" | "php");
    let at = |k: usize| -> Option<char> { t.get(k).copied() };
    while i < n {
        let c = t[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if cfam && c == '/' && i + 1 < n && (t[i + 1] == '/' || t[i + 1] == '*') {
            let j = if t[i + 1] == '/' {
                find(&t, "\n", i).unwrap_or(n)
            } else {
                find(&t, "*/", i + 2).map(|j| j + 2).unwrap_or(n)
            };
            o.wipe(&t, i, j);
            line += count_nl(&t, i, j);
            i = j;
            continue;
        }
        if lang == "rust"
            && c == 'r'
            && matches!(at(i + 1), Some('"') | Some('#'))
            && (i == 0 || !(is_alnum(t[i - 1]) || t[i - 1] == '_'))
        {
            let mut k = i + 1;
            while k < n && t[k] == '#' {
                k += 1;
            }
            if k < n && t[k] == '"' {
                let close: String = std::iter::once('"').chain(std::iter::repeat('#').take(k - i - 1)).collect();
                let j = find(&t, &close, k + 1).unwrap_or(n);
                strings.push((line, slice(&t, k + 1, j)));
                if !keep_strings {
                    o.wipe(&t, k + 1, j);
                }
                line += count_nl(&t, i, j);
                i = n.min(j + close.chars().count());
                continue;
            }
        }
        if lang == "rust" && c == '\'' && !(i + 2 < n && t[i + 2] == '\'') && !(i + 3 < n && t[i + 1] == '\\') {
            i += 1; // a lifetime ('a), not a char literal
            continue;
        }
        if lang == "php" && ((c == '#' && at(i + 1) != Some('[')) || (c == '?' && at(i + 1) == Some('>'))) {
            if c == '?' {
                let j = find(&t, "<?php", i).map(|j| j + 5).unwrap_or(n);
                o.wipe(&t, i, j);
                line += count_nl(&t, i, j);
                i = j;
                continue;
            }
            let j = find(&t, "\n", i).unwrap_or(n);
            o.wipe(&t, i, j);
            i = j;
            continue;
        }
        if lang == "php" && starts(&t, i, "<<<") {
            if let Some((ident, body_start)) = php_heredoc(&t, i) {
                // terminator: a line that starts with optional spaces then ident\b
                let mut j = n;
                let mut end_after = None;
                let mut k = body_start;
                while k <= n {
                    // line starts at k
                    let mut p = k;
                    while p < n && (t[p] == ' ' || t[p] == '\t') {
                        p += 1;
                    }
                    if starts(&t, p, &ident) {
                        let after = p + ident.chars().count();
                        if after >= n || !(is_alnum(t[after]) || t[after] == '_') {
                            j = k;
                            end_after = Some(after);
                            break;
                        }
                    }
                    match find(&t, "\n", k) {
                        Some(e) => k = e + 1,
                        None => break,
                    }
                }
                strings.push((line, slice(&t, body_start, j)));
                if !keep_strings {
                    o.wipe(&t, body_start, j);
                }
                line += count_nl(&t, i, j);
                i = end_after.unwrap_or(j);
                continue;
            }
        }
        if c == '#' && !cfam {
            let j = find(&t, "\n", i).unwrap_or(n);
            o.wipe(&t, i, j);
            i = j;
            continue;
        }
        if lang == "elixir" && c == '?' && i + 1 < n && !(if i > 0 { is_alnum(t[i - 1]) } else { false }) {
            i += 2; // ?# ?" char literals
            continue;
        }
        if lang == "elixir" && c == '~' && i + 2 < n && t[i + 1].is_alphabetic() && sigil_close(t[i + 2]).is_some() {
            let op = t[i + 2];
            let cl = sigil_close(op).unwrap();
            let three = slice(&t, i + 2, i + 5);
            let end;
            if three == "\"\"\"" || three == "'''" {
                end = find(&t, &three, i + 5).map(|e| e + 3).unwrap_or(n);
            } else {
                let mut j = i + 3;
                let mut depth = 1;
                while j < n {
                    if t[j] == '\\' {
                        j += 2;
                        continue;
                    }
                    if t[j] == cl && (op == cl || depth == 1) {
                        break;
                    }
                    if op != cl && t[j] == op {
                        depth += 1;
                    } else if op != cl && t[j] == cl {
                        depth -= 1;
                    }
                    j += 1;
                }
                end = n.min(j + 1);
            }
            let body_end = end.saturating_sub(1).max(i + 3);
            strings.push((line, slice(&t, i + 3, end.saturating_sub(1))));
            if !keep_strings {
                o.wipe(&t, i + 3, body_end);
            }
            line += count_nl(&t, i, end);
            i = end;
            continue;
        }
        if lang == "c" && c == '\'' && i > 0 && i < n - 1 && is_alnum(t[i - 1]) && is_alnum(t[i + 1]) {
            i += 1; // a C++14 digit separator (25'000)
            continue;
        }
        if lang == "js" && c == '/' && regex_start(&t, i) {
            let mut j = i + 1;
            let mut in_class = false;
            while j < n && t[j] != '\n' {
                if t[j] == '\\' {
                    j += 2;
                    continue;
                }
                if t[j] == '[' {
                    in_class = true;
                } else if t[j] == ']' {
                    in_class = false;
                } else if t[j] == '/' && !in_class {
                    break;
                }
                j += 1;
            }
            if j < n && t[j] == '/' {
                if !keep_strings {
                    o.wipe(&t, i + 1, j);
                }
                i = j + 1;
                continue;
            }
        }
        if lang == "c"
            && ((c == '"'
                && i > 0
                && (t[i - 1] == '@' || (i >= 2 && (slice(&t, i - 2, i) == "@$" || slice(&t, i - 2, i) == "$@"))))
                || c == '`')
        {
            let mut j = i + 1;
            while j < n {
                if t[j] == c {
                    if c == '"' && at(j + 1) == Some('"') {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            strings.push((line, slice(&t, i + 1, j)));
            if !keep_strings {
                o.wipe(&t, i + 1, j);
            }
            line += count_nl(&t, i, j);
            i = n.min(j + 1);
            continue;
        }
        if c == '"' || c == '\'' || (lang == "js" && c == '`') {
            let triple = i + 3 <= n && t[i] == c && t[i + 1] == c && t[i + 2] == c;
            let qlen = if triple { 3 } else { 1 };
            let mut j = i + qlen;
            while j < n {
                if t[j] == '\\' {
                    j += 2;
                    continue;
                }
                if (triple && j + 3 <= n && t[j] == c && t[j + 1] == c && t[j + 2] == c) || (!triple && t[j] == c) {
                    break;
                }
                if !triple
                    && t[j] == '\n'
                    && matches!(lang, "gdscript" | "python" | "js" | "kotlin" | "swift")
                    && c != '`'
                {
                    break; // unterminated: stop at end of line
                }
                j += 1;
            }
            let end = n.min(j + qlen);
            strings.push((line, slice(&t, i + qlen, j)));
            if !keep_strings {
                o.wipe(&t, i + qlen, j);
            }
            line += count_nl(&t, i, end);
            i = end;
            continue;
        }
        i += 1;
    }
    (o.render(&t), strings)
}

/// `<<<"ID"\n` at i: (ident, index after the opener's newline).
#[cfg(test)]
fn php_heredoc(t: &[char], i: usize) -> Option<(String, usize)> {
    let n = t.len();
    let mut k = i + 3;
    while k < n && (t[k] == ' ' || t[k] == '\t') {
        k += 1;
    }
    if k < n && (t[k] == '"' || t[k] == '\'') {
        k += 1;
    }
    let s = k;
    if k >= n || !(t[k].is_alphabetic() && t[k].is_ascii() || t[k] == '_') {
        return None;
    }
    while k < n && (is_alnum(t[k]) || t[k] == '_') {
        k += 1;
    }
    let ident = slice(t, s, k);
    if k < n && (t[k] == '"' || t[k] == '\'') {
        k += 1;
    }
    while k < n && (t[k] == ' ' || t[k] == '\t') {
        k += 1;
    }
    if k < n && t[k] == '\n' {
        return Some((ident, k + 1));
    }
    None
}

const REGEX_AFTER: &str = "(,=:[!&|?{};+-*%<>~^";
const REGEX_WORDS: [&str; 13] =
    ["return", "typeof", "case", "in", "of", "delete", "void", "throw", "new", "yield", "await", "else", "do"];

#[cfg(test)]
fn regex_start(t: &[char], i: usize) -> bool {
    match t.get(i + 1) {
        None | Some('/') | Some('*') => return false,
        _ => {}
    }
    let mut k = i as isize - 1;
    while k >= 0 && matches!(t[k as usize], ' ' | '\t' | '\r' | '\n') {
        k -= 1;
    }
    if k < 0 || REGEX_AFTER.contains(t[k as usize]) {
        return true;
    }
    let mut w = k;
    while w >= 0 && (is_alnum(t[w as usize]) || t[w as usize] == '_' || t[w as usize] == '$') {
        w -= 1;
    }
    let word = slice(t, (w + 1) as usize, (k + 1) as usize);
    REGEX_WORDS.contains(&word.as_str())
}

// --- the byte scanner -----------------------------------------------------------
// The same walk as scan.py, over UTF-8 bytes: every delimiter is ASCII, so only
// the steps that move by "one character" or ask whether a character is a letter
// decode (`ch`, `nx`, `prev`); a blanked character becomes one space per byte.

#[inline]
fn ch(b: &[u8], i: usize) -> Option<char> {
    if i >= b.len() {
        return None;
    }
    let c = b[i];
    if c < 0x80 {
        return Some(c as char);
    }
    let len = if c >= 0xf0 { 4 } else if c >= 0xe0 { 3 } else { 2 };
    std::str::from_utf8(&b[i..(i + len).min(b.len())]).ok().and_then(|s| s.chars().next())
}

/// The index of the character after the one at i.
#[inline]
fn nx(b: &[u8], i: usize) -> usize {
    if i >= b.len() {
        return i + 1;
    }
    let c = b[i];
    i + if c < 0x80 {
        1
    } else if c >= 0xf0 {
        4
    } else if c >= 0xe0 {
        3
    } else if c >= 0xc0 {
        2
    } else {
        1
    }
}

/// The character before byte index i.
#[inline]
fn prev(b: &[u8], i: usize) -> Option<(usize, char)> {
    if i == 0 {
        return None;
    }
    let mut k = i - 1;
    while k > 0 && (b[k] & 0xc0) == 0x80 {
        k -= 1;
    }
    ch(b, k).map(|c| (k, c))
}

#[inline]
fn bfind(b: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    if from >= b.len() {
        return None;
    }
    if pat.len() == 1 {
        return b[from..].iter().position(|&x| x == pat[0]).map(|p| p + from);
    }
    b[from..].windows(pat.len()).position(|w| w == pat).map(|p| p + from)
}

#[inline]
fn nl(b: &[u8], a: usize, z: usize) -> usize {
    let z = z.min(b.len());
    if a >= z {
        return 0;
    }
    b[a..z].iter().filter(|&&x| x == b'\n').count()
}

fn bstr(b: &[u8], a: usize, z: usize) -> String {
    let z = z.min(b.len());
    if a >= z {
        return String::new();
    }
    String::from_utf8_lossy(&b[a..z]).into_owned()
}

fn wipe(out: &mut [u8], a: usize, z: usize) {
    let z = z.min(out.len());
    for x in out.iter_mut().take(z).skip(a) {
        if *x != b'\n' {
            *x = b' ';
        }
    }
}

/// (blanked text, string literals). `lang`: a scan family (python, elixir,
/// gdscript, ruby, js, kotlin, rust, c, swift, php).
pub fn blank(text: &str, lang: &str, keep_strings: bool) -> (String, Vec<(usize, String)>) {
    if lang == "ruby" {
        return blank_ruby(text, keep_strings);
    }
    blank_bytes(text, lang, keep_strings, true)
}

/// Only the blanked text (what rules read): no string list to build.
pub fn blank_text(text: &str, lang: &str, keep_strings: bool) -> String {
    if lang == "ruby" {
        return blank_ruby(text, keep_strings).0;
    }
    blank_bytes(text, lang, keep_strings, false).0
}

fn blank_bytes(text: &str, lang: &str, keep_strings: bool, collect: bool) -> (String, Vec<(usize, String)>) {
    let t = text.as_bytes();
    let n = t.len();
    let mut out = t.to_vec();
    let mut strings: Vec<(usize, String)> = Vec::new();
    let (mut i, mut line) = (0usize, 1usize);
    let cfam = matches!(lang, "c" | "rust" | "kotlin" | "js" | "swift" | "php");
    let (rust, php, elixir, c_lang, js) = (lang == "rust", lang == "php", lang == "elixir", lang == "c", lang == "js");
    let at = |k: usize| -> u8 { if k < n { t[k] } else { 0 } };
    let alnum_before = |k: usize| prev(t, k).map(|(_, c)| c.is_alphanumeric()).unwrap_or(false);
    let mut starts = [false; 256];
    for x in b"\n/#\"'`r?~<" {
        starts[*x as usize] = true;
    }
    while i < n {
        if !starts[t[i] as usize] {
            // (a byte that starts nothing: plain code, or part of a multi-byte character)
            i += 1;
            continue;
        }
        let c = t[i];
        if c == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c >= 0x80 {
            i = nx(t, i);
            continue;
        }
        if cfam && c == b'/' && (at(i + 1) == b'/' || at(i + 1) == b'*') {
            let j = if at(i + 1) == b'/' {
                bfind(t, b"\n", i).unwrap_or(n)
            } else {
                bfind(t, b"*/", i + 2).map(|j| j + 2).unwrap_or(n)
            };
            wipe(&mut out, i, j);
            if collect {
                line += nl(t, i, j);
            }
            i = j;
            continue;
        }
        if rust
            && c == b'r'
            && (at(i + 1) == b'"' || at(i + 1) == b'#')
            && !prev(t, i).map(|(_, p)| p.is_alphanumeric() || p == '_').unwrap_or(false)
        {
            let mut k = i + 1;
            while k < n && t[k] == b'#' {
                k += 1;
            }
            if k < n && t[k] == b'"' {
                let mut close = vec![b'"'];
                close.extend(std::iter::repeat_n(b'#', k - i - 1));
                let j = bfind(t, &close, k + 1).unwrap_or(n);
                if collect {
                    strings.push((line, bstr(t, k + 1, j)));
                }
                if !keep_strings {
                    wipe(&mut out, k + 1, j);
                }
                if collect {
                    line += nl(t, i, j);
                }
                i = n.min(j + close.len());
                continue;
            }
        }
        if rust && c == b'\'' {
            // a lifetime ('a), not a char literal: Python looks 2 and 3 characters on
            let c1 = nx(t, i);
            let c2 = nx(t, c1);
            let c3 = nx(t, c2);
            let char_lit = c2 < n && t[c2] == b'\'';
            let escaped = c3 < n && at(c1) == b'\\';
            if !char_lit && !escaped {
                i += 1;
                continue;
            }
        }
        if php && ((c == b'#' && at(i + 1) != b'[') || (c == b'?' && at(i + 1) == b'>')) {
            if c == b'?' {
                let j = bfind(t, b"<?php", i).map(|j| j + 5).unwrap_or(n);
                wipe(&mut out, i, j);
                if collect {
                    line += nl(t, i, j);
                }
                i = j;
                continue;
            }
            let j = bfind(t, b"\n", i).unwrap_or(n);
            wipe(&mut out, i, j);
            i = j;
            continue;
        }
        if php && t[i..].starts_with(b"<<<") {
            if let Some((ident, body)) = php_heredoc_b(t, i) {
                let mut j = n;
                let mut end_after = None;
                let mut k = body;
                loop {
                    let mut q = k;
                    while q < n && (t[q] == b' ' || t[q] == b'\t') {
                        q += 1;
                    }
                    if t[q.min(n)..].starts_with(ident.as_bytes()) {
                        let after = q + ident.len();
                        if !ch(t, after).map(|x| x.is_alphanumeric() || x == '_').unwrap_or(false) {
                            j = k;
                            end_after = Some(after);
                            break;
                        }
                    }
                    match bfind(t, b"\n", k) {
                        Some(e) => k = e + 1,
                        None => break,
                    }
                }
                if collect {
                    strings.push((line, bstr(t, body, j)));
                }
                if !keep_strings {
                    wipe(&mut out, body, j);
                }
                if collect {
                    line += nl(t, i, j);
                }
                i = end_after.unwrap_or(j);
                continue;
            }
        }
        if c == b'#' && !cfam {
            let j = bfind(t, b"\n", i).unwrap_or(n);
            wipe(&mut out, i, j);
            i = j;
            continue;
        }
        if elixir && c == b'?' && i + 1 < n && !alnum_before(i) {
            i = nx(t, i + 1); // ?# ?" char literals
            continue;
        }
        if elixir && c == b'~' && i + 2 < n {
            let letter = ch(t, i + 1).filter(|x| x.is_alphabetic());
            let opi = nx(t, i + 1);
            if let (Some(_), Some(op)) = (letter, ch(t, opi).filter(|_| opi < n)) {
                if let Some(cl) = sigil_close(op) {
                    let body = opi + 1;
                    let end = if t[opi..].starts_with(b"\"\"\"") || t[opi..].starts_with(b"'''") {
                        bfind(t, &t[opi..opi + 3], opi + 3).map(|e| e + 3).unwrap_or(n)
                    } else {
                        let (opb, clb) = (op as u8, cl as u8);
                        let mut j = body;
                        let mut depth = 1;
                        while j < n {
                            if t[j] == b'\\' {
                                j = nx(t, j + 1);
                                continue;
                            }
                            if t[j] == clb && (opb == clb || depth == 1) {
                                break;
                            }
                            if opb != clb && t[j] == opb {
                                depth += 1;
                            } else if opb != clb && t[j] == clb {
                                depth -= 1;
                            }
                            j += 1;
                        }
                        n.min(j + 1)
                    };
                    let body_end = end.saturating_sub(1).max(body);
                    if collect {
                        strings.push((line, bstr(t, body, end.saturating_sub(1))));
                    }
                    if !keep_strings {
                        wipe(&mut out, body, body_end);
                    }
                    if collect {
                        line += nl(t, i, end);
                    }
                    i = end;
                    continue;
                }
            }
        }
        if c_lang && c == b'\'' && i > 0 && nx(t, i) < n && alnum_before(i) && ch(t, i + 1).map(|x| x.is_alphanumeric()).unwrap_or(false)
        {
            i += 1; // a C++14 digit separator (25'000)
            continue;
        }
        if js && c == b'/' && regex_start_b(t, i) {
            let mut j = i + 1;
            let mut in_class = false;
            while j < n && t[j] != b'\n' {
                if t[j] == b'\\' {
                    j = nx(t, j + 1);
                    continue;
                }
                if t[j] == b'[' {
                    in_class = true;
                } else if t[j] == b']' {
                    in_class = false;
                } else if t[j] == b'/' && !in_class {
                    break;
                }
                j += 1;
            }
            if j < n && t[j] == b'/' {
                if !keep_strings {
                    wipe(&mut out, i + 1, j);
                }
                i = j + 1;
                continue;
            }
        }
        if c_lang
            && ((c == b'"' && i > 0 && (t[i - 1] == b'@' || (i >= 2 && (&t[i - 2..i] == b"@$" || &t[i - 2..i] == b"$@"))))
                || c == b'`')
        {
            let mut j = i + 1;
            while j < n {
                if t[j] == c {
                    if c == b'"' && at(j + 1) == b'"' {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            if collect {
                strings.push((line, bstr(t, i + 1, j)));
            }
            if !keep_strings {
                wipe(&mut out, i + 1, j);
            }
            if collect {
                line += nl(t, i, j);
            }
            i = n.min(j + 1);
            continue;
        }
        if c == b'"' || c == b'\'' || (js && c == b'`') {
            let triple = t[i..].starts_with(&[c, c, c]);
            let ql = if triple { 3 } else { 1 };
            let stop_at_nl = !triple && c != b'`' && matches!(lang, "gdscript" | "python" | "js" | "kotlin" | "swift");
            let mut j = i + ql;
            while j < n {
                let x = t[j];
                if x == b'\\' {
                    j = nx(t, j + 1);
                    continue;
                }
                if x == c && (!triple || t[j..].starts_with(&[c, c, c])) {
                    break;
                }
                if x == b'\n' && stop_at_nl {
                    break; // unterminated: stop at end of line
                }
                j += 1;
            }
            let j = j.min(n);
            let end = n.min(j + ql);
            if collect {
                strings.push((line, bstr(t, i + ql, j)));
            }
            if !keep_strings {
                wipe(&mut out, i + ql, j);
            }
            if collect {
                line += nl(t, i, end);
            }
            i = end;
            continue;
        }
        i += 1;
    }
    let s = String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    (s, strings)
}

/// `<<<"ID"\n` at i: (ident, index after the opener's newline).
fn php_heredoc_b(t: &[u8], i: usize) -> Option<(String, usize)> {
    let n = t.len();
    let mut k = i + 3;
    while k < n && (t[k] == b' ' || t[k] == b'\t') {
        k += 1;
    }
    if k < n && (t[k] == b'"' || t[k] == b'\'') {
        k += 1;
    }
    let s = k;
    if k >= n || !(t[k].is_ascii_alphabetic() || t[k] == b'_') {
        return None;
    }
    while k < n && ch(t, k).map(|x| x.is_alphanumeric() || x == '_').unwrap_or(false) {
        k = nx(t, k);
    }
    let ident = bstr(t, s, k);
    if k < n && (t[k] == b'"' || t[k] == b'\'') {
        k += 1;
    }
    while k < n && (t[k] == b' ' || t[k] == b'\t') {
        k += 1;
    }
    if k < n && t[k] == b'\n' {
        return Some((ident, k + 1));
    }
    None
}

/// A `/` that starts a JS regex literal, not a division.
fn regex_start_b(t: &[u8], i: usize) -> bool {
    match t.get(i + 1) {
        None | Some(b'/') | Some(b'*') => return false,
        _ => {}
    }
    let mut k = i;
    while k > 0 && matches!(t[k - 1], b' ' | b'\t' | b'\r' | b'\n') {
        k -= 1;
    }
    if k == 0 || REGEX_AFTER.as_bytes().contains(&t[k - 1]) {
        return true;
    }
    // the word ending at k
    let mut w = k;
    while let Some((p, c)) = prev(t, w) {
        if c.is_alphanumeric() || c == '_' || c == '$' {
            w = p;
        } else {
            break;
        }
    }
    let word = &t[w..k];
    REGEX_WORDS.iter().any(|x| x.as_bytes() == word)
}

// --- Ruby ---------------------------------------------------------------------

const RB_VALUE_KW: [&str; 21] = [
    "if", "unless", "while", "until", "and", "or", "not", "return", "when", "in", "then", "else", "elsif", "do", "case",
    "yield", "puts", "print", "p", "raise", "next",
];
const RB_OPS: &str = "([{,=;!&|?:<>+-*/%~^";

struct Rb<'a> {
    t: &'a [char],
    n: usize,
    o: Out,
    strings: Vec<(usize, String)>,
    nls: Vec<usize>,
    pending: Vec<(String, bool, String)>,
    keep: bool,
}

impl<'a> Rb<'a> {
    fn line_of(&self, k: usize) -> usize {
        self.nls.partition_point(|&x| x < k) + 1
    }
    fn wipe(&mut self, a: usize, b: usize, literal: bool) {
        if literal && self.keep {
            return;
        }
        let t = self.t;
        self.o.wipe(t, a, b);
    }
    fn is_value(&self, k: usize) -> bool {
        let t = self.t;
        let mut j = k as isize - 1;
        while j >= 0 && (t[j as usize] == ' ' || t[j as usize] == '\t') {
            j -= 1;
        }
        if j < 0 || t[j as usize] == '\n' {
            return true;
        }
        let cj = t[j as usize];
        if RB_OPS.contains(cj) {
            return true;
        }
        if is_alnum(cj) || "_?!".contains(cj) {
            let mut s = j as usize;
            while s > 0 && (is_alnum(t[s - 1]) || t[s - 1] == '_') {
                s -= 1;
            }
            let w = slice(t, s, j as usize + 1);
            if (RB_VALUE_KW.contains(&w.as_str()) || w == "break") && (s == 0 || !".@$:".contains(t[s - 1])) {
                return true;
            }
            let nxt = t.get(k + 1).copied();
            return (j as usize) < k - 1 && !matches!(nxt, Some(' ') | Some('\t') | Some('=') | Some('\n') | None);
        }
        false
    }
    fn literal(&mut self, mut k: usize, close: char, open: char, interp: bool, regex: bool) -> usize {
        let t = self.t;
        let n = self.n;
        let start = k;
        let mut seg = k;
        let mut depth: i64 = 0;
        let mut cls = 0;
        while k < n {
            let ch = t[k];
            if ch == '\\' {
                k += 2;
                continue;
            }
            if regex && ch == '[' {
                cls += 1;
            } else if regex && ch == ']' && cls > 0 {
                cls -= 1;
            } else if interp && ch == '#' && t.get(k + 1) == Some(&'{') {
                self.wipe(seg, k, true);
                k = self.code(k + 2, true);
                seg = k + 1;
            } else if ch == close && cls == 0 && depth == 0 {
                break;
            } else if open != close && ch == open {
                depth += 1;
            } else if open != close && ch == close {
                depth -= 1;
            }
            k += 1;
        }
        let k = k.min(n);
        self.wipe(seg, k, true);
        let ln = self.line_of(start);
        self.strings.push((ln, slice(t, start, k)));
        k + 1
    }
    fn heredocs(&mut self, k: usize) -> usize {
        let t = self.t;
        let n = self.n;
        let mut pos = k + 1;
        let pending = std::mem::take(&mut self.pending);
        for (flag, interp, ident) in pending {
            let mut j = pos;
            while j < n {
                let e = find(t, "\n", j).unwrap_or(n);
                let ln = slice(t, j, e);
                let ln = ln.trim_end_matches('\r');
                let cmp = if flag.is_empty() { ln.to_string() } else { ln.trim().to_string() };
                if cmp == ident {
                    break;
                }
                j = e + 1;
            }
            let body_end = j.min(n);
            let mut seg = pos;
            let mut q = pos;
            while interp && q < body_end {
                if t[q] == '\\' {
                    q += 2;
                    continue;
                }
                if t[q] == '#' && t.get(q + 1) == Some(&'{') {
                    self.wipe(seg, q, true);
                    q = self.code(q + 2, true);
                    seg = q + 1;
                }
                q += 1;
            }
            self.wipe(seg, body_end, true);
            let ln = self.line_of(pos);
            self.strings.push((ln, slice(t, pos, body_end)));
            let e = find(t, "\n", body_end).unwrap_or(n);
            self.wipe(body_end, e, false);
            pos = e + 1;
        }
        (pos - 1).min(n)
    }
    fn heredoc_at(&self, k: usize) -> Option<(String, String, String, usize)> {
        // <<([~-]?)(['"`]?)([A-Za-z_]\w*)\2
        let t = self.t;
        let n = self.n;
        if !(starts(t, k, "<<")) {
            return None;
        }
        let mut i = k + 2;
        let mut flag = String::new();
        if i < n && (t[i] == '~' || t[i] == '-') {
            flag.push(t[i]);
            i += 1;
        }
        let mut q = String::new();
        if i < n && (t[i] == '\'' || t[i] == '"' || t[i] == '`') {
            q.push(t[i]);
            i += 1;
        }
        let s = i;
        if i >= n || !(t[i].is_ascii_alphabetic() || t[i] == '_') {
            return None;
        }
        while i < n && (is_alnum(t[i]) || t[i] == '_') {
            i += 1;
        }
        let ident = slice(t, s, i);
        if !q.is_empty() {
            if i < n && t[i].to_string() == q {
                i += 1;
            } else {
                return None;
            }
        }
        Some((flag, q, ident, i))
    }
    fn code(&mut self, mut k: usize, interp: bool) -> usize {
        let t = self.t;
        let n = self.n;
        let mut depth = 0;
        while k < n {
            let ch = t[k];
            let bol = k == 0 || t[k - 1] == '\n';
            if ch == '\n' {
                if !self.pending.is_empty() && !interp {
                    k = self.heredocs(k);
                }
                k += 1;
                continue;
            }
            if interp && ch == '{' {
                depth += 1;
            } else if interp && ch == '}' {
                if depth == 0 {
                    return k;
                }
                depth -= 1;
            } else if ch == '#' {
                let e = find(t, "\n", k).unwrap_or(n);
                self.wipe(k, e, false);
                k = e;
                continue;
            } else if bol && starts(t, k, "=begin") && matches!(t.get(k + 6), Some(' ') | Some('\t') | Some('\r') | Some('\n') | None) {
                let e = match find(t, "\n=end", k) {
                    None => n,
                    Some(e) => find(t, "\n", e + 1).unwrap_or(n),
                };
                self.wipe(k, e, false);
                k = e;
                continue;
            } else if bol && starts(t, k, "__END__") && matches!(t.get(k + 7), Some('\n') | Some('\r') | None) {
                self.wipe(k, n, false);
                return n;
            } else if ch == '"' || ch == '`' {
                k = self.literal(k + 1, ch, ch, true, false);
                continue;
            } else if ch == '\'' {
                k = self.literal(k + 1, ch, ch, false, false);
                continue;
            } else if ch == '?'
                && k + 1 < n
                && !matches!(t[k + 1], ' ' | '\t' | '\n')
                && (k == 0 || !(is_alnum(t[k - 1]) || "_)]}".contains(t[k - 1])))
            {
                if t[k + 1] == '\\' {
                    k += 3;
                    continue;
                }
                let c2 = t.get(k + 2).copied();
                if !(c2.map(is_alnum).unwrap_or(false) || c2 == Some('_')) {
                    k += 2;
                    continue;
                }
            } else if ch == '$' && k + 1 < n && "\"'`/\\;,.<>!@&~=*$?:".contains(t[k + 1]) {
                k += 2;
                continue;
            } else if ch == '<' && starts(t, k, "<<") {
                if let Some((flag, q, ident, end)) = self.heredoc_at(k) {
                    if !flag.is_empty() || !q.is_empty() || (ident.chars().next().unwrap().is_uppercase() && self.is_value(k)) {
                        self.pending.push((flag, q != "'", ident));
                        k = end;
                        continue;
                    }
                }
            } else if ch == '%' && self.is_value(k) {
                let kind = t.get(k + 1).copied();
                let c2 = t.get(k + 2).copied();
                let typed = matches!(kind, Some(x) if "qQwWiIrsx".contains(x))
                    && !matches!(c2, None | Some(' ') | Some('\t') | Some('\n'))
                    && !c2.map(is_alnum).unwrap_or(false);
                let d = if typed { k + 2 } else { k + 1 };
                let op = t.get(d).copied();
                if typed || matches!(op, Some(x) if "([{<|!/^".contains(x)) {
                    let lit = if typed { kind.unwrap() } else { '\0' };
                    let op = op.unwrap();
                    let close = match op {
                        '(' => ')',
                        '[' => ']',
                        '{' => '}',
                        '<' => '>',
                        x => x,
                    };
                    k = self.literal(d + 1, close, op, !"qwis".contains(lit) || lit == '\0', lit == 'r');
                    continue;
                }
            } else if ch == '/' && self.is_value(k) {
                k = self.literal(k + 1, '/', '/', true, true);
                while k < n && "imxounse".contains(t[k]) {
                    k += 1;
                }
                continue;
            } else if is_alnum(ch) || ch == '_' {
                while k < n && (is_alnum(t[k]) || t[k] == '_') {
                    k += 1;
                }
                continue;
            }
            k += 1;
        }
        n
    }
}

fn blank_ruby(text: &str, keep: bool) -> (String, Vec<(usize, String)>) {
    let t: Vec<char> = text.chars().collect();
    let n = t.len();
    let nls = t.iter().enumerate().filter(|(_, &c)| c == '\n').map(|(k, _)| k).collect();
    let mut rb = Rb { t: &t, n, o: Out { wiped: vec![false; n] }, strings: Vec::new(), nls, pending: Vec::new(), keep };
    rb.code(0, false);
    let s = rb.o.render(&t);
    (s, rb.strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte scanner agrees with the character transcription of scan.py
    /// (blanked chars as one space per byte) on every language and edge shape.
    #[test]
    fn byte_scanner_matches_the_reference() {
        let samples = [
            "x = 'é' # ü\nr\"a\" 'b' \"\"\"\nq\"\"\" y",
            "fn a<'a>(x: &'a str) { let c = 'é'; let r = r#\"raw \"q\" é\"#; } // ü\n/* ö\n*/ 'x'",
            "const r = /a[/]b\\/é/g; x = a / b; s = `t ${é} \\` u`; // c\nreturn /re/",
            "defmodule A do\n  @x ~w(a é b) ++ ~s\"\"\"\n é\n\"\"\" # c\n  y = ?# <> ?é <> \"s#{x}é\"\nend",
            "val s = \"a\\\"é\" // c\nval t = \"unterminated\nnext\"",
            "var v = @\"a\"\"é\"; var g = `raw é`; int x = 25'000; char c = 'é';",
            "<?php $a = <<<EOT\nhé\nEOT;\n# c\n#[Attr] ?> <b>é</b> <?php echo 'x';",
            "# py é\ns = '''a\né''' + \"b\\\n\" + 'c",
            "~é(x) ~a(é) ~b[x[y]z] r#\"\" 'a 'é' \\",
        ];
        for lang in ["python", "elixir", "gdscript", "js", "kotlin", "rust", "c", "swift", "php"] {
            for s in samples {
                for keep in [false, true] {
                    assert_eq!(blank(s, lang, keep), blank_chars(s, lang, keep), "{lang} keep={keep}: {s:?}");
                    assert_eq!(blank_text(s, lang, keep), blank(s, lang, keep).0);
                }
            }
        }
    }

    #[test]
    fn blanks_comments_and_strings() {
        let (c, s) = blank("a = \"x#y\" # c\nb", "python", false);
        assert_eq!(c, "a = \"   \"    \nb");
        assert_eq!(s, vec![(1, "x#y".to_string())]);
        let (c, _) = blank("f(\"é\") // ü\n", "js", true);
        assert_eq!(c.len(), "f(\"é\") // ü\n".len());
        assert!(c.starts_with("f(\"é\")"));
    }

    #[test]
    fn elixir_sigils_and_chars() {
        let (c, _) = blank("x = ~w(a b)\ny = ?#\n", "elixir", false);
        assert_eq!(c, "x = ~w(   )\ny = ?#\n");
    }
}
