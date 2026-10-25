//! Python and GDScript resolution: what core does for `.py` / `.gd` besides
//! the shared branches of `_build_edges` (core._resolve_py, _py_class,
//! _python_names, _typed_targets, _py_returned_closure, _groups, and the
//! supertype bookkeeping infer.resolve_bases / Types.ancestors keep for them).
//!
//! Calls that need a receiver's type (`types.chain`) and parameter flow wait
//! for the infer lane; `parity --edges --untyped` compares without them.

use std::collections::{HashMap, HashSet};

use crate::graph::Graph;
use crate::model::{Func, Module};
use crate::project::Project;
use crate::pyre::Rx;

/// _resolve_where's answer for an import from outside the project.
pub const EXTERNAL: &str = "ext:";

const PY_BUILTINS: &[&str] = &[
    "ArithmeticError", "AssertionError", "AttributeError", "BaseException", "BaseExceptionGroup", "BlockingIOError",
    "BrokenPipeError", "BufferError", "BytesWarning", "ChildProcessError", "ConnectionAbortedError", "ConnectionError",
    "ConnectionRefusedError", "ConnectionResetError", "DeprecationWarning", "EOFError", "Ellipsis", "EncodingWarning",
    "EnvironmentError", "Exception", "ExceptionGroup", "False", "FileExistsError", "FileNotFoundError",
    "FloatingPointError", "FutureWarning", "GeneratorExit", "IOError", "ImportError", "ImportWarning",
    "IndentationError", "IndexError", "InterruptedError", "IsADirectoryError", "KeyError", "KeyboardInterrupt",
    "LookupError", "MemoryError", "ModuleNotFoundError", "NameError", "None", "NotADirectoryError",
    "NotImplemented", "NotImplementedError", "OSError", "OverflowError", "PendingDeprecationWarning", "PermissionError",
    "ProcessLookupError", "PythonFinalizationError", "RecursionError", "ReferenceError", "ResourceWarning", "RuntimeError",
    "RuntimeWarning", "StopAsyncIteration", "StopIteration", "SyntaxError", "SyntaxWarning", "SystemError",
    "SystemExit", "TabError", "TimeoutError", "True", "TypeError", "UnboundLocalError",
    "UnicodeDecodeError", "UnicodeEncodeError", "UnicodeError", "UnicodeTranslateError", "UnicodeWarning", "UserWarning",
    "ValueError", "Warning", "ZeroDivisionError", "_IncompleteInputError", "__build_class__", "__debug__",
    "__doc__", "__import__", "__loader__", "__name__", "__package__", "__spec__",
    "abs", "aiter", "all", "anext", "any", "ascii",
    "bin", "bool", "breakpoint", "bytearray", "bytes", "callable",
    "chr", "classmethod", "compile", "complex", "copyright", "credits",
    "delattr", "dict", "dir", "divmod", "enumerate", "eval",
    "exec", "exit", "filter", "float", "format", "frozenset",
    "getattr", "globals", "hasattr", "hash", "help", "hex",
    "id", "input", "int", "isinstance", "issubclass", "iter",
    "len", "license", "list", "locals", "map", "max",
    "memoryview", "min", "next", "object", "oct", "open",
    "ord", "pow", "print", "property", "quit", "range",
    "repr", "reversed", "round", "set", "setattr", "slice",
    "sorted", "staticmethod", "str", "sum", "super", "tuple",
    "type", "vars", "zip",
];

/// Dotted-path suffix -> Python file modules (core._py_index): `flask.helpers`
/// is any module named `.../flask/helpers` (or a package `.../flask/helpers/__init__`).
pub struct PyIndex {
    idx: HashMap<String, Vec<String>>,
}

impl PyIndex {
    pub fn new(g: &Graph) -> PyIndex {
        let mut idx: HashMap<String, Vec<String>> = HashMap::new();
        for m in g.modules.values() {
            if !m.owner.is_empty() || !m.file.ends_with(".py") {
                continue;
            }
            let path = m.name.strip_suffix("/__init__").unwrap_or(&m.name);
            let parts: Vec<&str> = path.split('/').collect();
            for k in 0..parts.len() {
                idx.entry(parts[k..].join(".")).or_default().push(m.name.clone());
            }
        }
        PyIndex { idx }
    }

    /// core._resolve_py: a module name, "" when it can't be found, EXTERNAL
    /// for the stdlib or an installed package.
    pub fn resolve(&self, g: &Graph, where_: &str) -> String {
        let rest = &where_[3..];
        let (d, spec) = rest.split_once('\0').unwrap_or((rest, ""));
        if spec.starts_with('.') {
            let dots = spec.len() - spec.trim_start_matches('.').len();
            let mut base = d.to_string();
            for _ in 1..dots {
                base = match base.rsplit_once('/') {
                    Some((a, _)) => a.to_string(),
                    None => String::new(),
                };
            }
            let tail = spec[dots..].replace('.', "/");
            let path = if !base.is_empty() && !tail.is_empty() {
                format!("{base}/{tail}")
            } else if !base.is_empty() {
                base
            } else {
                tail
            };
            let init = if path.is_empty() { "__init__".to_string() } else { format!("{path}/__init__") };
            for cand in [path, init] {
                if g.modules.contains_key(&cand) {
                    return cand;
                }
            }
            return String::new();
        }
        let depth = spec.matches('.').count() + 1;
        // (the parts of h without its package's __init__, `parts[:len(parts) - depth]`)
        let root = |h: &str| -> String {
            let parts: Vec<&str> = h.strip_suffix("/__init__").unwrap_or(h).split('/').collect();
            let mut e = parts.len() as isize - depth as isize;
            if e < 0 {
                e = (e + parts.len() as isize).max(0);
            }
            parts[..e as usize].join("/")
        };
        let hits: Vec<&String> = self
            .idx
            .get(spec)
            .map(|v| v.iter().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|h| {
                let r = root(h);
                r.is_empty() || !g.modules.contains_key(&format!("{r}/__init__"))
            })
            .collect();
        if hits.is_empty() {
            return EXTERNAL.to_string();
        }
        if hits.len() == 1 {
            return hits[0].clone();
        }
        let nparts = |h: &str| root(h).split('/').count() as isize;
        let in_root = |h: &str| {
            let r = root(h);
            r.is_empty() || d == r || d.starts_with(&format!("{r}/"))
        };
        let mine: Vec<&String> = hits.iter().copied().filter(|h| in_root(h)).collect();
        let use_mine = !mine.is_empty();
        let pool: Vec<&String> = if use_mine { mine } else { hits.clone() };
        let key = |h: &str| -> isize {
            if use_mine {
                -nparts(h)
            } else if root(h).is_empty() {
                0
            } else {
                nparts(h)
            }
        };
        let best = pool.iter().map(|h| key(h)).min().unwrap();
        let top: Vec<&&String> = pool.iter().filter(|h| key(h) == best).collect();
        if top.len() == 1 {
            top[0].to_string()
        } else {
            String::new()
        }
    }
}

fn file_mod<'a>(g: &'a Graph, m: &'a Module) -> Option<&'a Module> {
    if m.owner.is_empty() {
        Some(m)
    } else {
        g.modules.get(&m.owner)
    }
}

/// The class module a Python name in `mod`'s file means (`Base`,
/// `models.Base`, an imported or re-exported class), or "" (core._py_class).
pub fn py_class(g: &Graph, m: &Module, name: &str, depth: usize) -> String {
    let Some(fm) = file_mod(g, m) else { return String::new() };
    if depth > 4 {
        return String::new();
    }
    if let Some((q, rest)) = name.rsplit_once('.') {
        let last = q.rsplit('.').next().unwrap_or(q);
        if let Some(target) = fm.aliases.get(last) {
            if let Some(tm) = g.modules.get(target) {
                let cand = format!("{target}.{rest}");
                if g.modules.contains_key(&cand) {
                    return cand;
                }
                return py_class(g, tm, rest, depth + 1);
            }
        }
        return String::new();
    }
    let own = format!("{}.{}", fm.name, name);
    if g.modules.contains_key(&own) {
        return own;
    }
    if let Some(a) = fm.aliases.get(name) {
        if g.modules.get(a).unwrap_or(m).owner.is_empty() {
            // (`project.modules.get(alias, mod).owner`: falsy)
        } else {
            return a.clone();
        }
    }
    if let Some((w, n)) = fm.imported.get(name) {
        if let Some(wm) = g.modules.get(w) {
            return py_class(g, wm, n, depth + 1);
        }
    }
    for w in &fm.reexports {
        if let Some(wm) = g.modules.get(w) {
            let hit = py_class(g, wm, name, depth + 1);
            if !hit.is_empty() {
                return hit;
            }
        }
    }
    String::new()
}

/// `from .compat import MutableMapping`, where compat imports it from a
/// library: the name is the library's (core._py_lib_reexport).
fn py_lib_reexport(g: &Graph, fm: &Module, name: &str, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    let Some((w, n)) = fm.imported.get(name) else { return false };
    let Some(src) = g.modules.get(w) else { return false };
    src.external.contains(n) || py_lib_reexport(g, src, n, depth + 1)
}

/// core._python_names: once every import is resolved, `from x import Foo` (a
/// class) and `from pkg import sub` (a submodule) become aliases, and each
/// class's base names its parent class.
pub fn python_names(g: &mut Graph) {
    let names: Vec<String> =
        g.modules.iter().filter(|(_, m)| m.file.ends_with(".py")).map(|(n, _)| n.clone()).collect();
    for n in &names {
        if !g.modules[n].owner.is_empty() {
            continue;
        }
        let imported: Vec<(String, (String, String))> =
            g.modules[n].imported.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (local, (w, nm)) in imported {
            let stem = if let Some(s) = w.strip_suffix("/__init__") {
                s.to_string()
            } else {
                w.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_default()
            };
            let sub = format!("{stem}/{nm}");
            let sub = sub.trim_start_matches('/').to_string();
            let init = format!("{sub}/__init__");
            let set = if g.modules.contains_key(&sub) {
                Some(sub)
            } else if g.modules.contains_key(&init) {
                Some(init)
            } else {
                None
            };
            let alias = match set {
                Some(s) => Some(s),
                None => {
                    let cls = g.modules.get(&w).map(|t| py_class(g, t, &nm, 0)).unwrap_or_default();
                    (!cls.is_empty()).then_some(cls)
                }
            };
            if let Some(a) = alias {
                g.modules.get_mut(n).unwrap().aliases.entry(local).or_insert(a);
            }
        }
    }
    for n in &names {
        let (owner, parent0, bases0) = {
            let m = &g.modules[n];
            (m.owner.clone(), m.parent.clone(), m.bases.clone())
        };
        if !owner.is_empty() {
            if let Some(fm) = g.modules.get(&owner) {
                let (a, i, u, r, e) = (fm.aliases.clone(), fm.imported.clone(), fm.uses.clone(), fm.reexports.clone(), fm.external.clone());
                let m = g.modules.get_mut(n).unwrap();
                m.aliases = a;
                m.imported = i;
                m.uses = u;
                m.reexports = r;
                m.external = e;
            }
        }
        let mut new_parent = None;
        if let Some(p) = parent0.strip_prefix("py:") {
            new_parent = Some(py_class(g, &g.modules[n], p, 0));
        }
        let mut upd: Option<(Vec<String>, bool)> = None;
        if bases0.first().map(|b| b.starts_with("py:")).unwrap_or(false) {
            let m = &g.modules[n];
            let fm = g.modules.get(&m.owner).unwrap_or(m);
            let got: Vec<String> = bases0.iter().map(|b| py_class(g, m, &b[3..], 0)).filter(|c| !c.is_empty()).collect();
            let unresolved = bases0.iter().any(|b| {
                let bn = &b[3..];
                py_class(g, m, bn, 0).is_empty()
                    && (fm.external.contains(bn.split('.').next().unwrap_or(""))
                        || PY_BUILTINS.contains(&bn)
                        || py_lib_reexport(g, fm, bn.split('.').next().unwrap_or(""), 0))
            });
            upd = Some((got, unresolved));
        }
        let m = g.modules.get_mut(n).unwrap();
        if let Some(p) = new_parent {
            m.parent = p;
        }
        if let Some((got, unresolved)) = upd {
            if unresolved {
                m.unresolved = true;
            }
            if m.parent.is_empty() {
                if let Some(f) = got.first() {
                    m.parent = f.clone();
                }
            }
            m.bases = got;
        }
    }
}

/// infer.resolve_bases, the part GDScript needs: `extends CharacterBody3D`
/// (an engine class) leaves an inherited method possibly the library's.
pub fn engine_parents(g: &mut Graph) {
    let names: Vec<String> = g.modules.keys().cloned().collect();
    for n in names {
        let m = &g.modules[&n];
        let engine = !m.parent.is_empty()
            && m.bases.is_empty()
            && !g.modules.contains_key(&m.parent)
            && m.parent.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
            && !matches!(crate::project::ext_of(&m.file), ".ex" | ".exs" | ".rb" | ".rake" | ".ru");
        if engine {
            g.modules.get_mut(&n).unwrap().unresolved = true;
        }
    }
}

/// infer.Types.ancestors: the module, then its supertypes, nearest first.
/// (Other parts of one type - a C++ header and source, a Swift extension -
/// are the infer lane's.)
pub fn ancestors(g: &Graph, mn: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut todo: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    todo.push_back(mn.to_string());
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(cur) = todo.pop_front() {
        if seen.contains(&cur) || !g.modules.contains_key(&cur) {
            continue;
        }
        seen.insert(cur.clone());
        out.push(cur.clone());
        let m = &g.modules[&cur];
        for b in &m.bases {
            if g.modules.contains_key(b) {
                todo.push_back(b.clone());
            }
        }
        if !m.parent.is_empty() {
            todo.push_back(m.parent.clone());
        }
    }
    out
}

/// Every module's descendants (project.children), for overrides.
pub fn build_children(g: &mut Graph) {
    let names: Vec<String> = g.modules.keys().cloned().collect();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for n in &names {
        for a in ancestors(g, n).into_iter().skip(1) {
            children.entry(a).or_default().push(n.clone());
        }
    }
    g.children = children;
}

/// core._groups: [project.groups] group id -> file, as "group:<id>" -> module.
pub fn groups(g: &Graph, p: &Project) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(t) = p.config.table("project").get("groups").and_then(|x| x.as_object()) {
        for (gid, rel) in t {
            if let Some(r) = rel.as_str() {
                if let Some(mn) = g.by_file.get(r).and_then(|v| v.first()) {
                    out.insert(format!("group:{gid}"), mn.clone());
                }
            }
        }
    }
    out
}

/// core._typed_targets (without Go): the functions `x.name()` reaches when x
/// is declared `ty`, and whether ty is a project type at all.
pub fn typed_targets<'a>(g: &'a Graph, m: &Module, ty: &str, name: &str) -> (Vec<&'a Func>, bool) {
    let mut mods: Vec<String> = Vec::new();
    if m.file.ends_with(".py") {
        let cls = py_class(g, m, ty, 0);
        if !cls.is_empty() {
            mods.push(cls);
        }
    }
    if let Some(a) = m.aliases.get(ty) {
        mods.push(a.clone());
    }
    if g.modules.contains_key(ty) {
        mods.push(ty.to_string());
    }
    if let Some(s) = g.short.get(ty) {
        mods.extend(s.iter().cloned());
    }
    let mut hits: Vec<&Func> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for mn in &mods {
        if !seen.insert(mn.as_str()) {
            continue;
        }
        if let Some(t) = g.lookup(mn, name) {
            if !hits.iter().any(|h| h.id == t.id) {
                hits.push(t);
            }
        }
    }
    if hits.is_empty() {
        // (a Kotlin extension on that type: `recv`)
        for id in g.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[]) {
            if let Some(f) = g.func(id) {
                if f.recv == ty {
                    hits.push(f);
                }
            }
        }
    }
    (hits, !mods.is_empty())
}

/// The function a Python decorator factory returns: `def route(...): def
/// decorator(f): ...; return decorator` gives route's `decorator`
/// (core._py_returned_closure).
pub fn returned_closure<'a>(g: &'a Graph, f: &Func) -> Option<&'a Func> {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| Rx::lit(r"(?m)^([ \t]*)return\s+([A-Za-z_]\w*)\s*$"));
    let rets = r.find_iter(&f.code);
    let best = rets.iter().min_by_key(|c| crate::lang::python::tab_width(c.s(1)))?; // (first of the least indented)
    let name = best.s(2);
    for id in g.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[]) {
        if let Some(h) = g.func(id) {
            if h.file == f.file && f.start < h.start && h.start <= f.end {
                return Some(h);
            }
        }
    }
    None
}

/// A set of every name some project class extends that the project doesn't
/// declare (infer.Types.extended): a GDScript `Node`, a Python `dict`.
pub fn extended(g: &Graph) -> HashSet<String> {
    let mut out = HashSet::new();
    for m in g.modules.values() {
        if !m.parent.is_empty() && !g.modules.contains_key(&m.parent) {
            out.insert(m.parent.clone());
        }
        for b in &m.bases {
            if !g.modules.contains_key(b) {
                out.insert(b.clone());
            }
        }
    }
    out
}

