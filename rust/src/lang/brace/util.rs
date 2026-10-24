//! Python string and regex idioms the brace adapter leans on, on byte
//! offsets. The blanked code keeps the original text's bytes in place, so a
//! byte offset is a position in both; where Python measures a window in
//! characters (`code[k:k + 200]`), these measure characters too.

use std::sync::Arc;

use crate::pyre::{self, Rx};

/// A pattern compiled once (Python's module-level `re.compile`).
macro_rules! re {
    ($p:expr) => {{
        static R: std::sync::OnceLock<crate::pyre::Rx> = std::sync::OnceLock::new();
        R.get_or_init(|| crate::pyre::Rx::lit($p))
    }};
}
pub(crate) use re;

/// A pattern built at run time (a name escaped into it), cached per run.
pub fn rxd(pat: &str) -> Arc<Rx> {
    pyre::rx(pat).unwrap_or_else(|e| panic!("bad built pattern {pat:?}: {e}"))
}

/// Python `re.escape`.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        if !(ch.is_alphanumeric() || ch == '_') && (ch as u32) < 128 {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The byte index at or before `i` that starts a character.
pub fn floor(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Python `s[a:b]` on byte offsets: clamped, empty when b <= a.
pub fn sl(s: &str, a: usize, b: usize) -> &str {
    let b = floor(s, b);
    let a = floor(s, a);
    if a >= b {
        ""
    } else {
        &s[a..b]
    }
}

/// Python `s[a:a + n]`: `n` characters from byte `a`.
pub fn win(s: &str, a: usize, n: usize) -> &str {
    let a = floor(s, a);
    pyre::take_chars(&s[a..], n)
}

/// `s[a:a + n]` with the `n` characters counted in `r`, the raw text `s`
/// (blanked) keeps the bytes of: Python counts the original's characters.
pub fn rw<'a>(s: &'a str, r: &str, a: usize, n: usize) -> &'a str {
    sl(s, a, ahead(r, a, n))
}

/// `s[a:b]` as Python sees it: a multi-byte character the blanker wiped is
/// ONE space there but several here, so collapse it (`raw` is the text `s`
/// was blanked from).
pub fn cs(s: &str, raw: &str, a: usize, b: usize) -> String {
    let (a, b) = (floor(s, a), floor(s, b));
    if a >= b {
        return String::new();
    }
    if raw.is_char_boundary(a) && raw.is_char_boundary(b) && raw[a..b].is_ascii() {
        return s[a..b].to_string();
    }
    let mut out = String::with_capacity(b - a);
    let mut k = a;
    while k < b {
        let ch = raw[k..].chars().next().unwrap();
        let l = ch.len_utf8().min(b - k);
        let seg = &s.as_bytes()[k..k + l];
        if l > 1 && seg.iter().all(|&x| x == b' ') {
            out.push(' ');
        } else {
            out.push_str(&s[k..k + l]);
        }
        k += l;
    }
    out
}

/// `s[b - n:b]`, the `n` characters counted in the raw text `r`.
pub fn rb<'a>(s: &'a str, r: &str, b: usize, n: usize) -> &'a str {
    let w = back(r, b, n);
    sl(s, b - w.len(), b)
}

/// Is byte `k` of `r` the first of a character? (loops that count characters)
pub fn lead(r: &str, k: usize) -> bool {
    r.as_bytes().get(k).map(|&c| c < 0x80 || c >= 0xc0).unwrap_or(true)
}

/// The byte offset `n` characters on from `a` (clamped to the end).
pub fn ahead(s: &str, a: usize, n: usize) -> usize {
    a + win(s, a, n).len()
}

/// Python `s[max(0, b - n):b]`: the `n` characters before byte `b`.
pub fn back(s: &str, b: usize, n: usize) -> &str {
    let b = floor(s, b);
    let mut start = b;
    for (k, (p, _)) in s[..b].char_indices().rev().enumerate() {
        if k == n {
            break;
        }
        start = p;
    }
    &s[start..b]
}

/// Python's whitespace for `strip()` (it counts \x1c-\x1f too).
pub fn pyws(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

pub fn strip(s: &str) -> &str {
    s.trim_matches(pyws)
}

pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(pyws)
}

pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(pyws)
}

pub fn ends_any(s: &str, xs: &[&str]) -> bool {
    xs.iter().any(|x| s.ends_with(x))
}

pub fn starts_any(s: &str, xs: &[&str]) -> bool {
    xs.iter().any(|x| s.starts_with(x))
}

/// The first character (Python `s[:1]`).
pub fn first(s: &str) -> &str {
    pyre::take_chars(s, 1)
}

/// Python `s[:1].isupper()`.
pub fn upper1(s: &str) -> bool {
    s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

/// Python `s[:1].islower()`.
pub fn lower1(s: &str) -> bool {
    s.chars().next().map(|c| c.is_lowercase()).unwrap_or(false)
}

/// The character before byte `i` ("" at 0).
pub fn prev_char(s: &str, i: usize) -> &str {
    back(s, i, 1)
}

/// Python `s.count("\n", a, b)`.
pub fn count_nl(s: &str, a: usize, b: usize) -> usize {
    sl(s, a, b).bytes().filter(|&c| c == b'\n').count()
}

/// Python `s.rfind("\n", 0, pos) + 1`: where the line holding `pos` starts.
pub fn line_begin(s: &str, pos: usize) -> usize {
    s.as_bytes()[..pos.min(s.len())].iter().rposition(|&c| c == b'\n').map(|k| k + 1).unwrap_or(0)
}

/// Python `s.find(sub, a)` (-1: none).
pub fn find_from(s: &str, sub: &str, a: usize) -> isize {
    if a > s.len() {
        return -1;
    }
    let a = floor(s, a);
    s[a..].find(sub).map(|k| (a + k) as isize).unwrap_or(-1)
}

/// Python `s.find(sub, a, b)`.
pub fn find_in(s: &str, sub: &str, a: usize, b: usize) -> isize {
    let b = floor(s, b.min(s.len()));
    if a > b {
        return -1;
    }
    let a = floor(s, a);
    s[a..b].find(sub).map(|k| (a + k) as isize).unwrap_or(-1)
}

/// Python `s.rfind(sub, 0, b)`.
pub fn rfind_before(s: &str, sub: &str, b: usize) -> isize {
    let b = floor(s, b.min(s.len()));
    s[..b].rfind(sub).map(|k| k as isize).unwrap_or(-1)
}

/// Characters in `raw[..b]` (a byte column -> Python's character column).
pub fn chars_to(raw: &str, b: usize) -> usize {
    let b = floor(raw, b);
    raw[..b].chars().count()
}

/// Python `"\x1f".join(chain)`.
pub fn join(ch: &[String]) -> String {
    ch.join("\x1f")
}

/// Add to a set kept as a vector (Python `set.add`, insertion order).
pub fn add(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

/// Python `str.split()` (whitespace runs).
pub fn words(s: &str) -> Vec<&str> {
    s.split(pyws).filter(|w| !w.is_empty()).collect()
}

/// `_stem_key`: a name without `-`, `_`, `.`, lowercased.
pub fn stem_key(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '-' | '_' | '.')).collect::<String>().to_lowercase()
}

/// `_ALLCAPS.match(s)`: only capitals, digits and `_`.
pub fn allcaps(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// Python `os.path.basename`.
pub fn basename(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Python `os.path.splitext(name)[0]` on a base name.
pub fn splitext0(base: &str) -> &str {
    let lead = base.len() - base.trim_start_matches('.').len();
    match base[lead..].rfind('.') {
        Some(k) => &base[..lead + k],
        None => base,
    }
}

/// `rel.rsplit("/", 1)[0] if "/" in rel else ""`
pub fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// `s.split(sep)[-1]`
pub fn last_part<'a>(s: &'a str, sep: &str) -> &'a str {
    s.rsplit(sep).next().unwrap_or(s)
}
