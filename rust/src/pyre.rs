//! Python `re` semantics on Rust's regex engines.
//!
//! seamcheck's contract is its TOML, and every pattern in it was written for
//! Python's `re`. Patterns go through [`translate`] (the few spots where the
//! two dialects read the same text differently), and matching keeps Python's
//! rules: `search` is leftmost-first, `finditer` allows an empty match right
//! after a non-empty one, `match` is anchored at a position, `fullmatch`
//! spans the whole text. Offsets are byte offsets (the texts seamcheck blanks
//! keep their byte layout, see `scan::blank`).
//!
//! A pattern the `regex` crate accepts runs there (finite automata, linear
//! time); only lookaround and backreferences go to `fancy-regex`'s
//! backtracker, which is two orders of magnitude slower on the same text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

enum Engine {
    Plain(regex::Regex),
    /// the backtracker, with `pre`: the pattern less its lookarounds, a
    /// superset of its matches (same start positions) that the fast engine
    /// finds, so the backtracker runs only from where a match can begin
    Fancy(fancy_regex::Regex, Option<regex::Regex>),
}

impl Engine {
    fn build(t: &str) -> Result<Engine, String> {
        if let Ok(re) = regex::RegexBuilder::new(t).size_limit(64 << 20).build() {
            return Ok(Engine::Plain(re));
        }
        let pre = regex::RegexBuilder::new(&strip_lookaround(t)).size_limit(64 << 20).build().ok();
        fancy_regex::RegexBuilder::new(t)
            .backtrack_limit(5_000_000)
            .build()
            .map(|re| Engine::Fancy(re, pre))
            .map_err(|e| e.to_string())
    }
    fn captures_at<'t>(&self, t: &'t str, pos: usize) -> Option<Caps<'t>> {
        match self {
            Engine::Plain(re) => re.captures_at(t, pos).map(Caps::P),
            Engine::Fancy(re, pre) => {
                // no match of the pattern starts before the first place its
                // lookaround-free superset matches
                let from = match pre {
                    Some(p) => p.find_at(t, pos)?.start(),
                    None => pos,
                };
                re.captures_from_pos(t, from).ok().flatten().map(Caps::F)
            }
        }
    }
    fn captures_len(&self) -> usize {
        match self {
            Engine::Plain(re) => re.captures_len(),
            Engine::Fancy(re, _) => re.captures_len(),
        }
    }
}

pub struct Rx {
    re: Engine,
    full: OnceLock<Option<Engine>>,
    src: String,
}

pub enum Caps<'t> {
    P(regex::Captures<'t>),
    F(fancy_regex::Captures<'t, str>),
}

impl<'t> Caps<'t> {
    fn span_of(&self, i: usize) -> Option<(usize, usize, &'t str)> {
        match self {
            Caps::P(c) => c.get(i).map(|m| (m.start(), m.end(), m.as_str())),
            Caps::F(c) => c.get(i).map(|m| (m.start(), m.end(), m.as_str())),
        }
    }
    fn len(&self) -> usize {
        match self {
            Caps::P(c) => c.len(),
            Caps::F(c) => c.len(),
        }
    }
    pub fn start(&self) -> usize {
        self.span_of(0).map(|m| m.0).unwrap_or(0)
    }
    pub fn end(&self) -> usize {
        self.span_of(0).map(|m| m.1).unwrap_or(0)
    }
    /// Group `i`'s text, None when it didn't take part (Python's None).
    pub fn get(&self, i: usize) -> Option<&'t str> {
        self.span_of(i).map(|m| m.2)
    }
    /// Group text or "" (Python `m.group(i) or ""`).
    pub fn s(&self, i: usize) -> &'t str {
        self.get(i).unwrap_or("")
    }
    pub fn span(&self, i: usize) -> Option<(usize, usize)> {
        self.span_of(i).map(|m| (m.0, m.1))
    }
    pub fn group0(&self) -> &'t str {
        self.s(0)
    }
    /// Number of groups, not counting group 0 (Python `re.groups`).
    pub fn ngroups(&self) -> usize {
        self.len().saturating_sub(1)
    }
    /// Python `m.groups()`: each group's text (None where it didn't match).
    pub fn groups(&self) -> Vec<Option<&'t str>> {
        (1..self.len()).map(|i| self.get(i)).collect()
    }
    pub fn name(&self, n: &str) -> Option<&'t str> {
        match self {
            Caps::P(c) => c.name(n).map(|m| m.as_str()),
            Caps::F(c) => c.name(n).map(|m| m.as_str()),
        }
    }
}

impl Rx {
    pub fn new(pat: &str) -> Result<Rx, String> {
        Engine::build(&translate(pat)).map(|re| Rx { re, full: OnceLock::new(), src: pat.to_string() })
    }

    /// For patterns written in this crate: a bad one is a bug, not input.
    pub fn lit(pat: &str) -> Rx {
        Rx::new(pat).unwrap_or_else(|e| panic!("bad built-in pattern {pat:?}: {e}"))
    }

    pub fn pattern(&self) -> &str {
        &self.src
    }

    /// Python `pattern.groups`.
    pub fn groups(&self) -> usize {
        self.re.captures_len().saturating_sub(1)
    }

    pub fn search<'t>(&self, t: &'t str) -> Option<Caps<'t>> {
        self.search_at(t, 0)
    }

    pub fn is_match(&self, t: &str) -> bool {
        match &self.re {
            Engine::Plain(re) => re.is_match(t),
            Engine::Fancy(re, pre) => {
                pre.as_ref().map(|p| p.is_match(t)).unwrap_or(true) && re.is_match(t).unwrap_or(false)
            }
        }
    }

    /// Python `pattern.search(t, pos)`.
    pub fn search_at<'t>(&self, t: &'t str, pos: usize) -> Option<Caps<'t>> {
        if pos > t.len() {
            return None;
        }
        self.re.captures_at(t, pos)
    }

    /// Python `pattern.search(t, a, b)`: `b` is the end of the text.
    pub fn search_range<'t>(&self, t: &'t str, a: usize, b: usize) -> Option<Caps<'t>> {
        let b = floor_char(t, b.min(t.len()));
        self.search_at(&t[..b], a.min(b))
    }

    /// Python `pattern.match(t, pos)`: a match starting exactly at `pos`.
    /// (Leftmost-first: if any match starts at `pos`, the search finds that one.)
    pub fn match_at<'t>(&self, t: &'t str, pos: usize) -> Option<Caps<'t>> {
        self.search_at(t, pos).filter(|c| c.start() == pos)
    }

    pub fn match_start<'t>(&self, t: &'t str) -> Option<Caps<'t>> {
        self.match_at(t, 0)
    }

    /// Python `pattern.fullmatch(t)`.
    pub fn fullmatch<'t>(&self, t: &'t str) -> Option<Caps<'t>> {
        let full = self.full.get_or_init(|| Engine::build(&format!(r"\A(?:{})\z", translate(&self.src))).ok());
        full.as_ref().and_then(|re| re.captures_at(t, 0))
    }

    pub fn find_iter<'t>(&self, t: &'t str) -> Vec<Caps<'t>> {
        self.find_iter_range(t, 0, t.len())
    }

    /// Python `pattern.finditer(t, a, b)`.
    pub fn find_iter_range<'t>(&self, t: &'t str, a: usize, b: usize) -> Vec<Caps<'t>> {
        let b = floor_char(t, b.min(t.len()));
        let hay = &t[..b];
        let mut out = Vec::new();
        let mut pos = a.min(b);
        while pos <= hay.len() {
            let Some(c) = self.re.captures_at(hay, pos) else { break };
            let (s, e) = (c.start(), c.end());
            out.push(c);
            if e > s {
                pos = e;
            } else {
                // an empty match: the next one starts one character on
                pos = next_char(hay, e);
                if pos > hay.len() || e >= hay.len() {
                    break;
                }
            }
        }
        out
    }

    /// Python `pattern.findall(t)`: group 1 (or the match) of each match;
    /// with several groups, use `find_iter`.
    pub fn findall<'t>(&self, t: &'t str) -> Vec<&'t str> {
        let g = self.groups();
        self.find_iter(t).into_iter().map(|c| if g == 0 { c.group0() } else { c.s(1) }).collect()
    }

    /// Python `pattern.sub(rep, t)` with Python's replacement syntax.
    pub fn sub(&self, rep: &str, t: &str) -> String {
        let tpl = parse_template(rep);
        let mut out = String::with_capacity(t.len());
        let mut last = 0;
        for c in self.find_iter(t) {
            out.push_str(&t[last..c.start()]);
            for piece in &tpl {
                match piece {
                    Piece::Lit(s) => out.push_str(s),
                    Piece::Group(i) => out.push_str(c.s(*i)),
                    Piece::Named(n) => out.push_str(c.name(n).unwrap_or("")),
                }
            }
            last = c.end();
        }
        out.push_str(&t[last..]);
        out
    }

    /// `sub` with a function computing each replacement.
    pub fn sub_with(&self, t: &str, mut f: impl FnMut(&Caps) -> String) -> String {
        let mut out = String::with_capacity(t.len());
        let mut last = 0;
        for c in self.find_iter(t) {
            out.push_str(&t[last..c.start()]);
            out.push_str(&f(&c));
            last = c.end();
        }
        out.push_str(&t[last..]);
        out
    }

    /// Python `pattern.split(t)` without groups.
    pub fn split<'t>(&self, t: &'t str) -> Vec<&'t str> {
        let mut out = Vec::new();
        let mut last = 0;
        for c in self.find_iter(t) {
            out.push(&t[last..c.start()]);
            last = c.end();
        }
        out.push(&t[last..]);
        out
    }
}

enum Piece {
    Lit(String),
    Group(usize),
    Named(String),
}

fn parse_template(rep: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let ch: Vec<char> = rep.chars().collect();
    let mut i = 0;
    while i < ch.len() {
        if ch[i] == '\\' && i + 1 < ch.len() {
            let n = ch[i + 1];
            if n.is_ascii_digit() {
                let mut j = i + 1;
                let mut num = String::new();
                while j < ch.len() && ch[j].is_ascii_digit() && num.len() < 2 {
                    num.push(ch[j]);
                    j += 1;
                }
                out.push(Piece::Lit(std::mem::take(&mut lit)));
                out.push(Piece::Group(num.parse().unwrap_or(0)));
                i = j;
                continue;
            }
            if n == 'g' && i + 2 < ch.len() && ch[i + 2] == '<' {
                if let Some(close) = ch[i + 3..].iter().position(|&c| c == '>') {
                    let name: String = ch[i + 3..i + 3 + close].iter().collect();
                    out.push(Piece::Lit(std::mem::take(&mut lit)));
                    match name.parse::<usize>() {
                        Ok(k) => out.push(Piece::Group(k)),
                        Err(_) => out.push(Piece::Named(name)),
                    }
                    i = i + 4 + close;
                    continue;
                }
            }
            let esc = match n {
                'n' => Some('\n'),
                't' => Some('\t'),
                'r' => Some('\r'),
                '\\' => Some('\\'),
                'a' => Some('\x07'),
                'f' => Some('\x0c'),
                'v' => Some('\x0b'),
                'b' => Some('\x08'),
                _ => None,
            };
            match esc {
                Some(e) => lit.push(e),
                None => {
                    lit.push('\\');
                    lit.push(n);
                }
            }
            i += 2;
            continue;
        }
        lit.push(ch[i]);
        i += 1;
    }
    out.push(Piece::Lit(lit));
    out
}

fn next_char(t: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < t.len() && !t.is_char_boundary(j) {
        j += 1;
    }
    j
}

pub fn floor_char(t: &str, mut i: usize) -> usize {
    while i > 0 && !t.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Python pattern text -> the same meaning in fancy-regex's dialect.
pub fn translate(pat: &str) -> String {
    let c: Vec<char> = pat.chars().collect();
    let mut out = String::with_capacity(pat.len() + 8);
    let mut i = 0;
    let mut in_class = false;
    let mut class_start = 0usize;
    while i < c.len() {
        let ch = c[i];
        if ch == '\\' && i + 1 < c.len() {
            let n = c[i + 1];
            match n {
                'Z' if !in_class => out.push_str(r"\z"),
                // Python: an escaped punctuation mark is itself (Rust reads `\<` `\>` as word boundaries)
                '<' | '>' | '/' | '\'' | '"' | '`' | '@' | '%' | '=' | ':' | ',' | ';' | '!' => out.push(n),
                _ => {
                    out.push('\\');
                    out.push(n);
                }
            }
            i += 2;
            continue;
        }
        if in_class {
            let first = i == class_start;
            match ch {
                ']' if first => out.push_str(r"\]"),
                ']' => {
                    out.push(']');
                    in_class = false;
                }
                '[' => out.push_str(r"\["),
                '&' | '~' if i + 1 < c.len() && c[i + 1] == ch => {
                    out.push('\\');
                    out.push(ch);
                }
                '-' if i + 1 < c.len() && c[i + 1] == '-' => out.push_str(r"\-"),
                _ => out.push(ch),
            }
            i += 1;
            continue;
        }
        match ch {
            '[' => {
                in_class = true;
                out.push('[');
                i += 1;
                if i < c.len() && c[i] == '^' {
                    out.push('^');
                    i += 1;
                }
                class_start = i;
                continue;
            }
            '{' => {
                // a quantifier only when it reads as one; otherwise a literal brace
                let rest: String = c[i..].iter().take(24).collect();
                if let Some(q) = quantifier(&rest) {
                    out.push_str(&q.0);
                    i += q.1;
                    continue;
                }
                out.push_str(r"\{");
            }
            '}' => out.push_str(r"\}"),
            '(' if c.get(i + 1) == Some(&'?') && c.get(i + 2) == Some(&'P') && c.get(i + 3) == Some(&'=') => {
                // (?P=name) -> \k<name>
                let close = c[i..].iter().position(|&x| x == ')').map(|p| p + i).unwrap_or(c.len() - 1);
                let name: String = c[i + 4..close].iter().collect();
                out.push_str(&format!(r"\k<{name}>"));
                i = close + 1;
                continue;
            }
            _ => out.push(ch),
        }
        i += 1;
    }
    out
}

/// The pattern without its lookaround groups (zero-width, so what remains
/// matches wherever the pattern does). Backreferences stay: such a pattern
/// won't compile in the fast engine and gets no prefilter.
fn strip_lookaround(t: &str) -> String {
    let c: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len());
    let mut i = 0;
    let mut in_class = false;
    while i < c.len() {
        if c[i] == '\\' && i + 1 < c.len() {
            out.push(c[i]);
            out.push(c[i + 1]);
            i += 2;
            continue;
        }
        if in_class {
            if c[i] == ']' {
                in_class = false;
            }
            out.push(c[i]);
            i += 1;
            continue;
        }
        if c[i] == '[' {
            in_class = true;
            out.push('[');
            i += 1;
            if i < c.len() && c[i] == '^' {
                out.push('^');
                i += 1;
            }
            if i < c.len() && c[i] == ']' {
                out.push(']');
                i += 1;
            }
            continue;
        }
        let rest: String = c[i..c.len().min(i + 4)].iter().collect();
        if rest.starts_with("(?=") || rest.starts_with("(?!") || rest.starts_with("(?<=") || rest.starts_with("(?<!") {
            // skip to the matching close paren
            let mut depth = 0;
            let mut cls = false;
            let mut j = i;
            while j < c.len() {
                match c[j] {
                    '\\' => {
                        j += 2;
                        continue;
                    }
                    '[' if !cls => cls = true,
                    ']' if cls => cls = false,
                    '(' if !cls => depth += 1,
                    ')' if !cls => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// `{m}`, `{m,}`, `{,n}`, `{m,n}` at the start of `s`: (rust text, chars consumed).
fn quantifier(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let mut i = 1;
    let mut m = String::new();
    while i < b.len() && b[i].is_ascii_digit() {
        m.push(b[i] as char);
        i += 1;
    }
    if i < b.len() && b[i] == b'}' {
        if m.is_empty() {
            return None;
        }
        return Some((format!("{{{m}}}"), i + 1));
    }
    if i >= b.len() || b[i] != b',' {
        return None;
    }
    i += 1;
    let mut n = String::new();
    while i < b.len() && b[i].is_ascii_digit() {
        n.push(b[i] as char);
        i += 1;
    }
    if i >= b.len() || b[i] != b'}' {
        return None;
    }
    let m = if m.is_empty() { "0".to_string() } else { m };
    Some((format!("{{{m},{n}}}"), i + 1))
}

static CACHE: OnceLock<Mutex<HashMap<String, Arc<Rx>>>> = OnceLock::new();

/// A compiled pattern, once per run (Python's `re` caches the same way).
pub fn rx(pat: &str) -> Result<Arc<Rx>, String> {
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(r) = cache.lock().unwrap().get(pat) {
        return Ok(r.clone());
    }
    let r = Arc::new(Rx::new(pat)?);
    cache.lock().unwrap().insert(pat.to_string(), r.clone());
    Ok(r)
}

/// Python's `repr()` of a str.
pub fn repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (0x80..0xa0).contains(&(c as u32)) => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `repr()` of an optional str (None for a missing key).
pub fn repr_opt(s: Option<&str>) -> String {
    s.map(repr).unwrap_or_else(|| "None".to_string())
}

/// Python `str.strip()` (whitespace, Unicode-aware like Python's).
pub fn strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

/// First `n` characters (Python `s[:n]`).
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_braces_and_z() {
        assert!(Rx::lit(r"a{b}").is_match("a{b}"));
        assert!(Rx::lit(r"x{2}").is_match("xx"));
        assert!(Rx::lit(r"x{,2}y").is_match("y"));
        assert!(Rx::lit(r"ab\Z").is_match("ab"));
        assert!(Rx::lit(r"\<x\>").is_match("<x>"));
        assert!(Rx::lit(r"[[]").is_match("["));
        assert!(Rx::lit(r"[]a]").is_match("]"));
    }

    #[test]
    fn finditer_empty_after_nonempty() {
        let r = Rx::lit("x*");
        let spans: Vec<_> = r.find_iter("axb").iter().map(|c| (c.start(), c.end())).collect();
        assert_eq!(spans, vec![(0, 0), (1, 2), (2, 2), (3, 3)]);
    }

    #[test]
    fn sub_template() {
        assert_eq!(Rx::lit(r":(\w+)").sub(r"<\1>", "/a/:id/b"), "/a/<id>/b");
        assert_eq!(Rx::lit(r"(?P<n>\d)").sub(r"\g<n>\g<1>", "a1"), "a11");
    }

    #[test]
    fn repr_like_python() {
        assert_eq!(repr("a'b"), "\"a'b\"");
        assert_eq!(repr("ab"), "'ab'");
        assert_eq!(repr("a\nb"), "'a\\nb'");
    }
}
