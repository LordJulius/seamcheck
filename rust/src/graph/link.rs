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

mod call;
mod flow;
mod helpers;

use call::Resolver;

/// (callee id, exact)
type Targets = Vec<(String, bool)>;
const SELF: &[&str] = &["self", "this", "Self", "cls"];
/// core.EXTERNAL: an import from outside the project
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
    crate::lang::ruby::link::link(g); // Ruby: requires, constants and supertypes, across files
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
    cpp_header_fields(g);
    // supertypes as modules, then every module's descendants (for overrides
    // and for an interface's implementations)
    crate::graph::infer::resolve_bases(g, p);
    let children = {
        let t = crate::graph::infer::Types::new(g, p);
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        for m in g.modules.values() {
            for a in t.ancestors(&m.name).into_iter().skip(1) {
                children.entry(a).or_default().push(m.name.clone());
            }
        }
        children
    };
    g.children = children;
    let groups = pylink::groups(g, p);
    let order = walk_order(g);
    let mut site_out: HashMap<(String, usize, i64, String), Targets> = HashMap::new();
    let mut edges: flow::Edges = Default::default();
    let mut redges: flow::Edges = Default::default();
    {
        let mut r = Resolver {
            g,
            p,
            t: crate::graph::infer::Types::new(g, p),
            locals: HashMap::new(),
            tests: HashMap::new(),
            module_js: HashMap::new(),
            groups,
            scope_cache: HashMap::new(),
            ext_cache: None,
            go_types: None,
            c_macros: None,
            js_imported: None,
            cs_global_static: None,
        };
        let mut pending: Vec<(String, crate::model::Call)> = Vec::new();
        let mut pending_recv: Vec<(String, crate::model::Call)> = Vec::new();
        let mut push = |edges: &mut flow::Edges, redges: &mut flow::Edges, from: &str, to: &str, line: usize, exact: bool| {
            edges.entry(from.to_string()).or_default().push((to.to_string(), line, exact));
            redges.entry(to.to_string()).or_default().push((from.to_string(), line, exact));
        };
        for fid in &order {
            let Some(f) = g.func(fid) else { continue };
            let Some(m) = g.modules.get(&f.module) else { continue };
            for c in &f.calls {
                let key = (f.file.clone(), c.line, c.col, c.name.clone());
                if let Some(done) = site_out.get(&key) {
                    // a call inside a nested function (a callback, an object literal's method) was
                    // resolved there, with that function's parameters and locals: the enclosing
                    // functions reach the same callees
                    for (tid, exact) in done {
                        push(&mut edges, &mut redges, &f.id, tid, c.line, *exact);
                    }
                    continue;
                }
                let out = r.resolve(f, m, c);
                for d in &out.direct {
                    push(&mut edges, &mut redges, &f.id, d, c.line, true);
                }
                if out.param_call {
                    pending.push((f.id.clone(), c.clone()));
                }
                if out.param_recv {
                    pending_recv.push((f.id.clone(), c.clone()));
                }
                site_out.insert(key, out.targets.clone());
                for (tid, exact) in &out.targets {
                    push(&mut edges, &mut redges, &f.id, tid, c.line, *exact);
                }
            }
        }
        r.param_flow(&mut edges, &mut redges, &pending);
        r.param_receivers(&mut edges, &mut redges, &pending_recv);
    }
    g.edges = edges;
    g.redges = redges;
}

/// C++: a class's members are declared in its header; the .cpp's functions
/// use them (`m_board->indexOf()`): the header's fields count for the .cpp.
fn cpp_header_fields(g: &mut Graph) {
    let mut add: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let files: Vec<String> = g.by_file.keys().cloned().collect();
    for rel in files {
        let (mut stem, mut ext) = match rel.rfind('.') {
            Some(k) if k > 0 && !rel[k..].contains('/') => (rel[..k].to_string(), rel[k..].to_string()),
            _ => (rel.clone(), String::new()),
        };
        let im_ = if matches!(ext.as_str(), ".h" | ".hpp" | ".hh") {
            crate::pyre::rx(r"^(.+?)[-_](?:inl|impl)$").unwrap().match_start(&stem).map(|c| c.s(1).to_string())
        } else {
            None
        };
        if im_.is_some() || matches!(ext.as_str(), ".inl" | ".ipp" | ".tpp") {
            // `logger-inl.h`, `format.inl`: a header's out-of-line definitions
            if let Some(s) = im_ {
                stem = s;
            }
            ext = ".cpp".into();
        }
        if matches!(ext.as_str(), ".cpp" | ".cc" | ".cxx" | ".c") && !g.by_file[&rel].is_empty() {
            let mut pairs: Vec<(String, String)> = Vec::new();
            for hext in [".h", ".hpp", ".hh"] {
                for hm in g.by_file.get(&format!("{stem}{hext}")).map(|v| v.as_slice()).unwrap_or(&[]) {
                    for (k, v) in &g.modules[hm].bound {
                        pairs.push((k.clone(), v.clone()));
                    }
                }
            }
            add.push((g.by_file[&rel][0].clone(), pairs));
        }
    }
    for (mn, pairs) in add {
        let m = g.modules.get_mut(&mn).unwrap();
        for (k, v) in pairs {
            m.bound.entry(k).or_insert(v);
        }
    }
}
