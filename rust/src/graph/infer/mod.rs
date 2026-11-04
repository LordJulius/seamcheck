//! Which class a call's receiver is: the type layer over the call graph
//! (infer.py).
//!
//! Adapters record what the code writes down (a call's receiver chain, a
//! variable's declared type or the expression it was assigned, a field's type,
//! a function's declared return type, a class's supertypes). This module walks
//! a chain one step at a time over those facts:
//!
//!   repo.find(id).save()   repo: Repo (a field)  ->  Repo.find returns Order  ->  Order.save
//!
//! Every step is a fact the code states; when one is missing the walk stops
//! and the call falls back to the older rules (and, last, a name guess). A
//! step that lands on a library type makes the rest of the chain the
//! library's too.
//!
//! Layout: this file the indexes and the type -> module lookups; `walk` the
//! chain / first / step walk; `held` what a binding holds; `extra` the
//! language-specific readers (JS expandos, Rust macros, Godot scenes...);
//! `scope` core's `_in_scope` family the walk shares with the resolver.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::graph::Graph;
use crate::model::{Func, Module};
use crate::project::{ext_of, Project};

mod arity;
mod extra;
pub mod flask;
mod held;
pub(crate) mod scope;
mod walk;

pub use arity::{arities, paren_close, top_split};

/// A pattern compiled once (Python's module-level `re.compile`).
macro_rules! re {
    ($p:expr) => {{
        static R: std::sync::OnceLock<crate::pyre::Rx> = std::sync::OnceLock::new();
        R.get_or_init(|| crate::pyre::Rx::lit($p))
    }};
}
pub(crate) use re;

/// A pattern built at run time (a name escaped into it), cached per run.
pub(crate) fn rxd(pat: &str) -> std::sync::Arc<crate::pyre::Rx> {
    crate::pyre::rx_built(pat).unwrap_or_else(|e| panic!("bad built pattern {pat:?}: {e}"))
}

/// Python `re.escape`.
pub(crate) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        if !(ch.is_alphanumeric() || ch == '_') && (ch as u32) < 128 {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub(crate) fn ends(s: &str, exts: &[&str]) -> bool {
    exts.iter().any(|e| s.ends_with(e))
}

/// Python `str.isupper()` of the first character (`name[:1].isupper()`).
pub(crate) fn upper1(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_uppercase())
}

/// Python `str.islower()` of the first character (`seg[:1].islower()`).
pub(crate) fn lower1(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_lowercase())
}

/// How deep a chain, an `x = <expr>` or a return type may lead before we stop.
pub const MAX: usize = 48;

pub const RB_EXTS: &[&str] = &[".rb", ".rake", ".ru"];
pub const C_EXTS: &[&str] = &[".c", ".h", ".cpp", ".cc", ".cxx", ".hpp", ".hh"];
pub const JS_EXTS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".qml"];
/// `_JS_EXTS[:-1]`: without .qml
pub const JS_FILE_EXTS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"];
pub const ES_MODULE: &[&str] = &[".ts", ".tsx", ".mts", ".cts"];
pub const EXPLICIT_SELF: &[&str] = &[".py", ".pyw", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".php"];
pub const DYNAMIC: &[&str] = &[".gd", ".py", ".pyw", ".js", ".mjs", ".cjs", ".rb", ".php", ".qml"];
pub const OVERLOADED: &[&str] = &[".java", ".cs", ".kt", ".kts", ".cpp", ".cc", ".cxx", ".h", ".hpp", ".hh"];

/// The value a receiver chain evaluates to (infer.py's `(kind, modules, exact)`
/// tuples).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Val {
    /// declared, of a type the code doesn't say (a QML `property var`)
    Unknown,
    /// a library value: every call on it is the library's (the type, when named)
    Ext(Vec<String>),
    /// the class itself: its static members
    Static(Vec<String>, bool),
    /// an instance of the class
    Inst(Vec<String>, bool),
    /// a Rust Option / Result around a value
    Wrap(Option<Box<Val>>),
    /// a namespace (QML module, C++ namespace)
    Ns,
    /// a package: `Pkg(vec![name])`
    Pkg(Vec<String>),
}

/// The library's value (`EXT`).
pub fn ext() -> Val {
    Val::Ext(Vec::new())
}

impl Val {
    pub fn is_ext(&self) -> bool {
        matches!(self, Val::Ext(_))
    }
    /// `v[0]`
    pub fn kind(&self) -> &'static str {
        match self {
            Val::Unknown => "unknown",
            Val::Ext(_) => "ext",
            Val::Static(..) => "static",
            Val::Inst(..) => "inst",
            Val::Wrap(_) => "wrap",
            Val::Ns => "ns",
            Val::Pkg(_) => "pkg",
        }
    }
    /// `v[1]` where it is a tuple of names
    pub fn mods(&self) -> &[String] {
        match self {
            Val::Ext(m) | Val::Static(m, _) | Val::Inst(m, _) | Val::Pkg(m) => m,
            _ => &[],
        }
    }
    /// `v[2]`
    pub fn exact(&self) -> bool {
        match self {
            Val::Unknown => false,
            Val::Static(_, e) | Val::Inst(_, e) => *e,
            _ => true,
        }
    }
    pub fn is_inst_or_static(&self) -> bool {
        matches!(self, Val::Inst(..) | Val::Static(..))
    }
}

/// `is_ext(v)` on an optional value.
pub fn is_ext(v: &Option<Val>) -> bool {
    v.as_ref().is_some_and(|v| v.is_ext())
}

/// (modules, exact, declared in the project)
pub type TypeMods = (Vec<String>, bool, bool);

#[derive(Default)]
pub(crate) struct Caches {
    type_mods: HashMap<(String, String), TypeMods>,
    alias: HashMap<(String, String), TypeMods>,
    anc: HashMap<String, Vec<String>>,
    parts: HashMap<String, Vec<String>>,
    arity: HashMap<(String, String), Vec<(usize, usize)>>,
    goimpl: HashMap<String, Vec<Vec<String>>>,
    cppal: Option<HashMap<String, String>>,
    cppstd: HashSet<String>,
    rsext: Option<HashSet<String>>,
    esm: HashMap<String, bool>,
    godot: HashMap<String, String>,
    expando: Option<std::rc::Rc<RefCell<Expando>>>,
    lam: HashMap<(String, String, String), Option<Val>>,
    elem: HashMap<(String, String, Vec<String>, usize), Option<Val>>,
    coll: HashMap<String, Option<(String, String)>>,
    jsx: Option<Option<Val>>,
    variants: Option<HashMap<String, Vec<(String, String)>>>,
    rsmacro: HashMap<String, Option<Val>>,
    libres: Option<HashSet<String>>,
    pkgname: Option<String>,
    scenes: Option<std::rc::Rc<Scenes>>,
    rbshort: Option<HashMap<String, IndexMap<String, HashSet<String>>>>,
    yielded: HashMap<String, Option<Val>>,
    pydeco: HashMap<String, Option<Val>>,
    tsinit: HashMap<String, HashMap<(String, String), String>>,
    gopkgvars: Option<HashMap<(String, String), Vec<String>>>,
    autoloads: Option<HashMap<(String, String), String>>,
    goroots: HashMap<String, String>,
    /// Flask-Login's user class (infer_flask.login_user)
    flask_user: Option<Option<Val>>,
    /// infer_flask's per-load caches: a class's written bases, model-ness, current_user imported
    flask_bases: HashMap<String, Vec<String>>,
    flask_model: HashMap<Vec<String>, bool>,
    flask_cu: HashMap<String, bool>,
    // core's scope helpers
    js_factory: HashMap<(String, String), Option<String>>,
    js_arg_members: HashMap<String, bool>,
    js_stored: HashMap<String, HashSet<String>>,
    cpp_ns: Option<HashSet<String>>,
}

/// JS expando assignments: (module, property) -> where code sets that
/// property on an object of that module: (module, function index, the value's chain).
pub(crate) type Expando = IndexMap<(String, String), Vec<(String, usize, Vec<String>)>>;
/// (script -> scenes whose root runs it, scene -> {node path: ("script"|"type", what)})
pub(crate) type Scenes = (HashMap<String, Vec<String>>, HashMap<String, HashMap<String, (String, String)>>);

/// Per-project indexes, built once after imports are resolved.
pub struct Types<'a> {
    pub g: &'a Graph,
    pub p: &'a Project,
    pub by_type: IndexMap<String, Vec<String>>,
    /// every name a project QML file declares (ids, properties)
    pub qml_names: HashSet<String>,
    /// library types project classes extend (GDScript's Node, Python's dict)
    pub extended: HashSet<String>,
    /// (receiver type, name) -> extension functions
    pub ext_fns: IndexMap<(String, String), Vec<String>>,
    /// the line of the call being resolved (a match arm's bindings hold on its lines only)
    pub line: Cell<usize>,
    pub(crate) c: RefCell<Caches>,
}

impl<'a> Types<'a> {
    pub fn new(g: &'a Graph, p: &'a Project) -> Types<'a> {
        let mut by_type: IndexMap<String, Vec<String>> = IndexMap::new();
        for m in g.modules.values() {
            for d in &m.decls {
                by_type.entry(d.clone()).or_default().push(m.name.clone());
            }
        }
        // `export { Hono as HonoBase }`: the class under its exported name
        for m in g.modules.values() {
            if m.owner.is_empty() && ends(&m.file, JS_EXTS) && !m.raw.is_empty() {
                let txt = m.raw.join("\n");
                if !txt.contains("export") {
                    continue;
                }
                for blk in re!(r"\bexport\s*\{([^}]*)\}(?!\s*from)").find_iter(&txt) {
                    for pair in re!(r"([A-Za-z_$][\w$]*)\s+as\s+([A-Za-z_$][\w$]*)").find_iter(blk.s(1)) {
                        let (a, b) = (pair.s(1), pair.s(2));
                        let own: Vec<String> = by_type
                            .get(a)
                            .map(|v| v.iter().filter(|mn| g.modules[*mn].file == m.file).cloned().collect())
                            .unwrap_or_default();
                        if !own.is_empty() && a != b {
                            let have = by_type.get(b).cloned().unwrap_or_default();
                            let e = by_type.entry(b.to_string()).or_default();
                            for x in own {
                                if !have.contains(&x) && !e.contains(&x) {
                                    e.push(x);
                                }
                            }
                        }
                    }
                }
            }
        }
        // a C++ class registered under a QML name
        for (qname, cls) in &g.qml_names {
            if qname != cls {
                let add = by_type.get(cls).cloned().unwrap_or_default();
                by_type.entry(qname.clone()).or_default().extend(add);
            }
        }
        let mut qml_names: HashSet<String> = HashSet::new();
        for m in g.modules.values() {
            if m.file.ends_with(".qml") {
                qml_names.extend(m.bound.keys().cloned());
                qml_names.extend(m.funcs.iter().map(|f| f.name.clone()));
            }
        }
        let mut extended: HashSet<String> = HashSet::new();
        for m in g.modules.values() {
            if !m.parent.is_empty() && !g.modules.contains_key(&m.parent) {
                extended.insert(m.parent.clone());
            }
            for b in &m.bases {
                if !g.modules.contains_key(b) {
                    extended.insert(b.clone());
                }
            }
        }
        let mut ext_fns: IndexMap<(String, String), Vec<String>> = IndexMap::new();
        for gf in g.funcs.keys().filter_map(|id| g.func(id)) {
            if !gf.recv.is_empty() && !gf.module.ends_with(&format!(".{}", gf.recv)) {
                for r in gf.recv.split('|') {
                    ext_fns.entry((r.to_string(), gf.name.clone())).or_default().push(gf.id.clone());
                }
            }
        }
        Types {
            g,
            p,
            by_type,
            qml_names,
            extended,
            ext_fns,
            line: Cell::new(0),
            c: RefCell::new(Caches::default()),
        }
    }

    pub fn m(&self, name: &str) -> &'a Module {
        self.g.modules.get(name).unwrap_or_else(|| panic!("KeyError: {name:?}"))
    }

    pub fn fx(&self, id: &str) -> Option<&'a Func> {
        self.g.func(id)
    }

    pub fn ext_funcs(&self, ty: &str, name: &str) -> Vec<&'a Func> {
        self.ext_fns
            .get(&(ty.to_string(), name.to_string()))
            .map(|v| v.iter().filter_map(|id| self.fx(id)).collect())
            .unwrap_or_default()
    }

    // --- types -> modules -----------------------------------------------------------

    pub fn file_mod(&self, m: &'a Module) -> &'a Module {
        if m.owner.is_empty() {
            m
        } else {
            self.g.modules.get(&m.owner).unwrap_or(m)
        }
    }

    pub fn searchable(&self, mn: &str) -> bool {
        self.g.modules.get(mn).is_some_and(|m| matches!(m.kind.as_str(), "class" | "interface" | "ext") || !m.owner.is_empty())
    }

    /// The modules declaring type `name` as seen from `mod`: (modules,
    /// exact, declared in the project). Nearest first: the file, an import,
    /// the package / namespace / directory, then anywhere (exact only when
    /// there is one).
    pub fn type_modules(&self, md: &'a Module, name: &str) -> TypeMods {
        let key = (md.name.clone(), name.to_string());
        if let Some(h) = self.c.borrow().type_mods.get(&key) {
            return h.clone();
        }
        let mut hit = self.type_modules_(md, name);
        if hit.0.len() > 1 && !name.contains('.') && !md.owner.is_empty() && !crate::model::is_py(&md.file) {
            // several `Source` classes in a test file: the enclosing classes' own nested one
            let mut o = md.name.clone();
            while let Some(om) = self.g.modules.get(&o) {
                if om.owner.is_empty() || om.file != md.file {
                    break;
                }
                let cand = format!("{o}.{name}");
                if hit.0.contains(&cand) {
                    if cand != md.name {
                        hit = (vec![cand], true, true);
                    }
                    break;
                }
                let up = o.rsplit_once('.').map(|x| x.0.to_string()).unwrap_or_else(|| o.clone());
                if up == o {
                    break;
                }
                o = up;
            }
        }
        self.c.borrow_mut().type_mods.insert(key, hit.clone());
        hit
    }

    /// C / C++ type aliases anywhere in the project: `using X = ns::Y<T>;`,
    /// `typedef struct Y X;` (name -> the aliased type's own name). An alias two
    /// files give different targets is left out.
    pub fn cpp_aliases(&self) -> HashMap<String, String> {
        if let Some(h) = &self.c.borrow().cppal {
            return h.clone();
        }
        let mut seen: IndexMap<String, Vec<String>> = IndexMap::new();
        let mut std_: Vec<String> = Vec::new();
        for m in self.g.modules.values() {
            if !m.owner.is_empty() || !ends(&m.file, C_EXTS) {
                continue;
            }
            let code = m.code.join("\n");
            let mut pairs: Vec<(String, String)> = Vec::new();
            for am in re!(r"\busing\s+(\w+)\s*=\s*(?:typename\s+)?([^;=]+);").find_iter(&code) {
                pairs.push((am.s(1).to_string(), am.s(2).to_string()));
            }
            for am in re!(
                r"\btypedef\s+(?:const\s+)?(?:struct\s+|class\s+|union\s+|enum\s+)?([\w:<>, ]+?)[\s*&]+(\w+)\s*;"
            )
            .find_iter(&code)
            {
                pairs.push((am.s(2).to_string(), am.s(1).to_string()));
            }
            for (al, target) in pairs {
                let stripped = re!(r"\b(?:const|typename|volatile)\b").sub("", &target);
                let t = crate::lang::chains::type_name(stripped.trim());
                if !t.is_empty() && t != al {
                    let last = t.split("::").last().unwrap_or("").split('.').last().unwrap_or("").to_string();
                    let e = seen.entry(al.clone()).or_default();
                    if !e.contains(&last) {
                        e.push(last);
                    }
                    if re!(r"\s*(?:typename\s+)?(?:::)?(?:std|boost|absl)::").match_start(&target).is_some() {
                        std_.push(al.clone());
                    }
                }
            }
        }
        let got: HashMap<String, String> =
            seen.into_iter().filter(|(_, v)| v.len() == 1).map(|(k, mut v)| (k, v.remove(0))).collect();
        let mut c = self.c.borrow_mut();
        c.cppstd.extend(std_);
        c.cppal = Some(got.clone());
        got
    }

    pub fn cpp_std(&self, name: &str) -> bool {
        self.c.borrow().cppstd.contains(name)
    }

    fn type_modules_(&self, md: &'a Module, name: &str) -> TypeMods {
        let g = self.g;
        let fm = self.file_mod(md);
        let mut name = name.to_string();
        if crate::model::is_py(&fm.file) {
            let cls = crate::graph::pylink::py_class(g, md, &name, 0);
            if !cls.is_empty() {
                return (vec![cls], true, true);
            }
        }
        let al = if upper1(&name) { fm.bound.get(&name).cloned().unwrap_or_default() } else { String::new() };
        if let Some(a) = al.strip_prefix("type:") {
            if a != name {
                name = a.to_string(); // `using X = A.B;` / PHP `use A\\B as X;`: B, by another name
            }
        }
        if ends(&fm.file, C_EXTS) && !self.by_type.contains_key(&name) {
            let mut seen = vec![name.clone()];
            loop {
                let aliases = self.cpp_aliases();
                match aliases.get(&name) {
                    Some(n) if !self.by_type.contains_key(&name) => {
                        name = n.clone();
                        if seen.contains(&name) {
                            break;
                        }
                        seen.push(name.clone());
                    }
                    _ => break,
                }
            }
        }
        let head = name.split('.').next().unwrap_or("").to_string();
        if name.contains('.') && ends(&fm.file, C_EXTS) && matches!(head.as_str(), "std" | "boost" | "absl") {
            return (vec![], false, false); // `std::string`: the library's, never a project class named string
        }
        if name.contains('.') && fm.file.ends_with(".rs") && self.rs_external().contains(&head) {
            return (vec![], false, false); // `walkdir::DirEntry`: that crate's, not the project's own DirEntry
        }
        if name.contains('.') {
            // `pkg.Type`, `Outer.Inner`, `ns.Type`
            let (q, last) = name.rsplit_once('.').unwrap();
            let (q, last) = (q.to_string(), last.to_string());
            let qlast = q.rsplit('.').next().unwrap_or(&q).to_string();
            let target = fm.aliases.get(&qlast).cloned().unwrap_or_default();
            if let Some(pk) = target.strip_prefix("pkg:") {
                let mods: Vec<String> =
                    self.by_type.get(&last).map(|v| v.iter().filter(|mn| self.pkg_of(mn) == pk).cloned().collect()).unwrap_or_default();
                let prims: HashSet<String> = mods.iter().map(|m| self.primary(m)).collect();
                return (mods.clone(), prims.len() <= 1 && !mods.is_empty(), !mods.is_empty());
            }
            if let Some(tm) = g.modules.get(&target) {
                let inner: Vec<String> = self
                    .by_type
                    .get(&last)
                    .map(|v| v.iter().filter(|mn| g.modules[*mn].file == tm.file).cloned().collect())
                    .unwrap_or_default();
                if !inner.is_empty() {
                    return (inner, true, true);
                }
            }
            let got = self.type_modules(md, &qlast);
            if !got.0.is_empty() {
                let inner: Vec<String> =
                    got.0.iter().map(|mn| format!("{mn}.{last}")).filter(|x| g.modules.contains_key(x)).collect();
                if !inner.is_empty() {
                    let one = inner.len() == 1;
                    return (inner, one, true);
                }
            }
            name = last;
        }
        let mut cands: Vec<String> = self.by_type.get(&name).cloned().unwrap_or_default();
        if cands.is_empty() && name.contains('`') {
            // `List`1` where only a plain `List` (Java, TS): that one
            name = name.split('`').next().unwrap().to_string();
            cands = self.by_type.get(&name).cloned().unwrap_or_default();
        } else if cands.is_empty() {
            // the plain name of a type only declared generic (`Foo` for `Foo<T>`, C#)
            let pre = format!("{name}`");
            cands = self.by_type.iter().filter(|(k, _)| k.starts_with(&pre)).flat_map(|(_, v)| v.iter().cloned()).collect();
        }
        if !cands.is_empty() {
            // a type is its own language family's (`Vec` in a .rs file is never a Python test's class)
            cands.retain(|mn| crate::graph::link::shares_family(&fm.file, &g.modules[mn].file));
        }
        if cands.is_empty() {
            return (vec![], false, false);
        }
        let exts: Vec<String> = cands.iter().filter(|mn| g.modules[*mn].kind == "ext").cloned().collect();
        let nonext: Vec<String> = cands.iter().filter(|mn| g.modules[*mn].kind != "ext").cloned().collect();
        let mut prim: Vec<String> = if nonext.is_empty() { exts.clone() } else { nonext };
        let mut broke = false;
        for k in 0..6 {
            let got: Vec<String> = prim
                .iter()
                .filter(|mn| match k {
                    0 => g.modules[*mn].file == md.file,
                    1 => self.imported(fm, &name, mn),
                    2 => !fm.package.is_empty() && self.pkg_of(mn) == fm.package,
                    3 => self.in_uses(fm, mn),
                    4 => md.file.ends_with(".gd") && self.godot(&g.modules[*mn].file) == self.godot(&md.file),
                    _ => dirname(&g.modules[*mn].file) == dirname(&md.file),
                })
                .cloned()
                .collect();
            if !got.is_empty() {
                prim = got;
                broke = true;
                break;
            }
        }
        if !broke && ends(&fm.file, ES_MODULE) && prim.iter().all(|mn| self.es_module(&g.modules[mn].file)) {
            // an ES module sees another module's type only by importing it: not imported,
            // `Response` is the runtime's (not client/types.ts's interface Response)
            return (vec![], false, false);
        }
        if fm.file.ends_with(".rs")
            && !prim.is_empty()
            && prim.iter().all(|mn| g.modules[mn].bound.get(&name).is_some_and(|b| b.starts_with("type:")))
            && prim.len() == 1
            && g.modules[&prim[0]].bound[&name][5..] != name
        {
            // `type Range = Match;` where it is declared: the type it names, read there
            let key = (prim[0].clone(), name.clone());
            if let Some(h) = self.c.borrow().alias.get(&key) {
                return h.clone();
            }
            self.c.borrow_mut().alias.insert(key.clone(), (vec![], false, false)); // (`type Result<T> = std::result::Result<T, E>` names itself)
            let tgt = g.modules[&prim[0]].bound[&name][5..].to_string();
            let last = tgt.rsplit('.').next().unwrap_or(&tgt).to_string();
            let arg = if last != name { last } else { "\0".to_string() };
            let got = self.type_modules(&g.modules[&prim[0]], &arg);
            self.c.borrow_mut().alias.insert(key, got.clone());
            return got;
        }
        let prims: HashSet<String> = prim.iter().map(|mn| self.primary(mn)).collect();
        let mut mods = prim.clone();
        for mn in &exts {
            if !mods.contains(mn) {
                mods.push(mn.clone());
            }
        }
        // one type built per platform (battery-macos.cpp / battery-win.cpp, one
        // directory, one name): a call reaches whichever is compiled, like an
        // interface reaches each implementation
        let dirs: HashSet<&str> = prim.iter().map(|mn| dirname(&g.modules[mn].file)).collect();
        let decl0: HashSet<String> = prim
            .iter()
            .map(|mn| {
                let m = &g.modules[mn];
                if !m.owner.is_empty() && !m.decls.is_empty() {
                    m.decls[0].clone()
                } else {
                    name.clone()
                }
            })
            .collect();
        let variants = dirs.len() == 1 && prims.len() > 1 && decl0.len() == 1;
        (mods, prims.len() == 1 || variants, true)
    }

    /// The Godot project a file is in (the nearest folder with a
    /// project.godot): a `class_name` is global to its project only.
    pub fn godot(&self, rel: &str) -> String {
        let d = dirname(rel).to_string();
        if let Some(h) = self.c.borrow().godot.get(&d) {
            return h.clone();
        }
        let mut up = d.clone();
        while !up.is_empty() && !self.p.root.join(&up).join("project.godot").exists() {
            up = dirname(&up).to_string();
        }
        self.c.borrow_mut().godot.insert(d, up.clone());
        up
    }

    /// A TS/JS file that is a module (imports or exports), not an ambient
    /// `.d.ts` / script whose declarations are global.
    pub fn es_module(&self, rel: &str) -> bool {
        if let Some(h) = self.c.borrow().esm.get(rel) {
            return *h;
        }
        let raw = self.g.by_file.get(rel).and_then(|v| v.first()).map(|n| self.m(n).raw.join("\n")).unwrap_or_default();
        let got = ends(rel, ES_MODULE)
            && re!(r"(?m)^[ \t]*(?:import|export)\b").search(&raw).is_some()
            && !raw.contains("declare global");
        self.c.borrow_mut().esm.insert(rel.to_string(), got);
        got
    }

    /// Which type a module is part of: a C++ class's header and source are
    /// one; a C# partial class's files (and a Swift extension, a Rust impl)
    /// are one per namespace and name.
    pub fn primary(&self, mn: &str) -> String {
        let m = self.m(mn);
        if ends(&m.file, &[".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx", ".c"]) {
            if m.kind == "ext" && !m.owner.is_empty() && !m.decls.is_empty() {
                // `Message::Message()` in a .cc: the one class Message declared
                let own: Vec<&String> = self
                    .by_type
                    .get(&m.decls[0])
                    .map(|v| {
                        v.iter()
                            .filter(|o| {
                                self.m(o).kind != "ext" && ends(&self.m(o).file, &[".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx", ".c"])
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if own.len() == 1 && own[0] != mn {
                    return self.primary(own[0]);
                }
            }
            let stem = m.file.rsplit_once('.').map(|x| x.0).unwrap_or(&m.file);
            let d = if !m.owner.is_empty() && !m.decls.is_empty() { m.decls[0].as_str() } else { "" };
            return format!("{stem}\0{d}"); // Foo.h + Foo.cpp: one class
        }
        if m.kind == "ext" && !m.decls.is_empty() {
            // a Swift extension / Rust impl of the project's one type of that name is part of it
            let own: Vec<&String> = self
                .by_type
                .get(&m.decls[0])
                .map(|v| v.iter().filter(|o| self.m(o).kind != "ext").collect())
                .unwrap_or_default();
            if own.len() == 1 && ends(&m.file, &[".swift", ".rs"]) {
                return self.primary(own[0]);
            }
            return format!("{}\0{}", self.pkg_of(mn), m.decls[0]);
        }
        mn.to_string()
    }

    pub fn pkg_of(&self, mn: &str) -> String {
        match self.g.modules.get(mn) {
            None => String::new(),
            Some(m) => self.file_mod(m).package.clone(),
        }
    }

    fn imported(&self, fm: &'a Module, name: &str, mn: &str) -> bool {
        let m = self.m(mn);
        let own = self.file_mod(m).name.clone();
        let w = fm
            .aliases
            .get(name)
            .filter(|s| !s.is_empty())
            .cloned()
            .or_else(|| fm.imported.get(name).map(|x| x.0.clone()).filter(|s| !s.is_empty()))
            .unwrap_or_default();
        if !w.is_empty() && (w == mn || w == own || (w.starts_with("pkg:") && self.pkg_of(mn) == w[4..])) {
            return true;
        }
        if self.g.modules.contains_key(&w) {
            // through the imported module's re-exports (a crate root's `pub use`)
            let dm = self.defining_module(&w, name, 0);
            if !dm.is_empty() && (dm == mn || dm == own) {
                return true;
            }
        }
        if !w.is_empty() {
            return false; // the import names another package's
        }
        for u in &fm.uses {
            if *u == format!("type:{name}") && m.decls.iter().any(|d| d == name) {
                return true;
            }
        }
        false
    }

    fn in_uses(&self, fm: &'a Module, mn: &str) -> bool {
        let own = self.file_mod(self.m(mn));
        for u in &fm.uses {
            if let Some(pk) = u.strip_prefix("pkg:") {
                if own.package == pk {
                    return true;
                }
            }
            if *u == own.name {
                return true;
            }
        }
        false
    }

    // --- methods ------------------------------------------------------------------

    /// The module, the other parts of its type (a C++ class's header and
    /// .cpp, a Swift extension, a Rust impl in another file), then its
    /// supertypes, nearest first.
    pub fn ancestors(&self, mn: &str) -> Vec<String> {
        if let Some(h) = self.c.borrow().anc.get(mn) {
            return h.clone();
        }
        let (mut out, mut todo, mut seen): (Vec<String>, Vec<String>, HashSet<String>) =
            (Vec::new(), vec![mn.to_string()], HashSet::new());
        while !todo.is_empty() {
            let cur = todo.remove(0);
            if seen.contains(&cur) || !self.g.modules.contains_key(&cur) {
                continue;
            }
            seen.insert(cur.clone());
            out.push(cur.clone());
            let m = self.m(&cur);
            let parts: Vec<String> = self.parts(m).into_iter().filter(|x| !seen.contains(x)).collect();
            todo.splice(0..0, parts);
            for b in &m.bases {
                if self.g.modules.contains_key(b) {
                    todo.push(b.clone());
                }
            }
            if !m.parent.is_empty() {
                todo.push(m.parent.clone());
            }
        }
        self.c.borrow_mut().anc.insert(mn.to_string(), out.clone());
        out
    }

    /// The other modules of the same type: a C++ class's header and source
    /// are one class; an `ext` module (a Swift extension, a Rust impl in
    /// another file) joins the type it extends, and that type gains it.
    pub fn parts(&self, m: &'a Module) -> Vec<String> {
        if m.decls.is_empty() {
            return vec![];
        }
        if let Some(h) = self.c.borrow().parts.get(&m.name) {
            return h.clone();
        }
        let name = if !m.owner.is_empty() || m.kind == "ext" { m.decls[0].clone() } else { m.decls[m.decls.len() - 1].clone() };
        let cands: Vec<String> =
            self.by_type.get(&name).map(|v| v.iter().filter(|x| **x != m.name).cloned().collect()).unwrap_or_default();
        let out: Vec<String> = if ends(&m.file, &[".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx", ".c"]) {
            let own = self.primary(&m.name);
            cands.into_iter().filter(|x| self.primary(x) == own).collect()
        } else if m.kind == "ext" {
            self.type_modules(m, &name).0.into_iter().filter(|x| *x != m.name).collect()
        } else {
            cands
                .into_iter()
                .filter(|x| self.m(x).kind == "ext" && self.type_modules(self.m(x), &name).0.contains(&m.name))
                .collect()
        };
        self.c.borrow_mut().parts.insert(m.name.clone(), out.clone());
        out
    }

    /// A supertype the project doesn't declare: an inherited method may be
    /// the library's.
    pub fn external_base(&self, mn: &str) -> bool {
        self.ancestors(mn).iter().any(|a| self.m(a).unresolved)
    }

    /// `name` on an instance of these modules' type: the definition on the
    /// type or its nearest supertype; else (an interface, an abstract
    /// method) every subtype's implementation. (funcs, found on the type).
    /// `argc`: the call's argument count, where overloads are by arity (Java,
    /// C#, Kotlin, C++).
    pub fn method(&self, mods: &[String], name: &str, argc: Option<usize>) -> (Vec<&'a Func>, bool) {
        let g = self.g;
        let mut found: Vec<&'a Func> = Vec::new();
        for mn in mods {
            let mut first: Option<&'a Func> = None;
            let mut hit = false;
            for a in self.ancestors(mn) {
                if let Some(f) = self.fx(&format!("{a}.{name}")) {
                    if first.is_none() {
                        first = Some(f);
                    }
                    if let Some(n) = argc {
                        if !self.takes(f, name, n) {
                            continue;
                        }
                    }
                    if !found.iter().any(|x| x.id == f.id) {
                        found.push(f);
                    }
                    hit = true;
                    break;
                }
            }
            if !hit {
                if let Some(f) = first {
                    if !found.iter().any(|x| x.id == f.id) {
                        found.push(f); // (no class's overloads take that count as read: the nearest)
                    }
                }
            }
        }
        if !found.is_empty() {
            return (found, true);
        }
        let mut fac: Vec<&'a Func> = Vec::new();
        for mn in mods {
            for a in self.ancestors(mn) {
                let got = self.ts_factory_member(&a, name);
                if !got.is_empty() {
                    for f in got {
                        if !fac.iter().any(|x| x.id == f.id) {
                            fac.push(f);
                        }
                    }
                    break;
                }
            }
        }
        if !fac.is_empty() {
            return (fac, true);
        }
        let mut impls: Vec<&'a Func> = Vec::new();
        let push = |impls: &mut Vec<&'a Func>, f: &'a Func| {
            if !impls.iter().any(|x| x.id == f.id) {
                impls.push(f);
            }
        };
        for mn in mods {
            if let Some(m) = g.modules.get(mn) {
                if !m.methods.is_empty() && m.file.ends_with(".go") && (m.methods.iter().any(|x| x == name) || self.go_embeds_lib(m)) {
                    // (`Write` of an interface embedding `http.ResponseWriter`: the implementing types' Write)
                    for f in self.go_impls(m, name) {
                        push(&mut impls, f);
                    }
                }
            }
            for sub in g.children.get(mn).map(|v| v.as_slice()).unwrap_or(&[]) {
                let mut got: Option<&'a Func> = None;
                let mut parts = vec![sub.clone()];
                parts.extend(self.parts(self.m(sub)));
                for part in parts {
                    let f = self.fx(&format!("{part}.{name}"));
                    if let Some(f) = f {
                        push(&mut impls, f);
                    }
                    if got.is_none() {
                        got = f;
                    }
                }
                if got.is_none() && self.m(sub).kind != "interface" {
                    // the implementing class inherits it: `class MapperConfigurationExpression : Profile,
                    // IMapperConfigurationExpression` runs Profile's CreateMap
                    for a in self.ancestors(sub).into_iter().skip(1) {
                        if let Some(f) = self.fx(&format!("{a}.{name}")) {
                            if self.m(&a).kind != "interface" {
                                push(&mut impls, f);
                            }
                            break;
                        }
                    }
                }
            }
        }
        for mn in mods {
            // Kotlin / C# / Swift extensions on the type, or on a supertype / interface it has
            for a in self.ancestors(mn) {
                let m = self.m(&a);
                let ds: Vec<&String> = if !m.owner.is_empty() || m.kind == "ext" {
                    m.decls.iter().take(1).collect()
                } else {
                    m.decls.iter().rev().take(1).collect()
                };
                for d in ds {
                    let mut got = self.ext_funcs(d, name);
                    if got.is_empty() && d.contains('`') {
                        got = self.ext_funcs(d.split('`').next().unwrap(), name);
                    }
                    for f in got {
                        push(&mut impls, f);
                    }
                }
                // (a library supertype: `class Response : Closeable` and the project's `fun Closeable.closeQuietly()`)
                for b in &m.lib_bases {
                    let key = b.split('<').next().unwrap().rsplit('.').next().unwrap().to_string();
                    for f in self.ext_funcs(&key, name) {
                        push(&mut impls, f);
                    }
                }
            }
            if !impls.is_empty() {
                return (impls, false); // (extensions: the receiver's type is sure, each overload may be the one)
            }
        }
        (impls, false)
    }

    /// Some overload of `name` in g's class takes `argc` arguments (or the
    /// overloads can't be read: assume so).
    pub fn takes(&self, gf: &Func, name: &str, argc: usize) -> bool {
        let key = (gf.module.clone(), name.to_string());
        let cached = self.c.borrow().arity.get(&key).cloned();
        let spans = match cached {
            Some(s) => s,
            None => {
                let s = arity::arities(self.m(&gf.module), name);
                self.c.borrow_mut().arity.insert(key, s.clone());
                s
            }
        };
        spans.is_empty() || spans.iter().any(|(lo, hi)| *lo <= argc && argc <= *hi)
    }

    /// These functions are one type's (overloads, or its parts).
    pub fn one_type(&self, funcs: &[&Func]) -> bool {
        funcs.iter().map(|f| self.primary(&f.module)).collect::<HashSet<_>>().len() == 1
    }

    /// A Go interface embedding a library interface (`http.ResponseWriter` on a line of its own).
    fn go_embeds_lib(&self, iface: &Module) -> bool {
        if iface.kind != "interface" || iface.raw.is_empty() {
            return false;
        }
        let text = iface.raw.join("\n");
        let d = iface.decls.first().cloned().unwrap_or_default();
        let Some(hm) = rxd(&format!(r"\btype\s+{}\s+interface\s*\{{", esc(&d))).search(&text) else { return false };
        let close = crate::lang::lambdas::matching(&text, hm.end() - 1);
        let end = if close > 0 { close as usize } else { text.len() };
        re!(r"(?m)^[ \t]*[a-z]\w*\.[A-Z]\w*[ \t]*$").search(&text[hm.end()..end]).is_some()
    }

    /// A Go interface's implementations: every project type whose methods
    /// (across its files) cover the interface's method set.
    fn go_impls(&self, iface: &Module, name: &str) -> Vec<&'a Func> {
        let cached = self.c.borrow().goimpl.get(&iface.name).cloned();
        let tys = match cached {
            Some(t) => t,
            None => {
                let mut have: IndexMap<(String, String), HashSet<String>> = IndexMap::new();
                let mut where_: IndexMap<(String, String), Vec<String>> = IndexMap::new();
                for m in self.g.modules.values() {
                    if !m.owner.is_empty() && m.file.ends_with(".go") && m.kind != "interface" && !m.decls.is_empty() {
                        let k = (self.pkg_of(&m.name), m.decls[0].clone());
                        have.entry(k.clone()).or_default().extend(m.funcs.iter().map(|f| f.name.clone()));
                        where_.entry(k).or_default().push(m.name.clone());
                    }
                }
                let need: HashSet<&String> = iface.methods.iter().collect();
                let t: Vec<Vec<String>> = where_
                    .into_iter()
                    .filter(|(k, _)| need.iter().all(|n| have[k].contains(*n)))
                    .map(|(_, v)| v)
                    .collect();
                self.c.borrow_mut().goimpl.insert(iface.name.clone(), t.clone());
                t
            }
        };
        tys.iter().flatten().filter_map(|mn| self.fx(&format!("{mn}.{name}"))).collect()
    }

    /// A field's value; Unknown when the class declares it without a type
    /// the code states (so it is not a library supertype's).
    pub fn field(&self, mods: &[String], name: &str, depth: usize) -> Option<Val> {
        let g = self.g;
        for mn in mods {
            for a in self.ancestors(mn) {
                let m = self.m(&a);
                if let Some(held) = m.bound.get(name).filter(|s| !s.is_empty()) {
                    let got = self.held(m, None, held, depth + 1, Some(&[mn.clone()]));
                    return Some(got.unwrap_or(Val::Unknown));
                }
            }
        }
        if !mods.is_empty() && ends(&self.m(&mods[0]).file, C_EXTS) && depth < MAX {
            for mn in mods {
                // `FilePath::IsRootDirectory` defined in a .cc: the class its header declares has the fields
                let m = self.m(mn);
                if m.owner.is_empty() || m.decls.is_empty() {
                    continue;
                }
                for other in self.type_modules(m, m.decls[0].split('`').next().unwrap()).0 {
                    let om = self.m(&other);
                    if other != *mn && om.file != m.file && om.bound.contains_key(name) {
                        let got = self.held(om, None, &om.bound[name], depth + 1, Some(&[other.clone()]));
                        return Some(got.unwrap_or(Val::Unknown));
                    }
                }
            }
        }
        if !mods.is_empty() && ends(&self.m(&mods[0]).file, JS_EXTS) && depth < MAX {
            for mn in mods {
                for a in self.ancestors(mn) {
                    let sets = self.expando().borrow().get(&(a.clone(), name.to_string())).cloned();
                    if let Some(sets) = sets {
                        if !sets[0].2[0].starts_with("\0fn:") {
                            // `ctx.response = Object.create(...)`
                            let mut got: Vec<Option<Val>> = Vec::new();
                            for (mm, fi, ch) in &sets {
                                let md = g.modules.get(mm).unwrap();
                                let r = self.chain(md, md.funcs.get(*fi), ch, depth + 1, None, None);
                                if !got.contains(&r) {
                                    got.push(r);
                                }
                            }
                            return if got.len() == 1 { got.pop().unwrap() } else { None };
                        }
                    }
                }
            }
        }
        None
    }
}

pub(crate) fn dirname(p: &str) -> &str {
    p.rsplit_once('/').map(|(a, _)| a).unwrap_or("")
}

/// Which crates a Rust project uses but doesn't hold: see `Types::rs_external`.
pub(crate) fn ext_of_file(rel: &str) -> &str {
    ext_of(rel)
}

/// C#'s keyword names for System types
pub const CS_KEYWORD_TYPES: &[&str] = &[
    "string", "object", "int", "uint", "long", "ulong", "short", "ushort", "byte", "sbyte", "bool", "char", "decimal",
    "double", "float", "nint", "nuint", "dynamic",
];
pub const PY_BUILTIN: &[&str] = &[
    "dict", "list", "set", "frozenset", "tuple", "str", "bytes", "bytearray", "int", "float", "bool", "defaultdict",
    "deque", "OrderedDict", "Counter",
];
pub const GO_BUILTIN: &[&str] = &[
    "string", "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64", "float32", "float64",
    "bool", "byte", "rune", "error", "any", "uintptr", "complex64", "complex128",
];

/// Each class's written supertypes -> modules (`parent` = the first); a
/// supertype the project doesn't declare marks the class `unresolved` (an
/// inherited method may be the library's) (infer.resolve_bases). Runs
/// before the `Types` the edges use are built.
pub fn resolve_bases(g: &mut Graph, p: &Project) {
    struct Upd {
        name: String,
        unresolved: Option<bool>,
        bases: Option<Vec<String>>,
        lib: Option<Vec<String>>,
        parent: Option<String>,
    }
    let mut upds: Vec<Upd> = Vec::new();
    {
        let t = Types::new(g, p);
        for m in g.modules.values() {
            let mut u = Upd { name: m.name.clone(), unresolved: None, bases: None, lib: None, parent: None };
            if !m.parent.is_empty()
                && m.bases.is_empty()
                && !g.modules.contains_key(&m.parent)
                && upper1(&m.parent)
                && !ends(&m.file, &[".ex", ".exs", ".rb", ".rake", ".ru"])
            {
                u.unresolved = Some(true); // GDScript `extends CharacterBody3D`: an engine class
            }
            if m.bases.is_empty() || ends(&m.file, &[".rb", ".rake", ".ru", ".py", ".pyw"]) {
                // Ruby, Python: resolved by their adapters
                if u.unresolved.is_some() {
                    upds.push(u);
                }
                continue;
            }
            let (mut got, mut missing, mut lib): (Vec<String>, bool, Vec<String>) = (vec![], false, vec![]);
            let fm = if m.owner.is_empty() { m } else { g.modules.get(&m.owner).unwrap_or(m) };
            for b in &m.bases {
                if fm.external.contains(b) {
                    // `use std::fmt::Debug`: the library's, not a project type of that name
                    missing = true;
                    lib.push(b.clone());
                    continue;
                }
                let (mut mods, _, declared) = t.type_modules(m, b);
                if m.file.ends_with(".go") {
                    // (Go: a type's methods are in `<file>.<Type>`, not the file's module)
                    let last = b.rsplit('.').next().unwrap().to_string();
                    mods = mods
                        .into_iter()
                        .map(|x| {
                            let c = format!("{x}.{last}");
                            if g.modules.contains_key(&c) { c } else { x }
                        })
                        .collect();
                }
                let nonext: Vec<String> =
                    mods.iter().filter(|x| **x != m.name && g.modules[*x].kind != "ext").cloned().collect();
                let mods: Vec<String> = if !nonext.is_empty() { nonext } else { mods.into_iter().filter(|x| *x != m.name).collect() };
                if !mods.is_empty() {
                    got.push(mods[0].clone());
                } else if fm.external.contains(b) || (!declared && !from_project(g, fm, b)) {
                    missing = true; // a library supertype (imported from outside, or declared nowhere)
                    lib.push(b.clone());
                }
            }
            if !got.is_empty() && m.parent.is_empty() {
                u.parent = Some(got[0].clone());
            }
            u.bases = Some(got);
            u.lib = Some(lib);
            u.unresolved = Some(missing);
            upds.push(u);
        }
    }
    for u in upds {
        let m = g.modules.get_mut(&u.name).unwrap();
        if let Some(x) = u.unresolved {
            m.unresolved = x;
        }
        if let Some(b) = u.bases {
            m.bases = b;
        }
        if let Some(l) = u.lib {
            m.lib_bases = l;
        }
        if let Some(pa) = u.parent {
            m.parent = pa;
        }
    }
}

/// `name` is imported from a module or package of the project (a re-export
/// we couldn't follow), not from a library (`import java.util.ArrayList`).
fn from_project(g: &Graph, fm: &Module, name: &str) -> bool {
    let w = fm
        .aliases
        .get(name)
        .filter(|s| !s.is_empty())
        .cloned()
        .or_else(|| fm.imported.get(name).map(|x| x.0.clone()))
        .unwrap_or_default();
    if w.is_empty() {
        return false;
    }
    if let Some(pk) = w.strip_prefix("pkg:") {
        return g.packages.contains_key(pk);
    }
    g.modules.contains_key(&w)
}
