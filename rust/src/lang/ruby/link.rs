//! Ruby linking (lang_ruby.link, run by the graph once every file is
//! parsed): `require`s to files, constants to their class modules, `< Base`
//! and `include M` to supertypes, bare calls to `self`, and every constant
//! in a chain, binding or return type to the full name of the class it means.
//!
//! A class reopened in several files is one class: one definition (the one
//! with `< Base`, else the file named after it) is the class, the others
//! `ext` modules linked both ways, so a method from any of them is found on it.
//!
//! Not here yet: `_infer_types` (constructor and method parameters typed from
//! their callers, library receivers, Ruby's own methods on untyped receivers),
//! which reads the type inference of `graph/infer.rs`.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use indexmap::IndexMap;

use super::pats::STDLIB_YIELDS;
use super::util::*;
use crate::graph::Graph;
use crate::model::Module;
use crate::pyre::{rx as cached, Rx};

pub const EXT: [&str; 3] = [".rb", ".rake", ".ru"];

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [OnceLock<Rx>; 8] = [const { OnceLock::new() }; 8];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

fn anch(k: usize, pat: &'static str) -> &'static Anch {
    static CELLS: [OnceLock<Anch>; 4] = [const { OnceLock::new() }; 4];
    CELLS[k].get_or_init(|| Anch::new(pat))
}

/// lang_ruby.link: everything that needs no receiver types.
pub fn link(g: &mut Graph) {
    let rb: Vec<String> =
        g.modules.values().filter(|m| EXT.iter().any(|e| m.file.ends_with(e))).map(|m| m.name.clone()).collect();
    if rb.is_empty() {
        return;
    }
    link_requires(g, &rb);
    let r = link_classes(g, &rb);
    bare_calls_on_self(g, &rb);
    block_forwarders(g, &rb);
    rewrite_constants(g, &rb, &r);
}

// --- requires ---------------------------------------------------------------------

/// Python `os.path.normpath` for a relative or absolute posix path.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let rooted = path.starts_with('/');
    let lead = if path.starts_with("//") && !path.starts_with("///") { 2 } else if rooted { 1 } else { 0 };
    let mut out: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (lead == 0 && out.is_empty()) || out.last() == Some(&"..") {
            out.push(comp);
        } else if !out.is_empty() {
            out.pop();
        }
    }
    let body = out.join("/");
    let s = format!("{}{}", "/".repeat(lead), body);
    if s.is_empty() {
        ".".into()
    } else {
        s
    }
}

fn join(d: &str, spec: &str) -> String {
    if spec.starts_with('/') || d.is_empty() {
        spec.to_string()
    } else if d.ends_with('/') {
        format!("{d}{spec}")
    } else {
        format!("{d}/{spec}")
    }
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// `require "jekyll/site"`: the file under a load path (`lib/`, `test/`)
/// nearest the requiring file.
fn pick_require(hits: &[String], spec: &str, d: &str) -> String {
    if hits.len() <= 1 {
        return hits.first().cloned().unwrap_or_default();
    }
    let root = |h: &str| -> String { h[..h.len().saturating_sub(spec.len())].trim_end_matches('/').to_string() };
    let dd = format!("{d}/");
    let mut ranked: Vec<&String> = hits.iter().collect();
    ranked.sort_by_key(|h| {
        let r = root(h);
        let lib = !(r.ends_with("lib") || r.ends_with("test") || r.ends_with("spec")) && !r.is_empty();
        (lib, -(common_prefix_len(&format!("{r}/"), &dd) as i64), h.chars().count())
    });
    ranked[0].clone()
}

fn link_requires(g: &mut Graph, rb: &[String]) {
    let files: HashSet<&str> = rb.iter().filter(|n| g.modules[*n].owner.is_empty()).map(|n| n.as_str()).collect();
    let mut suffix: HashMap<String, Vec<String>> = HashMap::new();
    for mn in rb.iter().filter(|n| files.contains(n.as_str())) {
        let parts: Vec<&str> = mn.split('/').collect();
        for k in 0..parts.len() {
            suffix.entry(parts[k..].join("/")).or_default().push(mn.clone());
        }
    }
    let mut set: Vec<(String, Vec<String>)> = Vec::new();
    for mn in rb {
        let m = &g.modules[mn];
        if !m.owner.is_empty() {
            continue;
        }
        let mut got: Vec<String> = Vec::new();
        for u in &m.uses {
            let Some(rest) = u.strip_prefix("rb:") else {
                got.push(u.clone());
                continue;
            };
            let mut it = rest.split('\0');
            let (d, spec0, rel) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
            let spec = spec0.strip_suffix(".rb").unwrap_or(spec0);
            let hit = if !rel.is_empty() {
                let h = normpath(&join(d, spec));
                if files.contains(h.as_str()) {
                    h
                } else {
                    String::new()
                }
            } else {
                pick_require(suffix.get(spec).map(|v| v.as_slice()).unwrap_or(&[]), spec, d)
            };
            if !hit.is_empty() && hit != m.name && !got.contains(&hit) {
                got.push(hit);
            }
        }
        set.push((mn.clone(), got));
    }
    // A require loads a file's top-level methods (and, in turn, what it
    // requires): a bare call follows them. It imports no names, so it doesn't
    // widen what the file "sees" for a method-name match. (The file's classes
    // share its lists.)
    let mut classes: HashMap<String, Vec<String>> = HashMap::new();
    for mn in rb {
        let o = &g.modules[mn].owner;
        if !o.is_empty() {
            classes.entry(o.clone()).or_default().push(mn.clone());
        }
    }
    for (mn, got) in set {
        for n in std::iter::once(&mn).chain(classes.get(&mn).into_iter().flatten()) {
            let m = g.modules.get_mut(n).unwrap();
            m.uses.clear();
            m.reexports = got.clone();
        }
    }
}

// --- classes ----------------------------------------------------------------------

/// The class among a constant's reopenings: the one stating a superclass,
/// then the file named after it (`site.rb` for Site), then the most methods.
fn canonical(g: &Graph, full: &str, names: &[String]) -> String {
    static CAMEL: OnceLock<Rx> = OnceLock::new();
    let camel = CAMEL.get_or_init(|| Rx::lit(r"(?<=[a-z0-9])([A-Z])"));
    let last_const = full.rsplit("::").next().unwrap_or(full);
    let last = camel.sub(r"_\1", last_const).to_lowercase();
    let key = |mn: &String| {
        let m = &g.modules[mn];
        let base = m.file.rsplit('/').next().unwrap_or(&m.file);
        let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
        let named = stem == last;
        (!m.bases.iter().any(|b| b.starts_with('S')), !named, -(m.funcs.len() as i64), m.file.clone())
    };
    names.iter().min_by_key(|n| key(n)).unwrap().clone()
}

/// Ruby constant lookup: the lexical scopes (innermost first), then the
/// innermost class's ancestors, then the top level.
pub struct Resolver {
    pub idx: IndexMap<String, Vec<String>>,
    pub canon: HashMap<String, String>,
    /// namespaces a top-level `include` mixes into Object
    pub globals: Vec<String>,
    /// class -> a library superclass it names (`< Hash`)
    pub ext_super: HashMap<String, String>,
    /// a reopening's module -> the constant it reopens
    pub full_of: HashMap<String, String>,
    pub full_bases: HashMap<String, Vec<String>>,
    /// `A::Stream` -> (A's nesting, "Sinatra::Helpers::Stream")
    pub aliases: HashMap<String, (Vec<String>, String)>,
}

fn nest_of(path: &str) -> Vec<String> {
    if path.is_empty() {
        return Vec::new();
    }
    let parts: Vec<&str> = path.split("::").collect();
    (1..=parts.len()).rev().map(|k| parts[..k].join("::")).collect()
}

impl Resolver {
    /// The constant a class module defines (or reopens), "" for a file.
    pub fn full(&self, g: &Graph, mn: &str) -> String {
        let m = &g.modules[mn];
        if !m.owner.is_empty() && !m.decls.is_empty() {
            m.decls[0].clone()
        } else {
            self.full_of.get(mn).cloned().unwrap_or_default()
        }
    }

    pub fn nesting(&self, g: &Graph, m: &Module) -> Vec<String> {
        let full = self.full(g, &m.name);
        nest_of(&full)
    }

    fn ancestors_full(&self, full: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut todo: Vec<String> = vec![full.to_string()];
        while !todo.is_empty() && out.len() < 20 {
            let cur = todo.remove(0);
            if out.contains(&cur) {
                continue;
            }
            if let Some(b) = self.full_bases.get(&cur) {
                todo.extend(b.iter().cloned());
            }
            out.push(cur);
        }
        out.into_iter().skip(1).collect()
    }

    pub fn resolve(&self, nest: &[String], written: &str, depth: usize) -> String {
        if let Some(w) = written.strip_prefix("::") {
            return if self.idx.contains_key(w) { w.to_string() } else { String::new() };
        }
        // `Minitest::Test`: the project reopens it without declaring Minitest
        for pre in nest.iter().map(|s| s.as_str()).chain(std::iter::once("")) {
            let cand = if pre.is_empty() { written.to_string() } else { format!("{pre}::{written}") };
            if depth < 4 {
                if let Some((a_nest, target)) = self.aliases.get(&cand) {
                    return self.resolve(a_nest, target, depth + 1);
                }
            }
            if self.idx.contains_key(&cand) {
                return cand;
            }
        }
        let (head, rest) = match written.split_once("::") {
            Some((h, r)) => (h, r),
            None => (written, ""),
        };
        let mut base = String::new();
        for pre in nest.iter().map(|s| s.as_str()).chain(std::iter::once("")) {
            let cand = if pre.is_empty() { head.to_string() } else { format!("{pre}::{head}") };
            if self.idx.contains_key(&cand) {
                base = cand;
                break;
            }
        }
        if base.is_empty() {
            let anc = if nest.is_empty() { Vec::new() } else { self.ancestors_full(&nest[0]) };
            for a in anc.iter().chain(self.globals.iter()) {
                if self.idx.contains_key(&format!("{a}::{head}")) {
                    base = format!("{a}::{head}");
                    break;
                }
            }
        }
        if base.is_empty() {
            return String::new();
        }
        let full = if rest.is_empty() { base } else { format!("{base}::{rest}") };
        if self.idx.contains_key(&full) {
            full
        } else {
            String::new()
        }
    }

    /// `Configuration[...]` on a project subclass of Hash / Array / Set that
    /// defines no `self.[]`: Ruby's `[]` builds one, like `new`.
    pub fn index_new(&self, g: &Graph, segs: Vec<String>) -> Vec<String> {
        let mut out = segs;
        for k in 1..out.len() {
            if (out[k] == "[]" || out[k] == "[]()") && self.canon.contains_key(&out[k - 1]) && self.collection_class(g, &out[k - 1]) {
                out[k] = "new()".into();
            }
        }
        out
    }

    fn collection_class(&self, g: &Graph, full: &str) -> bool {
        let mut seen: Vec<String> = Vec::new();
        let mut todo: Vec<String> = vec![self.canon[full].clone()];
        while !todo.is_empty() {
            let mn = todo.remove(0);
            if seen.contains(&mn) || !g.modules.contains_key(&mn) {
                continue;
            }
            seen.push(mn.clone());
            if g.funcs.contains_key(&format!("{mn}.[]")) {
                return false;
            }
            let m = &g.modules[&mn];
            if let Some(d) = m.decls.first() {
                if matches!(self.ext_super.get(d).map(|s| s.as_str()), Some("Hash" | "Array" | "Set")) {
                    return true;
                }
            }
            todo.extend(m.bases.iter().cloned());
            if !m.parent.is_empty() {
                todo.push(m.parent.clone());
            }
        }
        false
    }

    /// A written constant as the full name of the class it means; as written
    /// when the project declares no such class (a gem's, Ruby's).
    pub fn const_(&self, nest: &[String], written: &str) -> String {
        let r = self.resolve(nest, written, 0);
        if r.is_empty() {
            written.trim_start_matches(':').to_string()
        } else {
            r
        }
    }

    pub fn rewrite_held(&self, g: &Graph, nest: &[String], v: &str) -> String {
        if let Some(alts) = v.strip_prefix("alt:") {
            let parts: Vec<String> = alts.split('\x1e').map(|a| self.rewrite_held(g, nest, a)).collect();
            return format!("alt:{}", parts.join("\x1e"));
        }
        let Some(chain) = v.strip_prefix("expr:") else { return v.to_string() };
        let segs: Vec<String> =
            chain.split('\x1f').map(|s| if first_upper(s) { self.const_(nest, s) } else { s.to_string() }).collect();
        format!("expr:{}", self.index_new(g, segs).join("\x1f"))
    }

    /// `Foo.new`: Foo's (or an ancestor's) own `new` if it defines one, else
    /// the `initialize` it runs.
    pub fn constructor(&self, g: &Graph, full: &str) -> String {
        let Some(c) = self.canon.get(full) else { return "new".into() };
        let mut seen: Vec<String> = Vec::new();
        let mut todo: Vec<String> = vec![c.clone()];
        let mut found_init = false;
        while !todo.is_empty() {
            let mn = todo.remove(0);
            if seen.contains(&mn) || !g.modules.contains_key(&mn) {
                continue;
            }
            seen.push(mn.clone());
            if g.funcs.contains_key(&format!("{mn}.new")) {
                return "new".into();
            }
            found_init = found_init || g.funcs.contains_key(&format!("{mn}.initialize"));
            let m = &g.modules[&mn];
            todo.extend(m.bases.iter().cloned());
            if !m.parent.is_empty() {
                todo.push(m.parent.clone());
            }
        }
        if found_init { "initialize".into() } else { "new".into() }
    }
}

/// What a class module's `bases` specs come to: its superclass and mixins as
/// modules (Resolver.bases). Returns (bases, parent, full_bases additions).
fn resolve_bases(g: &Graph, r: &mut Resolver, mn: &str) -> (Vec<String>, String, Vec<String>) {
    let m = &g.modules[mn];
    let mut sup = String::new();
    let mut mixins: Vec<String> = Vec::new();
    let mut fulls: Vec<String> = Vec::new();
    for spec in &m.bases {
        let first = spec.chars().next();
        if !matches!(first, Some('S' | 'M')) || !spec.contains('\0') {
            continue;
        }
        let (wh, written) = spec[1..].split_once('\0').unwrap();
        let nest = nest_of(wh);
        let full = r.resolve(&nest, written, 0);
        if full.is_empty() && first == Some('S') {
            if let Some(d) = m.decls.first() {
                r.ext_super.insert(d.clone(), written.rsplit("::").next().unwrap_or(written).to_string());
            }
        }
        if !full.is_empty() {
            fulls.push(full.clone());
            let tgt = r.canon[&full].clone();
            if tgt == m.name {
                continue;
            }
            if first == Some('S') {
                sup = tgt;
            } else {
                mixins.push(tgt);
            }
        }
    }
    mixins.reverse(); // the last included module is looked up first
    let mut bases = mixins;
    if !sup.is_empty() {
        bases.push(sup.clone());
    }
    (bases, sup, fulls)
}

fn expandtabs2_len(s: &str) -> usize {
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            col += 2 - col % 2;
        } else {
            col += 1;
        }
    }
    col
}

/// `Stream = Sinatra::Helpers::Stream` in a class body: in that scope, Stream is that class.
fn const_aliases(g: &Graph, rb: &[String], r: &mut Resolver) {
    let scope_open = anch(0, r"^([ \t]*)(?:class|module)[ \t]+((?:::)?[A-Z][\w:]*)");
    let const_alias = anch(1, r"^([ \t]*)([A-Z]\w*)[ \t]*=[ \t]*((?:::)?[A-Z]\w*(?:::[A-Z]\w*)*)[ \t]*(?:#.*)?$");
    for mn in rb {
        let m = &g.modules[mn];
        if !m.owner.is_empty() {
            continue;
        }
        let mut stack: Vec<(usize, String)> = Vec::new();
        for ln in m.code.iter() {
            if let Some(om) = scope_open.start(ln) {
                let ind = expandtabs2_len(om.s(1));
                while stack.last().map(|x| x.0 >= ind).unwrap_or(false) {
                    stack.pop();
                }
                stack.push((ind, om.s(2).trim_start_matches(':').to_string()));
                continue;
            }
            let Some(am) = const_alias.start(ln) else { continue };
            if am.s(3).trim_start_matches(':') == am.s(2) {
                continue;
            }
            let ind = expandtabs2_len(am.s(1));
            while stack.last().map(|x| x.0 >= ind).unwrap_or(false) {
                stack.pop();
            }
            let path = stack.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join("::");
            let key = if path.is_empty() { am.s(2).to_string() } else { format!("{path}::{}", am.s(2)) };
            r.aliases.insert(key, (nest_of(&path), am.s(3).to_string()));
        }
    }
}

/// Each constant's class (one of its reopenings), its supertypes and mixins
/// as modules; the reopenings linked to it both ways.
fn link_classes(g: &mut Graph, rb: &[String]) -> Resolver {
    let mut idx: IndexMap<String, Vec<String>> = IndexMap::new();
    for mn in rb {
        let m = &g.modules[mn];
        if !m.owner.is_empty() && !m.decls.is_empty() {
            idx.entry(m.decls[0].clone()).or_default().push(m.name.clone());
        }
    }
    let canon: HashMap<String, String> = idx.iter().map(|(full, names)| (full.clone(), canonical(g, full, names))).collect();
    let mut r = Resolver {
        idx: idx.clone(),
        canon,
        globals: Vec::new(),
        ext_super: HashMap::new(),
        full_of: HashMap::new(),
        full_bases: HashMap::new(),
        aliases: HashMap::new(),
    };
    const_aliases(g, rb, &mut r);
    for mn in rb {
        if !g.modules[mn].owner.is_empty() {
            continue;
        }
        let gl: Vec<String> = g.modules[mn]
            .bases
            .iter()
            .filter_map(|b| b.strip_prefix("G\0"))
            .map(|w| r.resolve(&[], w, 0))
            .filter(|w| !w.is_empty())
            .collect();
        r.globals.extend(gl);
        g.modules.get_mut(mn).unwrap().bases.clear();
    }
    for (full, names) in &idx {
        let c = &r.canon[full];
        for mn in names {
            g.modules.get_mut(mn).unwrap().kind = if mn == c { "class".into() } else { "ext".into() };
        }
    }
    // a library class the project only adds to (`class Rack::Builder`, `class Minitest::Test`:
    // every declaration in that reopening form, none with a superclass or an `initialize`): still
    // a supertype test classes inherit the added methods from, but a value of that type is the
    // library's
    let lib_reopened: Vec<String> = idx
        .iter()
        .filter(|(_, names)| {
            names.iter().all(|mn| {
                let m = &g.modules[mn];
                m.bases.iter().any(|b| b == "C\0")
                    && !m.bases.iter().any(|b| b.starts_with('S'))
                    && !g.funcs.contains_key(&format!("{mn}.initialize"))
            })
        })
        .map(|(f, _)| f.clone())
        .collect();
    for mn in rb {
        if g.modules[mn].owner.is_empty() {
            continue;
        }
        let (bases, parent, fulls) = resolve_bases(g, &mut r, mn);
        let m = g.modules.get_mut(mn).unwrap();
        m.bases = bases;
        m.parent = parent;
        m.unresolved = true; // Object / Kernel / BasicObject: every class has a library ancestor
        if let Some(d) = m.decls.first().cloned() {
            let e = r.full_bases.entry(d.clone()).or_default();
            e.extend(fulls.into_iter().filter(|x| *x != d));
        }
    }
    for (full, names) in &idx {
        let cname = r.canon[full].clone();
        let others: Vec<String> = names.iter().filter(|mn| **mn != cname).cloned().collect();
        {
            let c = g.modules.get_mut(&cname).unwrap();
            let rest: Vec<String> = c.bases.iter().filter(|x| !others.contains(x)).cloned().collect();
            c.bases = others.iter().cloned().chain(rest).collect();
        }
        let cparent = g.modules[&cname].parent.clone();
        for mn in &others {
            let o = g.modules.get_mut(mn).unwrap();
            let rest: Vec<String> = o.bases.iter().filter(|x| **x != cname).cloned().collect();
            o.bases = std::iter::once(cname.clone()).chain(rest).collect();
            if o.parent.is_empty() {
                o.parent = cparent.clone();
            }
        }
        if cparent.is_empty() && !others.is_empty() {
            let p = others.iter().map(|mn| g.modules[mn].parent.clone()).find(|p| !p.is_empty()).unwrap_or_default();
            g.modules.get_mut(&cname).unwrap().parent = p;
        }
        for mn in &others {
            // only the class declares the constant: a lookup lands on it and reaches the
            // reopenings through its bases (a namespace reopened in a hundred files made every
            // lookup walk a hundred modules)
            r.full_of.insert(mn.clone(), full.clone());
            g.modules.get_mut(mn).unwrap().decls.clear();
        }
    }
    for full in &lib_reopened {
        for mn in &idx[full] {
            r.full_of.insert(mn.clone(), full.clone());
            g.modules.get_mut(mn).unwrap().decls.clear();
        }
    }
    r
}

/// File -> the top-level `def`s it can call: its own and those of the files
/// it requires (and they require).
fn top_defs_seen(g: &Graph, rb: &[String]) -> HashMap<String, HashSet<String>> {
    let own: HashMap<&str, HashSet<&str>> = rb
        .iter()
        .filter(|n| g.modules[*n].owner.is_empty())
        .map(|n| (n.as_str(), g.modules[n].funcs.iter().filter(|f| f.kind != "top").map(|f| f.name.as_str()).collect()))
        .collect();
    let mut out = HashMap::new();
    for mn in own.keys() {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut todo: Vec<&str> = vec![mn];
        let mut names: HashSet<String> = HashSet::new();
        while let Some(cur) = todo.pop() {
            if seen.contains(cur) || !own.contains_key(cur) {
                continue;
            }
            seen.insert(cur);
            names.extend(own[cur].iter().map(|s| s.to_string()));
            todo.extend(g.modules[cur].reexports.iter().map(|s| s.as_str()));
        }
        out.insert(mn.to_string(), names);
    }
    out
}

/// A bare call in a class is `self.x`: the class, its ancestors, and (a mixin
/// calling its includer's method, a base class its subclasses') the classes
/// below it. A top-level `def` the file can reach (a method of every object)
/// keeps the file's scope.
fn bare_calls_on_self(g: &mut Graph, rb: &[String]) {
    let top_defs = top_defs_seen(g, rb);
    let empty = HashSet::new();
    for mn in rb {
        let owner = g.modules[mn].owner.clone();
        if owner.is_empty() {
            continue;
        }
        let seen = top_defs.get(&owner).unwrap_or(&empty);
        let m = g.modules.get_mut(mn).unwrap();
        for f in m.funcs.iter_mut() {
            for c in f.calls.iter_mut() {
                if c.chain.is_empty() && c.qual.is_empty() && !seen.contains(&c.name) {
                    c.chain = vec!["self".into()];
                    c.qual = "self".into();
                }
            }
        }
    }
}

/// `def update_file(path, &block) ... File.open(path, 'w', &block)`: a block
/// given to update_file is File's, so `update_file(p) do |f|` has f as the
/// library's (when every method of that name hands it on alike).
fn block_forwarders(g: &mut Graph, rb: &[String]) {
    let head = rx(0, r"\s*def\s+(?:self\.)?\w+[?!]?\s*\([^)]*&(\w+)");
    let mut sorted: Vec<&str> = STDLIB_YIELDS.to_vec();
    sorted.sort();
    let alt = sorted.iter().map(|s| regex::escape(s)).collect::<Vec<_>>().join("|");
    let mut fwd: IndexMap<String, Vec<Option<String>>> = IndexMap::new();
    let mut note = |name: &str, v: Option<String>| {
        let e = fwd.entry(name.to_string()).or_default();
        if !e.contains(&v) {
            e.push(v);
        }
    };
    for mn in rb {
        for f in &g.modules[mn].funcs {
            let Some(hm) = head.match_start(&f.code) else {
                note(&f.name, None);
                continue;
            };
            let blk = hm.s(1).to_string();
            let pat = format!(r"(?<![\w.:])({alt})\s*\.\s*\w+[?!]?\s*\([^()\n]*&{blk}\s*\)");
            let got = cached(&pat).ok().and_then(|r| r.search(&f.code).map(|m| m.s(1).to_string()));
            note(&f.name, got);
        }
    }
    let names: IndexMap<String, String> = fwd
        .into_iter()
        .filter(|(_, cs)| cs.len() == 1 && cs[0].is_some())
        .map(|(n, cs)| (n, cs.into_iter().next().unwrap().unwrap()))
        .collect();
    if names.is_empty() {
        return;
    }
    let alt_names = names.keys().map(|n| regex::escape(n)).collect::<Vec<_>>().join("|");
    let pat = cached(&format!(r"(?<![\w.:@])({alt_names})\b[^\n{{]*?(?:\bdo|\{{)\s*\|([^|\n]*)\|")).unwrap();
    let lower = rx(1, r"[a-z_]\w*");
    for mn in rb {
        let m = g.modules.get_mut(mn).unwrap();
        for f in m.funcs.iter_mut() {
            for cm in pat.find_iter(&f.code) {
                for nm in lower.findall(cm.s(2)) {
                    if f.bound.get(nm).map(|h| h.ends_with("{}")).unwrap_or(false) {
                        f.bound.shift_remove(nm); // (rather than what the method yields, read in infer)
                    }
                    f.bound.entry(nm.to_string()).or_insert_with(|| format!("lib:{}", names[cm.s(1)]));
                }
            }
        }
    }
}

/// Every constant in a chain, a binding or a return type as the full name of
/// the class it means (`Site` in module Jekyll is `Jekyll::Site`); a gem's
/// stays as written (infer reads an undeclared constant as a library's).
fn rewrite_constants(g: &mut Graph, rb: &[String], r: &Resolver) {
    for mn in rb {
        let nest = r.nesting(g, &g.modules[mn]);
        // the module's own bindings
        let bound: Vec<(String, String)> = g.modules[mn].bound.iter().map(|(k, v)| (k.clone(), r.rewrite_held(g, &nest, v))).collect();
        let nfuncs = g.modules[mn].funcs.len();
        let mut fixes: Vec<(usize, Option<String>, Vec<(String, Option<String>)>, Vec<(usize, Vec<String>, Option<String>, Option<String>)>)> =
            Vec::new();
        for k in 0..nfuncs {
            let f = &g.modules[mn].funcs[k];
            let returns = if f.returns.starts_with("expr:") { Some(r.rewrite_held(g, &nest, &f.returns)) } else { None };
            let mut bnd: Vec<(String, Option<String>)> = Vec::new();
            for (key, v) in &f.bound {
                if let Some(lib) = v.strip_prefix("lib:") {
                    let (cst, ours) = match lib.split_once('\x1e') {
                        Some((c, o)) => (c, o),
                        None => (lib, ""),
                    };
                    if !r.resolve(&nest, cst, 0).is_empty() {
                        if !ours.is_empty() {
                            // `Sidekiq.redis { |conn| }`: what the project's method yields
                            bnd.push((key.clone(), Some(r.rewrite_held(g, &nest, ours))));
                        } else {
                            bnd.push((key.clone(), None)); // a project constant's block: we don't know what it yields
                        }
                    } else {
                        // what a library yields is the library's
                        bnd.push((key.clone(), Some(format!("expr:{cst}\x1fyield()"))));
                    }
                    continue;
                }
                bnd.push((key.clone(), Some(r.rewrite_held(g, &nest, v))));
            }
            let mut cs: Vec<(usize, Vec<String>, Option<String>, Option<String>)> = Vec::new();
            for (ci, c) in f.calls.iter().enumerate() {
                if c.chain.is_empty() {
                    continue;
                }
                let segs: Vec<String> =
                    c.chain.iter().map(|s| if first_upper(s) { r.const_(&nest, s) } else { s.clone() }).collect();
                let chain = r.index_new(g, segs);
                let mut qual = None;
                let mut name = None;
                if first_upper(chain.last().unwrap()) {
                    qual = Some(chain.last().unwrap().replace("::", "."));
                    if c.name == "new" && chain.len() == 1 {
                        name = Some(r.constructor(g, &chain[0]));
                    }
                }
                cs.push((ci, chain, qual, name));
            }
            fixes.push((k, returns, bnd, cs));
        }
        let m = g.modules.get_mut(mn).unwrap();
        for (k, v) in bound {
            m.bound.insert(k, v);
        }
        for (k, returns, bnd, cs) in fixes {
            let f = &mut m.funcs[k];
            if let Some(rt) = returns {
                f.returns = rt;
            }
            for (key, v) in bnd {
                match v {
                    Some(v) => {
                        f.bound.insert(key, v);
                    }
                    None => {
                        f.bound.shift_remove(&key);
                    }
                }
            }
            for (ci, chain, qual, name) in cs {
                let c = &mut f.calls[ci];
                c.chain = chain;
                if let Some(q) = qual {
                    c.qual = q;
                }
                if let Some(n) = name {
                    c.name = n;
                }
            }
        }
    }
}
