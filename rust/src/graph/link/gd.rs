//! The GDScript half of the call resolver (core._build_edges's bare-call
//! block for scripts, infer.Types.type_modules / method / external_base and
//! core._gd_declared / _gd_extends). Written as the slice of the type
//! machinery GDScript needs; the infer lane replaces it with the whole.

use std::collections::HashSet;

use super::{shares_family, Resolver, Targets};
use crate::graph::pylink::ancestors;
use crate::model::{Call, Func, Module};
use crate::project::ext_of;

/// What the bare-call block decided.
pub(super) enum Bare<'a> {
    /// not decided: the usual branches follow
    Go,
    /// the call reaches no project function
    Nothing,
    Targets(Vec<(&'a Func, bool)>),
}

fn dirname(p: &str) -> &str {
    p.rsplit_once('/').map(|(a, _)| a).unwrap_or("")
}

impl<'a> Resolver<'a> {
    fn file_mod(&self, m: &'a Module) -> &'a Module {
        if m.owner.is_empty() {
            m
        } else {
            self.g.modules.get(&m.owner).unwrap_or(m)
        }
    }

    fn searchable(&self, mn: &str) -> bool {
        self.g.modules.get(mn).map(|m| matches!(m.kind.as_str(), "class" | "interface" | "ext") || !m.owner.is_empty()).unwrap_or(false)
    }

    /// infer.Types.method: `name` on an instance of these modules' type: the
    /// definition on the type or its nearest supertype; else every subtype's
    /// implementation. (funcs, found on the type)
    pub(super) fn method(&self, mods: &[String], name: &str) -> (Vec<&'a Func>, bool) {
        let g = self.g;
        let mut found: Vec<&Func> = Vec::new();
        for mn in mods {
            for a in ancestors(g, mn) {
                if let Some(t) = g.func(&format!("{a}.{name}")) {
                    if !found.iter().any(|x| x.id == t.id) {
                        found.push(t);
                    }
                    break;
                }
            }
        }
        if !found.is_empty() {
            return (found, true);
        }
        let mut impls: Vec<&Func> = Vec::new();
        for mn in mods {
            for sub in g.children.get(mn).map(|v| v.as_slice()).unwrap_or(&[]) {
                let got = g.func(&format!("{sub}.{name}"));
                if let Some(t) = got {
                    if !impls.iter().any(|x| x.id == t.id) {
                        impls.push(t);
                    }
                }
                if got.is_none() && g.modules.get(sub).map(|m| m.kind != "interface").unwrap_or(false) {
                    for a in ancestors(g, sub).into_iter().skip(1) {
                        if let Some(t) = g.func(&format!("{a}.{name}")) {
                            if g.modules[&a].kind != "interface" && !impls.iter().any(|x| x.id == t.id) {
                                impls.push(t);
                            }
                            break;
                        }
                    }
                }
            }
        }
        (impls, false)
    }

    /// A supertype the project doesn't declare: an inherited method may be the library's.
    pub(super) fn external_base(&self, mn: &str) -> bool {
        ancestors(self.g, mn).iter().any(|a| self.g.modules[a].unresolved)
    }

    fn godot_root(&mut self, rel: &str) -> String {
        let d = dirname(rel).to_string();
        if let Some(r) = self.godot.get(&d) {
            return r.clone();
        }
        let mut up = d.clone();
        while !up.is_empty() && !self.p.root.join(&up).join("project.godot").exists() {
            up = dirname(&up).to_string();
        }
        self.godot.insert(d, up.clone());
        up
    }

    fn type_imported(&self, fm: &Module, name: &str, mn: &str) -> bool {
        let g = self.g;
        let m = &g.modules[mn];
        let own = self.file_mod(m).name.clone();
        let w = fm
            .aliases
            .get(name)
            .filter(|s| !s.is_empty())
            .cloned()
            .or_else(|| fm.imported.get(name).map(|x| x.0.clone()))
            .unwrap_or_default();
        if !w.is_empty() {
            if w == mn || w == own {
                return true;
            }
            if let Some(pk) = w.strip_prefix("pkg:") {
                if self.file_mod(m).package == pk {
                    return true;
                }
            }
            if g.modules.contains_key(&w) {
                let dm = self.defining_module(&w, name, 0);
                if !dm.is_empty() && (dm == mn || dm == own) {
                    return true;
                }
            }
            return false;
        }
        fm.uses.iter().any(|u| u == &format!("type:{name}") && m.decls.iter().any(|d| d == name))
    }

    fn in_uses(&self, fm: &Module, mn: &str) -> bool {
        let own = self.file_mod(&self.g.modules[mn]);
        fm.uses.iter().any(|u| u.strip_prefix("pkg:").map(|pk| own.package == pk).unwrap_or(false) || u == &own.name)
    }

    /// infer.Types.type_modules: the modules declaring type `name` as seen
    /// from `mod`: (modules, exact, declared in the project). Nearest first.
    pub(super) fn type_modules(&mut self, m: &'a Module, name: &str) -> (Vec<String>, bool, bool) {
        let g = self.g;
        let fm = self.file_mod(m);
        if fm.file.ends_with(".py") {
            let cls = crate::graph::pylink::py_class(g, m, name, 0);
            if !cls.is_empty() {
                return (vec![cls], true, true);
            }
        }
        let mut name = name.to_string();
        if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            if let Some(al) = fm.bound.get(&name).and_then(|s| s.strip_prefix("type:")) {
                if al != name {
                    name = al.to_string();
                }
            }
        }
        // lane (types): dotted names, C++ aliases, Rust crates
        let cands: Vec<String> = self.by_type.get(&name).cloned().unwrap_or_default();
        let cands: Vec<String> = cands.into_iter().filter(|mn| shares_family(&fm.file, &g.modules[mn].file)).collect();
        if cands.is_empty() {
            return (Vec::new(), false, false);
        }
        let exts: Vec<String> = cands.iter().filter(|mn| g.modules[*mn].kind == "ext").cloned().collect();
        let mut prim: Vec<String> = cands.iter().filter(|mn| g.modules[*mn].kind != "ext").cloned().collect();
        if prim.is_empty() {
            prim = exts.clone();
        }
        let godot_here = if m.file.ends_with(".gd") { Some(self.godot_root(&m.file)) } else { None };
        let narrows: [&dyn Fn(&mut Self, &String) -> bool; 6] = [
            &|_s, mn| g.modules[mn].file == m.file,
            &|s, mn| s.type_imported(fm, &name, mn),
            &|s, mn| !fm.package.is_empty() && s.file_mod(&g.modules[mn]).package == fm.package,
            &|s, mn| s.in_uses(fm, mn),
            &|s, mn| match &godot_here {
                Some(r) => s.godot_root(&g.modules[mn].file) == *r,
                None => false,
            },
            &|_s, mn| dirname(&g.modules[mn].file) == dirname(&m.file),
        ];
        for narrow in narrows.iter() {
            let got: Vec<String> = prim.clone().into_iter().filter(|mn| narrow(self, mn)).collect();
            if !got.is_empty() {
                prim = got;
                break;
            }
        }
        // lane (types): an ES module sees another's type only by importing it; Rust aliases
        let prims: HashSet<&String> = prim.iter().collect();
        let mut mods: Vec<String> = Vec::new();
        for mn in prim.iter().chain(exts.iter().filter(|e| !prim.contains(e))) {
            if !mods.contains(mn) {
                mods.push(mn.clone());
            }
        }
        let dirs: HashSet<&str> = prim.iter().map(|mn| dirname(&g.modules[mn].file)).collect();
        let decl0: HashSet<String> = prim
            .iter()
            .map(|mn| {
                let x = &g.modules[mn];
                if !x.owner.is_empty() && !x.decls.is_empty() { x.decls[0].clone() } else { name.clone() }
            })
            .collect();
        let variants = dirs.len() == 1 && prims.len() > 1 && decl0.len() == 1;
        (mods, prims.len() == 1 || variants, true)
    }

    /// core._gd_declared (a one-segment chain): the engine class a GDScript
    /// receiver is declared as (`var x: Camera3D`), or "" (untyped, or a
    /// project script's type). (lane (types): a longer chain)
    fn gd_declared(&self, f: &Func, m: &Module, chain: &[String]) -> String {
        if chain.len() != 1 {
            return String::new();
        }
        let held = f.bound.get(&chain[0]).filter(|s| !s.is_empty()).or_else(|| m.bound.get(&chain[0])).map(|s| s.as_str()).unwrap_or("");
        let ty = held.strip_prefix("type:").unwrap_or("");
        if !ty.is_empty() && !self.g.modules.contains_key(ty) && self.g.short.get(ty).map(|v| v.is_empty()).unwrap_or(true) {
            ty.to_string()
        } else {
            String::new()
        }
    }

    /// A script's `extends` chain reaches the engine class `engine`.
    fn gd_extends(&self, module: &str, engine: &str) -> bool {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut m = self.g.modules.get(module);
        while let Some(mm) = m {
            if !seen.insert(mm.name.as_str()) {
                break;
            }
            if mm.parent == engine {
                return true;
            }
            m = if mm.parent.is_empty() { None } else { self.g.modules.get(&mm.parent) };
        }
        false
    }

    /// The guess step's GDScript narrowing: `var part: RigidBody3D = ...`
    /// then `part.explode()`: the project's one script extending
    /// RigidBody3D with an explode().
    pub(super) fn gd_engine_script(&self, f: &Func, m: &Module, c: &Call, cands: &[&'a Func]) -> Option<(&'a Func, bool)> {
        if c.chain.is_empty() || ext_of(&f.file) != ".gd" {
            return None;
        }
        let eng = self.gd_declared(f, m, &c.chain);
        if eng.is_empty() {
            return None;
        }
        let sub: Vec<&Func> = cands.iter().copied().filter(|g| self.gd_extends(&g.module, &eng)).collect();
        let mods: HashSet<&str> = sub.iter().map(|g| g.module.as_str()).collect();
        (mods.len() == 1).then(|| (sub[0], true))
    }

    /// A bare call in a GDScript class: its own (or an inherited) method
    /// first; `Foo(...)`: Foo's constructor (core._build_edges).
    pub(super) fn gd_bare(&mut self, f: &Func, m: &'a Module, c: &Call) -> Bare<'a> {
        let g = self.g;
        let own: Option<Vec<String>> = if !m.owner.is_empty() || !m.kind.is_empty() { Some(vec![m.name.clone()]) } else { None };
        let (hits, on_type) = match &own {
            Some(o) => self.method(o, &c.name),
            None => (Vec::new(), false),
        };
        let fm_name = self.file_mod(m).name.clone();
        if hits.len() == 1 && on_type {
            return Bare::Targets(vec![(hits[0], true)]);
        }
        if c.name.chars().next().map(|ch| ch.is_uppercase()).unwrap_or(false) && self.in_scope(&fm_name, &c.name, 0).is_empty() {
            let (mut tmods, ex, mut declared) = self.type_modules(m, &c.name);
            if declared && !self.is_test(&f.file) {
                // product code never builds a test's type
                let keep: Vec<String> = tmods.iter().filter(|x| !self.is_test_mod(x)).cloned().collect();
                tmods = keep;
                declared = !tmods.is_empty();
            }
            if declared {
                let searchable: Vec<String> =
                    tmods.iter().filter(|x| self.searchable(x) || !g.modules[*x].kind.is_empty()).cloned().collect();
                let ctor0 = if tmods.is_empty() { Vec::new() } else { self.method(&searchable, &c.name).0 };
                let mut ctor: Vec<&Func> = ctor0.into_iter().filter(|t| t.name == c.name).collect();
                if ctor.is_empty() {
                    ctor = tmods
                        .iter()
                        .flat_map(|x| ["constructor", "init", "__construct", "new"].into_iter().filter_map(move |n| g.func(&format!("{x}.{n}"))))
                        .take(1)
                        .collect();
                }
                let mut seen = HashSet::new();
                ctor.retain(|t| seen.insert(t.id.clone()));
                if ctor.is_empty() {
                    return Bare::Nothing;
                }
                let sure = ex && (ctor.len() == 1 || self.one_type(&ctor));
                return Bare::Targets(ctor.into_iter().map(|t| (t, sure)).collect());
            }
        }
        if let Some(o) = &own {
            if hits.is_empty() && o.iter().any(|x| self.external_base(x)) && self.in_scope(&fm_name, &c.name, 0).is_empty() {
                // inherited from a library supertype (Godot's `queue_free`)
                return Bare::Nothing;
            }
        }
        Bare::Go
    }

    fn is_test_mod(&mut self, mn: &str) -> bool {
        let file = self.g.modules[mn].file.clone();
        self.is_test(&file)
    }
}

#[allow(dead_code)]
fn _t(_: Targets) {}
