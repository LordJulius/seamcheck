//! The land gate (tidy.gate), phase 1: rule errors anywhere, names new
//! code may not use (`[gate] forbid_added`), and checks pinned to a file the
//! change split up. Dead code, copies, do-nothing tests and the exactness
//! guard need the call graph and come with phase 2.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::process::Command;

use serde_json::Value;

use crate::config::{list_or, s, Rule};
use crate::glob::{match_any, Globs};
use crate::project::{Finding, Mode, Project};
use crate::pyre::Rx;
use crate::rules::{run_rules, suppressed};

pub const DEFAULT_TESTS: &[&str] = &[
    "test/**", "tests/**", "**/test/**", "**/tests/**", "**/__tests__/**", "**/src/test/**", "**/src/androidTest/**",
    "**/*.test.*", "**/*.spec.*", "**/*_test.*", "**/test_*.py", "**/*Test.kt", "**/*Test.java", "**/*_spec.rb",
    "**/spec/**/*.rb", "**/test_*.rb", "Tests/**", "**/Tests/**", "**/*Tests.swift", "**/*Tests.kt", "**/*Tests.java",
    "**/*.Tests/**", "**/*.Specs/**", "**/*Tests.cs", "**/*Test.cs", "test-helpers/**", "**/test-helpers/**",
    "test_helpers/**", "**/test_helpers/**", "test-utils/**", "**/test-utils/**", "testutils/**", "**/testutils/**",
];

const MANIFESTS: &[&str] = &[
    "mix.exs", "mix.lock", "package.json", "package-lock.json", "build.gradle", "build.gradle.kts",
    "libs.versions.toml", "Cargo.toml", "Cargo.lock", "pyproject.toml", "go.mod",
];

pub type Lines = BTreeMap<String, BTreeSet<usize>>;

fn git(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).current_dir(root).output().ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn is_test(p: &Project, rel: &str) -> bool {
    match p.config.table("tidy").get("tests") {
        Some(_) => match_any(rel, &list_or(p.config.table("tidy"), "tests", &[])),
        None => tests_default().any(rel),
    }
}

fn tests_default() -> &'static Globs {
    static G: std::sync::OnceLock<Globs> = std::sync::OnceLock::new();
    G.get_or_init(|| Globs::new(DEFAULT_TESTS))
}

fn hunk() -> &'static Rx {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    R.get_or_init(|| Rx::lit(r"\+(\d+)(?:,(\d+))?"))
}

/// Lines added or changed since `ref` (committed, staged, unstaged), per
/// file; an untracked file counts whole. `staged`: only what is staged.
pub fn changed_lines(p: &Project, r: &str, staged: bool) -> Lines {
    let mut out: Lines = BTreeMap::new();
    let mut args = vec!["diff", "-U0", "--no-color"];
    if staged {
        args.push("--cached");
    }
    args.push(r);
    let Some(diff) = git(&p.root, &args) else { return out };
    let untracked: Vec<String> = if staged {
        vec![]
    } else {
        git(&p.root, &["ls-files", "--others", "--exclude-standard"])
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect()
    };
    let mut cur: Option<String> = None;
    for line in diff.split('\n') {
        if let Some(rest) = line.strip_prefix("+++ ") {
            cur = rest.strip_prefix("b/").map(String::from);
            let _ = rest;
        } else if line.starts_with("@@") {
            if let Some(c) = &cur {
                if let Some(m) = hunk().search(line) {
                    let start: usize = m.s(1).parse().unwrap_or(0);
                    let n: usize = m.get(2).map(|x| x.parse().unwrap_or(0)).unwrap_or(1);
                    out.entry(c.clone()).or_default().extend(start..start + n);
                }
                out.entry(c.clone()).or_default();
            }
        }
    }
    for rel in untracked {
        if let Some(t) = crate::project::read_text(&p.path(&rel)) {
            let n = t.matches('\n').count();
            out.entry(rel).or_default().extend(1..n + 2);
        }
    }
    out
}

/// Added lines whose text the same change removed somewhere (moved code),
/// and the files code moved out of.
pub fn moved_lines(p: &Project, r: &str, staged: bool, lines: &Lines) -> (HashMap<String, HashSet<usize>>, BTreeSet<String>) {
    let mut args = vec!["diff", "-U0", "--no-color", "-M"];
    if staged {
        args.push("--cached");
    }
    args.push(r);
    let mut out: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut sources = BTreeSet::new();
    let Some(diff) = git(&p.root, &args) else { return (out, sources) };
    // stripped text -> [(origin file, count)] in first-seen order
    let mut removed: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    let mut src: Option<String> = None;
    for line in diff.split('\n') {
        if let Some(rest) = line.strip_prefix("--- ") {
            src = rest.strip_prefix("a/").map(String::from);
        } else if let (Some(body), Some(sf)) = (line.strip_prefix('-'), &src) {
            let t = crate::pyre::strip(body);
            if !t.is_empty() {
                let pool = removed.entry(t.to_string()).or_default();
                match pool.iter_mut().find(|(f, _)| f == sf) {
                    Some(slot) => slot.1 += 1,
                    None => pool.push((sf.clone(), 1)),
                }
            }
        }
    }
    if removed.is_empty() {
        return (out, sources);
    }
    for (rel, lns) in lines {
        let Some(text) = crate::project::read_text(&p.path(rel)) else { continue };
        let text: Vec<&str> = text.split('\n').collect();
        for &ln in lns {
            let t = if ln >= 1 && ln <= text.len() { crate::pyre::strip(text[ln - 1]) } else { "" };
            if t.is_empty() {
                continue;
            }
            if let Some(pool) = removed.get_mut(t) {
                if pool.iter().map(|x| x.1).sum::<i64>() > 0 {
                    let slot = pool.iter_mut().find(|x| x.1 > 0).unwrap();
                    slot.1 -= 1;
                    let origin = slot.0.clone();
                    out.entry(rel.clone()).or_default().insert(ln);
                    if origin != *rel && t.chars().count() > 12 {
                        sources.insert(origin);
                    }
                }
            }
        }
    }
    (out, sources)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MovedRef {
    pub file: String,
    pub line: usize,
    pub moved_from: String,
}

/// Tests and scripts that name a file code just moved out of, by path.
pub fn moved_refs(p: &Project, sources: &BTreeSet<String>, changed: &HashSet<String>) -> Vec<MovedRef> {
    if sources.is_empty() {
        return vec![];
    }
    let names: Vec<(String, Vec<String>)> = sources
        .iter()
        .map(|s| {
            let short = match s.find('/') {
                Some(k) => s[k + 1..].to_string(),
                None => s.clone(),
            };
            (s.clone(), vec![s.clone(), short])
        })
        .collect();
    let mut out = Vec::new();
    for rel in &p.files {
        if changed.contains(rel)
            || !(is_test(p, rel) || rel.starts_with("scripts/") || rel.starts_with("test/") || rel.starts_with("tests/"))
        {
            continue;
        }
        let Some(raw) = crate::project::read_text(&p.path(rel)) else { continue };
        let raw: Vec<String> = raw.split('\n').map(String::from).collect();
        for (i, text) in raw.iter().enumerate() {
            for (s, forms) in &names {
                if forms.iter().any(|f| text.contains(f.as_str())) && !suppressed(&raw, i + 1, "moved-ref") {
                    out.push(MovedRef { file: rel.clone(), line: i + 1, moved_from: s.clone() });
                }
            }
        }
    }
    out
}

pub fn is_generated(p: &Project, rel: &str) -> bool {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    let rx = R.get_or_init(|| Rx::lit(r"(?i)@generated|\bGENERATED\b|DO NOT EDIT|generated by|auto-?generated"));
    let Some(t) = crate::project::read_text(&p.path(rel)) else { return false };
    rx.is_match(crate::pyre::take_chars(&t, 600))
}

fn version_only(p: &Project, rel: &str, changed: &BTreeSet<usize>) -> bool {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    let rx = R.get_or_init(|| {
        Rx::lit(r#"^\s*(?:version\s*[:=]|"version"\s*:|versionCode\s*=|versionName\s*=|@version\b|@app_version\b)"#)
    });
    let base = rel.rsplit('/').next().unwrap_or(rel);
    if changed.is_empty() || base.ends_with(".lock") || base.ends_with("-lock.json") {
        return false;
    }
    let Some(text) = crate::project::read_text(&p.path(rel)) else { return false };
    let text: Vec<&str> = text.split('\n').collect();
    changed.iter().all(|&ln| ln >= 1 && ln <= text.len() && rx.match_start(text[ln - 1]).is_some())
}

/// A changed file outside `[project] include` that a rule still reads.
fn rule_scoped(p: &Project, lines: &Lines) -> bool {
    for r in p.config.rules() {
        let kind = s(r, "kind").unwrap_or("forbid");
        let globs: Vec<String> = if kind == "lint" {
            let name = s(r, "lint").unwrap_or("");
            let def = crate::lint::get(name);
            if def.map(|d| d.repo_wide).unwrap_or(false) || crate::config::truthy(r.get("repo")) {
                match r.get("in") {
                    Some(_) => list_or(r, "in", &[]),
                    None => def.map(|d| d.default_in.iter().map(|s| s.to_string()).collect()).unwrap_or_default(),
                }
            } else {
                vec![]
            }
        } else if (kind == "forbid" || kind == "require") && (crate::config::truthy(r.get("repo")) || kind == "require") {
            list_or(r, "in", &[])
        } else if kind == "command" {
            list_or(r, "when", &["**/*"])
        } else {
            vec![]
        };
        if !globs.is_empty() && lines.keys().any(|rel| match_any(rel, &globs)) {
            return true;
        }
    }
    false
}

/// Identifiers one of whose parts (split at _ and camelCase) is a word, or
/// the word with a plain ending: fallback, fallbacks, compatibility.
pub struct NamePart {
    word: Rx,
}

impl NamePart {
    pub fn new(words: &[String]) -> NamePart {
        let alt: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
        NamePart { word: Rx::lit(&format!("(?i)(?:{})(?:s|es|ed|ing|ity|ibility|ible|able)?", alt.join("|"))) }
    }
    fn ident() -> &'static Rx {
        static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
        R.get_or_init(|| Rx::lit(r"\b[A-Za-z_]\w*\b"))
    }
    fn part() -> &'static Rx {
        static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
        R.get_or_init(|| Rx::lit(r"[A-Z]+(?![a-z])|[A-Z]?[a-z0-9]+"))
    }
    pub fn finditer<'t>(&self, text: &'t str) -> Vec<&'t str> {
        NamePart::ident()
            .find_iter(text)
            .into_iter()
            .map(|m| m.group0())
            .filter(|id| NamePart::part().findall(id).iter().any(|p| self.word.fullmatch(p).is_some()))
            .collect()
    }
    pub fn search<'t>(&self, text: &'t str) -> Option<&'t str> {
        self.finditer(text).into_iter().next()
    }
}

pub struct Forbidden {
    pub file: String,
    pub line: usize,
    pub word: String,
}

pub struct Report {
    pub changed_files: Vec<String>,
    pub rules: Vec<Finding>,
    pub forbidden: Vec<Forbidden>,
    pub moved_refs: Vec<MovedRef>,
}

impl Report {
    pub fn failed(&self) -> bool {
        !self.rules.is_empty() || !self.forbidden.is_empty() || !self.moved_refs.is_empty()
    }
}

pub fn gate(p: &mut Project, r: &str, staged: bool) -> Report {
    let lines = changed_lines(p, r, staged);
    let touched: BTreeSet<String> = lines.keys().filter(|rel| p.file_set.contains(*rel)).cloned().collect();
    let (moved, moved_from) = moved_lines(p, r, staged, &lines);
    let fresh: HashMap<String, BTreeSet<usize>> = lines
        .iter()
        .map(|(rel, ln)| {
            let mv = moved.get(rel);
            (rel.clone(), ln.iter().copied().filter(|x| mv.map(|m| !m.contains(x)).unwrap_or(true)).collect())
        })
        .collect();
    let manifest_changed: Vec<String> = lines
        .iter()
        .filter(|(rel, ln)| MANIFESTS.contains(&rel.rsplit('/').next().unwrap_or(rel)) && !version_only(p, rel, ln))
        .map(|(rel, _)| rel.clone())
        .collect();
    let mut report =
        Report { changed_files: lines.keys().cloned().collect(), rules: vec![], forbidden: vec![], moved_refs: vec![] };
    if touched.is_empty() && manifest_changed.is_empty() && !rule_scoped(p, &lines) {
        return report;
    }
    let all_rules: Vec<Rule> = p.config.rules().cloned().collect();
    let deps_ids: HashSet<String> =
        all_rules.iter().filter(|r| s(r, "kind") == Some("deps")).map(|r| crate::config::id(r).to_string()).collect();
    let skip: Vec<String> = if manifest_changed.is_empty() { deps_ids.iter().cloned().collect() } else { vec![] };
    let mode: HashMap<String, String> = all_rules
        .iter()
        .map(|r| (crate::config::id(r).to_string(), r.get("gate").map(crate::config::py_str).unwrap_or_else(|| "all".into())))
        .collect();
    let changed: HashSet<String> = lines.keys().cloned().collect();
    p.gate_changed = Some(changed.clone());
    let owners: HashSet<String> = manifest_changed
        .iter()
        .map(|m| match m.rfind('/') {
            Some(k) => m[..k].to_string(),
            None => String::new(),
        })
        .collect();
    let blocks = |f: &Finding| -> bool {
        let m = mode.get(&f.rule).map(|s| s.as_str()).unwrap_or("all");
        if m == "off" || f.message.contains(" not checked here: ") {
            return false;
        }
        if m == "changed" {
            return (f.severity == "error" || f.severity == "warn")
                && fresh.get(&f.file).map(|s| s.contains(&f.line)).unwrap_or(false);
        }
        if m == "touched" {
            return (f.severity == "error" || f.severity == "warn") && (f.file.is_empty() || lines.contains_key(&f.file));
        }
        f.severity == "error"
    };
    let found = run_rules(p, None, Some(&skip));
    report.rules = found
        .into_iter()
        .filter(|f| {
            blocks(f)
                && (!deps_ids.contains(&f.rule) || owners.contains(match f.file.rfind('/') {
                    Some(k) => &f.file[..k],
                    None => "",
                }))
        })
        .collect();

    // [gate] forbid_added: words new code may not name, in identifiers on changed lines
    let words: Vec<String> = match p.config.table("gate").get("forbid_added") {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => vec![],
    };
    if !words.is_empty() {
        let rx = NamePart::new(&words);
        let mut generated_names: HashSet<String> = HashSet::new();
        for rel in &p.files {
            if rel.to_lowercase().contains("generated") && is_generated(p, rel) {
                for w in rx.finditer(&p.joined(rel, Mode::Code)) {
                    generated_names.insert(w.to_string());
                }
            }
        }
        for rel in &touched {
            if is_test(p, rel) || is_generated(p, rel) {
                continue;
            }
            let code = p.text_for(rel, Mode::Code);
            let raw = p.text_for(rel, Mode::Raw);
            for &ln in fresh.get(rel).into_iter().flatten() {
                if ln >= 1 && ln <= code.len() {
                    if let Some(w) = rx.search(&code[ln - 1]) {
                        if !suppressed(&raw, ln, "gate") && !generated_names.contains(w) {
                            report.forbidden.push(Forbidden { file: rel.clone(), line: ln, word: w.to_string() });
                        }
                    }
                }
            }
        }
    }
    report.moved_refs = moved_refs(p, &moved_from, &changed);
    report
}
