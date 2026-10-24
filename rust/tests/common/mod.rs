//! Fixture helpers for the ported Python tests: a temp project from a file
//! map, loaded the way `seamcheck check` loads one.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use seamcheck::project::{Finding, Project};
use seamcheck::rules::run_rules;

pub fn write(root: &Path, files: &[(&str, &str)]) {
    for (name, body) in files {
        let p = root.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
}

pub fn load(root: &Path) -> Project {
    Project::load(root, &[])
}

/// rule id -> messages, in report order (`by.setdefault(f.rule, []).append(f.message)`)
pub fn by_rule(found: &[Finding]) -> BTreeMap<String, Vec<String>> {
    let mut by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in found {
        by.entry(f.rule.clone()).or_default().push(f.message.clone());
    }
    by
}

pub fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

pub fn commit_all(root: &Path, msg: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", msg]);
}

fn copy_dir(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// A copy of tests/fixtures/<name> (the parity harness runs both seamchecks
/// on the same directories), loaded.
pub fn fixture(name: &str) -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name), dir.path());
    let p = Project::load(dir.path(), &[]);
    (dir, p)
}

// --- the lint tests' helpers (test_lint.py's `run` / `lint`) ---------------------

/// textwrap.dedent
pub fn dedent(s: &str) -> String {
    let lines: Vec<&str> = s.split('\n').collect();
    let margin = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| if l.trim().is_empty() { "" } else { &l[margin..] })
        .collect::<Vec<_>>()
        .join("\n")
}

pub type Got = BTreeMap<String, Vec<(String, usize, String)>>;

/// {rule id: [(file, line, message)]} for a tiny project (test_lint.run).
pub fn run(files: &[(&str, &str)], rules: &str) -> Got {
    std::env::set_var("SEAMCHECK_RS_QUIET", "1");
    let dir = tempfile::tempdir().unwrap();
    let dedented: Vec<(String, String)> = files.iter().map(|(n, b)| (n.to_string(), dedent(b))).collect();
    let refs: Vec<(&str, &str)> = dedented.iter().map(|(n, b)| (n.as_str(), b.as_str())).collect();
    write(dir.path(), &refs);
    std::fs::write(
        dir.path().join("seamcheck.toml"),
        format!("[project]\nname = \"t\"\ninclude = [\"**\"]\n{}", dedent(rules)),
    )
    .unwrap();
    let p = Project::load(dir.path(), &[]);
    let mut out: Got = BTreeMap::new();
    for f in run_rules(&p, None, None) {
        out.entry(f.rule.clone()).or_default().push((f.file.clone(), f.line, f.message.clone()));
    }
    out
}

/// One lint rule `x` with options (test_lint.lint).
pub fn lint(name: &str, files: &[(&str, &str)], options: &str) -> Vec<(String, usize, String)> {
    let mut rules = format!("[[rule]]\nid = \"x\"\nkind = \"lint\"\nlint = \"{name}\"\n");
    if !options.is_empty() {
        rules.push_str(&format!("options = {options}\n"));
    }
    let got = run(files, &rules);
    assert!(!got.contains_key("config"), "{:?}", got.get("config"));
    got.get("x").cloned().unwrap_or_default()
}

pub fn rule(lint: &str) -> String {
    format!("[[rule]]\nid = \"{lint}\"\nkind = \"lint\"\nlint = \"{lint}\"\n")
}
