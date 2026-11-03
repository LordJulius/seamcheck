//! The language-specific readers behind the walk: JS expandos and TS
//! factories, Rust macros and crates, Godot scenes and autoloads, Ruby's
//! yielded blocks, collections, lambdas (infer.Types' remaining methods).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use indexmap::IndexMap;

use super::{
    dirname, ends, esc, ext, is_ext, re, rxd, top_split, upper1, Expando, Scenes, Types, Val, JS_EXTS, JS_FILE_EXTS, MAX,
};
use crate::lang::chains::{expression, generic_name, type_name};
use crate::lang::lambdas;
use crate::model::{Func, Module};

const COLLECTIONS: &[&str] = &[
    "ArrayList", "List", "AbstractList", "LinkedList", "Vector", "Collection", "AbstractCollection", "Iterable", "HashSet",
    "Set", "TreeSet", "LinkedHashSet", "ArrayDeque", "Deque", "Queue", "CopyOnWriteArrayList", "MutableList",
    "MutableCollection", "ObservableCollection", "IList", "ICollection", "IEnumerable", "IReadOnlyList", "HashMap", "Map",
    "TreeMap", "LinkedHashMap", "Dictionary", "MutableMap",
];
const ELEMENT_HOFS_0: &[&str] = &[
    "map", "filter", "all", "any", "for_each", "find", "position", "flat_map", "filter_map", "take_while", "skip_while",
    "inspect", "max_by_key", "min_by_key", "sort_by_key", "partition", "find_map", "forEach", "some", "every", "findIndex",
    "findLast", "flatMap", "filterNot", "mapNotNull", "forEachIndexed", "associateBy", "groupBy", "sortedBy",
    "sortedByDescending", "first", "firstOrNull", "last", "lastOrNull", "count", "sumOf", "onEach", "compactMap",
    "contains", "allSatisfy", "removeAll", "Select", "Where", "Any", "All", "First", "FirstOrDefault", "ForEach",
    "SelectMany", "OrderBy",
];
const ELEMENT_HOFS_1: &[&str] = &["fold", "reduce", "try_fold", "mapIndexed", "filterIndexed"];
const ITER_SEGS: &[&str] = &[
    "iter()", "iter_mut()", "into_iter()", "drain()", "asSequence()", "stream()", "lazy", "values()", "copied()", "cloned()",
    "enumerate()", "rev()", "skip()", "take()", "peekable()", "by_ref()",
];
const RS_MACRO_STD: &[&str] = &[
    "vec", "format", "matches", "println", "write", "writeln", "assert", "assert_eq", "include_str", "concat", "env", "todo",
    "panic",
];

fn win(s: &str, start: usize, n: usize) -> &str {
    crate::pyre::take_chars(&s[start.min(s.len())..], n)
}

impl<'a> Types<'a> {
    // --- JS expandos ---------------------------------------------------------------

    /// JS: (module, property) -> where code sets that property on an object
    /// of that module (`const ctx = Object.create(this.context); ctx.request = ...`):
    /// (module, function, the value's chain).
    pub(super) fn expando(&self) -> Rc<RefCell<Expando>> {
        if let Some(h) = &self.c.borrow().expando {
            return h.clone();
        }
        let idx: Rc<RefCell<Expando>> = Rc::new(RefCell::new(IndexMap::new())); // (while it builds, a lookup sees what is in so far)
        self.c.borrow_mut().expando = Some(idx.clone());
        let names: Vec<&'a Module> = self.g.modules.values().collect();
        for m in names {
            if !ends(&m.file, JS_EXTS) || m.file.ends_with(".d.ts") {
                continue;
            }
            for (fi, f) in m.funcs.iter().enumerate() {
                let joined;
                let code: &str = if !f.code.is_empty() {
                    &f.code
                } else if f.kind == "top" {
                    joined = m.code.join("\n");
                    &joined
                } else {
                    ""
                };
                let inner: Vec<(usize, usize)> =
                    if f.kind == "top" { m.funcs.iter().filter(|g| g.kind != "top").map(|g| (g.start, g.end)).collect() } else { vec![] };
                for x in re!(r"(?<![\w$.])([A-Za-z_$][\w$]*)\.([A-Za-z_$][\w$]*)[ \t]*=(?![=>])").find_iter(code) {
                    let loc = x.s(1);
                    let held_ = f
                        .bound
                        .get(loc)
                        .filter(|s| !s.is_empty())
                        .cloned()
                        .or_else(|| if f.kind == "top" { m.bound.get(loc).cloned() } else { None })
                        .unwrap_or_default();
                    if loc == "this" || !(held_.starts_with("expr:") || held_.starts_with("mod:") || held_.starts_with("type:")) {
                        continue;
                    }
                    if held_.starts_with("type:")
                        && re!(r"\s*(?:async\s+)?(?:function\b|\((?:[^()]|\([^()]*\))*\)\s*(?::[^=\n]+?)?=>)")
                            .match_start(win(code, x.end(), 300))
                            .is_none()
                    {
                        continue; // (a typed local: only the functions stored on it, the rest is its type's)
                    }
                    let line_here = code[..x.start()].matches('\n').count() + 1;
                    if !inner.is_empty() && inner.iter().any(|(a, b)| *a < line_here && line_here <= *b) {
                        continue; // (inside a function: its own locals)
                    }
                    let mut ch = expression(code, x.end(), 40, Default::default());
                    let fm_ = re!(r"\s*(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)\s*\(").match_start(win(code, x.end(), 200));
                    let am_ = if fm_.is_none() {
                        re!(r"\s*(?:async\s+)?(?:\((?:[^()]|\([^()]*\))*\)\s*(?::[^=\n]+?)?=>|[A-Za-z_$][\w$]*\s*=>)")
                            .match_start(win(code, x.end(), 300))
                            .is_some()
                    } else {
                        false
                    };
                    let ln_ = (if f.code.is_empty() { 1 } else { f.start }) + code[..x.start()].matches('\n').count();
                    let arrow = if am_ { m.funcs.iter().find(|g| g.name == x.s(2) && g.start == ln_) } else { None };
                    let mut named: Option<String> = None;
                    if let Some(fm_) = &fm_ {
                        if self.g.funcs.contains_key(&format!("{}.{}", m.name, fm_.s(1))) {
                            named = Some(format!("{}.{}", m.name, fm_.s(1))); // `instance.create = function create(...)`
                        }
                    }
                    if let (None, Some(a)) = (&named, arrow) {
                        named = Some(a.id.clone()); // `ky.create = (defaults) => createInstance(defaults)`
                    }
                    if named.is_none()
                        && ch.len() == 1
                        && re!(r"^[A-Za-z_$][\w$]*$").is_match(&ch[0])
                        && re!(r"\s*[\w$]+\s*[;\n]").match_start(&format!("{}\n", win(code, x.end(), 100))).is_some()
                    {
                        if let Some(g_) = self.bare(m, Some(f), &ch[0]) {
                            if g_.name == ch[0] {
                                named = Some(g_.id.clone()); // `axios.isAxiosError = isAxiosError;`
                            }
                        }
                    }
                    if let Some(n) = named {
                        ch = vec![format!("\0fn:{n}")];
                    }
                    if ch.is_empty() {
                        continue;
                    }
                    let v = self.chain(m, Some(f), &[loc.to_string()], 1, None, None);
                    if let Some(v @ (Val::Static(..) | Val::Inst(..))) = &v {
                        if v.mods().len() == 1 && ends(&self.m(&v.mods()[0]).file, JS_EXTS) {
                            idx.borrow_mut().entry((v.mods()[0].clone(), x.s(2).to_string())).or_default().push((m.name.clone(), fi, ch));
                        }
                    }
                }
            }
        }
        idx
    }

    /// JS: the one function code assigns as this property of these modules'
    /// objects (`instance.create = function create(...)`), or None.
    pub fn expando_fn(&self, mods: &[String], name: &str) -> Option<&'a Func> {
        let ex = self.expando();
        let mut got: Vec<String> = Vec::new();
        for mn in mods {
            for a in self.ancestors(mn) {
                if let Some(sets) = ex.borrow().get(&(a.clone(), name.to_string())) {
                    for (_, _, ch) in sets {
                        if let Some(id) = ch[0].strip_prefix("\0fn:") {
                            if !got.contains(&id.to_string()) {
                                got.push(id.to_string());
                            }
                        }
                    }
                }
            }
        }
        if got.len() == 1 {
            Some(self.fx(&got[0]).unwrap_or_else(|| panic!("KeyError: {:?}", got[0])))
        } else {
            None
        }
    }

    // --- TS factories ---------------------------------------------------------------

    /// TS: `export interface $constructor { init(...) }` beside `export function
    /// $constructor(...)` that builds its values: the function the factory attaches as
    /// that member (`Object.defineProperty(_, "init", { value: init })`, `_.init = init`).
    pub(super) fn ts_factory_member(&self, mn: &str, name: &str) -> Vec<&'a Func> {
        let Some(m) = self.g.modules.get(mn) else { return vec![] };
        if m.kind != "interface" || m.owner.is_empty() || !ends(&m.file, JS_FILE_EXTS) {
            return vec![];
        }
        if let Some(own) = self.ts_initializer_member(m, name) {
            return vec![own];
        }
        let last = mn.rsplit('.').next().unwrap_or(mn);
        let fac = self.fx(&format!("{}.{}", m.owner, last));
        let raw = fac.map(|fac| {
            let om = self.m(&m.owner);
            om.raw[(fac.start - 1).min(om.raw.len())..fac.end.min(om.raw.len())].join("\n")
        });
        let n = esc(name);
        let ok = raw.as_ref().is_some_and(|raw| {
            rxd(&format!(
                r#"defineProperty\(\s*[\w$]+\s*,\s*["']{n}["']\s*,\s*\{{\s*value\s*:\s*{n}\b|[\w$]+\.{n}\s*=\s*{n}\s*;"#
            ))
            .search(raw)
            .is_some()
        });
        let Some(fac) = fac.filter(|_| ok) else { return vec![] };
        self.g
            .by_name
            .get(name)
            .map(|v| v.iter().filter_map(|id| self.fx(id)).filter(|g| g.file == fac.file && fac.start < g.start && g.start <= fac.end).take(1).collect())
            .unwrap_or_default()
    }

    /// `export const ZodType = core.$constructor("ZodType", (inst, def) => {
    /// inst.safeParse = (data) => ...` beside `export interface ZodType`: the
    /// function the initializer stores as that member, or None.
    fn ts_initializer_member(&self, m: &Module, name: &str) -> Option<&'a Func> {
        let cached = self.c.borrow().tsinit.get(&m.owner).cloned();
        let idx = match cached {
            Some(i) => i,
            None => {
                let mut idx: HashMap<(String, String), String> = HashMap::new();
                let fm = self.m(&m.owner);
                let raw = &fm.raw;
                let heads: Vec<usize> = raw
                    .iter()
                    .enumerate()
                    .filter(|(_, ln)| re!(r"(?:export\s+)?(?:const|let|var|function|class|interface|type)\b").match_start(ln).is_some())
                    .map(|(i, _)| i)
                    .collect();
                for (k, i) in heads.iter().enumerate() {
                    let Some(dm) = re!(r"(?:export\s+)?const\s+([A-Za-z_$][\w$]*)\b[^=]*=").match_start(&raw[*i]) else { continue };
                    let end = if k + 1 < heads.len() { heads[k + 1] } else { raw.len() };
                    for g in &fm.funcs {
                        if *i < g.start
                            && g.start <= end
                            && g.start > 0
                            && g.start <= raw.len()
                            && rxd(&format!(
                                r"\s*(?:inst\.{0}\s*=|(?:async\s+)?\*?\s*{0}\s*(?:\(|:\s*(?:async\s+)?function\b))",
                                esc(&g.name)
                            ))
                            .match_start(&raw[g.start - 1])
                            .is_some()
                        {
                            idx.entry((dm.s(1).to_string(), g.name.clone())).or_insert_with(|| g.id.clone());
                        }
                    }
                }
                self.c.borrow_mut().tsinit.insert(m.owner.clone(), idx.clone());
                idx
            }
        };
        idx.get(&(m.name.rsplit('.').next().unwrap_or(&m.name).to_string(), name.to_string())).and_then(|id| self.fx(id))
    }

    /// TS: `core.$ZodBase64` read as a value, where the module declares both an
    /// interface and a `const` of that name (`export interface $ZodBase64 ...`,
    /// `export const $ZodBase64: ... = $constructor(...)`): the const's value.
    pub(super) fn ts_value(&self, mods: &[String], seg: &str, depth: usize) -> Option<Val> {
        for mn in mods {
            let dm = self.defining_module(mn, seg, 0);
            if !dm.ends_with(&format!(".{seg}")) || !matches!(self.m(&dm).kind.as_str(), "interface" | "type") {
                continue;
            }
            let fm = &dm[..dm.len() - seg.len() - 1];
            let top = self.fx(&format!("{fm}.<top>"));
            let fmod = self.m(fm);
            let held = fmod.bound.get(seg).filter(|s| !s.is_empty()).cloned().or_else(|| top.and_then(|t| t.bound.get(seg).cloned()));
            if let Some(h) = held.filter(|h| h.starts_with("expr:")) {
                return self.held(fmod, top, &h, depth + 1, None);
            }
        }
        None
    }

    /// JS: the function a module-level name holds as a value
    /// (`const stringType = ZodString.create; export { stringType as string }`), or None.
    pub fn func_ref(&self, mods: &[String], name: &str, depth: usize) -> Option<&'a Func> {
        for mn in mods {
            let Some((m, n)) = self.var_module(mn, name, 0) else { continue };
            if depth > MAX {
                continue;
            }
            let held = m.bound.get(&n).cloned().unwrap_or_default();
            let ch: Vec<String> = match held.strip_prefix("expr:") {
                Some(h) => h.split('\x1f').map(|x| x.to_string()).collect(),
                None => vec![],
            };
            if ch.is_empty() || ch.last().unwrap().ends_with("()") || ch.last().unwrap().ends_with(']') || !re!(r"^[A-Za-z_$][\w$]*$").is_match(ch.last().unwrap()) {
                continue;
            }
            if ch.len() == 1 {
                let g = self.bare(m, None, &ch[0]);
                return g.filter(|g| g.name == ch[0]);
            }
            let v = self.chain(m, None, &ch[..ch.len() - 1], depth + 1, None, None);
            if let Some(v) = v.filter(|v| v.is_inst_or_static()) {
                let last = ch.last().unwrap();
                let mut hits: Vec<&Func> = Vec::new();
                if matches!(v, Val::Static(..)) {
                    for x in v.mods() {
                        hits.extend(self.in_scope(x, last, 0));
                    }
                }
                if hits.is_empty() {
                    hits = self.method(v.mods(), last, None).0;
                }
                if hits.len() == 1 {
                    return Some(hits[0]);
                }
            }
        }
        None
    }

    /// (module, its name there) for a module-level variable, through re-exports.
    pub(super) fn var_module(&self, where_: &str, name: &str, depth: usize) -> Option<(&'a Module, String)> {
        let m = self.g.modules.get(where_)?;
        if depth > 4 {
            return None;
        }
        if m.bound.contains_key(name)
            && m.owner.is_empty()
            && (crate::model::is_py(&m.file) || ends(&m.file, JS_EXTS) || (m.file.ends_with(".rs") && name.chars().all(|c| !c.is_lowercase()) && name.chars().any(|c| c.is_uppercase())))
        {
            return Some((m, name.to_string()));
        }
        if let Some((w, n)) = m.imported.get(name) {
            return self.var_module(w, n, depth + 1);
        }
        if ends(&m.file, JS_EXTS) {
            // `export * from "./types.js"`
            for w in &m.reexports {
                if let Some(got) = self.var_module(w, name, depth + 1) {
                    return Some(got);
                }
            }
        }
        None
    }

    /// `<mod>.<name>` is a class nested in mod, not a file named with a
    /// dot beside it (`ResilienceContextPool.Shared.cs`).
    pub(super) fn nested(&self, mn: &str) -> bool {
        let m = self.m(mn);
        !m.owner.is_empty() || crate::model::is_py(&m.file)
    }

    /// Python: the class a module (or package `__init__`) makes available
    /// as `name`, through its imports and `from x import *` re-exports.
    pub(super) fn py_classes(&self, mods: &[String], name: &str) -> Vec<String> {
        mods.iter()
            .filter_map(|mn| {
                let m = self.g.modules.get(mn)?;
                if !crate::model::is_py(&m.file) || !m.owner.is_empty() {
                    return None;
                }
                let c = crate::graph::pylink::py_class(self.g, m, name, 0);
                (!c.is_empty()).then_some(c)
            })
            .collect()
    }

    /// The modules whose exports these JS modules hand on whole
    /// (`module.exports = require('./lib/express')`).
    pub(super) fn js_default_reexport(&self, mods: &[String]) -> Vec<String> {
        let mut alt: Vec<String> = Vec::new();
        for mn in mods {
            let (w, n) = self.m(mn).imported.get("default").cloned().unwrap_or_default();
            let dm = if w.is_empty() { String::new() } else { self.defining_module(&w, &n, 0) };
            if !dm.is_empty() && !mods.contains(&dm) && !alt.contains(&dm) {
                alt.push(dm);
            }
        }
        alt
    }

    /// The file module of package `pkg` declaring package-level `name`.
    pub(super) fn go_package_var(&self, pkg: &str, name: &str) -> Option<&'a Module> {
        if self.c.borrow().gopkgvars.is_none() {
            let mut idx: HashMap<(String, String), Vec<String>> = HashMap::new();
            for m in self.g.modules.values() {
                if m.file.ends_with(".go") && m.owner.is_empty() && !m.package.is_empty() {
                    for k in m.bound.keys() {
                        idx.entry((m.package.clone(), k.clone())).or_default().push(m.name.clone());
                    }
                }
            }
            self.c.borrow_mut().gopkgvars = Some(idx);
        }
        let hits = self.c.borrow().gopkgvars.as_ref().unwrap().get(&(pkg.to_string(), name.to_string())).cloned().unwrap_or_default();
        if hits.len() == 1 {
            self.g.modules.get(&hits[0])
        } else {
            None
        }
    }

    /// The module of a Godot autoload `name` as the project.godot over
    /// `rel` registers it, or None.
    pub(super) fn autoload(&self, rel: &str, name: &str) -> Option<String> {
        if self.c.borrow().autoloads.is_none() {
            let mut loads: HashMap<(String, String), String> = HashMap::new();
            let mut stack = vec![self.p.root.clone()];
            while let Some(dir) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                let mut subs: Vec<std::path::PathBuf> = Vec::new();
                let mut has = false;
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    let ft = e.file_type().ok();
                    if ft.is_some_and(|t| t.is_dir()) {
                        if !n.starts_with('.') && !matches!(n.as_str(), "node_modules" | "build" | "target") {
                            subs.push(e.path());
                        }
                    } else if n == "project.godot" {
                        has = true;
                    }
                }
                stack.extend(subs);
                if !has {
                    continue;
                }
                let base = dir.strip_prefix(&self.p.root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                let base = if base.is_empty() { String::new() } else { format!("{base}/") };
                let Ok(bytes) = std::fs::read(dir.join("project.godot")) else { continue };
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let Some(sec) = re!(r"(?ms)^\[autoload\]\s*\n(.*?)(?=^\[|\Z)").search(&text) else { continue };
                for c in re!(r#"(?m)^(\w+)\s*=\s*"\*?res://([^"]+\.gd)""#).find_iter(sec.s(1)) {
                    let rel_ = format!("{base}{}", c.s(2));
                    if let Some(mods_) = self.g.by_file.get(&rel_) {
                        if let Some(first) = mods_.first() {
                            loads.insert((base.clone(), c.s(1).to_string()), first.clone());
                        }
                    }
                }
            }
            self.c.borrow_mut().autoloads = Some(loads);
        }
        let mut d = rel.to_string();
        loop {
            d = d.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default();
            let key = (if d.is_empty() { String::new() } else { format!("{d}/") }, name.to_string());
            let hit = self.c.borrow().autoloads.as_ref().unwrap().get(&key).cloned();
            if hit.is_some() || d.is_empty() {
                return hit;
            }
        }
    }

    // --- Rust ----------------------------------------------------------------------

    /// The crates a Rust project uses but doesn't hold: std / core / alloc
    /// and every Cargo.toml dependency that isn't a crate of the workspace.
    pub fn rs_external(&self) -> HashSet<String> {
        if let Some(h) = &self.c.borrow().rsext {
            return h.clone();
        }
        let mut got: HashSet<String> = ["std", "core", "alloc"].iter().map(|s| s.to_string()).collect();
        let mut own: HashSet<String> = HashSet::new();
        for rel in &self.p.files {
            if rel.rsplit('/').next() != Some("Cargo.toml") {
                continue;
            }
            let Ok(bytes) = std::fs::read(self.p.root.join(rel)) else { continue };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if let Some(pm) = re!(r#"(?ms)^\[package\][^\[]*?^name\s*=\s*"([^"]+)""#).search(&text) {
                own.insert(pm.s(1).replace('-', "_"));
            }
            let mut section = String::new();
            for line in text.lines() {
                if let Some(hm) = re!(r"^\s*\[([^\]]+)\]\s*(?:#.*)?$").match_start(line) {
                    section = hm.s(1).trim().to_string();
                    if let Some(dm) = re!(r"^(?:workspace\.|target\.[^\]]+?\.)?(?:dev-|build-)?dependencies\.([A-Za-z0-9_-]+)$").match_start(&section) {
                        got.insert(dm.s(1).replace('-', "_"));
                    }
                    continue;
                }
                if re!(r"^(?:workspace\.|target\.[^\]]+?\.)?(?:dev-|build-)?dependencies$").match_start(&section).is_some() {
                    if let Some(km) = re!(r"^([A-Za-z0-9_-]+)\s*=").match_start(line) {
                        got.insert(km.s(1).replace('-', "_"));
                    }
                }
            }
        }
        for o in own {
            got.remove(&o);
        }
        self.c.borrow_mut().rsext = Some(got.clone());
        got
    }

    /// What a project `macro_rules! name` builds: the one project type its body
    /// constructs (`$crate::Arg::new(...)`, `Arg::new(...)`); else unknown.
    pub(super) fn rs_macro_value(&self, md: &'a Module, name: &str) -> Option<Val> {
        if let Some(h) = self.c.borrow().rsmacro.get(name) {
            return h.clone();
        }
        self.c.borrow_mut().rsmacro.insert(name.to_string(), None);
        let mut bodies: Vec<(&'a Module, String)> = Vec::new();
        for m in self.g.modules.values() {
            if !m.owner.is_empty() || !m.file.ends_with(".rs") {
                continue;
            }
            let raw = m.raw.join("\n");
            for mm in rxd(&format!(r"\bmacro_rules!\s*{}\s*\{{", esc(name))).find_iter(&raw) {
                let e_ = lambdas::matching(&raw, mm.end() - 1);
                bodies.push((m, raw[mm.end()..if e_ > 0 { e_ as usize } else { raw.len() }].to_string()));
            }
        }
        let _ = md;
        if bodies.is_empty() {
            let v = if RS_MACRO_STD.contains(&name) { Some(ext()) } else { None };
            self.c.borrow_mut().rsmacro.insert(name.to_string(), v.clone());
            return v;
        }
        let mut tys: Vec<String> = Vec::new();
        for (_, body) in &bodies {
            for t in re!(r"(?:\$crate::(?:\w+::)*)?\b([A-Z]\w*)::(?:new|default|builder)\s*\(").find_iter(body) {
                if !tys.contains(&t.s(1).to_string()) {
                    tys.push(t.s(1).to_string());
                }
            }
        }
        if tys.len() == 1 {
            let got = self.instance(bodies[0].0, &tys[0], None);
            if got.as_ref().is_some_and(|g| !g.is_ext()) {
                self.c.borrow_mut().rsmacro.insert(name.to_string(), got);
            }
        }
        self.c.borrow().rsmacro.get(name).cloned().unwrap_or(None)
    }

    // --- Godot ---------------------------------------------------------------------

    /// GDScript `$Name` in `gd`: the node of that name in the scenes whose
    /// root runs `gd` (their .tscn files): a scripted node is its script's
    /// class, an engine node the engine's; unknown when the scenes disagree.
    pub(super) fn scene_node(&self, gd: &str, name: &str) -> Option<Val> {
        if self.c.borrow().scenes.is_none() {
            let s = Rc::new(self.read_scenes());
            self.c.borrow_mut().scenes = Some(s);
        }
        let scenes = self.c.borrow().scenes.clone().unwrap();
        let (roots, nodes) = &*scenes;
        let mut got: Vec<(String, String)> = Vec::new();
        for sc in roots.get(gd).map(|v| v.as_slice()).unwrap_or(&[]) {
            if let Some(v) = nodes.get(sc).and_then(|n| n.get(name)) {
                if !got.contains(v) {
                    got.push(v.clone());
                }
            }
        }
        if got.len() != 1 {
            return None;
        }
        let (kind, what) = got.remove(0);
        if kind == "script" {
            let mods = self.g.by_file.get(&what)?;
            return mods.first().map(|m| Val::Inst(vec![m.clone()], true));
        }
        let first = self.m(&self.g.by_file[gd][0]);
        let (mods, ex, declared) = self.type_modules(first, &what);
        if declared {
            Some(Val::Inst(mods, ex))
        } else {
            Some(ext())
        }
    }

    /// (script -> scenes whose root runs it, scene -> {node path: ("script", rel) | ("type", T)}).
    fn read_scenes(&self) -> Scenes {
        type RawNode = (HashMap<String, String>, Option<String>, Option<String>);
        let mut raw: IndexMap<String, Vec<RawNode>> = IndexMap::new();
        for rel in &self.p.files {
            if !rel.ends_with(".tscn") {
                continue;
            }
            let Ok(bytes) = std::fs::read(self.p.root.join(rel)) else { continue };
            let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            let base = self.godot(rel);
            let mut res: HashMap<String, String> = HashMap::new();
            for m in re!(r#"\[ext_resource[^\]]*?path="(res://[^"]+)"[^\]]*?id="([^"]+)""#).find_iter(&text) {
                let joined = if base.is_empty() { m.s(1)[6..].to_string() } else { format!("{base}/{}", &m.s(1)[6..]) };
                res.insert(m.s(2).to_string(), crate::lang::ruby::link::normpath(&joined));
            }
            let mut nodes: Vec<RawNode> = Vec::new();
            for m in re!(r"(?m)^\[node ([^\]]*)\]\n((?:(?!\[)[^\n]*\n)*)").find_iter(&text) {
                let mut attrs: HashMap<String, String> = HashMap::new();
                for a in re!(r#"(\w+)=(?:"([^"]*)"|ExtResource\("([^"]+)"\))"#).find_iter(m.s(1)) {
                    let v = if !a.s(2).is_empty() { a.s(2) } else { a.s(3) };
                    attrs.insert(a.s(1).to_string(), v.to_string());
                }
                let sm = re!(r#"(?m)^script = ExtResource\("([^"]+)"\)"#).search(m.s(2));
                let script = sm.and_then(|s| res.get(s.s(1)).cloned());
                let inst = attrs.get("instance").and_then(|i| res.get(i).cloned());
                nodes.push((attrs, script, inst));
            }
            raw.insert(rel.clone(), nodes);
        }
        let mut roots: HashMap<String, Vec<String>> = HashMap::new();
        let mut table: HashMap<String, HashMap<String, (String, String)>> = HashMap::new();
        fn value(raw: &IndexMap<String, Vec<RawNode>>, scene: &str, depth: usize) -> Option<(String, String)> {
            let nodes = raw.get(scene)?;
            if nodes.is_empty() || depth > 8 {
                return None;
            }
            let (attrs, script, inst) = &nodes[0];
            if let Some(s) = script {
                return Some(("script".into(), s.clone()));
            }
            if let Some(i) = inst {
                return value(raw, i, depth + 1);
            }
            attrs.get("type").filter(|t| !t.is_empty()).map(|t| ("type".to_string(), t.clone()))
        }
        for (scene, nodes) in &raw {
            if nodes.is_empty() {
                continue;
            }
            table.insert(scene.clone(), HashMap::new());
            if let Some(s) = &nodes[0].1 {
                let e = roots.entry(s.clone()).or_default();
                if !e.contains(scene) {
                    e.push(scene.clone());
                }
            }
            for (attrs, script, inst) in &nodes[1..] {
                let parent = attrs.get("parent").cloned().unwrap_or_default();
                let nm = attrs.get("name").cloned().unwrap_or_default();
                let path = if parent == "." || parent.is_empty() { nm } else { format!("{parent}/{nm}") };
                let v = if let Some(s) = script {
                    Some(("script".to_string(), s.clone()))
                } else if let Some(i) = inst {
                    value(&raw, i, 0)
                } else {
                    attrs.get("type").filter(|t| !t.is_empty()).map(|t| ("type".to_string(), t.clone()))
                };
                if let Some(v) = v {
                    table.get_mut(scene).unwrap().insert(path, v);
                }
            }
        }
        (roots, table)
    }

    // --- Ruby ----------------------------------------------------------------------

    /// Ruby: the value a method yields to its block (`yield conn`, `block.call(x)`, or
    /// the block handed on, `config.redis(&block)`), or None.
    pub(super) fn yielded(&self, gf: &'a Func, depth: usize) -> Option<Val> {
        if depth > MAX {
            return None;
        }
        let gm = self.g.modules.get(&gf.module)?;
        if let Some(h) = self.c.borrow().yielded.get(&gf.id) {
            return h.clone();
        }
        self.c.borrow_mut().yielded.insert(gf.id.clone(), None); // (a recursion through itself)
        let mut got: Vec<Option<Val>> = Vec::new();
        let body = match gf.code.find('\n') {
            Some(i) => &gf.code[i + 1..],
            None => "",
        };
        let first_line = match gf.code.find('\n') {
            Some(i) => &gf.code[..i + 1],
            None => &gf.code[..],
        };
        for m in re!(
            r"(?:\byield\b[ \t(]*|\b[a-z_]\w*\.call\(\s*)(@{0,2}[a-z_]\w*(?:\.[a-z_]\w*[?!]?)*)?|(?:\([^()\n]*\|\|\s*)?(@{0,2}[a-z_]\w*(?:\.[a-z_]\w*[?!]?)*)\)?\.([a-z_]\w*[?!]?)\(&\w*\)"
        )
        .find_iter(body)
        {
            // (`(Thread.current[:capsule] || default_configuration).redis(&block)`: the default's)
            let (text, last): (Option<&str>, Option<String>) = if let Some(l3) = m.get(3) {
                (m.get(2), Some(format!("{l3}{{}}")))
            } else if m.group0().starts_with("yield") || re!(r"(?:block|blk|b|callback|cb)\.call").match_start(m.group0()).is_some() {
                (m.get(1), None)
            } else {
                continue;
            };
            let Some(text) = text.filter(|t| !t.is_empty()) else {
                if !got.contains(&None) {
                    got.push(None);
                }
                continue;
            };
            let mut segs: Vec<String> = Vec::new();
            for (k, w) in text.split('.').enumerate() {
                let local = w.starts_with('@')
                    || w == "self"
                    || (k == 0 && gf.bound.contains_key(w))
                    || (k == 0 && rxd(&format!(r"(?:\(|,|\|)\s*{}\s*[,)|=]", esc(w))).search(first_line).is_some());
                segs.push(if local && k == 0 { w.to_string() } else { format!("{w}()") });
            }
            if let Some(l) = last {
                segs.push(l);
            }
            let r = self.chain(gm, Some(gf), &segs, depth + 1, None, None);
            if !got.contains(&r) {
                got.push(r);
            }
        }
        let out = if got.len() == 1 { got.pop().unwrap() } else { None };
        self.c.borrow_mut().yielded.insert(gf.id.clone(), out.clone());
        out
    }

    /// Ruby, an untyped receiver named after a class (`site`, `@document`,
    /// `collection`): by the language's convention it holds that class, when
    /// the project has exactly one class of that name.
    pub(super) fn rb_named(&self, md: &'a Module, name: &str) -> Option<Val> {
        if !ends(&md.file, &[".rb", ".rake", ".ru"]) {
            return None;
        }
        let cls: String = name
            .trim_start_matches('@')
            .split('_')
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect();
        if cls.is_empty() || !cls.chars().next().unwrap().is_alphabetic() {
            return None;
        }
        if self.c.borrow().rbshort.is_none() {
            // `Jekyll::Site` is `Site` by its last name
            let mut short: HashMap<String, IndexMap<String, HashSet<String>>> = HashMap::new();
            for (d, mns) in &self.by_type {
                for mn in mns {
                    let m = self.m(mn);
                    if m.file.ends_with(".rb") && m.kind != "ext" {
                        short
                            .entry(d.rsplit("::").next().unwrap().to_string())
                            .or_default()
                            .entry(d.clone())
                            .or_default()
                            .insert(mn.clone());
                    }
                }
            }
            self.c.borrow_mut().rbshort = Some(short);
        }
        let mut quals: IndexMap<String, HashSet<String>> =
            self.c.borrow().rbshort.as_ref().unwrap().get(&cls).cloned().unwrap_or_default();
        if quals.len() > 1 {
            // `Sidekiq::Config` and `Sidekiq::Web::Config`: the one the name `Config`
            // means where the receiver is used, by Ruby's lexical constant lookup
            let stem = md.file.rsplit_once('.').map(|x| x.0).unwrap_or(&md.file);
            let here: Vec<&str> = if md.name.starts_with(&format!("{stem}.")) {
                md.name[stem.len() + 1..].split("::").collect()
            } else {
                vec![]
            };
            for k in (0..=here.len()).rev() {
                let mut parts: Vec<&str> = here[..k].to_vec();
                parts.push(&cls);
                let q = parts.join("::");
                if let Some(v) = quals.get(&q).cloned() {
                    quals = IndexMap::from([(q, v)]);
                    break;
                }
            }
        }
        let mods: HashSet<String> = quals.values().flatten().cloned().collect();
        if quals.len() == 1 && mods.len() == 1 {
            Some(Val::Inst(mods.into_iter().collect(), true))
        } else {
            None
        }
    }

    // --- Python --------------------------------------------------------------------

    /// A Python function used as a value (`run_command.make_context()`):
    /// a library decorator made it the library's object (`@click.command`),
    /// a plain function's attributes are Python's; a project decorator may
    /// return anything (unknown).
    pub(super) fn py_function_value(&self, gf: &'a Func) -> Option<Val> {
        let m = self.g.modules.get(&gf.module);
        let raw: &[String] = m.map(|m| m.raw.as_slice()).unwrap_or(&[]);
        let mut decos: Vec<String> = Vec::new();
        let mut full: Vec<String> = Vec::new();
        let mut j = gf.start as isize - 2;
        let ind = if gf.start > 0 && gf.start <= raw.len() { raw[gf.start - 1].len() - raw[gf.start - 1].trim_start().len() } else { 0 };
        while j >= 0 && (j as usize) < raw.len() && !raw[j as usize].trim().is_empty() {
            let ln = &raw[j as usize];
            if ln.trim_start().starts_with('@') {
                decos.push(ln.trim()[1..].split('(').next().unwrap().to_string());
                full.push(ln.trim()[1..].to_string());
            } else if !(ln.len() - ln.trim_start().len() > ind || ln.trim_start().starts_with(')')) {
                break; // (an argument line of a multi-line decorator is indented deeper, or closes it)
            }
            j -= 1;
        }
        if decos.is_empty() {
            return Some(ext());
        }
        let fm = self.file_mod(m?);
        let root = decos.last().unwrap().split('.').next().unwrap();
        if fm.external.contains(root) || matches!(root, "functools" | "staticmethod" | "classmethod" | "property") {
            return Some(ext());
        }
        // a project decorator: what the outermost one, as written, evaluates to (`@click.group()`,
        // `@cli.command("x")`: a Group, a Command) when that is a project class's instance
        let top = full.last().unwrap();
        let ch = if top.is_empty() { vec![] } else { expression(&format!("{top} "), 0, 40, Default::default()) };
        if !ch.is_empty() && ch.len() <= 6 {
            if let Some(h) = self.c.borrow().pydeco.get(&gf.id) {
                return h.clone();
            }
            self.c.borrow_mut().pydeco.insert(gf.id.clone(), None);
            let got = self.chain(fm, None, &ch, 1, None, None);
            if let Some(g @ Val::Inst(..)) = got {
                self.c.borrow_mut().pydeco.insert(gf.id.clone(), Some(g.clone()));
                return Some(g);
            }
        }
        None // what it returns, we don't know
    }

    // --- elements and collections --------------------------------------------------

    /// The element type a container `name` was declared with (`name[]`):
    /// a local / parameter, else a field of the value it was read from.
    pub(super) fn element(&self, md: &'a Module, f: Option<&'a Func>, name: &str, owner: Option<&Val>, depth: usize) -> Option<Val> {
        let key = format!("{name}[]");
        match owner {
            None => {
                let held = f
                    .and_then(|f| f.bound.get(&key))
                    .filter(|s| !s.is_empty())
                    .or_else(|| md.bound.get(&key).filter(|s| !s.is_empty()))
                    .or_else(|| self.file_mod(md).bound.get(&key).filter(|s| !s.is_empty()));
                if let Some(h) = held {
                    return self.held(md, f, h, depth + 1, None);
                }
                let whole = f.and_then(|f| f.bound.get(name)).cloned().unwrap_or_default();
                if whole.starts_with("expr:") && whole.ends_with("()") && depth < MAX {
                    // `tokens := Tokenize(s)` then `tokens[3]`: an element of what the function returns
                    let ch: Vec<String> = whole[5..].split('\x1f').map(|x| x.to_string()).collect();
                    return self.loop_element(md, f, &ch, depth + 1);
                }
                self.own_class(md, f).and_then(|own| self.field(&own, &key, depth))
            }
            Some(o) => {
                if !o.is_ext() && o.is_inst_or_static() {
                    let got = self.field(o.mods(), &key, depth);
                    return if got == Some(Val::Unknown) { None } else { got };
                }
                None
            }
        }
    }

    /// The element of a project class that is a generic collection
    /// (`class Elements extends Nodes<Element>`, `class Nodes<T> extends
    /// ArrayList<T>`: an Element): (module the type is named in, type name).
    pub(super) fn collection_element(&self, mods: &[String]) -> Option<(String, String)> {
        for mn in mods {
            let cached = self.c.borrow().coll.get(mn).cloned();
            let v = match cached {
                Some(v) => v,
                None => {
                    self.c.borrow_mut().coll.insert(mn.clone(), None);
                    let v = self.coll_walk(mn, &HashMap::new(), 0);
                    self.c.borrow_mut().coll.insert(mn.clone(), v.clone());
                    v
                }
            };
            if v.is_some() {
                return v;
            }
        }
        None
    }

    fn coll_walk(&self, mn: &str, subst: &HashMap<String, String>, depth: usize) -> Option<(String, String)> {
        let m = self.g.modules.get(mn)?;
        if depth > 6 || !ends(&m.file, &[".java", ".kt", ".kts", ".cs", ".scala"]) {
            return None;
        }
        let cls = m.decls.first().map(|d| d.split('`').next().unwrap().to_string()).unwrap_or_default();
        if cls.is_empty() {
            return None;
        }
        let raw = m.raw.join("\n");
        let hm = rxd(&format!(
            r"\b(?:class|interface)\s+{}\s*(<(?:[^<>{{]|<[^<>{{]*>)*>)?\s*(?:\([^)]*\)\s*)?(?:extends|:)\s*([\w.]+)\s*<((?:[^<>{{]|<[^<>{{]*>)*)>",
            esc(&cls)
        ))
        .search(&raw)?;
        let own: Vec<String> = match hm.get(1) {
            Some(g1) => top_split(&g1[1..g1.len() - 1]).iter().map(|p| p.split_whitespace().next().unwrap_or("").split(':').next().unwrap().trim().to_string()).collect(),
            None => vec![],
        };
        let base = hm.s(2).rsplit('.').next().unwrap().to_string();
        let args: Vec<String> = top_split(hm.s(3)).iter().map(|a| a.trim().to_string()).collect();
        let args: Vec<String> = args.into_iter().map(|a| subst.get(&a).cloned().unwrap_or(a)).collect();
        if COLLECTIONS.contains(&base.as_str()) {
            let el = if matches!(base.as_str(), "HashMap" | "Map" | "TreeMap" | "LinkedHashMap" | "Dictionary" | "MutableMap") {
                args.last()?
            } else {
                args.first()?
            };
            let el = re!(r"^(?:\?\s*extends\s+|out\s+|in\s+)").sub("", el);
            let el = el.split('<').next().unwrap().trim().to_string();
            return if !el.is_empty() && upper1(&el) && !own.contains(&el) { Some((mn.to_string(), el)) } else { None };
        }
        let (bmods, _, declared) = self.type_modules(m, &base);
        for bm in if declared { bmods } else { vec![] } {
            let bmod = self.m(&bm);
            let bcls = bmod.decls.first().cloned().unwrap_or_default();
            let pm = rxd(&format!(r"\b(?:class|interface)\s+{}\s*<((?:[^<>{{]|<[^<>{{]*>)*)>", esc(&bcls))).search(&bmod.raw.join("\n")).map(|c| c.s(1).to_string());
            let params: Vec<String> = match pm {
                Some(p) => top_split(&p).iter().map(|p| p.split_whitespace().next().unwrap_or("").split(':').next().unwrap().trim().to_string()).collect(),
                None => vec![],
            };
            let sub: HashMap<String, String> = params.iter().cloned().zip(args.iter().cloned()).collect();
            if let Some(got) = self.coll_walk(&bm, &sub, depth + 1) {
                return if args.contains(&got.1) || !params.contains(&got.1) { Some((mn.to_string(), got.1)) } else { Some(got) };
            }
        }
        None
    }

    /// `c.req` read through a getter (`get req(): HonoRequest`, Python's
    /// `@property`, Kotlin's `val x get()`): what the getter returns.
    pub(super) fn getter(&self, mods: &[String], name: &str, depth: usize) -> Option<Val> {
        let (hits, on_type) = self.method(mods, name, None);
        if hits.is_empty() || !on_type {
            return None;
        }
        let gf = hits[0];
        let raw = &self.m(&gf.module).raw;
        let lo = gf.start.saturating_sub(3).min(raw.len());
        let hi = gf.start.min(raw.len());
        let head = raw[lo..hi.max(lo)].join(" ");
        let n = esc(name);
        let get_re = rxd(&format!(r"\bget\s+{n}\s*\("));
        let js_get = ends(&gf.file, JS_EXTS)
            && raw.iter().any(|ln| rxd(&format!(r"^\s*(?:(?:public|private|protected|static|override)\s+)*get\s+{n}\s*\(")).search(ln).is_some());
        if get_re.search(&head).is_some() || head.contains("@property") || js_get || head.contains("@cached_property") || head.contains("@functools.cached_property") {
            return self.returns(gf, Some(mods), depth + 1, None);
        }
        None
    }

    // --- lambdas -------------------------------------------------------------------

    /// A lambda's parameter: the type the called function's parameter at
    /// that place hands its callback (`AddPipeline(k, b => ...)` with
    /// `Action<Builder> configure`: a Builder). None unless every overload
    /// that takes a function there agrees.
    pub(super) fn lambda_param(&self, md: &'a Module, f: Option<&'a Func>, spec: &str, depth: usize) -> Option<Val> {
        let parts: Vec<&str> = spec.split('\x1e').collect();
        if parts.len() != 3 {
            return None;
        }
        let nums: Vec<i64> = parts[0].split('/').map(|x| x.parse().unwrap_or(0)).collect();
        let (argi, pi, nparams, targs) = (nums[0] as usize, nums[1] as usize, nums[2] as usize, nums[3]);
        let mut name = parts[1].to_string();
        let chain: Vec<String> = parts[2].split('\x1f').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
        let key = (md.name.clone(), f.map(|f| f.id.clone()).unwrap_or_default(), spec.to_string());
        if let Some(h) = self.c.borrow().lam.get(&key) {
            return h.clone();
        }
        self.c.borrow_mut().lam.insert(key.clone(), None); // (a lambda whose receiver is its own parameter)
        let done = |v: Option<Val>| -> Option<Val> {
            self.c.borrow_mut().lam.insert(key.clone(), v.clone());
            v
        };
        let js = ends(&md.file, JS_FILE_EXTS);
        if !chain.is_empty()
            && matches!(name.as_str(), "addEventListener" | "removeEventListener" | "on" | "once")
            && js
            && argi == 1
            && is_ext(&self.chain(md, f, &chain, depth + 1, None, None))
        {
            return done(Some(ext())); // `el.addEventListener('click', (e) => ...)`: a DOM event
        }
        if !chain.is_empty() && matches!(name.as_str(), "let" | "also" | "takeIf" | "takeUnless") && pi == 0 && ends(&md.file, &[".kt", ".kts"]) {
            let got = self.chain(md, f, &chain, depth + 1, None, None);
            if let Some(g) = got {
                if g != Val::Unknown && !matches!(g, Val::Wrap(_)) {
                    return done(Some(g)); // `x.let { it.go() }`: it is x
                }
            }
        }
        let hof_pos = if ELEMENT_HOFS_0.contains(&name.as_str()) {
            Some(0)
        } else if ELEMENT_HOFS_1.contains(&name.as_str()) {
            Some(1)
        } else {
            None
        };
        if !chain.is_empty() && hof_pos == Some(pi) {
            // `paths.iter().all(|p| ...)`, `xs.forEach(x => ...)`, `list.map { it }`: an element of the receiver
            let stripped: Vec<String> = chain.iter().filter(|s| !ITER_SEGS.contains(&s.as_str())).cloned().collect();
            let use_ = if stripped.is_empty() { &chain } else { &stripped };
            let got = self.loop_element(md, f, use_, depth + 1);
            if got.is_some() {
                return done(got);
            }
        }
        let mut hits: Vec<&'a Func> = Vec::new();
        let mut shift = 0usize;
        let mut v: Option<Val> = None;
        if !chain.is_empty() {
            v = self.chain(md, f, &chain, depth + 1, None, None);
            if v.is_none() || v == Some(Val::Unknown) {
                return None;
            }
            match v.as_ref().unwrap() {
                Val::Ext(tys) => {
                    if let Some(t0) = tys.first() {
                        hits = self.ext_funcs(t0, &name);
                        if hits.is_empty() {
                            hits = self.ext_funcs(t0.split('`').next().unwrap(), &name);
                        }
                    }
                    shift = 1; // (C#: `this T x` is the first parameter)
                    if hits.is_empty() && (js || md.file.ends_with(".cs")) {
                        return done(Some(ext())); // `http.createServer((req, res) => ...)`
                    }
                }
                Val::Static(mods, _) => {
                    for mn in mods {
                        hits.extend(self.in_scope(mn, &name, 0));
                    }
                    if hits.is_empty() {
                        hits = self.method(mods, &name, None).0;
                    }
                }
                Val::Inst(mods, _) => {
                    hits = self.method(mods, &name, None).0;
                    if hits.is_empty() && js && mods.iter().any(|mn| self.external_base(mn)) {
                        return done(Some(ext())); // `server.post('/', (req, res) => ...)` on `{...} & express.Express`
                    }
                }
                _ => {}
            }
        } else {
            if name == "new" && f.is_some() && md.file.ends_with(".cs") {
                let t = self.target_typed_new(md, f.unwrap());
                if !t.is_empty() {
                    name = t; // `Foo x = new(b => ...)`, `=> new(b => ...)`
                }
            }
            if let Some(g) = self.bare(md, f, &name) {
                hits.push(g);
            }
            if hits.is_empty() && upper1(&name) {
                // `new MapperConfiguration(cfg => ...)`: the type's constructor
                for mn in self.type_modules(md, &name).0 {
                    for ctor in [name.split('`').next().unwrap(), "__init__", "init", "constructor"] {
                        if let Some(c_) = self.fx(&format!("{mn}.{ctor}")) {
                            if !hits.iter().any(|h| h.id == c_.id) {
                                hits.push(c_);
                            }
                            break;
                        }
                    }
                }
            }
            if hits.is_empty() && upper1(&name) && js && !self.type_modules(md, &name).2 && !self.file_mod(md).imported.contains_key(&name) {
                return done(Some(ext())); // `new Promise((resolve) => ...)`: what a runtime class hands its callback
            }
            if hits.is_empty()
                && ends(&md.file, &[".swift", ".kt", ".kts"])
                && self.g.by_name.get(&name).map_or(true, |v| v.is_empty())
                && !(upper1(&name) && self.type_modules(md, &name).2)
            {
                return done(Some(ext())); // `withCheckedContinuation { continuation in }`: no project function of that name
            }
        }
        if hits.len() == 1 && js && depth < MAX && self.forwards_to_library(hits[0], argi, depth) {
            return done(Some(ext())); // `startHTTPServer((req, res) => ...)` whose handler goes to http.createServer(handler)
        }
        let mut cands: Vec<(i64, String, String)> = Vec::new(); // (type-parameter count, callee module, the lambda parameter's type)
        for gf in &hits {
            let mut sh = if !gf.recv.is_empty() && gf.file.ends_with(".cs") { shift } else { 0 };
            if gf.file.ends_with(".cs") && !chain.is_empty() && matches!(v, Some(Val::Inst(..))) && !gf.recv.is_empty() {
                sh = 1; // an extension method called on its receiver
            }
            if gf.ptypes.is_empty() {
                continue;
            }
            for ov in gf.ptypes.split(lambdas::OV) {
                let (tc, ov) = match ov.split_once(lambdas::TARGS) {
                    Some((a, b)) => (a, b),
                    None => ("", ov),
                };
                let ps: Vec<&str> = if ov.is_empty() { vec![] } else { ov.split(lambdas::PARAM).collect() };
                if argi + sh < ps.len() {
                    let lp = lambdas::lambda_params(ps[argi + sh]);
                    if lp.len() == nparams && pi < lp.len() && !lp[pi].is_empty() {
                        // (a two-parameter lambda fits `Action<A, B>`, not `Action<A>`)
                        cands.push((tc.parse().unwrap_or(0), gf.module.clone(), lp[pi].clone()));
                    }
                }
            }
        }
        if targs >= 0 {
            // `Add<T>(b => ...)`: the overloads taking that many type arguments
            cands.retain(|c_| c_.0 == targs);
        } else if cands.iter().map(|c_| &c_.2).collect::<HashSet<_>>().len() > 1 {
            // none given: a type parameter only a lambda's parameter mentions can't be
            // inferred (C# picks `Add<TKey>` over `Add<TKey, TResult>`)
            let low = cands.iter().map(|c_| c_.0).min().unwrap();
            cands.retain(|c_| c_.0 == low);
        }
        let mut tys: Vec<(String, String)> = Vec::new();
        for c_ in &cands {
            let e = (c_.1.clone(), c_.2.clone());
            if !tys.contains(&e) {
                tys.push(e);
            }
        }
        if tys.iter().map(|(_, t)| t).collect::<HashSet<_>>().len() != 1 {
            return None;
        }
        let (gm_name, ty) = tys.remove(0);
        let gm = self.g.modules.get(&gm_name);
        let bare_ty = re!(r"^(?:inout|borrowing|consuming)\s+").sub("", &ty).trim().to_string();
        if let (Some(gm), false) = (gm, chain.is_empty()) {
            let sub = self.generic_arg(md, f, &chain, gm, &bare_ty);
            if !sub.is_empty() {
                // `mutableState.write { $0 }` on a Protected<MutableState>: Value is MutableState
                return done(self.instance(md, &sub, None));
            }
        }
        let gm = gm?;
        if lambdas::generic_param(&bare_ty) || self.class_params(gm).contains(&bare_ty) {
            return None; // (a type parameter the receiver didn't say)
        }
        let ty = if gm.file.ends_with(".cs") { generic_name(&bare_ty) } else { type_name(&bare_ty) };
        if ty.is_empty() {
            return None;
        }
        done(self.instance(gm, &ty, None))
    }

    /// The type a C# `new(...)` builds: the declared type the line assigns to
    /// (`MapperConfiguration c = new(`), or the method's return type (`=> new(`).
    fn target_typed_new(&self, _md: &Module, f: &Func) -> String {
        let Some(m) = self.g.modules.get(&f.module) else { return String::new() };
        for ln in f.start..=f.end {
            let line = m.code.get(ln - 1).map(|s| s.as_str()).unwrap_or("");
            if re!(r"\bnew\s*\(").search(line).is_none() {
                continue;
            }
            if let Some(dm) = re!(r"([A-Z][\w.]*(?:<[^<>=]*>)?)\??\s+\w+\s*=\s*new\s*\(").search(line) {
                return generic_name(dm.s(1));
            }
            if re!(r"(?:=>|\breturn)\s*new\s*\(").search(line).is_some()
                && !f.returns.is_empty()
                && !f.returns.starts_with("expr:")
                && !f.returns.starts_with("byov:")
            {
                return generic_name(&f.returns);
            }
        }
        String::new()
    }
}

pub(crate) fn _unused(_: &str) -> &str {
    dirname("")
}
