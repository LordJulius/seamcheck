//! Ruby's types from the project's own call sites (lang_ruby._infer_types):
//! a constructor or method parameter every caller passes one class to has
//! that class (`def initialize(site); @site = site`), a receiver that starts
//! with a library's bare call is the library's, and a core method on a
//! receiver nothing types (`opts.each`) is Ruby's.
//!
//! Python mutates the graph while its type walk reads it. Here a pass reads
//! through `Types`, which borrows the graph, so each change is collected,
//! applied between steps, and the next step gets a fresh `Types`.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use super::link::{Resolver, EXT};
use super::pats::{is_core_method, test_file};
use super::recv;
use super::util::*;
use crate::graph::infer::{Types, Val};
use crate::graph::Graph;
use crate::model::Func;
use crate::project::Project;
use crate::pyre::{rx as cached, Rx};

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [std::sync::OnceLock<Rx>; 6] = [const { std::sync::OnceLock::new() }; 6];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

fn anch(k: usize, pat: &'static str) -> &'static Anch {
    static CELLS: [std::sync::OnceLock<Anch>; 4] = [const { std::sync::OnceLock::new() }; 4];
    CELLS[k].get_or_init(|| Anch::new(pat))
}

/// lang_ruby._infer_types.
pub fn infer_types(g: &mut Graph, p: &Project, rb: &[String], r: &Resolver) {
    // the type work sees each class's subclasses and includers (the graph's own `children` come once linking is done)
    let children = {
        let t = Types::new(g, p);
        let mut ch: HashMap<String, Vec<String>> = HashMap::new();
        for mn in rb {
            for a in t.ancestors(mn).into_iter().skip(1) {
                ch.entry(a).or_default().push(mn.clone());
            }
        }
        ch
    };
    g.children = children;
    ctor_params(g, p, rb, r);
    for mn in rb {
        // alternatives that never agreed
        g.modules.get_mut(mn).unwrap().bound.retain(|_, v| !v.starts_with("alt:"));
    }
    library_receivers(g, p, rb);
    core_on_unknown(g, p, rb);
    g.children = HashMap::new();
}

// --- a library's value, Ruby's own method ----------------------------------------------

/// lang_ruby._library_receivers: `last_response.body`, `params[:class].constantize`: a receiver that
/// starts with a bare call reaching no project method (Rack::Test's, Rails' `params`) is a library
/// value, so the call on it is the library's too: qualifier `Object`.
fn library_receivers(g: &mut Graph, p: &Project, rb: &[String]) {
    let mut set: Vec<(String, usize, usize)> = Vec::new();
    {
        let t = Types::new(g, p);
        let mut reach: HashMap<(String, String), bool> = HashMap::new();
        for mn in rb {
            let m = &g.modules[mn];
            let fm = if !m.owner.is_empty() && g.modules.contains_key(&m.owner) { m.owner.as_str() } else { m.name.as_str() };
            for (fi, f) in m.funcs.iter().enumerate() {
                for (ci, c) in f.calls.iter().enumerate() {
                    if !c.chain.is_empty() && c.qual != "Object" && library(&t, m, fm, f, &c.chain[0], 0, &mut reach) {
                        set.push((mn.clone(), fi, ci));
                    }
                }
            }
        }
    }
    mark_object(g, set);
}

fn mark_object(g: &mut Graph, set: Vec<(String, usize, usize)>) {
    for (mn, fi, ci) in set {
        g.modules.get_mut(&mn).unwrap().funcs[fi].calls[ci].qual = "Object".to_string();
    }
}

fn library<'a>(
    t: &Types<'a>,
    m: &'a crate::model::Module,
    fm: &str,
    f: &'a Func,
    head: &str,
    depth: usize,
    reach: &mut HashMap<(String, String), bool>,
) -> bool {
    if let Some(b) = f.bound.get(head) {
        if b.starts_with("expr:") && depth < 3 {
            // `error = assert_raises(Foo) do ... end`: a local holding one
            let inner = b[5..].split('\x1f').next().unwrap_or("").to_string();
            return library(t, m, fm, f, &inner, depth + 1, reach);
        }
    }
    let Some(name) = head.strip_suffix("()") else { return false };
    if !name.chars().next().is_some_and(|c| c.is_lowercase() || c == '_') {
        return false;
    }
    let key = (m.name.clone(), name.to_string());
    if !reach.contains_key(&key) {
        let hits = !m.owner.is_empty() && !t.method(std::slice::from_ref(&m.name), name, None).0.is_empty();
        reach.insert(key.clone(), hits || !t.in_scope(fm, name, 0).is_empty());
    }
    !reach[&key]
}

/// lang_ruby._core_on_unknown: `x.each`, `opts.fetch`, `@data.key?` where nothing types x / opts / @data:
/// Ruby's method. Its qualifier becomes `Object` (a library module, so core counts it as reaching
/// nothing; "named" in stats still shows it when a project function has the name). The chain stays.
fn core_on_unknown(g: &mut Graph, p: &Project, rb: &[String]) {
    let mut set: Vec<(String, usize, usize)> = Vec::new();
    {
        let t = Types::new(g, p);
        for mn in rb {
            let m = &g.modules[mn];
            for (fi, f) in m.funcs.iter().enumerate() {
                for (ci, c) in f.calls.iter().enumerate() {
                    if !c.chain.is_empty()
                        && c.qual != "Object"
                        && is_core_method(&c.name)
                        && t.chain(m, Some(f), &c.chain, 0, None, None).is_none()
                    {
                        set.push((mn.clone(), fi, ci));
                    }
                }
            }
        }
    }
    mark_object(g, set);
}

// --- constructor parameters, from their callers ---------------------------------------

/// One call site: (module, function index, call index).
#[derive(Clone)]
struct Site {
    mn: String,
    fi: usize,
    ci: usize,
}

/// initialize -> class built -> call sites
type ByInit = IndexMap<String, IndexMap<String, Vec<Site>>>;
/// a file's code joined, and the offset each line starts at
type Texts = HashMap<String, (String, Vec<usize>)>;

/// A change to the graph, applied between steps.
enum Mut {
    FuncBound { module: String, idx: usize, name: String, val: String },
    ModBound { module: String, name: String, val: String },
}

fn apply(g: &mut Graph, muts: &mut Vec<Mut>) {
    for m in muts.drain(..) {
        match m {
            Mut::FuncBound { module, idx, name, val } => {
                g.modules.get_mut(&module).unwrap().funcs[idx].bound.insert(name, val);
            }
            Mut::ModBound { module, name, val } => {
                g.modules.get_mut(&module).unwrap().bound.insert(name, val);
            }
        }
    }
}

/// Run `f` over the work items in order against one `Types` until an item changes something;
/// those changes are applied, and the rest runs against a fresh `Types` (building one costs a
/// pass over the project, so it is built when the graph has changed, not for every item).
/// Returns whether anything changed.
fn drive<W>(g: &mut Graph, p: &Project, work: &[W], mut f: impl for<'a> FnMut(&'a Graph, &Types<'a>, &W) -> Vec<Mut>) -> bool {
    let mut changed = false;
    let mut next = 0;
    while next < work.len() {
        let mut muts = {
            let t = Types::new(&*g, p);
            let mut got = Vec::new();
            while next < work.len() && got.is_empty() {
                got = f(&*g, &t, &work[next]);
                next += 1;
            }
            got
        };
        changed |= !muts.is_empty();
        apply(g, &mut muts);
    }
    changed
}

/// lang_ruby._ctor_params.
fn ctor_params(g: &mut Graph, p: &Project, rb: &[String], r: &Resolver) {
    let mut by_init: ByInit = IndexMap::new();
    for mn in rb {
        let m = &g.modules[mn];
        for (fi, f) in m.funcs.iter().enumerate() {
            for (ci, c) in f.calls.iter().enumerate() {
                if c.name != "initialize" {
                    continue;
                }
                let (cls, gf) = if c.qual == "super" {
                    (m.name.clone(), if m.parent.is_empty() { None } else { find_up(g, &m.parent, "initialize") })
                } else if c.chain.len() == 1 && r.canon.contains_key(&c.chain[0]) {
                    let cls = r.canon[&c.chain[0]].clone();
                    let gf = find_up(g, &cls, "initialize");
                    (cls, gf)
                } else {
                    (String::new(), None)
                };
                if let Some(gid) = gf {
                    if EXT.iter().any(|e| g.func(&gid).is_some_and(|x| x.file.ends_with(e))) {
                        by_init
                            .entry(gid)
                            .or_default()
                            .entry(cls)
                            .or_default()
                            .push(Site { mn: mn.clone(), fi, ci });
                    }
                }
            }
        }
    }
    // the fields each initialize builds from its parameters, as written
    let mut raw: HashMap<String, IndexMap<String, Vec<String>>> = HashMap::new();
    for gid in by_init.keys() {
        let gf = g.func(gid).unwrap();
        let params: HashSet<String> = positional(gf).into_iter().collect();
        let fields = g.modules[&gf.module]
            .bound
            .iter()
            .filter(|(_, v)| v.starts_with("expr:"))
            .map(|(k, v)| (k.clone(), v[5..].split('\x1f').map(String::from).collect::<Vec<String>>()))
            .filter(|(_, segs)| params.contains(&segs[0]))
            .collect();
        raw.insert(gid.clone(), fields);
    }
    let mut texts: Texts = HashMap::new();
    // (initialize, None): its option fields; (initialize, Some(k)): its parameter k
    let mut work: Vec<(String, Option<usize>)> = Vec::new();
    for gid in by_init.keys() {
        work.push((gid.clone(), None));
        work.extend((0..positional(g.func(gid).unwrap()).len()).map(|k| (gid.clone(), Some(k))));
    }
    for _ in 0..8 {
        let mut changed = drive(g, p, &work, |g, t, (gid, k)| {
            let per_cls = &by_init[gid];
            let gf = g.func(gid).unwrap();
            let own_test = test_file(&gf.file);
            let Some(k) = *k else {
                return option_fields(g, t, r, &mut texts, gid, per_cls, &raw[gid], own_test);
            };
            let name = &positional(gf)[k];
            let tys: IndexMap<&String, Option<String>> =
                per_cls.iter().map(|(cls, calls)| (cls, agreed(g, t, r, &mut texts, calls, k, own_test, ""))).collect();
            let mut every: Vec<&Option<String>> = Vec::new();
            for v in tys.values().filter(|v| v.is_some()) {
                if !every.contains(&v) {
                    every.push(v);
                }
            }
            let mut muts = Vec::new();
            if every.len() == 1 && every[0].as_deref() != Some("") {
                let ty = every[0].clone().unwrap();
                if gf.bound.get(name) != Some(&ty) {
                    muts.push(Mut::FuncBound { module: gf.module.clone(), idx: g.funcs[gid].1, name: name.clone(), val: ty.clone() });
                }
                type_fields(g, &gf.module, &raw[gid], name, &ty, &mut muts);
                return muts;
            }
            // each class built through this initialize types its own fields; the defining class
            // only when every other one has a type of its own (a subclass with none would
            // inherit the defining class's)
            let own_ok = tys.iter().filter(|(cls, _)| ***cls != gf.module).all(|(_, ty)| ty.as_deref().is_some_and(|s| !s.is_empty()));
            for (cls, ty) in &tys {
                if let Some(ty) = ty.as_deref().filter(|s| !s.is_empty()) {
                    if **cls != gf.module || own_ok {
                        type_fields(g, cls, &raw[gid], name, ty, &mut muts);
                    }
                }
            }
            muts
        });
        changed |= method_params(g, p, rb, r, &mut texts);
        changed |= drive(g, p, &[()], |g, t, _| settle_alternatives(g, t, rb, r));
        if !changed {
            break;
        }
    }
}

/// lang_ruby._method_params: the same for any method (`def read(site)` called only as
/// `reader.read(site)` with a Site). Only when every call of that name in the project resolves, the
/// name is never a symbol, and the method doesn't check what it got.
fn method_params(g: &mut Graph, p: &Project, rb: &[String], r: &Resolver, texts: &mut Texts) -> bool {
    let mut sites: IndexMap<String, Vec<Site>> = IndexMap::new();
    let mut unsure: HashSet<String> = HashSet::new();
    {
        let t = Types::new(&*g, p);
        for mn in rb {
            let m = &g.modules[mn];
            let fm = if m.owner.is_empty() { m } else { g.modules.get(&m.owner).unwrap_or(m) };
            for (fi, f) in m.funcs.iter().enumerate() {
                unsure.extend(f.refs.iter().cloned());
                for (ci, c) in f.calls.iter().enumerate() {
                    if c.name == "initialize" || c.qual == "super" || c.qual == "Object" {
                        continue;
                    }
                    let hits: Vec<&Func> = if c.chain.is_empty() {
                        t.in_scope(&fm.name, &c.name, 0)
                    } else {
                        let v = t.chain(m, Some(f), &c.chain, 0, None, None);
                        let (mods, is_static) = match v {
                            Some(Val::Ext(_)) => continue, // a library's method
                            Some(Val::Inst(mods, true)) => (mods, false),
                            Some(Val::Static(mods, true)) => (mods, true),
                            _ => {
                                unsure.insert(c.name.clone());
                                continue;
                            }
                        };
                        let mut hits = t.method(&mods, &c.name, None).0;
                        if is_static {
                            let mut by_id: IndexMap<&str, &Func> = IndexMap::new();
                            for mn in &mods {
                                for h in t.in_scope(mn, &c.name, 0) {
                                    by_id.insert(h.id.as_str(), h);
                                }
                            }
                            let found: Vec<&Func> = by_id.into_values().collect();
                            if !found.is_empty() {
                                hits = found;
                            }
                        }
                        hits
                    };
                    if hits.len() == 1 {
                        sites.entry(hits[0].id.clone()).or_default().push(Site { mn: mn.clone(), fi, ci });
                    } else if !hits.is_empty() {
                        unsure.insert(c.name.clone());
                    }
                }
            }
        }
    }
    let mut work: Vec<(String, usize)> = Vec::new();
    for gid in sites.keys() {
        let gf = g.func(gid).unwrap();
        let skip = unsure.contains(&gf.name)
            || gf.name == "initialize"
            || gf.kind == "test"
            || gf.kind == "top"
            || !EXT.iter().any(|e| gf.file.ends_with(e));
        if !skip {
            work.extend((0..positional(gf).len()).map(|k| (gid.clone(), k)));
        }
    }
    drive(g, p, &work, |g, t, (gid, k)| {
        let gf = g.func(gid).unwrap();
        let name = &positional(gf)[*k];
        let checks = cached(&format!(
            r"\b{n}\s*&?\.\s*(?:respond_to\?|is_a\?|kind_of\?|instance_of\?|class\b)|\bcase\s+{n}\b|===\s*{n}\b",
            n = escape(name)
        ))
        .unwrap();
        if gf.bound.contains_key(name) || checks.search(&gf.code).is_some() {
            return Vec::new();
        }
        match agreed(g, t, r, texts, &sites[gid], *k, test_file(&gf.file), "") {
            Some(ty) if !ty.is_empty() => vec![Mut::FuncBound { module: gf.module.clone(), idx: g.funcs[gid].1, name: name.clone(), val: ty }],
            _ => Vec::new(),
        }
    })
}

/// lang_ruby._option_fields: `@site = relations[:site]` with every caller passing
/// `:site => <a Site>` (or `site: ...`): the field is a Site.
#[allow(clippy::too_many_arguments)]
fn option_fields<'a>(
    g: &'a Graph,
    t: &Types<'a>,
    r: &Resolver,
    texts: &mut Texts,
    gid: &str,
    per_cls: &IndexMap<String, Vec<Site>>,
    raw: &IndexMap<String, Vec<String>>,
    own_test: bool,
) -> Vec<Mut> {
    let gf = g.func(gid).unwrap();
    let params = positional(gf);
    let mut muts = Vec::new();
    for (iv, segs) in raw {
        let km = if segs.len() == 2 { anch(0, r"\[:(\w+)\]$").start(&segs[1]) } else { None };
        let Some(km) = km else { continue };
        let Some(k) = params.iter().position(|x| *x == segs[0]) else { continue };
        let key = km.s(1).to_string();
        let tys: IndexMap<&String, Option<String>> =
            per_cls.iter().map(|(cls, calls)| (cls, agreed(g, t, r, texts, calls, k, own_test, &key))).collect();
        let mut every: Vec<&Option<String>> = Vec::new();
        for v in tys.values().filter(|v| v.is_some()) {
            if !every.contains(&v) {
                every.push(v);
            }
        }
        let single = every.len() == 1 && every[0].as_deref() != Some("");
        let targets: Vec<&String> = if single {
            vec![&gf.module]
        } else {
            tys.iter().filter(|(cls, ty)| ty.as_deref().is_some_and(|s| !s.is_empty()) && ***cls != gf.module).map(|(c, _)| *c).collect()
        };
        for cls in targets {
            let ty = if *cls == gf.module && single { every[0].clone().unwrap() } else { tys[cls].clone().unwrap_or_default() };
            let m = &g.modules[cls];
            if m.bound.get(iv) != Some(&ty) {
                muts.push(Mut::ModBound { module: cls.clone(), name: iv.clone(), val: ty });
            }
        }
    }
    muts
}

/// lang_ruby._type_fields: fields built from `param` (`@site = site`, `@site = doc.site`), typed.
fn type_fields(g: &Graph, module: &str, raw: &IndexMap<String, Vec<String>>, param: &str, ty: &str, muts: &mut Vec<Mut>) {
    let m = &g.modules[module];
    for (iv, segs) in raw {
        if segs[0] == param {
            let mut parts = vec![ty.to_string()];
            parts.extend(segs[1..].iter().cloned());
            let val = parts.join("\x1f");
            if m.bound.get(iv) != Some(&val) {
                muts.push(Mut::ModBound { module: module.to_string(), name: iv.clone(), val });
            }
        }
    }
}

/// lang_ruby._settle_alternatives: a field set to several expressions ("alt:"): the class they all
/// build, or nothing.
fn settle_alternatives<'a>(g: &'a Graph, t: &Types<'a>, rb: &[String], r: &Resolver) -> Vec<Mut> {
    let mut muts = Vec::new();
    for mn in rb {
        let m = &g.modules[mn];
        for (k, v) in &m.bound {
            let Some(alts) = v.strip_prefix("alt:") else { continue };
            let mut fulls: Vec<String> = Vec::new();
            for a in alts.split('\x1e') {
                let got = match a.strip_prefix("expr:") {
                    Some(c) => {
                        let ch: Vec<String> = c.split('\x1f').map(String::from).collect();
                        t.chain(m, None, &ch, 0, None, None)
                    }
                    None => None,
                };
                let full = full_of(g, r, &got);
                if !fulls.contains(&full) {
                    fulls.push(full);
                }
            }
            if fulls.iter().any(|f| f.is_empty()) || fulls.len() != 1 {
                continue; // (kept: a constructor's type may settle it on the next round)
            }
            muts.push(Mut::ModBound { module: mn.clone(), name: k.clone(), val: format!("expr:{}\x1fnew()", fulls[0]) });
        }
    }
    muts
}

/// The one class (full name) a value is an instance of, or "".
fn full_of(g: &Graph, r: &Resolver, v: &Option<Val>) -> String {
    match v {
        None => String::new(),
        // some library's value (`Rack::Builder.new` loses its name): calls on it are the library's
        Some(Val::Ext(_)) => "Object".into(),
        Some(Val::Inst(mods, true)) => {
            let mut fulls: Vec<String> = Vec::new();
            for mn in mods {
                let f = r.full(g, mn);
                if !f.is_empty() && !fulls.contains(&f) {
                    fulls.push(f);
                }
            }
            if fulls.len() == 1 { fulls.remove(0) } else { String::new() }
        }
        _ => String::new(),
    }
}

fn find_up(g: &Graph, mn: &str, name: &str) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut todo: Vec<String> = vec![mn.to_string()];
    while !todo.is_empty() {
        let cur = todo.remove(0);
        if seen.contains(&cur) || !g.modules.contains_key(&cur) {
            continue;
        }
        seen.push(cur.clone());
        let id = format!("{cur}.{name}");
        if g.funcs.contains_key(&id) {
            return Some(id);
        }
        let m = &g.modules[&cur];
        todo.extend(m.bases.iter().cloned());
        if !m.parent.is_empty() {
            todo.push(m.parent.clone());
        }
    }
    None
}

// --- reading a call's arguments -------------------------------------------------------

/// The positional parameters of a def, in order (none after a splat).
fn positional(gf: &Func) -> Vec<String> {
    let Some(m) = anch(1, r"\s*def\s+(?:[\w@]+\.)?\w+[?!=]?\s*\(").start(&gf.code) else { return Vec::new() };
    let mut out = Vec::new();
    for (a, b) in split_args(&gf.code, m.end() - 1) {
        let part = strip(&gf.code[a..b]);
        match anch(2, r"([a-z_]\w*)\s*(?:=|$)").start(part) {
            Some(pm) => out.push(pm.s(1).to_string()),
            None => break, // *rest, key:, &blk: what follows isn't positional
        }
    }
    out
}

/// code[k] is "(": the (start, end) of each top-level argument.
fn split_args(code: &str, k: usize) -> Vec<(usize, usize)> {
    let b = code.as_bytes();
    let (mut out, mut depth, mut a, mut k) = (Vec::new(), 0i32, k + 1, k);
    while k < b.len() {
        match b[k] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    if !strip(&code[a..k]).is_empty() {
                        out.push((a, k));
                    }
                    return out;
                }
            }
            b',' if depth == 1 => {
                out.push((a, k));
                a = k + 1;
            }
            _ => {}
        }
        k += 1;
    }
    out
}

/// The arguments of the call whose name ends at pos: `(a, b)` or a bare `a, b` to the end of the
/// line. None when they can't be read.
fn call_args(text: &str, pos: usize) -> Option<Vec<(usize, usize)>> {
    if text.as_bytes().get(pos) == Some(&b'(') {
        return Some(split_args(text, pos));
    }
    let e = find(text, "\n", pos).unwrap_or(text.len());
    let rest = &text[pos.min(e)..e];
    let lead = lstrip(rest);
    if strip(rest).is_empty()
        || [".", "{", "do", "if ", "unless ", ")", "]", "}", "&&", "||"].iter().any(|s| lead.starts_with(s))
    {
        return Some(Vec::new());
    }
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let seg = match rx(0, r"\s(?:do|if|unless)\b|\{").search(rest) {
        Some(c) => &rest[..c.start()],
        None => rest,
    };
    if rstrip(seg).ends_with(',') {
        return None; // arguments go on over lines
    }
    Some(split_args(&format!("({seg})"), 0).into_iter().map(|(a, b)| (pos + a - 1, pos + b - 1)).collect())
}

/// The one type every call site passes as argument k, as a binding. Product code must all be known;
/// a test's call site (often a double, a block parameter) only counts when it is known, and then
/// must agree (unless the class is a test's own: then its tests are its callers).
/// None: no say either way; Some(""): no one type.
#[allow(clippy::too_many_arguments)]
fn agreed<'a>(g: &'a Graph, t: &Types<'a>, r: &Resolver, texts: &mut Texts, calls: &[Site], k: usize, own_test: bool, key: &str) -> Option<String> {
    let mut got: Vec<String> = Vec::new();
    let mut product = 0;
    for s in calls {
        let ty = arg_type(g, t, r, texts, s, k, key);
        let authoritative = own_test || !test_file(&g.modules[&s.mn].file);
        if ty.is_empty() && authoritative {
            return Some(String::new());
        }
        if !ty.is_empty() {
            if !got.contains(&ty) {
                got.push(ty);
            }
            product += authoritative as usize;
        }
        if got.len() > 1 {
            return Some(String::new());
        }
    }
    if product == 0 {
        return None; // only tests (which agree, or can't be typed): no say either way
    }
    Some(if got.len() == 1 { format!("expr:{}\x1fnew()", got[0]) } else { String::new() })
}

fn file_text<'t>(g: &Graph, texts: &'t mut Texts, m: &crate::model::Module) -> &'t (String, Vec<usize>) {
    let fm = if m.owner.is_empty() { m } else { g.modules.get(&m.owner).unwrap_or(m) };
    texts.entry(fm.file.clone()).or_insert_with(|| {
        let mut starts = vec![0usize];
        for ln in &fm.code[..fm.code.len().saturating_sub(1)] {
            starts.push(starts.last().unwrap() + ln.len() + 1);
        }
        (fm.code.join("\n"), starts)
    })
}

/// The class (full name) argument k of a call is, or "".
fn arg_type<'a>(g: &'a Graph, t: &Types<'a>, r: &Resolver, texts: &mut Texts, s: &Site, k: usize, key: &str) -> String {
    let m = &g.modules[&s.mn];
    let f = &m.funcs[s.fi];
    let c = &f.calls[s.ci];
    let fm = if m.owner.is_empty() { m } else { g.modules.get(&m.owner).unwrap_or(m) };
    let (code, starts) = file_text(g, texts, m);
    if c.line < 1 || c.line - 1 >= starts.len() || c.col < 0 {
        return String::new();
    }
    // (the column counts characters of the written line; the text keeps its bytes in place)
    let line = fm.raw.get(c.line - 1).map(|s| s.as_str()).unwrap_or("");
    let off = line.char_indices().nth(c.col as usize).map(|x| x.0).unwrap_or(line.len());
    let pos = starts[c.line - 1] + off;
    let word = if c.qual == "super" {
        "super"
    } else if c.name == "initialize" {
        "new"
    } else {
        c.name.as_str()
    };
    if !starts_at(code, pos, word) {
        return String::new();
    }
    let args = match call_args(code, pos + word.len()) {
        Some(a) if a.len() > k => a,
        _ => return String::new(),
    };
    let (a, b) = if key.is_empty() {
        args[k]
    } else {
        // the value of `:key => v` / `key: v` among the trailing options
        let pat = cached(&format!(r"\A\s*(?::{key}\s*=>|{key}:)\s*", key = escape(key))).unwrap();
        let hit = args[k..].iter().find_map(|&(x, y)| pat.search(&code[x..y]).map(|km| (x + km.end(), y)));
        match hit {
            Some(h) => h,
            None => return String::new(),
        }
    };
    let arg = strip(&code[a..b]);
    let first = arg.chars().next();
    if arg.is_empty()
        || matches!(first, Some('*' | '&'))
        || rx(1, r"\w+:\s").match_start(arg).is_some()
        || arg.contains("=>")
        || arg.starts_with('{')
    {
        return String::new(); // a splat, a block, keywords: not argument k
    }
    let params = super::fields::params_of(f);
    let start = a + (code[a..b].len() - lstrip(&code[a..b]).len());
    let is_local = |n: &str, _p: usize| f.bound.contains_key(n) || params.contains(n);
    let ch = recv::expression(&code[..b], start, &is_local);
    let nest = r.nesting(g, m);
    let ch: Vec<String> = ch.into_iter().map(|s| if first_upper(&s) { r.const_(&nest, &s) } else { s }).collect();
    let v = if ch.is_empty() { None } else { t.chain(m, Some(f), &ch, 0, None, None) };
    full_of(g, r, &v)
}
