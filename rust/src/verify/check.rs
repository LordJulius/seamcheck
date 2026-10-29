//! `seamcheck verify --check`: the offline half, for the gate. No device.
//! Each finding is one `path:line: message` line (a `kind = "command"` rule
//! reads them), and the exit is 1 when there is any:
//!
//!   * a target or feature file that doesn't load (unknown keys, bad steps);
//!   * a step naming an id that its feature's `screens` files never mention
//!     (a renamed control);
//!   * a feature never proven (`draft`) or changed since its proof (`stale`);
//!   * with `--staged` / `--since`: a changed file under a target's `covers`
//!     that no feature's `screens` names.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::config::{feature_hash, load_features, load_target, targets, Feature, Target};
use crate::glob::match_any;

pub fn proof_path(t: &Target) -> PathBuf {
    t.feature_dir().join("proof.toml")
}

pub fn read_proof(t: &Target) -> toml::Table {
    std::fs::read_to_string(proof_path(t)).ok().and_then(|s| s.parse::<toml::Table>().ok()).unwrap_or_default()
}

/// proven | stale | draft
pub fn proof_state(t: &Target, f: &Feature, proof: &toml::Table) -> &'static str {
    match proof.get(&f.id).and_then(|e| e.get("hash")).and_then(|h| h.as_str()) {
        None => "draft",
        Some(h) if h == feature_hash(t, f) => "proven",
        Some(_) => "stale",
    }
}

pub fn write_proof(t: &Target, f: &Feature, hash: &str, at: &str, driven: &Value) -> Result<PathBuf, String> {
    let mut proof = read_proof(t);
    let mut e = toml::Table::new();
    e.insert("hash".into(), toml::Value::String(hash.into()));
    e.insert("at".into(), toml::Value::String(at.into()));
    for k in ["model", "version"] {
        if let Some(v) = driven[k].as_str().filter(|v| !v.is_empty()) {
            e.insert(k.into(), toml::Value::String(v.into()));
        }
    }
    proof.insert(f.id.clone(), toml::Value::Table(e));
    let body = toml::to_string(&proof).map_err(|e| e.to_string())?;
    let path = proof_path(t);
    let head = "# Written by `seamcheck verify --prove`: each feature that ran clean on a real app, by the hash of its\n\
                # file and the target's. A feature whose hash no longer matches is stale; one missing here is a draft.\n\n";
    std::fs::write(&path, format!("{head}{body}")).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned()
}

/// The line of a file where `needle` first appears (1 if nowhere).
fn line_of(path: &Path, needle: &str) -> usize {
    std::fs::read_to_string(path).ok().and_then(|t| t.lines().position(|l| l.contains(needle))).map(|i| i + 1).unwrap_or(1)
}

/// Does the source name this id? Literally, or as a stem the code fills in (`finance.tab.#{id}`, a doc's
/// `dives.card.<dive id>`, `:"#{prefix}.pin_pad"`).
fn mentioned(texts: &[String], id: &str) -> bool {
    if texts.iter().any(|t| t.contains(id)) {
        return true;
    }
    let segs: Vec<&str> = id.split('.').collect();
    (1..segs.len()).any(|k| {
        let stem = format!("{}.", segs[..k].join("."));
        let tail = format!("}}.{}", segs[k..].join("."));
        texts.iter().any(|t| t.contains(&format!("{stem}#{{")) || t.contains(&format!("{stem}<")) || t.contains(&tail))
    })
}

fn changed(root: &Path, staged: bool, since: Option<&str>) -> Option<Vec<String>> {
    let out = if staged {
        crate::gate::git(root, &["diff", "--cached", "--name-only"])?
    } else {
        let r = since?;
        let mut o = crate::gate::git(root, &["diff", "--name-only", r])?;
        o.push_str(&crate::gate::git(root, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default());
        o
    };
    Some(out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
}

pub fn main(root: &Path, staged: bool, since: Option<&str>) -> i32 {
    let changed = changed(root, staged, since);
    let mut findings: Vec<String> = Vec::new();
    for name in targets(root) {
        let (t, problems) = match load_target(root, &name) {
            Ok(x) => x,
            Err(e) => {
                findings.push(format!("{}/{name}.toml:1: {e}", super::config::DIR));
                continue;
            }
        };
        let (features, fproblems) = load_features(&t);
        for p in problems.iter().chain(&fproblems) {
            let (file, msg) = p.split_once(": ").unwrap_or(("", p));
            findings.push(format!("{}:1: {msg}", rel(root, Path::new(file))));
        }
        let proof = read_proof(&t);
        let target_rel = rel(root, &t.file);
        let common = t.strs("target", "common");
        for f in &features {
            let frel = rel(root, &f.file);
            let mut files = crate::walk::list_files(root, &f.screens, &[]);
            if !files.is_empty() {
                files.extend(crate::walk::list_files(root, &common, &[]));
            }
            if files.is_empty() && !f.screens.is_empty() {
                findings.push(format!("{frel}:{}: screens {:?} match no file", line_of(&f.file, "screens"), f.screens));
            }
            let texts: Vec<String> = files.iter().filter_map(|p| std::fs::read_to_string(root.join(p)).ok()).collect();
            if !texts.is_empty() {
                for st in &f.steps {
                    if let Some(id) = st.element() {
                        if !mentioned(&texts, id) {
                            findings.push(format!(
                                "{frel}:{}: step id '{id}' is in none of the feature's screens files (renamed or removed control?)",
                                line_of(&f.file, &format!("\"{id}\""))
                            ));
                        }
                    }
                }
            }
            // With a change in hand, proof findings only for features the change touches.
            let touched = match &changed {
                None => true,
                Some(ch) => ch.iter().any(|c| *c == frel || *c == target_rel || match_any(c, &f.screens)),
            };
            if touched {
                match proof_state(&t, f, &proof) {
                    "draft" => findings.push(format!("{frel}:1: feature '{}' is a draft: never proven (seamcheck verify --prove {} {})", f.id, t.name, f.id)),
                    "stale" => findings.push(format!("{frel}:1: feature '{}' changed since its proof (seamcheck verify --prove {} {})", f.id, t.name, f.id)),
                    _ => {}
                }
            }
        }
        let covers = t.strs("target", "covers");
        if let Some(ch) = &changed {
            for c in ch.iter().filter(|c| match_any(c, &covers)) {
                if !features.iter().any(|f| match_any(c, &f.screens)) {
                    findings.push(format!(
                        "{c}:1: a {} screen no verify feature covers: add one under {}/",
                        t.name,
                        rel(root, &t.feature_dir())
                    ));
                }
            }
        }
    }
    for f in &findings {
        println!("{f}");
    }
    (!findings.is_empty()) as i32
}
