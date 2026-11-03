//! `seamcheck init` (init.py): a first seamcheck.toml for a repo that has none.
//!
//! It writes only what it can see: the languages present, the folders their
//! code lives in (as `include` and as impact `areas`), the test layout when
//! it is not one of the usual ones, and the rule packs of the frameworks it
//! finds. Rules are the project's to write; the file shows one of each common
//! kind, commented out, to start from. `--hook` also installs the pre-commit
//! gate.

use std::collections::BTreeSet;
use std::path::Path;

use indexmap::IndexMap;

use crate::pyre::rx;

const LANG: &[(&str, &str)] = &[
    (".ex", "Elixir"),
    (".exs", "Elixir"),
    (".kt", "Kotlin"),
    (".kts", "Kotlin"),
    (".java", "Java"),
    (".ts", "TypeScript"),
    (".tsx", "TypeScript"),
    (".js", "JavaScript"),
    (".jsx", "JavaScript"),
    (".mjs", "JavaScript"),
    (".py", "Python"),
    (".pyw", "Python"),
    (".rs", "Rust"),
    (".go", "Go"),
    (".cs", "C#"),
    (".c", "C"),
    (".cpp", "C++"),
    (".cc", "C++"),
    (".h", "C++"),
    (".hpp", "C++"),
    (".qml", "QML"),
    (".gd", "GDScript"),
    (".swift", "Swift"),
    (".php", "PHP"),
];

/// folders that hold code no one maintains by hand
const SKIP_TOP: &[&str] = &[
    "vendor",
    "third_party",
    "thirdparty",
    "external",
    "dist",
    "build",
    "out",
    "bin",
    "obj",
    "node_modules",
    "deps",
    "_build",
    "target",
    "generated",
    ".github",
    "docs",
    "doc",
];

const EXAMPLES: &str = r#"
# Rules are yours: each one a contract no single file shows. Uncomment and
# adapt; `seamcheck check` runs them, `seamcheck gate` blocks a change on them.
#
# [[rule]]
# id = "writes-go-through-the-store"        # a pattern allowed only in one place
# kind = "forbid"
# pattern = 'db\.execute\('
# allow = ["src/store/**"]
# why = "the store invalidates the cache; a direct write leaves it stale"
#
# [[rule]]
# id = "handlers-reach-auth"                 # selected functions must reach a check
# kind = "reach"
# from = ["src/handlers/**:*"]
# to = ["require_user"]
# depth = 3
#
# [[rule]]
# id = "every-event-has-a-handler"           # ids defined in one place, used in another
# kind = "coverage"
# defined = { in = ["src/events.*"], pattern = 'EVENT_(\w+)' }
# used = { in = ["src/**"], pattern = 'on_(\w+)' }
# check = ["undefined", "unused"]

[gate]
# new code may not name these (a second path beside the old one)
forbid_added = ["legacy", "compat", "shim"]
"#;

pub const HOOK: &str = "#!/bin/sh
# seamcheck: may this commit land? (installed by `seamcheck init --hook`)
seamcheck gate --staged || { echo \"fix the above, or waive a line with 'seamcheck: allow <rule>'\"; exit 1; }
";

fn lang_of(ext: &str) -> Option<&'static str> {
    LANG.iter().find(|l| l.0 == ext).map(|l| l.1)
}

/// `os.path.splitext(p)[1]`.
fn splitext(p: &str) -> &str {
    let base = p.rsplit('/').next().unwrap_or(p);
    match base.rfind('.') {
        Some(i) if base[..i].chars().any(|c| c != '.') => &base[i..],
        _ => "",
    }
}

/// top-level folder ("" = the root) -> extension counts of its code files.
fn top_dirs(files: &[String]) -> IndexMap<String, IndexMap<String, usize>> {
    let mut out: IndexMap<String, IndexMap<String, usize>> = IndexMap::new();
    for rel in files {
        let ext = splitext(rel);
        if lang_of(ext).is_none() {
            continue;
        }
        let top = rel.split_once('/').map(|x| x.0).unwrap_or("");
        if SKIP_TOP.contains(&top.to_lowercase().as_str()) || top.starts_with('.') {
            continue;
        }
        *out.entry(top.to_string()).or_default().entry(ext.to_string()).or_default() += 1;
    }
    out
}

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
        (
            "compose",
            &["build.gradle", "build.gradle.kts", "libs.versions.toml"],
            r"\bcompose\s*=\s*true|androidx\.compose|org\.jetbrains\.compose",
        ),
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

/// An impact area's TOML key for a folder (`_key`).
fn key(d: &str) -> String {
    let k: String = d.chars().map(|ch| if ch.is_alphanumeric() || ch == '_' { ch } else { '_' }).collect();
    if !k.is_empty() && !k.starts_with(|c: char| c.is_ascii_digit()) {
        k
    } else {
        format!("_{k}")
    }
}

fn quoted<S: AsRef<str>>(v: &[S]) -> String {
    v.iter().map(|g| format!("\"{}\"", g.as_ref())).collect::<Vec<_>>().join(", ")
}

/// `os.path.abspath`.
pub fn abspath(p: &str) -> String {
    let joined = if p.starts_with('/') {
        p.to_string()
    } else {
        let cwd = std::env::current_dir().map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        format!("{cwd}/{p}")
    };
    let mut out: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            x => out.push(x),
        }
    }
    format!("/{}", out.join("/"))
}

/// The config text, and a summary of what was found (one line each).
pub fn render(root: &Path) -> (String, Vec<String>) {
    let exclude: Vec<String> = crate::walk::DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect();
    let files = crate::walk::list_files(root, &["**/*".to_string()], &exclude);
    let tops = top_dirs(&files);
    let mut langs: IndexMap<&str, usize> = IndexMap::new();
    for c in tops.values() {
        for (ext, n) in c {
            *langs.entry(lang_of(ext).unwrap_or("")).or_default() += n;
        }
    }
    let mut code_dirs: Vec<String> = tops.keys().filter(|d| !d.is_empty()).cloned().collect();
    code_dirs.sort();
    let root_files: usize = tops.get("").map(|c| c.values().sum()).unwrap_or(0);
    let mut include: Vec<String> = code_dirs.iter().map(|d| format!("{d}/**")).collect();
    if root_files > 0 {
        let exts: BTreeSet<String> = tops[""].keys().map(|e| format!("*{e}")).collect();
        include.extend(exts);
    }
    let tests: Vec<String> = code_dirs
        .iter()
        .filter(|d| {
            ["test", "tests", "spec", "specs", "__tests__", "e2e", "testing"].contains(&d.to_lowercase().as_str())
        })
        .cloned()
        .collect();
    let areas: Vec<&String> = code_dirs.iter().filter(|d| !tests.contains(d)).collect();
    let abs = abspath(&root.to_string_lossy());
    let name = abs.rsplit('/').next().unwrap_or("");
    let mut lines: Vec<String> = vec![
        "# seamcheck config (https://github.com/LordJulius/seamcheck). Written by `seamcheck init`;".into(),
        "# everything here is yours to change.".into(),
        String::new(),
        "[project]".into(),
        format!("name = \"{name}\""),
    ];
    let mut packs = detect_packs(root, &files);
    packs.push("security".into()); // (request data into a shell / SQL / eval: any web code)
    lines.push("# rules true for any project on these frameworks (seamcheck/packs; override one by its id)".into());
    lines.push(format!("packs = [{}]", quoted(&packs)));
    if !include.is_empty() {
        lines.push(format!("include = [{}]", quoted(&include)));
    }
    lines.push(r#"exclude = ["**/generated/**", "**/*.min.js"]"#.into());
    if areas.len() > 1 {
        lines.push("# how `seamcheck impact` groups what a change reaches".into());
        let a: Vec<String> = areas.iter().map(|d| format!("{} = [\"{d}/**\"]", key(d))).collect();
        lines.push(format!("areas = {{ {} }}", a.join(", ")));
    }
    let defaults = crate::gate::DEFAULT_TESTS;
    let unusual: Vec<&String> =
        tests.iter().filter(|d| !crate::glob::match_any(&format!("{d}/x.py"), defaults)).collect();
    if !unusual.is_empty() {
        let mut t: Vec<String> = defaults.iter().map(|s| s.to_string()).collect();
        t.extend(unusual.iter().map(|d| format!("{d}/**")));
        lines.extend([String::new(), "[tidy]".into(), format!("tests = [{}]", quoted(&t))]);
    }
    let text = lines.join("\n") + "\n" + EXAMPLES;
    // Counter.most_common: by count, ties in first-seen order
    let mut ranked: Vec<(&str, usize)> = langs.into_iter().collect();
    ranked.sort_by_key(|x| std::cmp::Reverse(x.1));
    let mut summary: Vec<String> = ranked.iter().map(|(lang, n)| format!("{n:>6} files  {lang}")).collect();
    summary.push(format!("rule packs: {}", packs.join(", ")));
    if ranked.is_empty() {
        let all: BTreeSet<&str> = LANG.iter().map(|l| l.1).collect();
        summary.push(format!(
            "no source files seamcheck reads (it reads {})",
            all.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    (text, summary)
}

/// Write .git/hooks/pre-commit (never over an existing hook).
pub fn install_hook(root: &str) -> String {
    let mut git = format!("{root}/.git");
    if Path::new(&git).is_file() {
        // a worktree: `gitdir: ...`
        let text = std::fs::read_to_string(&git).unwrap_or_default();
        if let Some(rest) = crate::pyre::strip(&text).strip_prefix("gitdir:") {
            let target = crate::pyre::strip(rest);
            git = abspath(&if target.starts_with('/') { target.to_string() } else { format!("{root}/{target}") });
        }
    }
    if !Path::new(&git).is_dir() {
        return "not a git checkout: no hook installed".into();
    }
    let hooks = format!("{git}/hooks");
    let _ = std::fs::create_dir_all(&hooks);
    let path = format!("{hooks}/pre-commit");
    if Path::new(&path).exists() {
        return format!("{path} exists: add `seamcheck gate --staged` to it yourself");
    }
    if let Err(e) = std::fs::write(&path, HOOK) {
        return format!("{path}: {e}");
    }
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(&path) {
        let mut perm = m.permissions();
        perm.set_mode(perm.mode() | 0o111);
        let _ = std::fs::set_permissions(&path, perm);
    }
    format!("installed {path}")
}

/// `seamcheck init [--hook] [--force]` in `root` (an absolute path).
pub fn main(root: &str, hook: bool, force: bool) -> i32 {
    let path = format!("{root}/seamcheck.toml");
    let (text, summary) = render(Path::new(root));
    if Path::new(&path).exists() && !force {
        println!("{path} exists (--force overwrites it)");
    } else {
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("seamcheck: {path}: {e}");
            return 1;
        }
        println!("wrote {path}");
        for s in summary {
            println!("  {s}");
        }
    }
    if hook {
        println!("{}", install_hook(root));
    }
    println!(
        "next: `seamcheck stats` (how much of the graph is exact), `seamcheck tidy` (dead code, copies, tests that test nothing), then write the first rule"
    );
    0
}
