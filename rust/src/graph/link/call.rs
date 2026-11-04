//! One call's callees (the body of core._build_edges' loop).

use std::collections::{HashMap, HashSet};

use super::helpers::{swift_conforms, DOM_GLOBALS, RS_STD_METHODS};
use super::{file_mod_of, is_static, m_or_owner_imports, shares_family, Targets, CTOR_NAMES, SELF};
use crate::gate::is_test;
use crate::graph::infer::{self, ends, esc, lower1, re, rxd, upper1, Types, Val};
use crate::graph::pylink;
use crate::graph::Graph;
use crate::model::{Call, Func, Module};
use crate::project::{ext_of, Project};

fn module_q(q: &str) -> bool {
    re!(r"(?![A-Z0-9_]+$)[A-Z]\w*(?:\.[A-Z]\w*)*$").match_start(q).is_some()
}

const PARAM_FLOW_EXTS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".py", ".pyw"];
const EXPLICIT_RECEIVER: &[&str] = &[".go", ".rs", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".php"];
const C_FAMILY: &[&str] = &[".c", ".h", ".cpp", ".cc", ".cxx", ".hpp", ".hh"];
const CPP_EXTS: &[&str] = &[".cpp", ".cc", ".cxx", ".hpp", ".hh"];

/// What `resolve` found: the callees (`Targets`), and edges drawn directly
/// (a C macro's expansion) that the enclosing functions don't inherit.
pub(super) struct Resolved {
    pub targets: Targets,
    pub direct: Vec<String>,
    /// the call reaches a parameter: what its callers pass (core `pending`)
    pub param_call: bool,
    /// a guessed call on a parameter (core `pending_recv`)
    pub param_recv: bool,
    /// how many of `targets` the resolver found before adding overrides
    /// (what the counters and the trace judge the call by)
    pub primary: usize,
}

impl Resolved {
    fn nothing() -> Resolved {
        Resolved { targets: vec![], direct: vec![], param_call: false, param_recv: false, primary: 0 }
    }
}

pub(super) struct Resolver<'a> {
    pub g: &'a Graph,
    pub p: &'a Project,
    pub t: Types<'a>,
    pub locals: HashMap<String, HashSet<String>>,
    pub tests: HashMap<String, bool>,
    pub module_js: HashMap<String, bool>,
    /// [project.groups]: "group:<id>" -> module
    pub groups: HashMap<String, String>,
    pub scope_cache: HashMap<String, HashSet<String>>,
    pub ext_cache: Option<HashMap<(String, String), Vec<String>>>,
    pub go_types: Option<HashMap<(String, String), Vec<String>>>,
    pub c_macros: Option<HashMap<String, Vec<String>>>,
    pub js_imported: Option<HashSet<String>>,
    pub cs_global_static: Option<HashSet<String>>,
}

impl<'a> Resolver<'a> {
    pub fn is_test(&mut self, rel: &str) -> bool {
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
                || re!(r#"(?m)\brequire\s*\(\s*['"]|\bmodule\.exports\b|^[ \t]*exports\.\w+\s*=|^[ \t]*(?:import|export)\b"#)
                    .search(&raw)
                    .is_some()
                    && re!(r"(?m)^[ \t]*(?:global|globalThis|window)\.\w+\s*=").search(&raw).is_none();
            self.module_js.insert(m.file.clone(), hit);
            return hit;
        }
        matches!(
            ext,
            ".ex" | ".exs" | ".kt" | ".kts" | ".java" | ".ts" | ".tsx" | ".mts" | ".cts" | ".py" | ".pyw" | ".go" | ".rs" | ".swift"
                | ".php" | ".rb" | ".rake" | ".ru"
        )
    }

    /// `name` is a parameter or local of `f`, so a bare `name()` calls that
    /// value (core._shadowed).
    pub fn shadowed(&mut self, f: &Func, name: &str) -> bool {
        if !self.locals.contains_key(&f.id) {
            let rb = matches!(ext_of(&f.file), ".rb" | ".rake" | ".ru");
            let mut loc: HashSet<String> = HashSet::new();
            let locals = re!(r"\b(?:val|var|let|const)\s+([A-Za-z_$][\w$]*)|[(,]\s*([A-Za-z_$][\w$]*)\s*:(?!:)");
            for m in locals.find_iter(&f.code) {
                let a = m.get(1).unwrap_or("");
                let b = m.get(2).unwrap_or("");
                let v = if !a.is_empty() { a } else if rb { "" } else { b };
                if !v.is_empty() {
                    loc.insert(v.to_string());
                }
            }
            let destr = re!(r"\b(?:val|var|let|const)\s*[\[{(]([^\]})=;]*)[\]})]\s*(?::[^=;]+)?=");
            let spread = re!(r"^\s*\.\.\.");
            let ident = re!(r"^[A-Za-z_$][\w$]*$");
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

    fn is_test_mod(&mut self, mn: &str) -> bool {
        let file = self.g.modules[mn].file.clone();
        self.is_test(&file)
    }

    /// One call's callees, as _build_edges's loop body finds them.
    pub(super) fn resolve(&mut self, f: &'a Func, md: &'a Module, c: &Call) -> Resolved {
        let g = self.g;
        let t = &self.t;
        let q = c.qual.as_str();
        let fext = ext_of(&f.file).to_string();
        let mut targets: Vec<(&'a Func, bool)> = Vec::new();
        let mut recv: Vec<String> = Vec::new(); // the receiver's type, when known: overrides only in its subtypes run
        let mut pending = false;
        let mut pending_recv = false;
        let is_js = ends(&f.file, infer::JS_EXTS);
        let is_js_file = ends(&f.file, infer::JS_FILE_EXTS);
        let nothing = || Resolved::nothing();

        if c.chain.len() == 1 && c.chain[0] == "<implicit>" {
            // `.option(x)` / `case .value(let v)`: an enum case or a static member of the
            // type the context expects. A pattern runs nothing; with the expected type
            // known, that type's static function (or its enum case: nothing); else a
            // guess among the project's static functions of that name
            let (kind_, got) = self.implicit_member(md, f, c);
            if kind_ == "none" {
                return nothing();
            }
            if kind_ == "exact" {
                targets = got.into_iter().map(|h| (h, true)).collect();
            } else {
                let stat: Vec<&Func> = g
                    .by_name
                    .get(&c.name)
                    .map(|ids| ids.iter().filter_map(|id| g.func(id)).filter(|h| is_static(g, h)).collect())
                    .unwrap_or_default();
                if stat.is_empty() {
                    return nothing();
                }
                targets = stat.into_iter().map(|h| (h, false)).collect();
            }
        } else if !c.chain.is_empty() && ends(&f.file, C_FAMILY) && {
            let mut tg = Vec::new();
            let r = self.cpp_namespace_call(f, c, &mut tg);
            if r {
                targets = tg;
            }
            r
        } {
            if targets.is_empty() {
                return nothing(); // `std::move(x)`: the standard library's
            }
        } else if !c.chain.is_empty() {
            // the receiver's type, step by step (see infer)
            let t = &self.t;
            t.line.set(c.line);
            let mut v = t.chain(md, Some(f), &c.chain, 0, None, None);
            if matches!(v, Some(Val::Wrap(_))) {
                v = Some(infer::ext()); // `self.bump().map(...)`: Option's own method
            }
            if infer::is_ext(&v) {
                // a library value; a Kotlin / C# / Swift extension the project
                // declares on that library type is the one exception
                let tys = v.as_ref().unwrap().mods().to_vec();
                let mut exts: Vec<&'a Func> = if let Some(t0) = tys.first() {
                    let a = t.ext_funcs(t0, &c.name);
                    if a.is_empty() { t.ext_funcs(t0.split('`').next().unwrap(), &c.name) } else { a }
                } else {
                    vec![]
                };
                if exts.is_empty() && !tys.is_empty() && f.file.ends_with(".cs") {
                    // `connection.Execute(...)` on a DbConnection with Dapper's `Execute(this IDbConnection cnn)`:
                    // a library interface the value's library class implements, by .NET's naming
                    // (IDbConnection / DbConnection, SqlConnection; IDataReader / DbDataReader)
                    let words = |s: &str| -> Vec<String> { re!(r"[A-Z][a-z0-9]*").find_iter(s).iter().map(|x| x.group0().to_string()).collect() };
                    let lastw: Vec<String> = words(tys[0].split('`').next().unwrap()).into_iter().rev().take(1).collect();
                    let mut cand: Vec<&Func> = Vec::new();
                    for ((rt, nm), gs) in &t.ext_fns {
                        if *nm == c.name
                            && !t.by_type.contains_key(rt.split('`').next().unwrap())
                            && re!(r"I[A-Z]").match_start(rt).is_some()
                            && words(rt.split('`').next().unwrap()).into_iter().rev().take(1).collect::<Vec<_>>() == lastw
                        {
                            cand.extend(gs.iter().filter_map(|id| t.fx(id)));
                        }
                    }
                    if !cand.is_empty() && !lastw.is_empty() && t.one_type(&cand) {
                        exts = cand;
                    }
                }
                if exts.is_empty() && !tys.is_empty() && f.file.ends_with(".swift") {
                    // `sample.wrapped(to:)` on a String: an extension of a protocol String conforms to
                    for pr in swift_conforms(&tys[0]) {
                        let key = (pr.to_string(), c.name.clone());
                        if t.ext_fns.contains_key(&key) {
                            exts = t.ext_funcs(pr, &c.name);
                            break;
                        }
                    }
                }
                if !exts.is_empty() && f.file.ends_with(".swift") {
                    exts.retain(|h| self.swift_labels_match(md, c, h));
                }
                let own: Vec<&Func> = exts.iter().copied().filter(|h| h.file == f.file).collect();
                if own.len() == 1 {
                    exts = own;
                }
                if exts.is_empty() {
                    return nothing();
                }
                let one = exts.len() == 1;
                targets = exts.into_iter().map(|h| (h, one)).collect();
                v = None;
            }
            if let Some(Val::Static(mods, _)) = &v {
                if mods.len() > 1 && f.file.ends_with(".rs") {
                    let root_ = self.rs_crate_dir(&f.file); // (each test binary's own `mod utils`)
                    let own_: Vec<String> = if root_.is_empty() { vec![] } else { mods.iter().filter(|mn| g.modules[*mn].file.starts_with(&root_)).cloned().collect() };
                    if own_.len() == 1 {
                        v = Some(Val::Static(own_, true));
                    }
                }
            }
            if matches!(v, Some(Val::Static(..)) | Some(Val::Inst(..))) && is_js {
                // `ctx.redirect()`: delegated to ctx.response
                if let Some(fw) = self.t.forward(md, v.as_ref().unwrap(), &c.name, 0) {
                    v = Some(fw);
                }
            }
            if let Some(vv) = v.clone() {
                let kind = vv.kind();
                let mods_: Vec<String> = vv.mods().to_vec();
                let ex = vv.exact();
                let t = &self.t;
                let (mut hits, on_type): (Vec<&'a Func>, bool);
                if kind == "ns" {
                    hits = vec![];
                    on_type = true;
                } else if kind == "pkg" {
                    hits = t.in_scope(&format!("pkg:{}", mods_[0]), &c.name, 0);
                    on_type = true;
                } else if kind == "static" {
                    hits = mods_.iter().flat_map(|mn| t.in_scope(mn, &c.name, 0)).collect();
                    let mut on = true;
                    if hits.is_empty() && is_js {
                        if let Some(g_) = t.func_ref(&mods_, &c.name, 0) {
                            hits = vec![g_]; // `z.string()` with `export { stringType as string }`
                        }
                    }
                    if hits.is_empty() && is_js {
                        if let Some(g_) = t.expando_fn(&mods_, &c.name) {
                            hits = vec![g_]; // `validators.transitional()`
                        }
                    }
                    if hits.is_empty() {
                        let (h, o) = t.method(&mods_, &c.name, self.argc(md, c));
                        hits = h;
                        on = o;
                    }
                    on_type = on;
                } else {
                    let (h, o) = t.method(&mods_, &c.name, self.argc(md, c));
                    hits = h;
                    let mut on = o;
                    if hits.is_empty() && kind == "inst" && is_js {
                        if let Some(g_) = t.expando_fn(&mods_, &c.name) {
                            hits = vec![g_];
                            on = true; // `axios.create()`: `instance.create = function create(...)`
                        }
                    }
                    if kind == "inst" {
                        // (`super.x()` runs the parent's x itself: no subtype's)
                        recv = if matches!(c.chain.last().map(|s| s.as_str()), Some("super" | "base" | "parent")) { vec!["\0super".to_string()] } else { mods_.clone() };
                    }
                    on_type = on;
                }
                if hits.len() > 1 && c.targs >= 0 {
                    // C#: `NoOp()` is not `NoOp<T>()`
                    let want = c.targs.to_string();
                    let kept: Vec<&Func> = hits.iter().copied().filter(|h| h.targs.is_empty() || h.targs.split(',').any(|x| x == want)).collect();
                    if !kept.is_empty() {
                        hits = kept;
                    }
                }
                if !hits.is_empty() {
                    // on the type: one definition (or one type's overloads); through an
                    // interface: each implementation runs. Several same-named types and
                    // only one has the method: the receiver's name settles it.
                    let sure = (ex && (hits.len() == 1 || !on_type || t.one_type(&hits))) || (on_type && t.one_type(&hits));
                    targets = hits.into_iter().map(|h| (h, sure)).collect();
                } else if kind != "pkg" && mods_.iter().any(|mn| t.external_base(mn)) {
                    return nothing(); // inherited from a library supertype
                } else if kind == "inst"
                    && !mods_.is_empty()
                    && is_js
                    && mods_.iter().any(|mn| t.ancestors(mn).iter().any(|a| g.modules[a].kind == "interface" && g.modules[a].methods.contains(&c.name)))
                {
                    return nothing(); // a TS type's member no project class implements: an object's, unknown
                } else if kind == "inst"
                    && !mods_.is_empty()
                    && mods_.iter().all(|mn| {
                        let m = &g.modules[mn];
                        m.kind == "interface"
                            && ((m.file.ends_with(".d.ts") && !t.es_module(&m.file)) || DOM_GLOBALS.contains(&m.decls.first().map(|s| s.as_str()).unwrap_or("")))
                    })
                {
                    return nothing(); // `interface CanvasRenderingContext2D {...}` in global.d.ts adds to the DOM's
                } else if kind == "inst" && !mods_.is_empty() && mods_.iter().all(|mn| g.modules[mn].kind == "ext") && ends(&f.file, &[".swift", ".kt", ".kts", ".cs"]) {
                    return nothing(); // `Data` the project only extends: a method not in an extension is the library's
                }
            }
        }
        if q.is_empty() && !md.name.is_empty() && targets.is_empty() && ends(&f.file, infer::JS_FILE_EXTS) && f.bound.get(&c.name).is_some_and(|b| b.starts_with("lam:")) {
            self.t.line.set(c.line);
            if infer::is_ext(&self.t.first(md, Some(f), &c.name, 0, None)) {
                return nothing(); // `new Promise((resolve) => resolve(x))`: the runtime's function
            }
        }
        if q.is_empty() && !ends(&f.file, &[".py", ".pyw", ".ex", ".exs"]) && targets.is_empty() {
            // a bare call in a class: its own (or an inherited) method first;
            // `Foo(...)` / `new Foo(...)` / `: Base(args)`: Foo's constructor
            let own = if !md.owner.is_empty() || !md.kind.is_empty() { self.t.own_class(md, Some(f)) } else { None };
            // (Go, Rust, JS/TS, PHP: a method needs its receiver, `c.Name()` / `self.x()` /
            // `this.x()` / `$this->x()`; a bare `MarkFlagRequired(...)` in a method is the package's)
            let (mut hits, on_type) = match &own {
                Some(o) => self.t.method(o, &c.name, None),
                None => (vec![], false),
            };
            if ends(&f.file, EXPLICIT_RECEIVER) {
                hits.retain(|h| !self.needs_receiver(h));
            }
            let fm_name = file_mod_of(g, md).name.clone();
            if !hits.is_empty() && on_type && hits.len() == 1 {
                targets = vec![(hits[0], true)];
            } else if upper1(&c.name) && self.t.in_scope(&fm_name, &c.name, 0).is_empty() {
                let (mut tmods, mut ex, mut declared) = self.t.type_modules(md, &c.name);
                let fm_i = file_mod_of(g, md);
                if !declared && is_js && fm_i.imported.contains_key(&c.name) {
                    let (w, n) = fm_i.imported[&c.name].clone();
                    let dm_ = self.t.defining_module(&w, &n, 0); // `const Koa = require('..')`; `new Koa()`
                    if !dm_.is_empty() && (!g.modules[&dm_].decls.is_empty() || g.modules[&dm_].kind == "class") {
                        tmods = vec![dm_];
                        ex = true;
                        declared = true;
                    }
                }
                if declared && !self.is_test(&f.file) {
                    // product code never builds a test's type (a test target's `extension URL`)
                    tmods = tmods.into_iter().filter(|m_| !self.is_test_mod(m_)).collect();
                    declared = !tmods.is_empty();
                }
                if declared {
                    let (ctor0, _) = if !tmods.is_empty() {
                        let ok: Vec<String> = tmods.iter().filter(|m_| self.t.searchable(m_) || !g.modules[*m_].kind.is_empty()).cloned().collect();
                        self.t.method(&ok, &c.name, None)
                    } else {
                        (vec![], false)
                    };
                    let mut ctor: Vec<&Func> = ctor0.into_iter().filter(|h| h.name == c.name).collect();
                    if ctor.is_empty() {
                        ctor = tmods
                            .iter()
                            .flat_map(|m_| ["constructor", "init", "__construct", "new"].into_iter().filter_map(move |n| g.func(&format!("{m_}.{n}"))))
                            .take(1)
                            .collect();
                    }
                    let mut seen = HashSet::new();
                    ctor.retain(|h| seen.insert(h.id.clone()));
                    if !ctor.is_empty() {
                        let sure = ex && (ctor.len() == 1 || self.t.one_type(&ctor));
                        targets = ctor.into_iter().map(|h| (h, sure)).collect();
                    } else {
                        return nothing(); // a project type with no constructor of its own: no function runs
                    }
                }
            }
            if targets.is_empty() {
                if let Some(o) = &own {
                    if hits.is_empty() && o.iter().any(|m_| self.t.external_base(m_)) && self.t.in_scope(&fm_name, &c.name, 0).is_empty() {
                        // inherited from a library supertype (Qt's `disconnect`, `data`)
                        return nothing();
                    }
                }
            }
        }
        if !targets.is_empty() {
        } else if q == "super" && !md.parent.is_empty() {
            if let Some(h) = g.lookup(&md.parent, &c.name) {
                targets = vec![(h, true)];
            }
        } else if q == "super" && ends(&f.file, &[".rb", ".rake", ".ru"]) {
            // Ruby's `super`: the same method in the nearest ancestor (superclass, included modules)
            let own = self.t.own_class(md, Some(f));
            let mut found = false;
            if let Some(o) = &own {
                for a in self.t.ancestors(&o[0]).into_iter().skip(1) {
                    if let Some(h) = g.func(&format!("{a}.{}", c.name)) {
                        if h.id != f.id {
                            targets = vec![(h, true)];
                            found = true;
                            break;
                        }
                    }
                }
                if !found && self.t.external_base(&o[0]) {
                    return nothing(); // a library superclass's
                }
            }
        } else if SELF.contains(&q) && g.lookup(&md.name, &c.name).is_some() && (q != "cls" || !md.owner.is_empty()) {
            // `self.x()` / `this.x()` / `Self::x()`: the caller's own class
            // (a Python class is its own module; elsewhere, the file)
            targets = vec![(g.lookup(&md.name, &c.name).unwrap(), true)];
        } else if !q.is_empty() && md.external.contains(q) && !f.bound.contains_key(q) {
            // `json.dumps()`, `fmt.Println()`, `path.join()`: a library the file imports
            return nothing();
        } else if !q.is_empty() {
            // each way to name the qualifier, most certain first
            let held = f.bound.get(q).filter(|s| !s.is_empty()).or_else(|| md.bound.get(q)).cloned().unwrap_or_default();
            if let Some(mn) = self.groups.get(&held).cloned() {
                if let Some(h) = g.lookup(&mn, &c.name) {
                    targets.push((h, true));
                }
            } else if let Some(ty) = held.strip_prefix("type:") {
                // a declared type: its module's function, or a Kotlin extension on it. A
                // type outside the project: nothing (a `String`'s `split` is not ours); a
                // project type that misses (inherited from a library, an extension we can't see):
                // the usual ways
                let (mut hits, in_project) = self.typed_targets(md, ty, &c.name);
                let own: Vec<&Func> = hits.iter().copied().filter(|h| h.file == f.file).collect();
                if own.len() == 1 {
                    hits = own; // a file's own (private) extension shadows the others'
                }
                if !hits.is_empty() {
                    let one = hits.len() == 1;
                    targets = hits.into_iter().map(|h| (h, one)).collect();
                } else if !in_project && !(ends(&f.file, infer::DYNAMIC) && self.t.extended.contains(ty)) {
                    return nothing(); // (`n: Node` in GDScript may hold a project script: not this)
                }
            }
            if targets.is_empty() {
                if let Some(a) = md.aliases.get(q) {
                    let hits = self.t.in_scope(a, &c.name, 0); // (through a package's re-exports)
                    if hits.len() == 1 {
                        targets.push((hits[0], true));
                    }
                }
            }
            if targets.is_empty() && g.modules.contains_key(q) {
                if let Some(h) = g.lookup(q, &c.name) {
                    targets.push((h, true));
                }
            }
            if targets.is_empty() {
                if let Some((w, n)) = md.imported.get(q) {
                    // a named import (`import { API } from "./helpers"`): its module's function
                    let dm = self.t.defining_module(w, n, 0);
                    if let Some(h) = (!dm.is_empty()).then(|| g.lookup(&dm, &c.name)).flatten() {
                        targets.push((h, true));
                    }
                }
            }
            if targets.is_empty() {
                let mut names = vec![q.to_string()];
                if let Some(a) = md.aliases.get(q) {
                    names.push(a.rsplit('.').next().unwrap_or(a).to_string());
                }
                for nm in names {
                    let hits: Vec<&Func> = g.short.get(&nm).map(|ms| ms.iter().filter_map(|mn| g.lookup(mn, &c.name)).collect()).unwrap_or_default();
                    if !hits.is_empty() {
                        let one = hits.len() == 1;
                        targets = hits.into_iter().map(|h| (h, one)).collect();
                        break;
                    }
                }
            }
        } else if crate::model::is_py(&f.file)
            && f.kind != "top"
            && c.name.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
            && (f.imported.contains_key(&c.name) || rxd(&format!(
                r"(?m)^[ \t]+(?:from[ \t]+[\w.]+[ \t]+)?import[ \t][^\n]*(?:(?<![\w.]){n}\b(?![ \t]+as\b)|\bas[ \t]+{n}\b)",
                n = esc(&c.name)
            ))
            .search(&f.code)
            .is_some()
            && m_or_owner_imports(g, md, &c.name))
        {
            // `from ._termui_impl import get_pager_file` inside the function: that one, not the
            // module's own function of the same name (Python scoping: the local binding wins), and
            // not another function's import of the same name (f.imported: two routes, two render_pdf)
            let (w_, n_) = f.imported.get(&c.name).cloned().unwrap_or_else(|| file_mod_of(g, md).imported[&c.name].clone());
            let hits = if w_.is_empty() { Vec::new() } else { self.t.in_scope(&w_, &n_, 0) };
            if hits.len() == 1 {
                targets.push((hits[0], true));
            } else {
                return nothing();
            }
        } else if self.shadowed(f, &c.name) {
            // a parameter or local holding a function: the helper the
            // caller itself defines (`const nudge = () => ...` inside a
            // component) when there is one, else a name guess at most
            let head = rxd(&format!(r"^\s*{}\s*:", esc(&c.name)));
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
                        && !(h.start >= 1 && md.raw.get(h.start - 1).is_some_and(|l| head.match_start(l).is_some())) // (not `key: () =>`)
                })
                .collect();
            if inner.len() == 1 {
                targets.push((inner[0], true));
            } else if inner.is_empty() && self.scoped_language(&f.file, md) {
                if ends(&f.file, PARAM_FLOW_EXTS) && self.param_names(f).contains(&c.name) {
                    pending = true; // a parameter: what the callers pass (after this pass)
                }
                return Resolved { targets: vec![], direct: vec![], param_call: pending, param_recv: false, primary: 0 }; // a closure or a parameter: no project function by that name
            }
        } else {
            // own module, then a name imported by itself, then modules and
            // packages imported whole, then the rest of its own package.
            // (A bare name in a Python method means the file's, not the class's.)
            let smod = file_mod_of(g, md);
            let mut hits = self.t.in_scope(&smod.name, &c.name, 0);
            if hits.is_empty() {
                if let Some((w, n)) = smod.imported.get(&c.name) {
                    hits = self.t.in_scope(w, n, 0);
                }
            }
            if hits.is_empty() {
                for w in &smod.uses {
                    hits = self.t.in_scope(w, &c.name, 0);
                    if !hits.is_empty() {
                        break;
                    }
                }
            }
            if hits.is_empty() && !smod.package.is_empty() {
                hits = self.t.in_scope(&format!("pkg:{}", smod.package), &c.name, 0);
            }
            if hits.is_empty() && crate::model::is_py(&f.file) {
                // `Foo(...)` builds a Foo: its __init__
                let cls = pylink::py_class(g, md, &c.name, 0);
                match (!cls.is_empty()).then(|| g.lookup(&cls, "__init__")).flatten() {
                    Some(h) => hits = vec![h],
                    None if !cls.is_empty() => return nothing(), // a project class with no __init__ of its own: no function runs
                    None => {}
                }
            }
            // Go: two functions of one name in one package are build variants
            // (`_windows.go` / `_unix.go`): the call reaches whichever is built
            let variants = f.file.ends_with(".go") && {
                let mods: HashSet<&String> = hits.iter().map(|h| &h.module).collect();
                mods.len() == hits.len() && {
                    let pk: HashSet<String> = hits
                        .iter()
                        .map(|h| {
                            let hm = &g.modules[&h.module];
                            let o = if hm.owner.is_empty() { &h.module } else { &hm.owner };
                            g.modules.get(o).unwrap_or(hm).package.clone()
                        })
                        .collect();
                    pk.len() == 1
                }
            };
            let mut seen = HashSet::new();
            hits.retain(|h| seen.insert(h.id.clone())); // (overloads share an id: one function)
            let exact = hits.len() == 1 || variants || self.t.one_type(&hits);
            targets = hits.into_iter().map(|h| (h, exact)).collect();
            if targets.is_empty() && md.external.contains(&c.name) {
                return nothing();
            }
            if targets.is_empty() && self.scoped_language(&f.file, md) && !md.imported.contains_key(&c.name) {
                // Elixir, Kotlin, Java, an ES module: a bare call reaches only
                // what is local, imported or in its package. Nothing in scope
                // means a library or the language's own (Enum, Compose's Row,
                // vitest's expect, ConnTest's post): not a guess at every
                // project function of that name.
                return nothing();
            }
        }
        if targets.is_empty() && !q.is_empty() && crate::model::is_py(&f.file) && upper1(&c.name) {
            let cls = pylink::py_class(g, md, &format!("{q}.{}", c.name), 0);
            if !cls.is_empty() {
                // `sessions.Session()`: a class through its module builds one, so its __init__ runs
                match g.lookup(&cls, "__init__") {
                    Some(h) => targets.push((h, true)),
                    None => return nothing(), // (a class with no __init__ of its own: no project function runs)
                }
            }
        }
        if targets.is_empty() && !q.is_empty() && module_q(q) && q != "Self" {
            // A qualifier that names a module (`Enum.reject`, `Repo.rollback`)
            // and misses: the function lives outside the project (or the alias
            // is wrong). Nothing, not a guess.
            return nothing();
        }
        if targets.is_empty() && !c.chain.is_empty() && f.file.ends_with(".rs") && c.chain.iter().all(|s| re!(r"^[a-z_]\w*$").is_match(s)) {
            // `utils::assert_output(...)` in an integration test: the `mod utils` of the
            // caller's own crate root (tests/builder/main.rs has its utils.rs, tests/derive another)
            let path_ = c.chain.join("/");
            let root_ = self.rs_crate_dir(&f.file);
            let mine: Vec<&Func> = g
                .by_name
                .get(&c.name)
                .map(|v| v.iter().filter_map(|id| g.func(id)).collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .filter(|h| {
                    h.file.starts_with(&root_) && (h.file[..h.file.len().saturating_sub(3)].ends_with(&format!("/{path_}")) || h.file.ends_with(&format!("/{path_}/mod.rs")))
                })
                .collect();
            if mine.iter().map(|h| &h.file).collect::<HashSet<_>>().len() == 1 {
                targets = vec![(mine[0], true)];
            }
        }
        if targets.is_empty()
            && !q.is_empty()
            && ((c.name == "GetType" && f.file.ends_with(".cs")) || (c.name == "getClass" && ends(&f.file, &[".java", ".kt", ".kts"])))
        {
            return nothing(); // `o.GetType()`: System.Object's, non-virtual (no project override exists)
        }
        if targets.is_empty() && !q.is_empty() && RS_STD_METHODS.contains(&c.name.as_str()) && f.file.ends_with(".rs") {
            // `.unwrap()`, `.len()`, `.push()` on a receiver of unknown type:
            // Option / Result / Vec / String, not a project function of that name
            return nothing();
        }
        if targets.is_empty()
            && q.is_empty()
            && (ends(&f.file, &[".c", ".h"])
                || (ends(&f.file, CPP_EXTS)
                    && g.by_name.get(&c.name).is_some_and(|v| v.iter().any(|id| g.func(id).is_some_and(|h| ends(&h.file, &[".c", ".h"]))))))
        {
            // (a C++ file calling the project's C functions: the same linker rule)
            // C: no imports, the linker's rule. A `static` function is its file's;
            // any other is global to the program: the call reaches the file's own
            // definition, else the project's one non-static definition of that name
            let cfuncs: Vec<&Func> = g
                .by_name
                .get(&c.name)
                .map(|v| v.iter().filter_map(|id| g.func(id)).filter(|h| h.kind != "top" && ends(&h.file, &[".c", ".h"])).collect())
                .unwrap_or_default();
            let inc_c = self.c_included(&f.file); // (`#include "../cJSON.c"`: its statics are this file's too)
            let stem = f.file.rsplit_once('.').map(|x| x.0).unwrap_or(&f.file).to_string();
            let mut own: Vec<&Func> = cfuncs.iter().copied().filter(|h| h.file == f.file || h.file == format!("{stem}.h")).collect();
            if own.is_empty() {
                own = cfuncs.iter().copied().filter(|h| inc_c.contains(&h.file)).collect();
            }
            let glob_: Vec<&Func> = cfuncs.iter().copied().filter(|h| !self.c_static(h) || h.file.ends_with(".h")).collect(); // (a header's static inline: its includers')
            if let Some(o) = own.first() {
                targets = vec![(o, true)];
            } else if !glob_.is_empty() && glob_.iter().map(|h| &h.file).collect::<HashSet<_>>().len() == 1 {
                targets = vec![(glob_[0], true)];
            } else if cfuncs.is_empty() {
                // libc, a library's, or a macro: a project macro (`JV_ARRAY(a, b)`) runs the
                // functions its body calls, resolved by the same rule
                let direct: Vec<String> = self.c_macro_targets(&f.file, &c.name, 0).into_iter().map(|h| h.id.clone()).collect();
                return Resolved { targets: vec![], direct, param_call: false, param_recv: false, primary: 0 };
            }
        }
        if targets.is_empty() && q.is_empty() && c.chain.is_empty() && f.file.ends_with(".cs") && upper1(&c.name) {
            // C# has no free functions: a bare call is the enclosing classes' (or their bases'),
            // or a `using static` type's (`Call(...)`, `Default(...)`: System.Linq.Expressions.Expression's)
            let statics = self.cs_static_usings(md);
            let (mut scope, mut o): (HashSet<String>, String) = (HashSet::new(), md.name.clone());
            while let Some(om) = g.modules.get(&o) {
                scope.extend(self.t.ancestors(&o));
                if om.owner.is_empty() {
                    break;
                }
                o = o.rsplit_once('.').map(|x| x.0.to_string()).unwrap_or(o);
            }
            let cands_: Vec<&Func> = g
                .by_name
                .get(&c.name)
                .map(|v| v.iter().filter_map(|id| g.func(id)).filter(|h| h.file.ends_with(".cs") && h.kind != "top").collect())
                .unwrap_or_default();
            let decl_names: HashSet<String> = cands_
                .iter()
                .filter_map(|h| g.modules[&h.module].decls.first().map(|d| d.split('`').next().unwrap().to_string()))
                .collect();
            if !cands_.is_empty()
                && !cands_.iter().any(|h| {
                    scope.contains(&h.module) || g.modules[&h.module].decls.first().is_some_and(|d| statics.contains(d.split('`').next().unwrap()))
                })
                && !decl_names.contains(&c.name)
            {
                return nothing();
            }
        }
        if targets.is_empty() && q.is_empty() && c.chain.is_empty() && ends(&f.file, &[".cpp", ".cc", ".cxx", ".h", ".hpp", ".hh"]) {
            let cands_: Vec<&Func> = g
                .by_name
                .get(&c.name)
                .map(|v| v.iter().filter_map(|id| g.func(id)).filter(|h| h.kind != "top" && shares_family(&h.file, &f.file)).collect())
                .unwrap_or_default();
            if !cands_.is_empty()
                && cands_.iter().all(|h| {
                    h.file != f.file
                        && ends(&h.file, &[".cpp", ".cc", ".cxx"])
                        && ((!h.ns.is_empty() && !(f.ns == h.ns || f.ns.starts_with(&format!("{}::", h.ns)) || (h.ns.starts_with(&format!("{}::", f.ns)) && !f.ns.is_empty())))
                            || (!g.modules[&h.module].owner.is_empty() && h.module != md.name && h.module != md.owner))
                })
            {
                return nothing(); // `fclose(f)`: libc's; a `test::fclose` in another .cc isn't visible here
            }
        }
        if targets.is_empty() && !q.is_empty() && !c.chain.is_empty() && is_js_file {
            let cands_: Vec<&Func> = g
                .by_name
                .get(&c.name)
                .map(|v| v.iter().filter_map(|id| g.func(id)).filter(|h| h.kind != "top" && shares_family(&h.file, &f.file)).collect())
                .unwrap_or_default();
            if !cands_.is_empty() && cands_.iter().all(|h| self.t.js_plain_function(h)) {
                return nothing(); // `server.close()`: no `function close(server)` declaration is a method
            }
        }
        if targets.is_empty() && q.is_empty() && c.chain.is_empty() && is_js {
            let any = g.by_name.get(&c.name).is_some_and(|v| {
                v.iter().filter_map(|id| g.func(id)).any(|h| {
                    h.kind != "top"
                        && shares_family(&h.file, &f.file)
                        && g.modules[&h.module].owner.is_empty()
                        && !matches!(g.modules[&h.module].kind.as_str(), "class" | "interface")
                        && !self.t.js_property_fn(h)
                })
            });
            if !any {
                return nothing(); // `setTimeout(...)`: a JS method needs its receiver; no free function has the name
            }
        }
        if targets.is_empty() {
            targets = self.guess(f, md, c, &mut pending_recv);
        }
        // Virtual dispatch: a call that lands on a parent's function can
        // run any subclass override of it too.
        let mut more: Vec<(&'a Func, bool)> = Vec::new();
        let below: Option<HashSet<String>> = if recv.is_empty() {
            None
        } else {
            let mut s: HashSet<String> = HashSet::new();
            for r in &recv {
                if let Some(rm) = g.modules.get(r) {
                    s.insert(r.clone());
                    s.extend(self.t.parts(rm));
                    s.extend(g.children.get(r).cloned().unwrap_or_default());
                }
            }
            Some(s)
        };
        for (tf, exact) in &targets {
            if CTOR_NAMES.contains(&tf.name.as_str()) || (upper1(&tf.name) && c.name == tf.name) {
                continue; // a constructor builds that type, not a subclass
            }
            let n_args = self.argc(md, c);
            for sub in g.children.get(&tf.module).map(|v| v.as_slice()).unwrap_or(&[]) {
                if below.as_ref().is_some_and(|b| !b.contains(sub)) {
                    continue; // a sibling's override: not this receiver's
                }
                if let Some(o) = g.func(&format!("{sub}.{}", tf.name)) {
                    if n_args.map_or(true, |n| self.t.takes(o, &o.name, n)) {
                        // (an override of another overload: not this call's)
                        more.push((o, *exact));
                    }
                }
            }
        }
        if q.is_empty() && ends(&f.file, PARAM_FLOW_EXTS) && !targets.iter().any(|(_, e)| *e) && self.param_names(f).contains(&c.name) {
            pending = true; // `checkFn(x)` on a parameter: what the callers pass (after this pass)
        }
        if crate::model::is_py(&f.file)
            && c.line > 0
            && c.line <= md.raw.len()
            && md.raw[c.line - 1].trim_start().starts_with('@')
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
        if !targets.is_empty()
            && !targets.iter().any(|(_, e)| *e)
            && !c.chain.is_empty()
            && is_js_file
            && !f.bound.contains_key(&c.chain[0])
            && self.param_names(f).contains(&c.chain[0])
        {
            pending_recv = true; // `headers.set(...)` on a parameter: typed by what the callers pass
        } else {
            pending_recv = false;
        }
        Resolved { targets: out, direct: vec![], param_call: pending, param_recv: pending_recv, primary: targets.len() }
    }

    /// A method on some object, or an import we can't see: every function
    /// with that name is a candidate (a fuzzy edge), narrowed where the
    /// caller's scope settles it.
    fn guess(&mut self, f: &'a Func, md: &'a Module, c: &Call, _pr: &mut bool) -> Vec<(&'a Func, bool)> {
        let g = self.g;
        let q = c.qual.as_str();
        let fext = ext_of(&f.file);
        let in_test = self.is_test(&f.file);
        let by_name: Vec<&'a Func> = g.by_name.get(&c.name).map(|v| v.iter().filter_map(|id| g.func(id)).collect()).unwrap_or_default();
        let mut cands: Vec<&'a Func> = Vec::new();
        for h in &by_name {
            if h.kind != "top" && shares_family(&f.file, &h.file) && (in_test || !self.is_test(&h.file)) {
                cands.push(h); // product code never calls a test
            }
        }
        if f.file.ends_with(".cs") && c.targs > 0 {
            // `multi.Read<int>()` takes a method with one type parameter
            let want = c.targs.to_string();
            cands.retain(|h| h.targs.is_empty() || h.targs.split(',').any(|x| x == want));
        }
        let vague = cands.len() > 8; // (a very common name, before the ones out of reach are dropped)
        let js_file = ends(&f.file, infer::JS_FILE_EXTS);
        if js_file {
            cands.retain(|h| !self.t.js_scoped_patch(h, f));
        }
        if in_test && js_file {
            // a JS test file no module imports keeps its own functions (`on()` of one test's
            // mock socket): another file can't be handed them
            let imported = self.js_imported_files().clone();
            let tests: Vec<bool> = cands.iter().map(|h| self.is_test(&h.file)).collect();
            let mut k = 0;
            cands.retain(|h| {
                let keep = h.file == f.file || !tests[k] || imported.contains(&h.file);
                k += 1;
                keep
            });
        }
        let mut targets: Vec<(&'a Func, bool)> = Vec::new();
        if !q.is_empty() && c.line > 0 && c.line <= md.raw.len() && md.raw[c.line - 1].contains(&format!("{q}::{}", c.name)) {
            // `veridian::api::parseClaim(...)`: the namespace names where it lives
            let named: Vec<&Func> = cands
                .iter()
                .copied()
                .filter(|h| format!("/{}", h.file).contains(&format!("/{q}/")) || h.module.rsplit("::").next() == Some(q) || h.module.ends_with(&format!(".{q}")))
                .collect();
            if named.len() == 1 {
                cands = named.clone();
                targets = vec![(named[0], true)];
            }
        }
        // A guess by name is never "exact", even when only one function has
        // the name: `icon()` on a lambda parameter matched PlacesNav.icon.
        // Except a method call (`store.getState().applyOp()`,
        // `gestures.onTouch()`) whose name exists in exactly one module
        // the file imports or shares a package with: that is the one.
        let shadowed = self.shadowed(f, &c.name);
        let seen_mods: HashSet<String> = if shadowed {
            HashSet::new()
        } else if let Some(h) = self.scope_cache.get(&md.name) {
            h.clone()
        } else {
            let s = self.t.scope(md);
            self.scope_cache.insert(md.name.clone(), s.clone());
            s
        };
        let mut mine: Vec<&Func> = cands.iter().copied().filter(|h| seen_mods.contains(&h.module)).collect();
        if crate::model::is_py(&f.file) {
            // x.f() reaches a module-level Python function only through
            // the module's own name (resolved above), never `session.get()`
            mine.retain(|h| !g.modules[&h.module].owner.is_empty());
        }
        if !q.is_empty() {
            // an untyped receiver can hold anything: a name only one module in scope
            // has is no proof (`res.json()` on a fetch Response is not Context.json,
            // `@app.call` not Logger#call); the receiver's type is what settles it
            mine.clear();
        }
        let own: Vec<&Func> = if !ends(&f.file, &[".cs", ".go"]) { cands.iter().copied().filter(|h| h.file == f.file && !h.recv.is_empty()).collect() } else { vec![] };
        // (Go: a method's receiver is no extension; `err[i].Error()` on an `error` isn't this file's Error)
        let fm_ = if md.owner.is_empty() { Some(md) } else { g.modules.get(&md.owner).or(Some(md)) };
        if targets.is_empty() && !c.chain.is_empty() && f.file.ends_with(".gd") {
            // `var part: RigidBody3D = ...` then `part.explode()`: the object is a script
            // extending RigidBody3D; the project's one such script with an explode() it is
            let eng = self.gd_declared(md, f, &c.chain);
            let sub: Vec<&Func> = if eng.is_empty() { vec![] } else { cands.iter().copied().filter(|h| self.gd_extends(&h.module, &eng)).collect() };
            if sub.iter().map(|h| &h.module).collect::<HashSet<_>>().len() == 1 {
                targets = vec![(sub[0], true)];
            }
        }
        if !targets.is_empty() {
        } else if !q.is_empty()
            && ends(&f.file, &[".kt", ".kts"])
            && cands.len() == 1
            && !cands[0].recv.is_empty()
            && fm_.is_some_and(|fm| cands[0].file == f.file || fm.imported.get(&c.name).map(|x| x.1.as_str()) == Some(c.name.as_str()))
        {
            // Kotlin: the project's one function of that name is an extension this file
            // declares or imports by name (`import ...alias`): nothing else is in scope
            targets = vec![(cands[0], true)];
        } else if own.len() == 1 && !q.is_empty() && cands.len() > 1 {
            // an extension the caller's own file declares (each generated
            // parser's private JSONObject.textOrNull): it shadows the others'
            targets = vec![(own[0], true)];
        } else if mine.len() == 1 && !q.is_empty() {
            targets = vec![(mine[0], true)];
        } else if mine.len() > 1 && mine.len() <= 4 && !q.is_empty() && mine.iter().map(|h| &h.module).collect::<HashSet<_>>().len() == 1 {
            // overloads in the one module in scope (JSONObject.textOrNull and
            // JSONArray.textOrNull side by side): the call is one of them
            targets = mine.into_iter().map(|h| (h, true)).collect();
        } else {
            if vague || cands.len() > 8 {
                cands.clear(); // a very common name (run, get, init...): too vague to be a useful edge
            }
            targets = cands.into_iter().map(|h| (h, false)).collect();
        }
        let _ = (fext, lower1("a"), re!("a"));
        targets
    }

    /// A JS / TS / Python function's parameter names, in order ("" for one
    /// this can't name: a destructuring, `*args`) (core._param_names).
    pub(super) fn param_names(&self, f: &Func) -> Vec<String> {
        let Some(k) = f.code.find('(') else { return vec![] };
        if k > 400 {
            return vec![];
        }
        let e = crate::lang::lambdas::matching(&f.code, k);
        if e < 0 {
            return vec![];
        }
        let pat = re!(r"(?s)^\s*(?:(?:public|private|protected|readonly)\s+)?([A-Za-z_$][\w$]*)\s*[?]?\s*(?:[:=].*)?$");
        crate::lang::lambdas::split_top(&f.code[k + 1..e as usize], ',')
            .iter()
            .map(|p_| match pat.match_start(p_) {
                Some(m) if m.s(1) != "self" && m.s(1) != "cls" => m.s(1).to_string(),
                _ => String::new(),
            })
            .collect()
    }
}
