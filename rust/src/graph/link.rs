//! Calls to edges (core._resolve_imports + core._build_edges).
//!
//! The order of the Python resolver is kept step for step, so each branch
//! can be checked against its original. Branches that need a language not
//! ported yet, or the type inference (receiver chains, supertypes), are
//! marked `lane:` with what they need; a call from a ported language never
//! reaches them (Elixir calls carry no chain, no bound locals, no package).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::gate::is_test;
use crate::graph::pylink::{self, PyIndex, EXTERNAL};
use crate::graph::Graph;
use crate::model::{Call, Func, Module};
use crate::project::{ext_of, Project};
use crate::pyre::Rx;

mod gd;

/// (callee id, exact)
type Targets = Vec<(String, bool)>;

const SELF: &[&str] = &["self", "this", "Self", "cls"];
const CTOR_NAMES: &[&str] = &["init", "__init__", "constructor", "__construct", "initialize"];

/// Which files a call can land in (core._FAMILIES): a Kotlin call never
/// runs a TypeScript function of the same name.
pub fn family(rel: &str) -> Vec<&str> {
    let ext = ext_of(rel);
    match ext {
        ".ex" | ".exs" => vec!["ex"],
        ".kt" | ".kts" | ".java" => vec!["jvm"],
        ".ts" | ".tsx" | ".jsx" | ".mjs" | ".cjs" => vec!["web"],
        ".js" => vec!["web", "qt"],
        ".qml" | ".c" | ".cpp" | ".cc" | ".cxx" | ".h" | ".hpp" | ".hh" => vec!["qt"],
        ".py" => vec!["py"],
        ".gd" => vec!["gd"],
        ".rs" => vec!["rs"],
        ".go" => vec!["go"],
        ".cs" => vec!["cs"],
        ".rb" | ".rake" | ".ru" => vec!["rb"],
        _ => vec![ext],
    }
}

pub(super) fn shares_family(a: &str, b: &str) -> bool {
    let fb = family(b);
    family(a).iter().any(|x| fb.contains(x))
}

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [OnceLock<Rx>; 8] = [const { OnceLock::new() }; 8];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

/// A qualifier that names a module (`Enum`, `Repo.Foo`), not an all-caps constant.
fn module_q(q: &str) -> bool {
    rx(0, r"(?![A-Z0-9_]+$)[A-Z]\w*(?:\.[A-Z]\w*)*$").match_start(q).is_some()
}

/// The modules an Elixir `use X` (or `use X, :controller`) imports into the
/// caller (core._injected_imports): the `import` lines of X's `__using__` /
/// `using do` block or its `def controller` (and the helpers it unquotes),
/// and of the modules those blocks `use` in turn.
fn injected_imports(g: &Graph, use_: &str, depth: i32) -> Vec<String> {
    let (name, which) = match use_.find(':') {
        Some(k) => (&use_[..k], &use_[k + 1..]),
        None => (use_, ""),
    };
    let Some(m) = g.modules.get(name) else { return Vec::new() };
    if depth < 0 {
        return Vec::new();
    }
    let lines = &m.code;
    let heads: Vec<std::sync::Arc<Rx>> = if !which.is_empty() {
        vec![crate::pyre::rx(&format!(r"^\s*def\s+{which}\b")).unwrap()]
    } else {
        vec![
            crate::pyre::rx(r"^\s*defmacro\s+__using__\b").unwrap(),
            crate::pyre::rx(r"^\s*using\b.*\bdo\s*$").unwrap(),
        ]
    };
    let indent = |s: &str| s.len() - s.trim_start().len();
    let span_of = |i: usize| -> (usize, usize) {
        let ind = indent(&lines[i]);
        for j in i + 1..lines.len() {
            let t = &lines[j];
            if !t.trim().is_empty() && indent(t) <= ind {
                return (i, j);
            }
        }
        (i, lines.len() - 1)
    };
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        if heads.iter().any(|h| h.match_start(ln).is_some()) {
            spans.push(span_of(i));
        }
    }
    // `unquote(html_helpers())` inside the block: that helper's imports too
    let unq = rx(1, r"unquote\((\w+)\(\)\)");
    for (a, b) in spans.clone() {
        let text = lines[a..=b].join("\n");
        for helper in unq.find_iter(&text) {
            let h = crate::pyre::rx(&format!(r"^\s*defp?\s+{}\b", helper.s(1))).unwrap();
            for (i, ln) in lines.iter().enumerate() {
                if h.match_start(ln).is_some() {
                    spans.push(span_of(i));
                }
            }
        }
    }
    let imp = rx(2, r"^\s*import\s+([A-Z]\w*(?:\.[A-Z]\w*)*|__MODULE__)");
    let usep = rx(3, r"^\s*use\s+([A-Z]\w*(?:\.[A-Z]\w*)*|__MODULE__)(?:\s*,\s*:(\w+))?");
    let expand = |q: &str| -> String {
        let head = q.split('.').next().unwrap_or(q);
        format!("{}{}", m.aliases.get(head).map(|s| s.as_str()).unwrap_or(head), &q[head.len()..])
    };
    let mut out = Vec::new();
    for (a, b) in spans {
        for ln in &lines[a..=b] {
            if let Some(im) = imp.match_start(ln) {
                let q = im.s(1);
                out.push(if q == "__MODULE__" { m.name.clone() } else { expand(q) });
            } else if let Some(us) = usep.match_start(ln) {
                let mut q = expand(us.s(1));
                if let Some(w) = us.get(2) {
                    q = format!("{q}:{w}");
                }
                out.extend(injected_imports(g, &q, depth - 1));
            }
        }
    }
    out
}

/// A module name for an adapter's import (core._resolve_where): "" when it
/// can't be found, EXTERNAL for a package from outside the project. Plain
/// names stand for themselves; the prefixed forms ("js:", "rs:", "inc:",
/// "go:") are their languages' lanes.
fn resolve_where(g: &Graph, py: &PyIndex, where_: &str) -> String {
    if where_.starts_with("py:") {
        return py.resolve(g, where_);
    }
    for pre in ["js:", "rs:", "inc:", "go:", "go?:"] {
        if where_.starts_with(pre) {
            return String::new(); // lane: that language's import resolution
        }
    }
    where_.to_string()
}

/// core._resolve_imports, for what the ported adapters produce.
fn resolve_imports(g: &mut Graph) {
    let names: Vec<String> = g.modules.keys().cloned().collect();
    for n in &names {
        let m = g.modules.get_mut(n).unwrap();
        if !m.package.is_empty() {
            let (pk, nm) = (m.package.clone(), m.name.clone());
            g.packages.entry(pk).or_default().push(nm);
        }
    }
    let py = PyIndex::new(g);
    type Resolved = (
        indexmap::IndexMap<String, String>,
        indexmap::IndexMap<String, (String, String)>,
        Vec<String>,
        Vec<String>,
        std::collections::BTreeSet<String>,
    );
    let mut done: Vec<(String, Resolved)> = Vec::new();
    for n in &names {
        let m = &g.modules[n];
        let mut ext: std::collections::BTreeSet<String> = Default::default();
        let mut aliases = indexmap::IndexMap::new();
        for (k, v) in &m.aliases {
            let r = resolve_where(g, &py, v);
            if r == EXTERNAL {
                ext.insert(k.clone());
            } else if !r.is_empty() {
                aliases.insert(k.clone(), r);
            }
        }
        let mut imported = indexmap::IndexMap::new();
        for (k, (v, nm)) in &m.imported {
            let r = resolve_where(g, &py, v);
            if r == EXTERNAL {
                ext.insert(k.clone());
            } else if !r.is_empty() {
                imported.insert(k.clone(), (r, nm.clone()));
            }
        }
        let uses: Vec<String> =
            m.uses.iter().map(|u| resolve_where(g, &py, u)).filter(|r| !r.is_empty() && r != EXTERNAL).collect();
        // (lane JS: `export * from "@scope/pkg"` marks the module `_ext_star`)
        let reexports: Vec<String> =
            m.reexports.iter().map(|u| resolve_where(g, &py, u)).filter(|r| !r.is_empty() && r != EXTERNAL).collect();
        let external = ext.into_iter().filter(|k| !aliases.contains_key(k) && !imported.contains_key(k)).collect();
        done.push((n.clone(), (aliases, imported, uses, reexports, external)));
    }
    for (n, (aliases, imported, uses, reexports, external)) in done {
        let m = g.modules.get_mut(&n).unwrap();
        m.aliases = aliases;
        m.imported = imported;
        m.uses = uses;
        m.reexports = reexports;
        m.external = external;
    }
    // lane JS: `export { a as b }` aliases and libraries re-exported wholesale
    pylink::python_names(g);
    // lane: Ruby link, Qt D-Bus / QML page props, QML context
    let mut add: Vec<(String, Vec<String>)> = Vec::new();
    for n in &names {
        let m = &g.modules[n];
        let mut got = Vec::new();
        for u in &m.using {
            for w in injected_imports(g, u, 2) {
                got.push(w);
            }
        }
        if !got.is_empty() {
            add.push((n.clone(), got));
        }
    }
    for (n, ws) in add {
        let m = g.modules.get_mut(&n).unwrap();
        for w in ws {
            if !m.uses.contains(&w) {
                m.uses.push(w);
            }
        }
    }
}

struct Resolver<'a> {
    g: &'a Graph,
    p: &'a Project,
    locals: HashMap<String, HashSet<String>>,
    tests: HashMap<String, bool>,
    module_js: HashMap<String, bool>,
    /// [project.groups]: "group:<id>" -> module
    groups: HashMap<String, String>,
    /// names project classes extend that the project doesn't declare (infer.Types.extended)
    extended: HashSet<String>,
    /// type name -> modules declaring it (infer.Types.by_type)
    by_type: HashMap<String, Vec<String>>,
    /// directory -> its Godot project root
    godot: HashMap<String, String>,
}

impl<'a> Resolver<'a> {
    fn is_test(&mut self, rel: &str) -> bool {
        if let Some(&t) = self.tests.get(rel) {
            return t;
        }
        let t = is_test(self.p, rel);
        self.tests.insert(rel.to_string(), t);
        t
    }

    /// core._scoped_language: a bare name resolves only through the file's
    /// own definitions and imports.
    fn scoped_language(&mut self, rel: &str, m: &Module) -> bool {
        let ext = ext_of(rel);
        if matches!(ext, ".js" | ".jsx" | ".mjs" | ".cjs") {
            if let Some(&h) = self.module_js.get(&m.file) {
                return h;
            }
            let raw = m.raw.join("\n");
            let hit = matches!(ext, ".mjs" | ".cjs")
                || crate::pyre::rx(
                    r#"(?m)\brequire\s*\(\s*['"]|\bmodule\.exports\b|^[ \t]*exports\.\w+\s*=|^[ \t]*(?:import|export)\b"#,
                )
                .unwrap()
                .search(&raw)
                .is_some()
                    && crate::pyre::rx(r"(?m)^[ \t]*(?:global|globalThis|window)\.\w+\s*=").unwrap().search(&raw).is_none();
            self.module_js.insert(m.file.clone(), hit);
            return hit;
        }
        matches!(
            ext,
            ".ex" | ".exs" | ".kt" | ".kts" | ".java" | ".ts" | ".tsx" | ".mts" | ".cts" | ".py" | ".go" | ".rs"
                | ".swift" | ".php" | ".rb" | ".rake" | ".ru"
        )
    }

    /// `name` is a parameter or local of `f`, so a bare `name()` calls that
    /// value (core._shadowed).
    fn shadowed(&mut self, f: &Func, name: &str) -> bool {
        if !self.locals.contains_key(&f.id) {
            let rb = matches!(ext_of(&f.file), ".rb" | ".rake" | ".ru");
            let mut loc: HashSet<String> = HashSet::new();
            let locals = rx(4, r"\b(?:val|var|let|const)\s+([A-Za-z_$][\w$]*)|[(,]\s*([A-Za-z_$][\w$]*)\s*:(?!:)");
            for m in locals.find_iter(&f.code) {
                let a = m.get(1).unwrap_or("");
                let b = m.get(2).unwrap_or("");
                let v = if !a.is_empty() { a } else if rb { "" } else { b };
                if !v.is_empty() {
                    loc.insert(v.to_string());
                }
            }
            let destr = rx(5, r"\b(?:val|var|let|const)\s*[\[{(]([^\]})=;]*)[\]})]\s*(?::[^=;]+)?=");
            let spread = rx(6, r"^\s*\.\.\.");
            let ident = rx(7, r"^[A-Za-z_$][\w$]*$");
            for dm in destr.find_iter(&f.code) {
                for part in dm.s(1).split(',') {
                    let part = spread.sub("", part);
                    let head = part.split('=').next().unwrap_or("");
                    let nm = head.rsplit(':').next().unwrap_or("").trim();
                    if ident.match_start(nm).is_some() {
                        loc.insert(nm.to_string());
                    }
                }
            }
            self.locals.insert(f.id.clone(), loc);
        }
        self.locals[&f.id].contains(name)
    }

    /// core._in_scope: `name` in a module (or a "pkg:" package), following
    /// what the module imports or re-exports.
    fn in_scope(&self, where_: &str, name: &str, depth: usize) -> Vec<&'a Func> {
        let g = self.g;
        if let Some(pk) = where_.strip_prefix("pkg:") {
            return g
                .packages
                .get(pk)
                .map(|ms| ms.iter().filter_map(|mn| g.lookup(mn, name)).collect())
                .unwrap_or_default();
        }
        if where_.starts_with("type:") {
            return Vec::new();
        }
        // lane: JS `default` exports, call-argument / assigned members
        if let Some(t) = g.lookup(where_, name) {
            return vec![t];
        }
        let Some(m) = g.modules.get(where_) else { return Vec::new() };
        if depth >= 8 {
            return Vec::new();
        }
        if let Some((w, n)) = m.imported.get(name) {
            let hits = self.in_scope(w, n, depth + 1);
            if !hits.is_empty() {
                return hits;
            }
        }
        for w in &m.reexports {
            // lane: JS drops class members here (_js_class_member)
            let hits = self.in_scope(w, name, depth + 1);
            if !hits.is_empty() {
                return hits;
            }
        }
        // lane: JS `export const a = b`, factory-made functions
        Vec::new()
    }

    /// core._defining_module: the module that defines what a named import refers to.
    fn defining_module(&self, where_: &str, name: &str, depth: usize) -> String {
        let g = self.g;
        let Some(m) = g.modules.get(where_) else { return String::new() };
        if depth > 4 {
            return String::new();
        }
        let sub = format!("{where_}.{name}");
        if g.modules.contains_key(&sub) {
            return sub;
        }
        if m.decls.iter().any(|d| d == name) || g.funcs.contains_key(&sub) {
            return where_.to_string();
        }
        if let Some((w, n)) = m.imported.get(name) {
            return self.defining_module(w, n, depth + 1);
        }
        // lane: JS namespace imports re-exported, CommonJS default objects
        for w in &m.reexports {
            let hit = self.defining_module(w, name, depth + 1);
            if !hit.is_empty() {
                return hit;
            }
        }
        String::new()
    }

    /// core._scope: the modules a file can see.
    fn scope(&self, m: &Module) -> HashSet<String> {
        let g = self.g;
        let fm = if m.owner.is_empty() { m } else { g.modules.get(&m.owner).unwrap_or(m) };
        let mut out: HashSet<String> = HashSet::new();
        out.insert(m.name.clone());
        out.insert(fm.name.clone());
        out.extend(fm.aliases.values().cloned());
        out.extend(fm.imported.values().map(|(w, _)| w.clone()));
        let pkg = (!fm.package.is_empty()).then(|| format!("pkg:{}", fm.package));
        for w in fm.uses.iter().chain(pkg.iter()) {
            if let Some(pk) = w.strip_prefix("pkg:") {
                out.extend(g.packages.get(pk).cloned().unwrap_or_default());
            } else if let Some(t) = w.strip_prefix("type:") {
                out.extend(g.short.get(t).cloned().unwrap_or_default());
            } else {
                out.insert(w.clone());
            }
        }
        let files: HashSet<&str> = out.iter().filter_map(|w| g.modules.get(w)).map(|m| m.file.as_str()).collect();
        for f in files {
            out.extend(g.by_file.get(f).cloned().unwrap_or_default());
        }
        out
    }

    /// infer.one_type: these functions are one type's. (lane: `_primary`,
    /// a type's parts across files)
    fn one_type(&self, fs: &[&Func]) -> bool {
        fs.iter().map(|f| f.module.as_str()).collect::<HashSet<_>>().len() == 1
    }

    /// One call's callees, as _build_edges's loop body finds them; empty:
    /// it reaches no project function.
    fn resolve(&mut self, f: &Func, m: &'a Module, c: &Call) -> Targets {
        let g = self.g;
        let q = c.qual.as_str();
        let ext = ext_of(&f.file);
        let mut targets: Vec<(&Func, bool)> = Vec::new();
        // lane (resolution): `<implicit>` members, C++ namespaces, receiver
        // chains through the type inference, JS lambda parameters, a bare
        // call in a class (own methods, constructors). None applies to a call
        // without a chain from a file that isn't .py / .ex / .exs.
        if c.chain.len() == 1 && c.chain[0] == "<implicit>" {
            // `.name(...)`: a static member of the type the context expects. (lane Swift:
            // _implicit_member reads the type; without one, the project's static functions
            // of that name are the guess, and none means a library's or a pattern)
            let stat: Vec<&Func> = g
                .by_name
                .get(&c.name)
                .map(|ids| ids.iter().filter_map(|id| g.func(id)).filter(|h| is_static(g, h)).collect())
                .unwrap_or_default();
            if stat.is_empty() {
                return Vec::new();
            }
            targets = stat.into_iter().map(|h| (h, false)).collect();
        }
        if q.is_empty() && targets.is_empty() && ext == ".gd" {
            // a bare call in a script: its own (or an inherited) method first
            match self.gd_bare(f, m, c) {
                gd::Bare::Nothing => return Vec::new(),
                gd::Bare::Targets(t) => targets = t,
                gd::Bare::Go => {}
            }
        }
        if !targets.is_empty() {
        } else if q == "super" && !m.parent.is_empty() {
            if let Some(t) = g.lookup(&m.parent, &c.name) {
                targets.push((t, true));
            }
        } else if q == "super" && matches!(ext, ".rb" | ".rake" | ".ru") {
            // lane (Ruby): the nearest ancestor's method
        } else if SELF.contains(&q)
            && g.lookup(&m.name, &c.name).is_some()
            && (q != "cls" || !m.owner.is_empty())
        {
            targets.push((g.lookup(&m.name, &c.name).unwrap(), true));
        } else if !q.is_empty() && m.external.contains(q) && !f.bound.contains_key(q) {
            return Vec::new(); // a library the file imports
        } else if !q.is_empty() {
            // each way to name the qualifier, most certain first
            let held = f.bound.get(q).filter(|s| !s.is_empty()).or_else(|| m.bound.get(q)).map(|s| s.as_str()).unwrap_or("");
            // a local the project says what it is ([project.groups]): exact
            if let Some(mn) = self.groups.get(held) {
                if let Some(t) = g.lookup(mn, &c.name) {
                    targets.push((t, true));
                }
            } else if let Some(ty) = held.strip_prefix("type:") {
                // a declared type: its module's function, or a Kotlin extension on it. A
                // type outside the project: nothing (a `String`'s `split` is not ours); a
                // project type that misses, or a library type a project class extends in a
                // dynamic language (`n: Node` may hold a project script): the usual ways
                let (mut hits, in_project) = pylink::typed_targets(g, m, ty, &c.name);
                let own: Vec<&Func> = hits.iter().copied().filter(|t| t.file == f.file).collect();
                if own.len() == 1 {
                    hits = own;
                }
                if !hits.is_empty() {
                    let one = hits.len() == 1;
                    targets = hits.into_iter().map(|t| (t, one)).collect();
                } else if !in_project
                    && !(matches!(ext, ".gd" | ".py" | ".js" | ".mjs" | ".cjs" | ".rb" | ".php" | ".qml")
                        && self.extended.contains(ty))
                {
                    return Vec::new();
                }
            }
            if targets.is_empty() {
                if let Some(a) = m.aliases.get(q) {
                    let hits = self.in_scope(a, &c.name, 0);
                    if hits.len() == 1 {
                        targets.push((hits[0], true));
                    }
                }
            }
            if targets.is_empty() && g.modules.contains_key(q) {
                if let Some(t) = g.lookup(q, &c.name) {
                    targets.push((t, true));
                }
            }
            if targets.is_empty() {
                if let Some((w, n)) = m.imported.get(q) {
                    let dm = self.defining_module(w, n, 0);
                    if let Some(t) = (!dm.is_empty()).then(|| g.lookup(&dm, &c.name)).flatten() {
                        targets.push((t, true));
                    }
                }
            }
            if targets.is_empty() {
                let mut names = vec![q.to_string()];
                if let Some(a) = m.aliases.get(q) {
                    names.push(a.rsplit('.').next().unwrap_or(a).to_string());
                }
                for nm in names {
                    let hits: Vec<&Func> = g
                        .short
                        .get(&nm)
                        .map(|ms| ms.iter().filter_map(|mn| g.lookup(mn, &c.name)).collect())
                        .unwrap_or_default();
                    if !hits.is_empty() {
                        let one = hits.len() == 1;
                        targets = hits.into_iter().map(|t| (t, one)).collect();
                        break;
                    }
                }
            }
        } else if ext == ".py"
            && f.kind != "top"
            && c.name.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
            && m_or_owner_imports(g, m, &c.name)
            && crate::pyre::rx(&format!(
                r"(?m)^[ \t]+(?:from[ \t]+[\w.]+[ \t]+)?import[ \t][^\n]*(?:(?<![\w.]){n}\b(?![ \t]+as\b)|\bas[ \t]+{n}\b)",
                n = regex::escape(&c.name)
            ))
            .unwrap()
            .search(&f.code)
            .is_some()
        {
            // `from ._termui_impl import get_pager_file` inside the function: that one, not
            // the module's own function of the same name (Python scoping: the local binding wins)
            let (w_, n_) = file_mod_of(g, m).imported[&c.name].clone();
            let hits = self.in_scope(&w_, &n_, 0);
            if hits.len() == 1 {
                targets.push((hits[0], true));
            } else {
                return Vec::new();
            }
        } else if self.shadowed(f, &c.name) {
            // a parameter or local holding a function: the helper the caller
            // itself defines when there is one, else a name guess at most
            let head = crate::pyre::rx(&format!(r"^\s*{}\s*:", regex::escape(&c.name))).unwrap();
            let inner: Vec<&Func> = g
                .by_name
                .get(&c.name)
                .map(|ids| ids.iter().filter_map(|id| g.func(id)).collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .filter(|h| {
                    h.file == f.file
                        && f.start <= h.start
                        && h.start <= f.end
                        && h.id != f.id
                        && !(h.start >= 1
                            && m.raw.get(h.start - 1).map(|l| head.match_start(l).is_some()).unwrap_or(false))
                })
                .collect();
            if inner.len() == 1 {
                targets.push((inner[0], true));
            } else if inner.is_empty() && self.scoped_language(&f.file, m) {
                // lane (JS / Python): a parameter is typed by what its callers pass
                return Vec::new();
            }
        } else {
            // own module, then a name imported by itself, then modules and
            // packages imported whole, then the rest of its own package
            let smod = if m.owner.is_empty() { m } else { g.modules.get(&m.owner).unwrap_or(m) };
            let mut hits = self.in_scope(&smod.name, &c.name, 0);
            if hits.is_empty() {
                if let Some((w, n)) = smod.imported.get(&c.name) {
                    hits = self.in_scope(w, n, 0);
                }
            }
            if hits.is_empty() {
                for w in &smod.uses {
                    hits = self.in_scope(w, &c.name, 0);
                    if !hits.is_empty() {
                        break;
                    }
                }
            }
            if hits.is_empty() && !smod.package.is_empty() {
                hits = self.in_scope(&format!("pkg:{}", smod.package), &c.name, 0);
            }
            if hits.is_empty() && ext == ".py" {
                // `Foo(...)` builds a Foo: its __init__
                let cls = pylink::py_class(g, m, &c.name, 0);
                match (!cls.is_empty()).then(|| g.lookup(&cls, "__init__")).flatten() {
                    Some(t) => hits = vec![t],
                    None if !cls.is_empty() => return Vec::new(), // a project class with no __init__ of its own
                    None => {}
                }
            }
            // lane (Go): build variants
            let mut seen = HashSet::new();
            hits.retain(|t| seen.insert(t.id.clone()));
            let exact = hits.len() == 1 || self.one_type(&hits);
            targets = hits.into_iter().map(|t| (t, exact)).collect();
            if targets.is_empty() && m.external.contains(&c.name) {
                return Vec::new();
            }
            if targets.is_empty() && self.scoped_language(&f.file, m) && !m.imported.contains_key(&c.name) {
                // a bare call reaches only what is local, imported or in its
                // package: nothing in scope means a library or the language's own
                return Vec::new();
            }
        }
        if targets.is_empty() && !q.is_empty() && ext == ".py" && c.name.chars().next().is_some_and(|ch| ch.is_uppercase()) {
            let cls = pylink::py_class(g, m, &format!("{q}.{}", c.name), 0);
            if !cls.is_empty() {
                // `sessions.Session()`: a class through its module builds one, so its __init__ runs
                match g.lookup(&cls, "__init__") {
                    Some(t) => targets.push((t, true)),
                    None => return Vec::new(),
                }
            }
        }
        if targets.is_empty() && !q.is_empty() && module_q(q) && q != "Self" {
            // a module qualifier that misses: outside the project, not a guess
            return Vec::new();
        }
        // lane: Rust module paths, GetType / getClass, Rust std methods, C's
        // linker rule and macros, C# / C++ / JS bare-name checks
        if targets.is_empty() {
            targets = self.guess(f, m, c);
        }
        // virtual dispatch: a call landing on a parent's function can run a
        // subclass override (lane (types): receivers, argument counts)
        let mut more: Vec<(&Func, bool)> = Vec::new();
        for (t, exact) in &targets {
            if CTOR_NAMES.contains(&t.name.as_str())
                || (t.name.chars().next().is_some_and(|ch| ch.is_uppercase()) && c.name == t.name)
            {
                continue;
            }
            for sub in g.children.get(&t.module).map(|v| v.as_slice()).unwrap_or(&[]) {
                if let Some(o) = g.func(&format!("{sub}.{}", t.name)) {
                    more.push((o, *exact));
                }
            }
        }
        if ext == ".py"
            && c.line > 0
            && c.line <= m.raw.len()
            && m.raw[c.line - 1].trim_start().starts_with('@')
            && targets.len() == 1
            && targets[0].1
        {
            // `@app.route("/")`: route() runs, then the function it returns gets the decorated one
            if let Some(inner) = pylink::returned_closure(g, targets[0].0) {
                more.push((inner, true));
            }
        }
        let mut seen: HashSet<String> = targets.iter().map(|(t, _)| t.id.clone()).collect();
        let mut out: Targets = targets.iter().map(|(t, e)| (t.id.clone(), *e)).collect();
        for (o, e) in more {
            if seen.insert(o.id.clone()) {
                out.push((o.id.clone(), e));
            }
        }
        out
    }

    /// A method on some object, or an import we can't see: every function
    /// with that name is a candidate (a fuzzy edge), narrowed where the
    /// caller's scope settles it.
    fn guess(&mut self, f: &Func, m: &Module, c: &Call) -> Vec<(&'a Func, bool)> {
        let g = self.g;
        let q = c.qual.as_str();
        let in_test = self.is_test(&f.file);
        let mut cands: Vec<&Func> = Vec::new();
        for id in g.by_name.get(&c.name).map(|v| v.as_slice()).unwrap_or(&[]) {
            let Some(h) = g.func(id) else { continue };
            if h.kind != "top" && shares_family(&f.file, &h.file) && (in_test || !self.is_test(&h.file)) {
                cands.push(h);
            }
        }
        // lane (C#): type-argument counts
        let vague = cands.len() > 8;

        // lane (JS): scoped patches, test files no module imports
        if !q.is_empty() && c.line > 0 && c.line <= m.raw.len() && m.raw[c.line - 1].contains(&format!("{q}::{}", c.name)) {
            let named: Vec<&Func> = cands
                .iter()
                .copied()
                .filter(|h| {
                    format!("/{}", h.file).contains(&format!("/{q}/"))
                        || h.module.rsplit("::").next() == Some(q)
                        || h.module.ends_with(&format!(".{q}"))
                })
                .collect();
            if named.len() == 1 {
                return vec![(named[0], true)];
            }
        }
        let shadowed = self.shadowed(f, &c.name);
        let seen_mods = if shadowed { HashSet::new() } else { self.scope(m) };
        let mut mine: Vec<&Func> = cands.iter().copied().filter(|h| seen_mods.contains(&h.module)).collect();
        if ext_of(&f.file) == ".py" {
            mine.retain(|h| g.modules.get(&h.module).map(|hm| !hm.owner.is_empty()).unwrap_or(false));
        }
        if !q.is_empty() {
            // an untyped receiver can hold anything: the receiver's type settles it
            mine.clear();
        }
        let own: Vec<&Func> = if matches!(ext_of(&f.file), ".cs" | ".go") {
            Vec::new()
        } else {
            cands.iter().copied().filter(|h| h.file == f.file && !h.recv.is_empty()).collect()
        };
        if let Some(t) = self.gd_engine_script(f, m, c, &cands) {
            return vec![t];
        }
        // lane (Kotlin): an imported extension
        if own.len() == 1 && !q.is_empty() && cands.len() > 1 {
            return vec![(own[0], true)];
        }
        if mine.len() == 1 && !q.is_empty() {
            return vec![(mine[0], true)];
        }
        if mine.len() > 1
            && mine.len() <= 4
            && !q.is_empty()
            && mine.iter().map(|h| h.module.as_str()).collect::<HashSet<_>>().len() == 1
        {
            return mine.into_iter().map(|h| (h, true)).collect();
        }
        if vague || cands.len() > 8 {
            return Vec::new(); // a very common name: too vague to be a useful edge
        }
        cands.into_iter().map(|h| (h, false)).collect()
    }
}

/// core._is_static: declared `static` / `class func`.
fn is_static(g: &Graph, h: &Func) -> bool {
    let line = g.modules.get(&h.module).and_then(|m| m.raw.get(h.start.wrapping_sub(1))).map(|s| s.as_str()).unwrap_or("");
    crate::pyre::rx(r"\b(?:static|class)\s+(?:func|var|let)\b").unwrap().search(line).is_some()
}

fn file_mod_of<'a>(g: &'a Graph, m: &'a Module) -> &'a Module {
    if m.owner.is_empty() {
        m
    } else {
        g.modules.get(&m.owner).unwrap_or(m)
    }
}

fn m_or_owner_imports(g: &Graph, m: &Module, name: &str) -> bool {
    file_mod_of(g, m).imported.contains_key(name)
}

/// core.walk_order: innermost function first (shortest span), so a call in
/// a nested function is resolved where it is made.
pub fn walk_order(g: &Graph) -> Vec<String> {
    let mut ids: Vec<(usize, &String)> =
        g.funcs.keys().map(|id| (g.func(id).map(|f| f.end - f.start).unwrap_or(0), id)).collect();
    ids.sort_by_key(|(span, _)| *span);
    ids.into_iter().map(|(_, id)| id.clone()).collect()
}

/// Imports resolved, then every call of every function to edges.
pub fn build_edges(g: &mut Graph, p: &Project) {
    resolve_imports(g);
    // short names for qualifiers in path-named languages: the file's stem
    // and the types it declares (a class that is its own module: its types only)
    let mut short: HashMap<String, Vec<String>> = HashMap::new();
    for m in g.modules.values() {
        let base = m.file.rsplit('/').next().unwrap_or(&m.file);
        let stem = match base.rfind('.') {
            Some(0) | None => base,
            Some(k) => &base[..k],
        };
        let mut names: Vec<&str> = Vec::new();
        if m.owner.is_empty() {
            names.push(stem);
        }
        names.extend(m.decls.iter().map(|s| s.as_str()));
        for nm in names {
            let v = short.entry(nm.to_string()).or_default();
            if !v.contains(&m.name) {
                v.push(m.name.clone());
            }
        }
    }
    g.short = short;
    // lane (C++): headers' fields for their .cpp; lane (types): supertypes
    // of the other languages. GDScript's engine classes leave a script's
    // inherited methods possibly the library's; then every module's descendants.
    pylink::engine_parents(g);
    pylink::build_children(g);
    let groups = pylink::groups(g, p);
    let extended = pylink::extended(g);
    let mut by_type: HashMap<String, Vec<String>> = HashMap::new();
    for m in g.modules.values() {
        for d in &m.decls {
            by_type.entry(d.clone()).or_default().push(m.name.clone());
        }
    }
    let order = walk_order(g);
    let mut site_out: HashMap<(String, usize, i64, String), Targets> = HashMap::new();
    let mut found: Vec<(String, usize, Targets)> = Vec::new();
    {
        let mut r = Resolver {
            g,
            p,
            locals: HashMap::new(),
            tests: HashMap::new(),
            module_js: HashMap::new(),
            groups,
            extended,
            by_type,
            godot: HashMap::new(),
        };
        for fid in &order {
            let Some(f) = g.func(fid) else { continue };
            let Some(m) = g.modules.get(&f.module) else { continue };
            for c in &f.calls {
                let key = (f.file.clone(), c.line, c.col, c.name.clone());
                if let Some(done) = site_out.get(&key) {
                    // resolved where it was made (a nested function): the
                    // enclosing ones reach the same callees
                    found.push((fid.clone(), c.line, done.clone()));
                    continue;
                }
                let t = r.resolve(f, m, c);
                site_out.insert(key, t.clone());
                found.push((fid.clone(), c.line, t));
            }
        }
    }
    for (fid, line, ts) in found {
        for (t, exact) in ts {
            g.edges.entry(fid.clone()).or_default().push((t.clone(), line, exact));
            g.redges.entry(t).or_default().push((fid.clone(), line, exact));
        }
    }
    // lane (JS / Python): parameter flow, parameter receivers
}
