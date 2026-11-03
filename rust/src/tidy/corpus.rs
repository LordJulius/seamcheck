//! Every identifier in every file (project files + `[tidy] corpus` globs),
//! indexed once (tidy.Corpus): "is this name mentioned anywhere else?" is a
//! map lookup.

use std::collections::{BTreeSet, HashMap, HashSet};

use rayon::prelude::*;

use crate::config::list_or;
use crate::project::{read_text, Project};

pub const DEFAULT_CORPUS: &[&str] = &[
    "scripts/**", "config/**", "deploy/**", "priv/**", "rel/**", "bin/**", "tools/**", "Makefile", "justfile", "*.toml",
    "*.json", "*.yml", "*.yaml", ".github/**", "**/*.service", "**/*.sh", "**/*.tscn", "**/*.tres", "**/*.heex",
    "**/*.eex", "**/*.html", "**/AndroidManifest.xml", "**/res/layout/**", "**/*.qml", "**/*.ui",
];
pub const DEFAULT_CORPUS_SKIP: &[&str] = &[
    "**/node_modules/**", "**/_build/**", "**/deps/**", "**/build/**", "**/dist/**", "priv/static/**", "**/.git/**",
    "**/target/**", "**/.venv/**", "**/venv/**", "**/site-packages/**", "**/__pycache__/**", "**/.gradle/**",
    "**/media/**",
];
const SKIP_SUFFIX: &[&str] = &[".lock", "-lock.json", ".min.js", ".map", ".svg", ".png", ".jpg", ".webp", ".pdf"];

fn ident() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(&crate::pyre::translate(r"[A-Za-z_$][\w$]*[?!]?")).unwrap())
}

pub struct Corpus {
    /// identifier -> the files naming it (`seed!` also names `seed`)
    pub where_: HashMap<String, HashSet<String>>,
    /// file -> identifier -> count
    pub counts: HashMap<String, HashMap<String, usize>>,
}

/// One file's identifiers, as Python counts them: the first million
/// characters; a bundle (a line over 5,000 characters) names nothing.
fn count(text: &str) -> HashMap<String, usize> {
    let text = crate::pyre::take_chars(text, 1_000_000);
    let head = crate::pyre::take_chars(text, 200_000);
    let mut c: HashMap<String, usize> = HashMap::new();
    if !text.is_empty() && head.split('\n').any(|l| l.len() > 5000 && l.chars().count() > 5000) {
        return c;
    }
    for m in ident().find_iter(text) {
        *c.entry(m.as_str().to_string()).or_insert(0) += 1;
    }
    c
}

impl Corpus {
    pub fn new(p: &Project) -> Corpus {
        let mut files: BTreeSet<String> = p.files.iter().cloned().collect();
        let extra = list_or(crate::tidy::cfg(p), "corpus", DEFAULT_CORPUS);
        let mut exclude = p.exclude();
        exclude.extend(DEFAULT_CORPUS_SKIP.iter().map(|s| s.to_string()));
        files.extend(crate::walk::list_files(&p.root, &extra, &exclude));
        let todo: Vec<String> = files.into_iter().filter(|r| !SKIP_SUFFIX.iter().any(|s| r.ends_with(s))).collect();
        let per: Vec<(String, HashMap<String, usize>)> = todo
            .into_par_iter()
            .filter_map(|rel| {
                let text = read_text(&p.path(&rel))?;
                let c = count(&text);
                Some((rel, c))
            })
            .collect();
        let mut where_: HashMap<String, HashSet<String>> = HashMap::new();
        for (rel, c) in &per {
            for w in c.keys() {
                where_.entry(w.clone()).or_default().insert(rel.clone());
                if w.ends_with('?') || w.ends_with('!') {
                    where_.entry(w[..w.len() - 1].to_string()).or_default().insert(rel.clone());
                }
            }
        }
        Corpus { where_, counts: per.into_iter().collect() }
    }

    /// The files other than `own` naming `name` (and not `skip`ped), sorted.
    pub fn elsewhere(&self, name: &str, own: &str, skip: impl Fn(&str) -> bool) -> Vec<String> {
        let mut v: Vec<String> = self
            .where_
            .get(name)
            .map(|s| s.iter().filter(|r| r.as_str() != own && !skip(r)).cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    pub fn in_own(&self, name: &str, own: &str) -> usize {
        self.counts.get(own).and_then(|c| c.get(name)).copied().unwrap_or(0)
    }

    /// (file, how often it names any of `spell`) for each file naming `name`, sorted.
    pub fn mentions(&self, name: &str, spell: &[&str]) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self
            .where_
            .get(name)
            .map(|s| {
                s.iter()
                    .map(|r| {
                        let c = self.counts.get(r);
                        (r.clone(), spell.iter().map(|w| c.and_then(|c| c.get(*w)).copied().unwrap_or(0)).sum())
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
}
