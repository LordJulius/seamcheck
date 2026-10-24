//! Python-flavoured string helpers for the Ruby adapter: it indexes the
//! blanked code by position, as lang_ruby.py does by character. Positions
//! here are byte offsets (the blanked text keeps the raw bytes in place);
//! every delimiter the adapter looks for is ASCII, and the few tests that
//! ask about a letter decode the character at the position.

use crate::pyre::{floor_char, Caps, Rx};

pub fn clamp(s: &str, i: usize) -> usize {
    floor_char(s, i.min(s.len()))
}

/// Python `s[a:b]` for non-negative a, b (clamped, empty when a >= b).
pub fn sub(s: &str, a: usize, b: usize) -> &str {
    let b = clamp(s, b);
    let a = clamp(s, a);
    if a >= b {
        ""
    } else {
        &s[a..b]
    }
}

/// Python `s[a:]`.
pub fn from(s: &str, a: usize) -> &str {
    sub(s, a, s.len())
}

/// Python `s.find(needle, start)`.
pub fn find(s: &str, needle: &str, start: usize) -> Option<usize> {
    if start > s.len() {
        return None;
    }
    let st = clamp(s, start);
    s[st..].find(needle).map(|k| k + st)
}

/// Python `s.rfind(needle, 0, hi)`.
pub fn rfind(s: &str, needle: &str, hi: usize) -> Option<usize> {
    let hi = clamp(s, hi);
    s[..hi].rfind(needle)
}

/// Python `s.rfind(needle, lo, hi)`.
pub fn rfind_range(s: &str, needle: &str, lo: usize, hi: usize) -> Option<usize> {
    let hi = clamp(s, hi);
    let lo = clamp(s, lo);
    if lo > hi {
        return None;
    }
    s[lo..hi].rfind(needle).map(|k| k + lo)
}

pub fn starts_at(s: &str, pos: usize, pat: &str) -> bool {
    s.get(pos..).map(|r| r.starts_with(pat)).unwrap_or(false)
}

/// The character starting at byte `i`.
pub fn chr_at(s: &str, i: usize) -> Option<char> {
    s.get(i..)?.chars().next()
}

/// The character ending at byte `i` (Python `s[i - 1]`).
pub fn prev_chr(s: &str, i: usize) -> Option<char> {
    s.get(..i)?.chars().next_back()
}

/// The byte at `i`, 0 past the end (ASCII tests only).
pub fn byte_at(s: &str, i: usize) -> u8 {
    s.as_bytes().get(i).copied().unwrap_or(0)
}

pub fn isalnum(c: char) -> bool {
    c.is_alphanumeric()
}

/// `str.isupper()` of a one-character string.
pub fn upper(c: char) -> bool {
    c.is_uppercase()
}

pub fn lower(c: char) -> bool {
    c.is_lowercase()
}

pub fn first_upper(s: &str) -> bool {
    s.chars().next().map(upper).unwrap_or(false)
}

pub fn first_lower(s: &str) -> bool {
    s.chars().next().map(lower).unwrap_or(false)
}

/// The character column of byte `b` in `line`.
pub fn col_of(line: &str, b: usize) -> i64 {
    let b = clamp(line, b);
    if line.is_ascii() {
        b as i64
    } else {
        line[..b].chars().count() as i64
    }
}

/// `str.strip()` (Unicode whitespace).
pub fn strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(|c: char| c.is_whitespace())
}

pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_whitespace())
}

/// A regex anchored at a position (Python `pattern.match(text, pos)`):
/// matched against the text from `pos`, offsets given back absolute.
pub struct Anch(Rx);

pub struct M<'t> {
    c: Caps<'t>,
    off: usize,
}

impl<'t> M<'t> {
    pub fn start(&self) -> usize {
        self.c.start() + self.off
    }
    pub fn end(&self) -> usize {
        self.c.end() + self.off
    }
    pub fn s(&self, i: usize) -> &'t str {
        self.c.s(i)
    }
    pub fn get(&self, i: usize) -> Option<&'t str> {
        self.c.get(i)
    }
    pub fn span(&self, i: usize) -> Option<(usize, usize)> {
        self.c.span(i).map(|(a, b)| (a + self.off, b + self.off))
    }
    pub fn start_of(&self, i: usize) -> usize {
        self.span(i).map(|x| x.0).unwrap_or(0)
    }
}

impl Anch {
    pub fn new(pat: &str) -> Anch {
        Anch(Rx::lit(&format!(r"\A(?:{pat})")))
    }
    pub fn at<'t>(&self, s: &'t str, pos: usize) -> Option<M<'t>> {
        let rest = s.get(pos..)?;
        self.0.search(rest).map(|c| M { c, off: pos })
    }
    pub fn start<'t>(&self, s: &'t str) -> Option<M<'t>> {
        self.at(s, 0)
    }
}

/// Python `re.split(r"\s*,\s*", s)`.
pub fn split_commas(s: &str) -> Vec<String> {
    use std::sync::OnceLock;
    static R: OnceLock<Rx> = OnceLock::new();
    R.get_or_init(|| Rx::lit(r"\s*,\s*")).split(s).into_iter().map(String::from).collect()
}

/// `re.findall(pat, s)` with a single group: its text for each match.
pub fn findall<'t>(rx: &Rx, s: &'t str) -> Vec<&'t str> {
    rx.find_iter(s).iter().map(|c| c.s(1)).collect()
}

pub fn join_us(ch: &[String]) -> String {
    ch.join("\x1f")
}

/// Python `re.escape`, enough for a regex built from a method name.
pub fn escape(s: &str) -> String {
    regex::escape(s)
}

/// Python `s.rfind(needle, 0, hi)` with Python's negative-index rules.
pub fn py_rfind(s: &str, needle: &str, hi: isize) -> isize {
    let n = s.len() as isize;
    let mut h = hi;
    if h < 0 {
        h += n;
        if h < 0 {
            h = 0;
        }
    }
    let h = clamp(s, h.min(n) as usize);
    s[..h].rfind(needle).map(|x| x as isize).unwrap_or(-1)
}

/// Python `s.find(needle, start)` (start may be negative).
pub fn py_find(s: &str, needle: &str, start: isize) -> isize {
    let n = s.len() as isize;
    let mut st = start;
    if st < 0 {
        st += n;
        if st < 0 {
            st = 0;
        }
    }
    find(s, needle, st as usize).map(|x| x as isize).unwrap_or(-1)
}

/// Python `s[a:b]` with negative indexes.
pub fn py_slice(s: &str, a: isize, b: isize) -> &str {
    let n = s.len() as isize;
    let norm = |i: isize| -> usize {
        let mut i = i;
        if i < 0 {
            i += n;
            if i < 0 {
                i = 0;
            }
        }
        i.min(n) as usize
    };
    sub(s, norm(a), norm(b))
}
