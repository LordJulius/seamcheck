//! Near-duplicate functions in different files (tidy.dups): 6-token
//! shingles, Jaccard, grouped by file pair: copies to fold into one helper.
//!
//! A shingle is kept as a 64-bit hash of its six tokens' hashes (Python
//! keeps the tuple): two different shingles colliding would shift one
//! similarity by one shingle in a few million, never seen.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use indexmap::IndexMap;
use rayon::prelude::*;
use serde_json::{json, Value};

use super::Tidy;
use crate::model::Func;
use crate::pyre::Rx;

#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub a: String,
    pub b: String,
    pub similarity: f64,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub files: [String; 2],
    pub pairs: Vec<Pair>,
    pub lines: usize,
}

impl Group {
    pub fn json(&self) -> Value {
        json!({"files": self.files,
               "pairs": self.pairs.iter().map(|p| json!({"a": p.a, "b": p.b, "similarity": p.similarity})).collect::<Vec<_>>(),
               "lines": self.lines})
    }
}

fn tok() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(&crate::pyre::translate(r"[A-Za-z_]\w*|\d+|[^\s\w]")).unwrap())
}

fn literal_start() -> &'static Rx {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Rx::lit(r"^\s*(?:return\s+|=\s*)?(?:\[|\{|%\w*(?:\.\w+)*\{|(?:listOf|mapOf|setOf|arrayOf|mutableListOf|mutableMapOf|List\.of|Map\.of|dict|frozenset)\s*\()")
    })
}

/// A function whose whole body is one literal table (a list of skill
/// structs, a map of labels): two such tables share their shape by design.
pub fn data_only(f: &Func) -> bool {
    let lines: Vec<&str> = f.code.split('\n').collect();
    let text = if lines.len() > 2 { lines[1..lines.len() - 1].join("\n") } else { String::new() };
    let Some(m) = literal_start().match_start(&text) else { return false };
    let b = text.as_bytes();
    let mut depth = 0i64;
    for i in m.end() - 1..b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return crate::pyre::strip(&text[i + 1..]).is_empty();
                }
            }
            _ => {}
        }
    }
    false
}

fn h64<T: Hash>(t: T) -> u64 {
    let mut s = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}

fn tokens(code: &str) -> Vec<u64> {
    tok().find_iter(code).map(|m| h64(m.as_str())).collect()
}

/// Python's round(x, 2): the nearest of the two-place decimals to the
/// double's exact value, an exact tie to the even one.
pub fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x)
}

/// Near-duplicate pairs across files, grouped by file pair, biggest first.
/// With `only`, just the pairs that include one of those ids.
pub fn dups(t: &Tidy, min_lines: usize, threshold: f64, only: Option<&HashSet<String>>) -> Vec<Group> {
    let g = t.g;
    let size = |f: &Func| f.end + 1 - f.start;
    let eligible = |f: &Func| f.kind != "top" && size(f) >= min_lines && !t.is_test(&f.file) && !data_only(f);
    let mut want: Option<HashMap<String, HashSet<u64>>> = None;
    if let Some(o) = only {
        // shingle only functions sharing most of their words with a changed one
        let mut w = HashMap::new();
        for fid in o {
            if let Some(f) = g.func(fid) {
                if eligible(f) {
                    let toks = tokens(&f.code);
                    if toks.len() >= 60 {
                        w.insert(fid.clone(), toks.into_iter().collect::<HashSet<u64>>());
                    }
                }
            }
        }
        if w.is_empty() {
            return vec![];
        }
        want = Some(w);
    }
    let funcs: Vec<&Func> = t.funcs().collect();
    let sh: Vec<Option<(String, HashSet<u64>)>> = funcs
        .par_iter()
        .map(|f| {
            if size(f) < min_lines || !eligible(f) {
                return None;
            }
            let toks = tokens(&f.code);
            if toks.len() < 60 {
                return None;
            }
            if let Some(w) = &want {
                if !w.contains_key(&f.id) {
                    let words: HashSet<u64> = toks.iter().copied().collect();
                    let close = w.values().any(|ws| {
                        let common = if words.len() < ws.len() {
                            words.iter().filter(|x| ws.contains(x)).count()
                        } else {
                            ws.iter().filter(|x| words.contains(x)).count()
                        };
                        common as f64 >= threshold * words.len().min(ws.len()) as f64
                    });
                    if !close {
                        return None;
                    }
                }
            }
            let shingles: HashSet<u64> = toks.windows(6).map(h64).collect();
            Some((f.id.clone(), shingles))
        })
        .collect();
    let sh: IndexMap<String, HashSet<u64>> = sh.into_iter().flatten().collect();
    let ids: Vec<&String> = sh.keys().collect();
    let mut idx: HashMap<u64, Vec<u32>> = HashMap::new();
    for (k, s) in sh.values().enumerate() {
        for x in s {
            idx.entry(*x).or_default().push(k as u32);
        }
    }
    let only_has = |k: u32| only.map_or(true, |o| o.contains(ids[k as usize]));
    let mut shared: HashMap<(u32, u32), usize> = HashMap::new();
    for fids in idx.values() {
        if fids.len() < 2 || fids.len() > 12 || (only.is_some() && !fids.iter().any(|k| only_has(*k))) {
            continue;
        }
        let mut sorted = fids.clone();
        sorted.sort_by(|a, b| ids[*a as usize].cmp(ids[*b as usize]));
        for i in 0..sorted.len() {
            for j in i + 1..sorted.len() {
                let (a, b) = (sorted[i], sorted[j]);
                if only.is_none() || only_has(a) || only_has(b) {
                    *shared.entry((a, b)).or_insert(0) += 1;
                }
            }
        }
    }
    let mut groups: IndexMap<(String, String), Group> = IndexMap::new();
    let mut pairs: Vec<((u32, u32), usize)> = shared.into_iter().filter(|(_, n)| *n >= 15).collect();
    pairs.sort_by(|x, y| (ids[x.0 .0 as usize], ids[x.0 .1 as usize]).cmp(&(ids[y.0 .0 as usize], ids[y.0 .1 as usize])));
    for ((a, b), _) in pairs {
        let (ia, ib) = (ids[a as usize], ids[b as usize]);
        let (fa, fb) = (g.func(ia).unwrap(), g.func(ib).unwrap());
        if fa.file == fb.file {
            continue;
        }
        let (sa, sb) = (&sh[ia], &sh[ib]);
        let common = sa.iter().filter(|x| sb.contains(x)).count();
        let j = common as f64 / (sa.len() + sb.len() - common) as f64;
        if j < threshold {
            continue;
        }
        let key = if fa.file <= fb.file { (fa.file.clone(), fb.file.clone()) } else { (fb.file.clone(), fa.file.clone()) };
        let gr = groups
            .entry(key.clone())
            .or_insert_with(|| Group { files: [key.0.clone(), key.1.clone()], pairs: vec![], lines: 0 });
        gr.pairs.push(Pair { a: ia.clone(), b: ib.clone(), similarity: round2(j) });
        gr.lines += (fa.end - fa.start).min(fb.end - fb.start) + 1;
    }
    let mut out: Vec<Group> = groups.into_values().collect();
    out.sort_by_key(|gr| std::cmp::Reverse(gr.lines));
    out
}
