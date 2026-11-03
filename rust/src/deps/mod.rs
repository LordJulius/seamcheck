//! Declared dependencies against what the code uses (deps.py).
//!
//! Every manifest under the root (mix.exs, package.json, build.gradle[.kts]
//! with libs.versions.toml, Cargo.toml, pyproject.toml / requirements*.txt,
//! go.mod) is read, and each dependency is looked for in the files that
//! manifest owns (its directory, minus nested projects of the same kind).
//! What a dependency is "called" in code comes from the installed package
//! where it can be found (mix `deps/<name>` module names, `node_modules/<name>`
//! bins, Gradle-cache jar packages, Python dist-info `top_level.txt`), and is
//! guessed from the name otherwise (the finding then says "guessed").
//!
//! Each dependency gets one status:
//!
//!   unused     no reference in code, tests, config or scripts
//!   test-only  a runtime dependency only tests use (belongs in the test scope)
//!   dead-only  used only inside functions nothing calls (goes with that code)
//!   peer       not referenced, but another declared dependency needs it
//!   plugin     not referenced, but it wires itself in at runtime (a service
//!              file, components in its Android manifest): check before removing
//!   config     referenced only from config / build files / scripts (tools)
//!   used       referenced from code; `files` says where (one file = removed with it)
//!
//! `analyze` is the analysis, `report` the text the `deps` command prints,
//! `rule` the `[[rule]] kind = "deps"` check (core._deps).

mod fsx;
mod gradle;
mod langs;
mod mix;
mod npm;

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{json, Value};

use crate::config::{id, list_or, s_or, Rule};
use crate::graph::Graph;
use crate::output::dumps;
use crate::project::{Finding, Project};
use crate::pyre::{rx, Rx};
use crate::rules::suppressed;

pub use fsx::main_checkout;

/// The statuses that need a look (the `deps` command fails on them).
pub const ACTIONABLE: [&str; 3] = ["unused", "test-only", "dead-only"];

const PRUNE: &[&str] = &[
    "node_modules",
    "deps",
    "_build",
    "build",
    ".git",
    "target",
    ".gradle",
    "dist",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".idea",
    ".cache",
    "out",
    ".next",
    ".svelte-kit",
    "coverage",
    ".godot",
    ".elixir_ls",
    ".cxx",
    "intermediates",
];

#[derive(Debug, Clone, Default)]
pub struct Dep {
    pub eco: String,
    pub name: String,
    /// relative path
    pub manifest: String,
    pub line: usize,
    /// runtime | dev | test | tool | bom
    pub scope: String,
    pub guessed: bool,
    pub status: String,
    /// (file, line) uses in product code
    pub code: Vec<(String, usize)>,
    pub tests: Vec<(String, usize)>,
    pub config: Vec<(String, usize)>,
    /// declares it optional / peer: must be declared here
    pub needed_by: Vec<String>,
    /// pulls it in anyway (declaring it is redundant)
    pub via: Vec<String>,
    /// wires itself in at runtime (service file, library manifest): why
    pub plugin: String,
    /// functions (ids) the code uses sit in, when all are dead
    pub dead: Vec<String>,
    /// its manifest's packages aren't installed here or in the main checkout
    pub uninstalled: bool,
}

/// Where a use was found.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Where {
    Code,
    Tests,
    Config,
}

impl Dep {
    pub(crate) fn new(eco: &str, name: &str, manifest: &str, line: usize, scope: &str) -> Dep {
        Dep {
            eco: eco.into(),
            name: name.into(),
            manifest: manifest.into(),
            line,
            scope: scope.into(),
            ..Default::default()
        }
    }

    /// The files of its code uses, sorted.
    pub fn files(&self) -> Vec<String> {
        self.code.iter().map(|(f, _)| f.clone()).collect::<BTreeSet<_>>().into_iter().collect()
    }

    pub(crate) fn push(&mut self, w: Where, rel: &str, line: usize) {
        let v = match w {
            Where::Code => &mut self.code,
            Where::Tests => &mut self.tests,
            Where::Config => &mut self.config,
        };
        v.push((rel.to_string(), line));
    }

    /// `_add`: a use, judged inside the manifest's project unless `w` says
    /// (a sub-project under deploy/ has code).
    pub(crate) fn add(&mut self, rel: &str, line: usize, w: Option<Where>) {
        let w = w.unwrap_or_else(|| {
            let d = fsx::dirname(&self.manifest);
            where_of(&fsx::relpath(rel, if d.is_empty() { "." } else { d }))
        });
        self.push(w, rel, line);
    }

    /// As `deps --json` prints it (the dataclass's fields, then `files`).
    pub fn to_json(&self) -> Value {
        let pairs = |v: &[(String, usize)]| v.iter().map(|(f, l)| json!([f, l])).collect::<Vec<_>>();
        json!({
            "eco": self.eco, "name": self.name, "manifest": self.manifest, "line": self.line, "scope": self.scope,
            "guessed": self.guessed, "status": self.status, "code": pairs(&self.code), "tests": pairs(&self.tests),
            "config": pairs(&self.config), "needed_by": self.needed_by, "via": self.via, "plugin": self.plugin,
            "dead": self.dead, "uninstalled": self.uninstalled, "files": self.files(),
        })
    }
}

pub(crate) fn lit(cell: &'static OnceLock<Rx>, pat: &str) -> &'static Rx {
    cell.get_or_init(|| Rx::lit(pat))
}

/// tests / config / code, by a path inside the manifest's project (`_where`).
pub(crate) fn where_of(rel: &str) -> Where {
    static TEST: OnceLock<Rx> = OnceLock::new();
    static CONFIG: OnceLock<Rx> = OnceLock::new();
    let test = lit(
        &TEST,
        r"(^|/)(test|tests|__tests__|spec|androidTest|testFixtures|test-support)(/|$)|\.(test|spec)\.[cm]?[jt]sx?$|_test\.(exs|go|py|rs)$|(^|/)test_[^/]*\.py$",
    );
    let config = lit(
        &CONFIG,
        r"(^|/)(config|scripts|\.github|deploy|gradle)/|\.config\.[cm]?[jt]s$|(^|/)(mix\.exs|build\.gradle(\.kts)?|settings\.gradle(\.kts)?|Makefile|Dockerfile|justfile|package\.json|Cargo\.toml|pyproject\.toml|setup\.py|go\.mod|tsconfig[\w.]*\.json)$|\.(toml|ya?ml|sh|ini|cfg)$",
    );
    if test.is_match(rel) {
        Where::Tests
    } else if config.is_match(rel) {
        Where::Config
    } else {
        Where::Code
    }
}

/// What every manifest reader shares: the root, the project's excludes, the
/// parsed files' blanked code (`_CODE`) and the main checkout.
pub(crate) struct Ctx<'a> {
    pub root: &'a Path,
    exclude: Vec<String>,
    g: &'a Graph,
    main: OnceLock<String>,
}

impl<'a> Ctx<'a> {
    pub fn excluded(&self, rel: &str) -> bool {
        !self.exclude.is_empty() && crate::glob::match_any(rel, &self.exclude)
    }

    /// `_read(root, rel)`.
    pub fn read(&self, rel: &str) -> String {
        fsx::read(&self.root.join(rel))
    }

    /// The blanked code of a file the project parsed (its first module's), or
    /// the file blanked as `family`.
    pub fn code(&self, rel: &str, raw: &str, family: &str) -> String {
        let parsed = self
            .g
            .by_file
            .get(rel)
            .and_then(|ms| ms.first())
            .and_then(|m| self.g.modules.get(m))
            .map(|m| m.code.join("\n"))
            .unwrap_or_default();
        if parsed.is_empty() {
            crate::scan::blank_text(raw, family, false)
        } else {
            parsed
        }
    }

    pub fn main(&self) -> &str {
        self.main.get_or_init(|| fsx::main_checkout(self.root))
    }

    /// `_installed`: the folder of a manifest's installed packages.
    pub fn installed(&self, mdir: &str, sub: &str) -> Option<String> {
        fsx::installed(self.root, self.main(), mdir, sub)
    }

    /// `_walk`: the files under `d` with these endings (or names) that the
    /// manifest owns: a sub-folder holding one of `manifests` is a nested
    /// project, and not its files.
    pub fn walk(&self, d: &str, exts: &[&str], manifests: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        fsx::walk(
            self.root,
            d,
            |dir, x| {
                if PRUNE.contains(&x) || x.starts_with('.') && x != ".github" {
                    return false;
                }
                let sub = self.root.join(fsx::join(dir, x));
                !manifests.iter().any(|m| sub.join(m).exists())
            },
            |dir, files| {
                for f in files {
                    if exts.iter().any(|e| f.ends_with(e)) {
                        let rel = fsx::join(dir, f);
                        if !self.excluded(&rel) {
                            out.push(rel);
                        }
                    }
                }
            },
        );
        out
    }
}

/// Every manifest under the root, sorted (`find_manifests`).
fn find_manifests(ctx: &Ctx) -> Vec<String> {
    static REQ: OnceLock<Rx> = OnceLock::new();
    let req = lit(&REQ, r"requirements[\w-]*\.txt");
    let mut out = Vec::new();
    fsx::walk(
        ctx.root,
        "",
        |_, x| !PRUNE.contains(&x) && !x.starts_with('.'),
        |dir, files| {
            for f in files {
                if reader(f).is_some() || req.fullmatch(f).is_some() {
                    let rel = fsx::join(dir, f);
                    if !ctx.excluded(&rel) {
                        out.push(rel);
                    }
                }
            }
        },
    );
    out.sort();
    out
}

type Reader = fn(&Ctx, &str) -> Vec<Dep>;

fn reader(base: &str) -> Option<Reader> {
    Some(match base {
        "mix.exs" => mix::read,
        "package.json" => npm::read,
        "build.gradle.kts" | "build.gradle" => gradle::read,
        "Cargo.toml" => langs::cargo,
        "pyproject.toml" => langs::python,
        "go.mod" => langs::gomod,
        _ => return None,
    })
}

const ENGINE_CALLBACKS: &str = r"^(_ready|_process|_physics_process|_input|_unhandled_input|_gui_input|_draw|_init|_enter_tree|_exit_tree|_notification|_to_string|_get|_set|_on_\w+|handle_\w+|init|mount|render|main|__\w+__|setup|test_\w+|call|start_link|child_spec|terminate|code_change)$";

/// The ids of functions nothing calls (core.dead).
fn dead_ids(g: &Graph) -> HashSet<String> {
    static CB: OnceLock<Rx> = OnceLock::new();
    let cb = lit(&CB, ENGINE_CALLBACKS);
    let mut called: HashSet<&str> = HashSet::new();
    let mut refs: HashSet<&str> = HashSet::new();
    for fid in g.funcs.keys() {
        let Some(f) = g.func(fid) else { continue };
        refs.extend(f.refs.iter().map(String::as_str));
        for (t, _, _) in g.edges.get(&f.id).map(|v| v.as_slice()).unwrap_or(&[]) {
            called.insert(t);
        }
    }
    let mut out = HashSet::new();
    for fid in g.funcs.keys() {
        let Some(f) = g.func(fid) else { continue };
        if f.kind == "top"
            || f.kind == "test"
            || called.contains(f.id.as_str())
            || refs.contains(f.name.as_str())
            || cb.is_match(&f.name)
        {
            continue;
        }
        out.insert(f.id.clone());
    }
    out
}

/// Every declared dependency with its status (see the module doc).
pub fn analyze(p: &Project, g: &Graph) -> Vec<Dep> {
    fsx::with_listings(|| analyze_here(p, g))
}

fn analyze_here(p: &Project, g: &Graph) -> Vec<Dep> {
    let ctx = Ctx { root: &p.root, exclude: p.exclude(), g, main: OnceLock::new() };
    let ignore: HashSet<String> = list_or(p.config.table("deps"), "ignore", &[]).into_iter().collect();
    let dead = dead_ids(g);
    let mut out: Vec<Dep> = Vec::new();
    for mrel in find_manifests(&ctx) {
        let read = reader(fsx::basename(&mrel)).unwrap_or(langs::python);
        out.extend(read(&ctx, &mrel).into_iter().filter(|d| !ignore.contains(&d.name)));
    }
    for d in &mut out {
        if !d.code.is_empty() {
            // all code uses inside functions nothing calls: the dep goes with them
            let mut funcs: BTreeSet<String> = BTreeSet::new();
            let mut live = false;
            for (rel, line) in &d.code {
                let f = g
                    .by_file
                    .get(rel)
                    .into_iter()
                    .flatten()
                    .filter_map(|m| g.modules.get(m).and_then(|m| m.func_at(*line)))
                    .find(|f| f.kind != "top");
                match f {
                    Some(f) if dead.contains(&f.id) => {
                        funcs.insert(f.id.clone());
                    }
                    _ => {
                        live = true;
                        break;
                    }
                }
            }
            d.status = if live { "used" } else { "dead-only" }.into();
            d.dead = if live { Vec::new() } else { funcs.into_iter().collect() };
        } else if !d.tests.is_empty() {
            d.status = if d.scope == "runtime" { "test-only" } else { "used" }.into();
        } else if !d.config.is_empty() {
            d.status = "config".into();
        } else if !d.needed_by.is_empty() {
            d.status = "peer".into();
        } else if !d.plugin.is_empty() {
            d.status = "plugin".into();
        } else {
            d.status = "unused".into();
        }
        if d.uninstalled && d.guessed && ACTIONABLE.contains(&d.status.as_str()) {
            // its real module / bin names are unknown: "unused" would be a guess
            d.status = "uninstalled".into();
        }
    }
    out
}

fn join_first(v: &[String], n: usize) -> String {
    v.iter().take(n).cloned().collect::<Vec<_>>().join(", ")
}

fn rank(status: &str) -> usize {
    ACTIONABLE.iter().position(|s| *s == status).unwrap_or(9)
}

/// The `deps` command's text: the actionable dependencies per manifest
/// (every one with `show_all`), or one dependency's uses (`name`).
pub fn report(deps: &[Dep], show_all: bool, name: Option<&str>) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(name) = name {
        for d in deps {
            if d.name == name || d.name.ends_with(&format!(":{name}")) {
                lines.push(format!(
                    "{}  ({}, {}, {}:{})  status: {}{}",
                    d.name,
                    d.eco,
                    d.scope,
                    d.manifest,
                    d.line,
                    d.status,
                    if d.guessed { "  (name guessed)" } else { "" }
                ));
                for (kind, uses) in [("code", &d.code), ("tests", &d.tests), ("config", &d.config)] {
                    for (rel, ln) in uses {
                        lines.push(format!("  {kind:<6} {rel}:{ln}"));
                    }
                }
                for n in &d.needed_by {
                    lines.push(format!("  needed by {n}"));
                }
            }
        }
        return if lines.is_empty() {
            format!("no dependency named {}", crate::pyre::repr(name))
        } else {
            lines.join("\n")
        };
    }
    let mut sorted: Vec<&Dep> = deps.iter().collect();
    sorted.sort_by(|a, b| (&a.manifest, rank(&a.status), &a.name).cmp(&(&b.manifest, rank(&b.status), &b.name)));
    let mut cur: Option<&str> = None;
    for d in sorted {
        if !show_all && !ACTIONABLE.contains(&d.status.as_str()) {
            continue;
        }
        if cur != Some(d.manifest.as_str()) {
            cur = Some(&d.manifest);
            let n: Vec<&Dep> = deps.iter().filter(|x| x.manifest == d.manifest).collect();
            let counts: Vec<String> = ["used", "unused", "test-only", "dead-only", "peer", "plugin", "config"]
                .iter()
                .filter(|s| n.iter().any(|x| x.status == **s))
                .map(|s| format!("{} {s}", n.iter().filter(|x| x.status == *s).count()))
                .collect();
            lines.push(format!("\n{}  ({} deps: {})", d.manifest, n.len(), counts.join(", ")));
        }
        let extra = match d.status.as_str() {
            "test-only" => format!(
                "only tests use it ({} files), declared {}",
                d.tests.iter().map(|(f, _)| f).collect::<HashSet<_>>().len(),
                d.scope
            ),
            "dead-only" => format!("only in uncalled code: {}", join_first(&d.dead, 4)),
            "peer" => format!("needed by {}", join_first(&d.needed_by, 4)),
            "config" => {
                let fs: Vec<String> =
                    d.config.iter().map(|(f, _)| f.clone()).collect::<BTreeSet<_>>().into_iter().collect();
                format!("only {}", join_first(&fs, 3))
            }
            "used" => {
                let fs = d.files();
                format!(
                    "{} file{}{}",
                    fs.len(),
                    if fs.len() != 1 { "s" } else { "" },
                    if fs.len() == 1 { format!(": {}", fs[0]) } else { String::new() }
                )
            }
            "plugin" => d.plugin.clone(),
            "unused" => format!(
                "no reference in code, tests, config or scripts{}",
                if d.via.is_empty() {
                    String::new()
                } else {
                    format!(
                        " (and {} pull{} it in anyway)",
                        join_first(&d.via, 3),
                        if d.via.len() == 1 { "s" } else { "" }
                    )
                }
            ),
            _ => String::new(),
        };
        lines.push(format!(
            "  {:<9} {:<40} {}{}",
            d.status,
            d.name,
            extra,
            if d.guessed { "  (name guessed)" } else { "" }
        ));
    }
    lines.join("\n").trim_start_matches('\n').to_string()
}

/// `seamcheck deps [NAME] [--all] [--json]`.
pub fn cli(root: &Path, config: &[String], args: &[String], all: bool, json_out: bool, t0: Instant) -> i32 {
    let p = Project::load(root, config);
    let g = Graph::load(&p);
    let found = analyze(&p, &g);
    if json_out {
        println!("{}", dumps(&Value::Array(found.iter().map(Dep::to_json).collect())));
    } else {
        println!("{}", report(&found, all, args.first().map(String::as_str)));
        if args.is_empty() {
            let n = found.iter().filter(|d| ACTIONABLE.contains(&d.status.as_str())).count();
            println!(
                "\nseamcheck deps: {} dependencies, {n} to look at  ({} ms)",
                found.len(),
                t0.elapsed().as_millis()
            );
        }
    }
    (found.iter().any(|d| ACTIONABLE.contains(&d.status.as_str())) && args.is_empty()) as i32
}

/// `[[rule]] kind = "deps"` (core._deps): dependencies with a status in
/// `report` (default unused / test-only / dead-only), minus `ignore`, one
/// finding each; a manifest whose packages aren't installed anywhere gets
/// one note instead. `check` calls this with the project's graph.
pub fn rule(p: &Project, g: &Graph, r: &Rule) -> Vec<Finding> {
    let rid = id(r);
    let want: Vec<String> = list_or(r, "report", &ACTIONABLE);
    let skip: Vec<String> = list_or(r, "ignore", &[]);
    let found = analyze(p, g);
    let mut out = Vec::new();
    // packages not installed (here or in the main checkout): one note per
    // manifest, never a finding per dependency (a fresh tree is not "unused")
    let mut pending: std::collections::BTreeMap<&str, Vec<&Dep>> = std::collections::BTreeMap::new();
    for d in &found {
        if d.status == "uninstalled" && !skip.contains(&d.name) {
            pending.entry(&d.manifest).or_default().push(d);
        }
    }
    for (man, ds) in &pending {
        let cmd = match ds[0].eco.as_str() {
            "mix" => "mix deps.get",
            "npm" => "npm ci",
            _ => "install its dependencies",
        };
        let dir = fsx::dirname(man);
        out.push(Finding::new(
            rid,
            "warn",
            man,
            ds[0].line,
            "",
            format!(
                "{} {} dependencies not checked: nothing installed here or in the main checkout (run `{cmd}` in {} for a real answer)",
                ds.len(),
                ds[0].eco,
                if dir.is_empty() { "." } else { dir }
            ),
        ));
    }
    for d in &found {
        if !want.contains(&d.status) || skip.contains(&d.name) {
            continue;
        }
        let raw: Vec<String> = p.text(&d.manifest).split('\n').map(String::from).collect();
        if suppressed(&raw, d.line, rid) {
            continue;
        }
        let why = match d.status.as_str() {
            "unused" => "no reference in code, tests, config or scripts".to_string(),
            "test-only" => format!("declared {} but only tests use it", d.scope),
            "dead-only" => format!("used only in functions nothing calls: {}", join_first(&d.dead, 3)),
            s => s.to_string(),
        };
        let msg = match r.get("message").and_then(|m| m.as_str()) {
            Some(m) => m.to_string(),
            None => format!("{} dependency {}: {why}", d.eco, d.name),
        };
        out.push(Finding::new(rid, s_or(r, "severity", "warn"), &d.manifest, d.line, "", msg).snip(if d.guessed {
            "(name guessed: install deps for a sure answer)".into()
        } else {
            String::new()
        }));
    }
    out
}

/// A pattern built from names at run time (dependency names escaped in).
pub(crate) fn dyn_rx(pat: &str) -> std::sync::Arc<Rx> {
    rx(pat).unwrap_or_else(|e| panic!("deps pattern {pat:?}: {e}"))
}

/// Python `re.escape` (the metacharacters escaped).
pub(crate) fn escape(s: &str) -> String {
    regex::escape(s)
}

/// Python's `\w` for one character (`[\p{L}\p{N}_]`, as pyre reads it).
pub(crate) fn py_word(c: char) -> bool {
    static W: OnceLock<regex::Regex> = OnceLock::new();
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    let mut b = [0u8; 4];
    W.get_or_init(|| regex::Regex::new(r"\A[\p{L}\p{N}]\z").unwrap()).is_match(c.encode_utf8(&mut b))
}

fn next_char(t: &str, i: usize) -> usize {
    t[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i + 1)
}

/// `finditer` of `(?<![class])body`: the body as a plain pattern, the
/// one-character lookbehind checked by `bad` (fancy-regex walks every
/// position of a big file; this is the same answer in one pass). `rescue`
/// is tried at a start the lookbehind refused: the pattern's alternatives
/// that the lookbehind doesn't guard (None when it guards the whole pattern).
pub(crate) fn find_after<'t>(
    body: &Rx,
    t: &'t str,
    bad: impl Fn(char) -> bool,
    guarded: impl Fn(&crate::pyre::Caps) -> bool,
    rescue: Option<&Rx>,
) -> Vec<crate::pyre::Caps<'t>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos <= t.len() {
        let Some(c) = body.search_at(t, pos) else { break };
        let s = c.start();
        let c = if guarded(&c) && t[..s].chars().next_back().is_some_and(&bad) {
            match rescue.and_then(|r| r.match_at(t, s)) {
                Some(r) => r,
                None => {
                    pos = next_char(t, s);
                    continue;
                }
            }
        } else {
            c
        };
        let e = c.end();
        out.push(c);
        pos = if e > s { e } else { next_char(t, e) };
    }
    out
}

/// `re.search(rf"(?<![\w-]){re.escape(name)}(?![\w-])", text)`.
pub(crate) fn has_word(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let edge = |c: char| py_word(c) || c == '-';
    let mut from = 0;
    while let Some(i) = text[from..].find(name).map(|i| i + from) {
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        if !before.is_some_and(edge) && !after.is_some_and(edge) {
            return true;
        }
        from = next_char(text, i);
    }
    false
}
