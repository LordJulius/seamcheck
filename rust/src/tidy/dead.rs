//! Dead and test-only functions (tidy.test_only, tidy.unused).

use serde_json::{json, Value};

use super::{Corpus, Tidy};

/// A product function's row: id, place, size (and the tests that call it).
#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub lines: usize,
    pub tests: Option<Vec<String>>,
}

impl Row {
    pub fn json(&self) -> Value {
        let mut v = json!({"id": self.id, "file": self.file, "line": self.line, "lines": self.lines});
        if let Some(t) = &self.tests {
            v["tests"] = json!(t);
        }
        v
    }
}

/// Product functions whose every caller is a test (and at least one exact),
/// whose name no product/script/config file mentions: dead code its tests
/// keep alive.
pub fn test_only(t: &Tidy, corpus: &Corpus) -> Vec<Row> {
    let g = t.g;
    let mut out = Vec::new();
    for f in t.funcs() {
        if f.kind != "func" || t.is_test(&f.file) || t.is_entry(f) {
            continue;
        }
        let cs: Vec<(&String, bool)> = g
            .redges
            .get(&f.id)
            .map(|v| v.iter().filter(|(c, _, _)| g.funcs.contains_key(c)).map(|(c, _, ex)| (c, *ex)).collect())
            .unwrap_or_default();
        if cs.is_empty() || !cs.iter().any(|(_, ex)| *ex) {
            continue;
        }
        if cs.iter().any(|(c, _)| !t.is_test(&g.func(c).unwrap().file)) {
            continue;
        }
        if corpus.in_own(&f.name, &f.file) > 1 || !corpus.elsewhere(&f.name, &f.file, |r| t.is_test(r)).is_empty() {
            continue;
        }
        let mut tests: Vec<String> = cs.iter().map(|(c, _)| (*c).clone()).collect();
        tests.sort();
        tests.dedup();
        out.push(Row { id: f.id.clone(), file: f.file.clone(), line: f.start, lines: f.end - f.start + 1, tests: Some(tests) });
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.lines));
    out
}

/// Product functions nothing calls or references, whose name appears in no
/// other file at all and only once in their own.
pub fn unused(t: &Tidy, corpus: &Corpus) -> Vec<Row> {
    let mut out = Vec::new();
    for f in t.g.dead() {
        if f.kind != "func" || t.is_test(&f.file) || t.is_entry(f) {
            continue;
        }
        if corpus.in_own(&f.name, &f.file) > 1 || !corpus.elsewhere(&f.name, &f.file, |_| false).is_empty() {
            continue;
        }
        out.push(Row { id: f.id.clone(), file: f.file.clone(), line: f.start, lines: f.end - f.start + 1, tests: None });
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.lines));
    out
}
