//! `seamcheck init` (init.py): the rule packs a repo's manifests ask for.
//! The first-config writer joins these helpers in phase 4.

use std::collections::BTreeSet;
use std::path::Path;

use crate::pyre::rx;

/// A package.json depends on `dep` (or is it).
fn uses(text: &str, dep: &str) -> bool {
    let Ok(pkg) = serde_json::from_str::<serde_json::Value>(text) else { return false };
    let Some(o) = pkg.as_object() else { return false };
    o.get("name").and_then(|n| n.as_str()) == Some(dep)
        || ["dependencies", "devDependencies", "peerDependencies"]
            .iter()
            .any(|k| o.get(*k).and_then(|d| d.as_object()).is_some_and(|d| d.contains_key(dep)))
}

/// The rule packs (seamcheck/packs) whose framework a manifest declares.
/// QML needs its files too: a Qt Widgets app links Qt Quick for one view.
pub fn detect_packs(root: &Path, files: &[String]) -> Vec<String> {
    // a framework is in use when its manifest says so: pack -> (manifest names, what one holds)
    let frameworks: [(&str, &[&str], &str); 3] = [
        ("phoenix", &["mix.exs"], r"\{:phoenix\b"),
        ("compose", &["build.gradle", "build.gradle.kts", "libs.versions.toml"], r"\bcompose\s*=\s*true|androidx\.compose|org\.jetbrains\.compose"),
        ("qml", &["CMakeLists.txt"], r"Qt6?::Quick\b|\bqt_add_qml_module\b|\bQuick\b"),
    ];
    let mut found: BTreeSet<String> = BTreeSet::new();
    for rel in files {
        let base = rel.rsplit('/').next().unwrap_or(rel);
        if base != "package.json" && !frameworks.iter().any(|f| f.1.contains(&base)) {
            continue;
        }
        let Ok(bytes) = std::fs::read(root.join(rel)) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        if base == "package.json" {
            for p in ["react", "express", "hono"] {
                if uses(&text, p) {
                    found.insert(p.to_string());
                }
            }
        }
        for (pack, names, pat) in &frameworks {
            if names.contains(&base) && rx(pat).map(|r| r.search(&text).is_some()).unwrap_or(false) {
                found.insert(pack.to_string());
            }
        }
    }
    if !files.iter().any(|r| r.ends_with(".qml")) {
        found.remove("qml");
    }
    found.into_iter().collect()
}
