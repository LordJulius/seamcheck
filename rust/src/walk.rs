//! The file walker (core.list_files): what `rg --files --hidden --follow
//! -g '!.git' -g INCLUDE... -g '!EXCLUDE...'` lists (`.gitignore` honoured),
//! filtered again by seamcheck's own globs, each real file once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::glob::{glob_re, Globs};

pub const DEFAULT_EXCLUDE: &[&str] = &[
    "**/.git/**", "**/node_modules/**", "**/deps/**", "**/_build/**", "**/target/**", "**/.godot/**",
    "**/.claude/**", "**/dist/**", "**/build/**", "**/.venv/**", "**/__pycache__/**", "**/vendor/**",
    "**/third_party/**", "**/*.min.js",
];

/// DEFAULT_EXCLUDE less what the project's `include` names.
pub fn default_exclude(include: &[String]) -> Vec<String> {
    let named = include.join(" ");
    DEFAULT_EXCLUDE
        .iter()
        .filter(|d| {
            let head = d.trim_matches(|c| c == '*' || c == '/').split('/').next().unwrap_or("");
            !named.contains(head)
        })
        .map(|s| s.to_string())
        .collect()
}

/// A directory every file under which an exclude glob drops (`X/**`): not walked.
fn dead_dirs(exclude: &[String]) -> Vec<regex::Regex> {
    exclude
        .iter()
        .filter_map(|g| g.strip_suffix("/**"))
        .filter(|p| !p.is_empty())
        .map(|p| (*glob_re(p)).clone())
        .collect()
}

pub fn list_files(root: &Path, include: &[String], exclude: &[String]) -> Vec<String> {
    let mut ov = ignore::overrides::OverrideBuilder::new(root);
    let _ = ov.add("!.git");
    for g in include {
        let _ = ov.add(g);
    }
    for g in exclude {
        let _ = ov.add(&format!("!{g}"));
    }
    let overrides = ov.build().ok();
    let mut wb = ignore::WalkBuilder::new(root);
    wb.hidden(false).follow_links(true).add_custom_ignore_filename(".rgignore");
    if let Some(o) = overrides {
        wb.overrides(o);
    }
    let dead = dead_dirs(exclude);
    let root_buf = root.to_path_buf();
    wb.filter_entry(move |e| {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if let Ok(rel) = e.path().strip_prefix(&root_buf) {
                let rel = rel.to_string_lossy();
                if !rel.is_empty() && dead.iter().any(|r| r.is_match(&rel)) {
                    return false;
                }
            }
        }
        true
    });
    let inc = Globs::new(include);
    let exc = Globs::new(exclude);
    let mut out = Vec::new();
    for e in wb.build().flatten() {
        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Ok(rel) = e.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().to_string();
        if inc.any(&rel) && !exc.any(&rel) {
            out.push(rel);
        }
    }
    out.sort();
    one_copy(root, out)
}

/// Each real file once: a symlink inside the repo to a folder it already
/// holds would define everything twice. The path without a link wins.
fn one_copy(root: &Path, files: Vec<String>) -> Vec<String> {
    let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut seen: HashMap<PathBuf, (usize, bool)> = HashMap::new();
    let mut out: Vec<String> = Vec::new();
    for rel in files {
        let real = std::fs::canonicalize(root.join(&rel)).unwrap_or_else(|_| root.join(&rel));
        let direct = real == real_root.join(&rel);
        if let Some(&(idx, was_direct)) = seen.get(&real) {
            if direct && !was_direct {
                out[idx] = rel;
                seen.insert(real, (idx, true));
            }
            continue;
        }
        seen.insert(real, (out.len(), direct));
        out.push(rel);
    }
    out.sort();
    out
}
