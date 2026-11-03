//! A rule that doesn't parse is reported in Python's words (`SyntaxError`'s
//! text, as `ast.parse` raises it): what Python's tokenizer refuses first
//! (an unclosed or unmatched bracket, an unterminated string, a bad number),
//! then what its parser says.

/// `msg (<unknown>, line N)`, as `str(SyntaxError)` reads.
pub fn message(msg: &str, line: usize) -> String {
    format!("{msg} (<unknown>, line {line})")
}

/// The line (1-based) of a byte offset.
pub fn line_at(src: &str, pos: usize) -> usize {
    src.as_bytes()[..pos.min(src.len())].iter().filter(|b| **b == b'\n').count() + 1
}

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

const STRING_PREFIXES: &[&str] = &["r", "u", "f", "b", "t", "br", "rb", "fr", "rf", "tr", "rt"];
/// Words Python lets follow a number directly (`1if x else 2`).
const AFTER_NUMBER: &[&str] = &["and", "else", "for", "if", "in", "is", "not", "or"];

/// What Python's tokenizer refuses in `src`, first in source order, as the
/// whole SyntaxError text; None when it tokenizes.
pub fn tokenizer_error(src: &str) -> Option<String> {
    let cs: Vec<(usize, char)> = src.char_indices().collect();
    let at = |k: usize| cs.get(k).map(|x| x.1);
    let line = |k: usize| line_at(src, cs.get(k).map(|x| x.0).unwrap_or(src.len()));
    let last_line = line_at(src, src.len());
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut k = 0;
    while k < cs.len() {
        let c = cs[k].1;
        if c == '#' {
            while k < cs.len() && cs[k].1 != '\n' {
                k += 1;
            }
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = k;
            while at(k).is_some_and(ident) {
                k += 1;
            }
            let w: String = cs[start..k].iter().map(|x| x.1).collect::<String>().to_ascii_lowercase();
            if matches!(at(k), Some('\'' | '"')) && STRING_PREFIXES.contains(&w.as_str()) {
                continue; // the string itself comes next
            }
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && at(k + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = k;
            let radix = if c == '0' { at(k + 1).map(|x| x.to_ascii_lowercase()) } else { None };
            if let Some(r @ ('x' | 'o' | 'b')) = radix {
                k += 2;
                let ok = |d: char| match r {
                    'x' => d.is_ascii_hexdigit(),
                    'o' => ('0'..='7').contains(&d),
                    _ => d == '0' || d == '1',
                };
                let digits_from = k;
                while at(k).is_some_and(|d| ok(d) || d == '_') {
                    k += 1;
                }
                let kind = match r {
                    'x' => "hexadecimal",
                    'o' => "octal",
                    _ => "binary",
                };
                if k == digits_from || at(k).is_some_and(ident) || cs[k - 1].1 == '_' {
                    return Some(message(&format!("invalid {kind} literal"), line(start)));
                }
                continue;
            }
            let bad = || Some(message("invalid decimal literal", line(start)));
            let digits = |k: &mut usize| -> bool {
                // digits with single underscores between them
                while at(*k).is_some_and(|d| d.is_ascii_digit()) || (at(*k) == Some('_') && at(*k + 1).is_some_and(|d| d.is_ascii_digit())) {
                    *k += 1;
                }
                at(*k) != Some('_')
            };
            if !digits(&mut k) {
                return bad();
            }
            if at(k) == Some('.') {
                k += 1;
                if !digits(&mut k) {
                    return bad();
                }
            }
            if matches!(at(k), Some('e' | 'E')) {
                let mut j = k + 1;
                if matches!(at(j), Some('+' | '-')) {
                    j += 1;
                }
                if !at(j).is_some_and(|d| d.is_ascii_digit()) {
                    // `1else`: a word may follow; `1e` / `1e+` is a bad number
                    let w: String = cs[k..].iter().map(|x| x.1).take_while(|c| ident(*c)).collect();
                    if !AFTER_NUMBER.contains(&w.as_str()) {
                        return bad();
                    }
                } else {
                    k = j;
                    if !digits(&mut k) {
                        return bad();
                    }
                }
            }
            if matches!(at(k), Some('j' | 'J')) {
                k += 1;
            }
            if at(k).is_some_and(ident) {
                let w: String = cs[k..].iter().map(|x| x.1).take_while(|c| ident(*c)).collect();
                if !AFTER_NUMBER.iter().any(|a| w.starts_with(a)) {
                    return bad();
                }
            }
            continue;
        }
        if c == '\'' || c == '"' {
            let start = k;
            let triple = at(k + 1) == Some(c) && at(k + 2) == Some(c);
            k += if triple { 3 } else { 1 };
            loop {
                match at(k) {
                    None => {
                        let n = cs.len();
                        let escaped = n >= 2 && cs[n - 1].1 == c && cs[n - 2].1 == '\\';
                        let msg = if triple {
                            format!("unterminated triple-quoted string literal (detected at line {last_line})")
                        } else {
                            format!(
                                "unterminated string literal (detected at line {}){}",
                                line(start),
                                if escaped { "; perhaps you escaped the end quote?" } else { "" }
                            )
                        };
                        return Some(message(&msg, line(start)));
                    }
                    Some('\\') => k += 2,
                    Some('\n') if !triple => {
                        let escaped = k >= 2 && at(k - 1) == Some(c) && at(k - 2) == Some('\\');
                        let msg = format!(
                            "unterminated string literal (detected at line {}){}",
                            line(start),
                            if escaped { "; perhaps you escaped the end quote?" } else { "" }
                        );
                        return Some(message(&msg, line(start)));
                    }
                    Some(q) if q == c && (!triple || (at(k + 1) == Some(c) && at(k + 2) == Some(c))) => {
                        k += if triple { 3 } else { 1 };
                        break;
                    }
                    Some(_) => k += 1,
                }
            }
            continue;
        }
        match c {
            '(' | '[' | '{' => stack.push((c, line(k))),
            ')' | ']' | '}' => {
                let want = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                match stack.pop() {
                    None => return Some(message(&format!("unmatched '{c}'"), line(k))),
                    Some((o, ol)) if o != want => {
                        let on = if ol != line(k) { format!(" on line {ol}") } else { String::new() };
                        return Some(message(&format!("closing parenthesis '{c}' does not match opening parenthesis '{o}'{on}"), line(k)));
                    }
                    Some(_) => {}
                }
            }
            _ => {}
        }
        k += 1;
    }
    stack.last().map(|(o, l)| message(&format!("'{o}' was never closed"), *l))
}

/// The byte offset of the first newline outside brackets and strings (one
/// not escaped by a backslash): where Python's eval input ends.
pub fn top_level_newline(src: &str) -> Option<usize> {
    let b = src.as_bytes();
    let (mut depth, mut k) = (0i32, 0usize);
    while k < b.len() {
        match b[k] {
            b'\\' => k += 1,
            b'#' => {
                while k < b.len() && b[k] != b'\n' {
                    k += 1;
                }
                continue;
            }
            q @ (b'\'' | b'"') => {
                let triple = b.get(k + 1) == Some(&q) && b.get(k + 2) == Some(&q);
                k += if triple { 3 } else { 1 };
                while k < b.len() {
                    if b[k] == b'\\' {
                        k += 2;
                        continue;
                    }
                    if b[k] == q && (!triple || (b.get(k + 1) == Some(&q) && b.get(k + 2) == Some(&q))) {
                        k += if triple { 2 } else { 0 };
                        break;
                    }
                    k += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'\n' if depth <= 0 => return Some(k),
            _ => {}
        }
        k += 1;
    }
    None
}
