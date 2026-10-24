//! `kind = "lint"`: structural checks that know a language or UI toolkit
//! (lint.py, lint_infra.py, lint_qml.py). The registry keeps Python's names,
//! default scopes, options and flags; a check that needs the call graph is
//! registered without a function and reported as deferred (phase 2).

pub mod css;
pub mod infra;
pub mod qml;
pub mod structure;
pub mod ui;

use std::sync::OnceLock;

use serde_json::{Map, Value};

use crate::project::Project;
use crate::pyre::Rx;
use crate::rules::RuleErr;

pub type Hits = Vec<(usize, String)>;
/// (rel, raw text, code text, options, project) -> (line, message) hits
pub type LintFn = fn(&str, &str, &str, &Map<String, Value>, &Project) -> Result<Hits, RuleErr>;

pub struct LintDef {
    pub name: &'static str,
    /// None: needs the call graph (phase 2)
    pub f: Option<LintFn>,
    pub default_in: &'static [&'static str],
    pub options: &'static [&'static str],
    /// reads files outside `[project] include`
    pub repo_wide: bool,
    /// a pure function of the one file (plus a named sibling)
    pub per_file: bool,
}

macro_rules! lint {
    ($name:expr, $f:expr, [$($i:expr),*], [$($o:expr),*], $repo:expr, $per:expr) => {
        LintDef { name: $name, f: $f, default_in: &[$($i),*], options: &[$($o),*], repo_wide: $repo, per_file: $per }
    };
}

pub static LINTS: &[LintDef] = &[
    // lint.py
    lint!("disabled-by-data", Some(ui::disabled_by_data), ["**/*.kt", "**/*.tsx", "**/*.jsx", "**/*.qml"], ["status", "busy", "targets"], false, true),
    lint!("inert-control", Some(ui::inert_control), ["**/*.kt", "**/*.tsx", "**/*.jsx", "**/*.qml"], [], false, false),
    lint!("qml-anchors-in-positioner", Some(ui::qml_anchors_in_positioner), ["**/*.qml"], [], false, true),
    lint!("kotlin-init-order", Some(ui::kotlin_init_order), ["**/*.kt"], ["depth"], false, true),
    lint!("id-kind-mix", Some(ui::id_kind_mix), ["**/*.kt", "**/*.ts", "**/*.tsx", "**/*.js", "**/*.ex"], ["named"], false, true),
    lint!("ets-owner", None, ["**/*.ex"], ["owners", "depth", "tests"], false, false),
    lint!("handle-info-catch-all", Some(ui::handle_info_catch_all), ["**/*.ex"], ["subscribe"], false, false),
    lint!("css-popup-clipped", Some(ui::css_popup_clipped), ["**/*.tsx", "**/*.jsx"], ["css"], false, false),
    lint!("css-overlay-order", Some(ui::css_overlay_order), ["**/*.css"], ["css", "overlays", "shell", "above", "min_shell_z"], false, false),
    lint!("elixir-rescue-debug", Some(ui::elixir_rescue_debug), ["**/*.ex"], [], false, true),
    // lint_infra.py
    lint!("shell-reexec-watch", Some(infra::shell_reexec_watch), ["**/*.sh", "deploy/**", "scripts/**"], [], true, true),
    lint!("shell-name-domain", Some(infra::shell_name_domain), ["**/*.sh", "deploy/**", "scripts/**"], ["names"], true, true),
    lint!("home-paths-mounted", Some(infra::home_paths_mounted), ["lib/**/*.ex"], ["compose", "service", "home", "allow"], false, false),
    lint!("auth-in-background", None, ["lib/**/*.ex"], ["auth", "roots", "depth"], false, false),
    lint!("stale-module-ref", Some(infra::stale_module_ref), ["lib/**/*.ex"], ["namespaces"], false, true),
    lint!("lockfile-sync", Some(infra::lockfile_sync), ["**/package-lock.json"], [], true, true),
    lint!("path-unit-oneshot", Some(infra::path_unit_oneshot), ["**/*.path"], ["settles"], true, true),
    lint!("css-braces", Some(infra::css_braces), ["**/*.css", "**/*.scss"], [], false, true),
    lint!("toolchain-pin", Some(infra::toolchain_pin), ["scripts/**", "**/*.sh", "**/build.gradle.kts"], [], true, false),
    lint!("test-reads-home", Some(infra::test_reads_home), ["test/**/*.exs", "**/*.test.mjs", "**/*.test.ts", "**/src/test/**/*.kt"], ["pattern"], false, true),
    lint!("shell-pkill-self", Some(infra::shell_pkill_self), ["**/*.sh", "deploy/**", "scripts/**", "bin/**", "tools/**"], [], true, true),
    // lint_qml.py
    lint!("qml-unknown-type", Some(qml::qml_unknown_type), ["**/*.qml"], ["types"], true, false),
    lint!("qml-singleton-import", Some(qml::qml_singleton_import), ["**/*.qml"], [], true, false),
    lint!("qml-js-shadows-type", Some(qml::qml_js_shadows_type), ["**/*.js", "**/*.mjs"], [], true, false),
];

pub fn get(name: &str) -> Option<&'static LintDef> {
    LINTS.iter().find(|l| l.name == name)
}

/// `kind = "essential"` (lint.essential): (file, line, message).
pub fn essential(project: &Project, r: &Map<String, Value>) -> Result<Vec<(String, usize, String)>, RuleErr> {
    ui::essential(project, r)
}

/// Is `kind = "essential"` ported (else the rule is deferred)?
pub fn essential_ported() -> bool {
    true
}

// --- shared helpers -------------------------------------------------------------------

/// `_line(text, pos)`: 1-based line of a byte offset.
pub(crate) fn line(text: &str, pos: usize) -> usize {
    let pos = pos.min(text.len());
    text.as_bytes()[..pos].iter().filter(|&&b| b == b'\n').count() + 1
}

/// `text[a:b]` with Python's forgiving bounds (clamped, empty when b < a).
pub(crate) fn sl(text: &str, a: usize, b: usize) -> &str {
    let a = crate::pyre::floor_char(text, a.min(text.len()));
    let b = crate::pyre::floor_char(text, b.min(text.len()));
    if b <= a {
        ""
    } else {
        &text[a..b]
    }
}

/// From i, the end of an argument/expression: the first stop char at depth 0.
pub(crate) fn balanced_end(text: &str, mut i: usize, stops: &str) -> usize {
    let b = text.as_bytes();
    let stops = stops.as_bytes();
    let mut depth = 0i64;
    while i < b.len() {
        let c = b[i];
        if matches!(c, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(c, b')' | b']' | b'}') {
            if depth == 0 {
                return i;
            }
            depth -= 1;
        } else if depth == 0 && stops.contains(&c) {
            return i;
        }
        i += 1;
    }
    i
}

pub(crate) fn opt_str(o: &Map<String, Value>, k: &str, d: &str) -> String {
    match o.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => crate::config::py_str(v),
        None => d.to_string(),
    }
}

pub(crate) fn opt_list(o: &Map<String, Value>, k: &str, d: &[&str]) -> Vec<String> {
    crate::config::list_or(o, k, d)
}

pub(crate) fn opt_int(o: &Map<String, Value>, k: &str, d: i64) -> i64 {
    match o.get(k) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(d),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(d),
        _ => d,
    }
}

/// A project-wide index built once even when every file's thread asks for it
/// at the same moment (`project.memo` alone may build it twice). `f` must not
/// use rayon (a worker blocked here could be handed a job that waits on it).
pub(crate) fn shared<T: Send + Sync + 'static>(p: &Project, key: &str, f: impl FnOnce() -> T) -> std::sync::Arc<T> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let cell = {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        p.memo(key, OnceLock::<std::sync::Arc<T>>::new)
    };
    cell.get_or_init(|| std::sync::Arc::new(f())).clone()
}

static CELLS: [OnceLock<Rx>; 128] = [const { OnceLock::new() }; 128];

/// A built-in pattern, compiled once: each call site owns a slot id
/// (ui 0..63, infra 64..99, qml 100..127).
pub(crate) fn rxs(id: usize, pat: &str) -> &'static Rx {
    let r = CELLS[id].get_or_init(|| Rx::lit(pat));
    debug_assert_eq!(r.pattern(), pat, "lint pattern slot {id} reused");
    r
}
