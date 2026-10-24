//! Path globs (`**` spans directories), fnmatch for function names, and
//! difflib's close-match suggestions, each as Python's seamcheck reads them.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

static GLOBS: OnceLock<RwLock<HashMap<String, Arc<regex::Regex>>>> = OnceLock::new();
static FN: OnceLock<RwLock<HashMap<String, Arc<regex::Regex>>>> = OnceLock::new();

/// fnmatch with `**` spanning directories (core.glob_re).
pub fn glob_source(g: &str) -> String {
    let c: Vec<char> = g.chars().collect();
    let mut out = String::from(r"\A(?s:");
    let mut i = 0;
    while i < c.len() {
        if c[i] == '*' && c.get(i + 1) == Some(&'*') && c.get(i + 2) == Some(&'/') {
            out.push_str("(?:.*/)?");
            i += 3;
        } else if c[i] == '*' && c.get(i + 1) == Some(&'*') {
            out.push_str(".*");
            i += 2;
        } else if c[i] == '*' {
            out.push_str("[^/]*");
            i += 1;
        } else if c[i] == '?' {
            out.push_str("[^/]");
            i += 1;
        } else {
            out.push_str(&regex::escape(&c[i].to_string()));
            i += 1;
        }
    }
    // Python's glob_re has no DOTALL: `.` stops at a newline (paths have none)
    out = out.replacen(r"\A(?s:", r"\A(?:", 1);
    out.push_str(r")\z");
    out
}

pub fn glob_re(g: &str) -> Arc<regex::Regex> {
    let cache = GLOBS.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(r) = cache.read().unwrap().get(g) {
        return r.clone();
    }
    let r = Arc::new(regex::Regex::new(&glob_source(g)).expect("glob regex"));
    cache.write().unwrap().insert(g.to_string(), r.clone());
    r
}

pub fn glob_match(path: &str, g: &str) -> bool {
    glob_re(g).is_match(path)
}

pub fn match_any<S: AsRef<str>>(path: &str, globs: &[S]) -> bool {
    globs.iter().any(|g| glob_re(g.as_ref()).is_match(path))
}

/// A list of globs compiled once (hot loops over every file).
pub struct Globs(Vec<Arc<regex::Regex>>);

impl Globs {
    pub fn new<S: AsRef<str>>(globs: &[S]) -> Globs {
        Globs(globs.iter().map(|g| glob_re(g.as_ref())).collect())
    }
    pub fn any(&self, path: &str) -> bool {
        self.0.iter().any(|r| r.is_match(path))
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// fnmatch.translate + fullmatch (case-sensitive: fnmatchcase).
fn fn_source(pat: &str) -> String {
    let c: Vec<char> = pat.chars().collect();
    let n = c.len();
    let mut out = String::from(r"\A(?s:");
    let mut i = 0;
    while i < n {
        let ch = c[i];
        i += 1;
        match ch {
            '*' => {
                while i < n && c[i] == '*' {
                    i += 1;
                }
                out.push_str(".*");
            }
            '?' => out.push('.'),
            '[' => {
                let mut j = i;
                if j < n && c[j] == '!' {
                    j += 1;
                }
                if j < n && c[j] == ']' {
                    j += 1;
                }
                while j < n && c[j] != ']' {
                    j += 1;
                }
                if j >= n {
                    out.push_str(r"\[");
                } else {
                    let mut stuff: String = c[i..j].iter().collect();
                    i = j + 1;
                    stuff = stuff.replace('\\', r"\\");
                    let mut cls = String::from("[");
                    let mut s = stuff.as_str();
                    if let Some(rest) = s.strip_prefix('!') {
                        cls.push('^');
                        s = rest;
                    } else if s.starts_with('^') || s.starts_with('[') {
                        cls.push('\\');
                    }
                    for (k, sc) in s.chars().enumerate() {
                        if (sc == '[' || sc == '&' || sc == '~' || sc == '|') || (sc == ']' && k == 0) {
                            cls.push('\\');
                        }
                        cls.push(sc);
                    }
                    cls.push(']');
                    out.push_str(&cls);
                }
            }
            _ => out.push_str(&regex::escape(&ch.to_string())),
        }
    }
    out.push_str(r")\z");
    out
}

pub fn fnmatchcase(name: &str, pat: &str) -> bool {
    let cache = FN.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(r) = cache.read().unwrap().get(pat) {
        return r.is_match(name);
    }
    let r = Arc::new(regex::Regex::new(&fn_source(pat)).unwrap_or_else(|_| regex::Regex::new(r"\A\z").unwrap()));
    let ok = r.is_match(name);
    cache.write().unwrap().insert(pat.to_string(), r);
    ok
}

// --- difflib.get_close_matches(word, possibilities, n=1, cutoff=0.6) ---------------

fn longest_match(a: &[char], b: &[char], alo: usize, ahi: usize, blo: usize, bhi: usize) -> (usize, usize, usize) {
    // no junk, no autojunk (the strings are short): plain longest common block,
    // earliest in a, then earliest in b, like SequenceMatcher.find_longest_match
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, &ch) in b.iter().enumerate() {
        b2j.entry(ch).or_default().push(j);
    }
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for i in alo..ahi {
        let mut newj2len: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(&a[i]) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = j2len.get(&(j.wrapping_sub(1))).copied().unwrap_or(0) + 1;
                let k = if j == 0 { 1 } else { k };
                newj2len.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = newj2len;
    }
    while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
        besti -= 1;
        bestj -= 1;
        bestsize += 1;
    }
    while besti + bestsize < ahi && bestj + bestsize < bhi && a[besti + bestsize] == b[bestj + bestsize] {
        bestsize += 1;
    }
    (besti, bestj, bestsize)
}

fn matching_chars(a: &[char], b: &[char]) -> usize {
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut total = 0;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, b, alo, ahi, blo, bhi);
        if k > 0 {
            total += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    total
}

/// SequenceMatcher(None, a, b).ratio()
pub fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let l = a.len() + b.len();
    if l == 0 {
        return 1.0;
    }
    2.0 * matching_chars(&a, &b) as f64 / l as f64
}

/// difflib.get_close_matches(word, possibilities, 1, 0.6)[0]
pub fn close_match<S: AsRef<str>>(word: &str, possibilities: &[S]) -> Option<String> {
    let mut best: Option<(f64, String)> = None;
    for x in possibilities {
        let x = x.as_ref();
        // SequenceMatcher(seq1=x, seq2=word)
        let r = ratio(x, word);
        if r >= 0.6 {
            let better = match &best {
                None => true,
                Some((s, bx)) => r > *s || (r == *s && x > bx.as_str()),
            };
            if better {
                best = Some((r, x.to_string()));
            }
        }
    }
    best.map(|b| b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("lib/a/b.ex", "lib/**"));
        assert!(glob_match("a.ex", "**/*.ex"));
        assert!(glob_match("x/y/a.ex", "**/*.ex"));
        assert!(!glob_match("lib/a.ex", "*.ex"));
        assert!(glob_match("x/node_modules/y", "**/node_modules/**"));
    }

    #[test]
    fn fnmatch() {
        assert!(fnmatchcase("handle_info", "handle_*"));
        assert!(!fnmatchcase("Handle", "handle*"));
        assert!(fnmatchcase("a1", "a[0-9]"));
        assert!(fnmatchcase("ab", "a[!0-9]"));
    }

    #[test]
    fn close() {
        assert_eq!(close_match("wehre", &["where", "when", "why"]).as_deref(), Some("where"));
        assert_eq!(close_match("zzz", &["where"]), None);
    }
}
