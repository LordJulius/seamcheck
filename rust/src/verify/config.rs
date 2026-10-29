//! The verify config (docs/verify.md): `seamcheck.d/verify/<target>.toml` and
//! one `seamcheck.d/verify/<target>/<feature>.toml` per feature. Unknown keys
//! are errors, so a typo can't quietly switch a check off.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::config::toml_to_json;

pub const DIR: &str = "seamcheck.d/verify";

/// The step kinds and their keys. There is no x/y step: a control is reached by its id or not at all.
pub const ACTIONS: &[&str] = &["tap", "long_press", "type", "swipe", "back", "key", "open", "wait", "gone", "probe"];
const STEP_KEYS: &[&str] = &["at", "text", "dir", "expect", "timeout", "scroll", "why"];
const EXPECTS: &[&str] = &["changed", "unchanged", "any"];

const TARGET_KEYS: &[(&str, &[&str])] = &[
    ("target", &["driver", "why", "covers", "common"]),
    ("adb", &["device", "deny", "package", "activity", "release_only", "id_pattern"]),
    ("launch", &["start", "reset", "ready", "timeout"]),
    ("doctor", &["checks", "probe"]),
    ("evidence", &["dir", "logcat"]),
    ("runtime", &["fail_on", "prefix"]),
];
const PROBE_KEYS: &[&str] = &["id", "url", "token", "why"];
const FEATURE_KEYS: &[&str] = &["id", "title", "user", "screens", "capture", "keep", "steps"];
const RULE_KEYS: &[&str] = &["id", "assert", "when", "where", "each", "for", "key", "severity", "why", "act"];

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub file: PathBuf,
    pub raw: Value,
}

#[derive(Clone, Debug, Default)]
pub struct Step {
    pub action: String,
    /// The id the step works on (tap, long_press, type, swipe, wait, gone), the URI (open), key name, probe id.
    pub arg: String,
    pub at: String,
    pub text: String,
    pub dir: String,
    pub expect: String,
    pub timeout: f64,
    /// wait: swipe this container (by id) towards `dir` until the element shows.
    pub scroll: String,
}

impl Step {
    pub fn label(&self) -> String {
        match self.action.as_str() {
            "back" => "back".into(),
            "type" => format!("type:{}", self.arg),
            _ => format!("{}:{}", self.action, self.arg),
        }
    }
    /// Steps that name a control on screen.
    pub fn element(&self) -> Option<&str> {
        matches!(self.action.as_str(), "tap" | "long_press" | "type" | "swipe" | "wait" | "gone").then_some(self.arg.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct Feature {
    pub id: String,
    pub file: PathBuf,
    pub title: String,
    pub screens: Vec<String>,
    /// full (screenshots + tree + logcat) | tree (no screenshot, logcat only fail_on lines)
    pub capture: String,
    /// Ids (globs) whose subtrees are all a `tree` capture records.
    pub keep: Vec<String>,
    pub steps: Vec<Step>,
    pub rules: Value,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn strs(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(x)) => vec![x.clone()],
        Some(Value::Array(xs)) => xs.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

impl Target {
    pub fn get(&self, table: &str, key: &str) -> Option<&Value> {
        self.raw.get(table).and_then(|t| t.get(key))
    }
    pub fn str(&self, table: &str, key: &str) -> String {
        self.get(table, key).and_then(Value::as_str).unwrap_or("").to_string()
    }
    pub fn strs(&self, table: &str, key: &str) -> Vec<String> {
        strs(self.get(table, key))
    }
    pub fn driver(&self) -> String {
        self.str("target", "driver")
    }
    pub fn probes(&self) -> Vec<Value> {
        self.raw.get("probe").and_then(Value::as_array).cloned().unwrap_or_default()
    }
    pub fn feature_dir(&self) -> PathBuf {
        self.file.with_extension("")
    }
}

fn read_toml(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let t: toml::Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(toml_to_json(t))
}

fn unknown(table: &str, v: &Value, allowed: &[&str], out: &mut Vec<String>, file: &Path) {
    if let Some(m) = v.as_object() {
        for k in m.keys() {
            if !allowed.contains(&k.as_str()) {
                let hint = crate::glob::close_match(k, allowed).map(|m| format!(" (did you mean '{m}'?)")).unwrap_or_default();
                out.push(format!("{}: [{table}] unknown key '{k}'{hint}", file.display()));
            }
        }
    }
}

/// Every target under `root`, by name.
pub fn targets(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join(DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().is_some_and(|x| x == "toml")).then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    out.sort();
    out
}

pub fn load_target(root: &Path, name: &str) -> Result<(Target, Vec<String>), String> {
    let file = root.join(DIR).join(format!("{name}.toml"));
    if !file.exists() {
        let known = targets(root);
        return Err(format!("no verify target '{name}' ({} has: {})", DIR, if known.is_empty() { "none".into() } else { known.join(", ") }));
    }
    let raw = read_toml(&file)?;
    let mut problems = Vec::new();
    for (k, v) in raw.as_object().into_iter().flatten() {
        match TARGET_KEYS.iter().find(|(t, _)| t == k) {
            Some((t, keys)) => unknown(t, v, keys, &mut problems, &file),
            None if k == "probe" => {
                for p in v.as_array().into_iter().flatten() {
                    unknown("probe", p, PROBE_KEYS, &mut problems, &file);
                }
            }
            None => problems.push(format!("{}: unknown table [{k}]", file.display())),
        }
    }
    let t = Target { name: name.into(), file, raw };
    match t.driver().as_str() {
        "adb" => {
            if t.str("adb", "package").is_empty() {
                problems.push(format!("{}: [adb] needs a package", t.file.display()));
            }
        }
        "" => problems.push(format!("{}: [target] needs a driver (adb)", t.file.display())),
        other => problems.push(format!("{}: driver '{other}' is not built yet (adb is; http, pty and cdp are planned)", t.file.display())),
    }
    Ok((t, problems))
}

fn step(v: &Value, file: &Path, n: usize, problems: &mut Vec<String>) -> Option<Step> {
    let m = v.as_object()?;
    let actions: Vec<&String> = m.keys().filter(|k| ACTIONS.contains(&k.as_str())).collect();
    for k in m.keys() {
        if !ACTIONS.contains(&k.as_str()) && !STEP_KEYS.contains(&k.as_str()) {
            problems.push(format!("{}: step {n}: unknown key '{k}'", file.display()));
        }
    }
    if actions.len() != 1 {
        problems.push(format!("{}: step {n}: needs exactly one of {}", file.display(), ACTIONS.join(", ")));
        return None;
    }
    let action = actions[0].clone();
    let arg = match &m[&action] {
        Value::String(x) => x.clone(),
        Value::Bool(true) if action == "back" => String::new(),
        other => {
            problems.push(format!("{}: step {n}: {action} takes a string, not {other}", file.display()));
            return None;
        }
    };
    let observing = matches!(action.as_str(), "wait" | "gone" | "probe");
    let expect = s(v, "expect");
    let expect = if expect.is_empty() { if observing { "any".to_string() } else { "changed".to_string() } } else { expect };
    if !EXPECTS.contains(&expect.as_str()) {
        problems.push(format!("{}: step {n}: expect '{expect}' (one of {})", file.display(), EXPECTS.join(", ")));
    }
    if !s(v, "scroll").is_empty() && action != "wait" {
        problems.push(format!("{}: step {n}: scroll goes with wait (swipe a container until the element shows)", file.display()));
    }
    if action == "type" && s(v, "text").is_empty() {
        problems.push(format!("{}: step {n}: type needs text", file.display()));
    }
    Some(Step {
        action,
        arg,
        at: s(v, "at"),
        text: s(v, "text"),
        dir: s(v, "dir"),
        expect,
        timeout: v.get("timeout").and_then(Value::as_f64).unwrap_or(8.0),
        scroll: s(v, "scroll"),
    })
}

pub fn load_features(t: &Target) -> (Vec<Feature>, Vec<String>) {
    let mut problems = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(t.feature_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml") && p.file_name().is_some_and(|n| n != "proof.toml"))
        .collect();
    files.sort();
    let mut out: Vec<Feature> = Vec::new();
    for file in files {
        let raw = match read_toml(&file) {
            Ok(r) => r,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        for (k, v) in raw.as_object().into_iter().flatten() {
            match k.as_str() {
                "feature" => unknown("feature", v, FEATURE_KEYS, &mut problems, &file),
                "invariant" | "contract" => {
                    for r in v.as_array().into_iter().flatten() {
                        unknown(k, r, RULE_KEYS, &mut problems, &file);
                    }
                }
                _ => problems.push(format!("{}: unknown table [{k}]", file.display())),
            }
        }
        let f = raw.get("feature").cloned().unwrap_or(json!({}));
        let id = s(&f, "id");
        if id.is_empty() {
            problems.push(format!("{}: [feature] needs an id", file.display()));
            continue;
        }
        if out.iter().any(|x| x.id == id) {
            problems.push(format!("{}: feature '{id}' is already defined", file.display()));
            continue;
        }
        let capture = match s(&f, "capture").as_str() {
            "" => "full".to_string(),
            c @ ("full" | "tree") => c.to_string(),
            other => {
                problems.push(format!("{}: capture '{other}' (full or tree)", file.display()));
                "tree".to_string()
            }
        };
        let keep = strs(f.get("keep"));
        if capture == "tree" && keep.is_empty() {
            problems.push(format!("{}: capture = \"tree\" needs keep (the ids whose subtrees may be recorded)", file.display()));
        }
        let steps: Vec<Step> = f
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(n, v)| step(v, &file, n + 1, &mut problems))
            .collect();
        if steps.is_empty() {
            problems.push(format!("{}: feature '{id}' has no steps", file.display()));
        }
        let probes: Vec<String> = t.probes().iter().map(|p| s(p, "id")).collect();
        for st in steps.iter().filter(|st| st.action == "probe") {
            if !probes.contains(&st.arg) {
                problems.push(format!("{}: probe '{}' is not a [[probe]] of {}", file.display(), st.arg, t.name));
            }
        }
        let rules = json!({
            "runtime": t.raw.get("runtime").cloned().unwrap_or(json!({})),
            "invariant": raw.get("invariant").cloned().unwrap_or(json!([])),
            "contract": raw.get("contract").cloned().unwrap_or(json!([])),
        });
        out.push(Feature {
            id,
            file: file.clone(),
            title: s(&f, "title"),
            screens: strs(f.get("screens")),
            capture,
            keep,
            steps,
            rules,
        });
    }
    (out, problems)
}

/// The proof hash of a feature: its own file plus the target's, so a change to either makes a proof stale.
pub fn feature_hash(t: &Target, f: &Feature) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(std::fs::read(&t.file).unwrap_or_default());
    h.update(b"\0");
    h.update(std::fs::read(&f.file).unwrap_or_default());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
