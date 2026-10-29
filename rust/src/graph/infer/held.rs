//! What a binding holds (infer.held and the readers it dispatches to).

use super::{ends, esc, ext, is_ext, re, rxd, upper1, Types, Val, C_EXTS, JS_EXTS, MAX, RB_EXTS};
use crate::lang::chains::type_name;
use crate::model::{Func, Module};

const SEP_CH: char = '\x1f';

fn split_chain(s: &str) -> Vec<String> {
    s.split(SEP_CH).map(|x| x.to_string()).collect()
}

impl<'a> Types<'a> {
    /// The value a binding holds: "type:T", "expr:<chain>", "group:<id>".
    pub fn held(&self, md: &'a Module, f: Option<&'a Func>, held: &str, depth: usize, self_mods: Option<&[String]>) -> Option<Val> {
        if depth > MAX {
            return None;
        }
        if let Some(t) = held.strip_prefix("type:") {
            let head = t.split('.').next().unwrap_or("");
            if matches!(head, "std" | "boost" | "absl") && ends(&md.file, C_EXTS) && !self.by_type.contains_key(head) {
                return Some(ext()); // `std::vector<...> formatters_`: the standard library's
            }
            return self.instance(md, t, self_mods);
        }
        if let Some(r) = held.strip_prefix("mod:") {
            // a component whose file names no type (`mousepad.qml`'s root id)
            return if self.g.modules.contains_key(r) { Some(Val::Inst(vec![r.to_string()], true)) } else { None };
        }
        if let Some(e) = held.strip_prefix("expr:") {
            return self.chain(md, f, &split_chain(e), depth + 1, self_mods, None);
        }
        if let Some(e) = held.strip_prefix("either:") {
            // a conditional's two values: the one they agree on
            let mut got: Vec<Option<Val>> = Vec::new();
            for x in e.split('\x1c') {
                let r = self.chain(md, f, &split_chain(x), depth + 1, self_mods, None);
                if !got.contains(&r) {
                    got.push(r);
                }
            }
            return if got.len() == 1 { got.pop().unwrap() } else { None };
        }
        if let Some(e) = held.strip_prefix("oneof:") {
            // what every site hands a QML property, each read where it is handed
            let mut got: Vec<Val> = Vec::new();
            for alt in e.split('\x1c') {
                let (mn, inner) = match alt.split_once('\x1b') {
                    Some((a, b)) => (a, b),
                    None => (alt, ""),
                };
                let v = if self.g.modules.contains_key(mn) && inner != "?" {
                    self.held(self.m(mn), None, inner, depth + 1, None)
                } else {
                    None
                };
                match v {
                    None | Some(Val::Unknown) => return None,
                    Some(v) => {
                        if !got.contains(&v) {
                            got.push(v)
                        }
                    }
                }
            }
            return if got.len() == 1 { got.pop() } else { None };
        }
        if let Some(r) = held.strip_prefix("reexport:") {
            // `exports.Router = Router`: that name's value in the module
            return if md.external.contains(r) { Some(ext()) } else { None };
        }
        if let Some(r) = held.strip_prefix("pack:") {
            return self.pack_param(r);
        }
        if let Some(r) = held.strip_prefix("variant:") {
            return self.variant(md, r, f);
        }
        if let Some(r) = held.strip_prefix("tuple:") {
            // `let (a, b) = f();`: that element of what f returns
            let (k_, ch_) = r.split_once('\x1e').unwrap_or((r, ""));
            let k: usize = k_.parse().unwrap_or(0);
            let chain = split_chain(ch_);
            let last = chain.last().unwrap();
            let last = &last[..last.len().saturating_sub(2)];
            let hits: Vec<&Func> = if chain.len() == 1 {
                self.bare(md, f, last).into_iter().collect()
            } else {
                let v = self.chain(md, f, &chain[..chain.len() - 1], depth + 1, None, None);
                match v {
                    Some(v) if !v.is_ext() && v != Val::Unknown && v.is_inst_or_static() => self.method(v.mods(), last, None).0,
                    _ => vec![],
                }
            };
            let mut tys: Vec<(String, String)> = Vec::new();
            for gf in hits {
                if gf.returns.starts_with("tuple:") {
                    let parts: Vec<&str> = gf.returns.split('|').collect();
                    let n = gf.returns[6..].split('|').count();
                    if k < n {
                        let e = (gf.module.clone(), parts[k].to_string());
                        if !tys.contains(&e) {
                            tys.push(e);
                        }
                    }
                }
            }
            let distinct: std::collections::HashSet<&String> = tys.iter().map(|(_, t)| t).collect();
            if distinct.len() != 1 {
                return None;
            }
            let (gm_, ty_) = tys.remove(0);
            return if ty_.is_empty() {
                None
            } else {
                self.instance(self.m(&gm_), ty_.strip_prefix("tuple:").unwrap_or(&ty_), None)
            };
        }
        if held == "jsx:" {
            return self.jsx_element();
        }
        if let Some(r) = held.strip_prefix("elem:") {
            return self.loop_element(md, f, &split_chain(r), depth);
        }
        if let Some(r) = held.strip_prefix("lam:") {
            return self.lambda_param(md, f, r, depth);
        }
        if let Some(r) = held.strip_prefix("fixture:") {
            return match self.fixture(md, r, f) {
                Some(gf) => self.returns(gf, None, depth + 1, None),
                None => None,
            };
        }
        None
    }

    /// What a JSX literal evaluates to: the result of the project's own JSX
    /// runtime (hono's `jsxDEV(): JSXNode`) when tsconfig's jsxImportSource
    /// names this package; else a library's element (React's).
    fn jsx_element(&self) -> Option<Val> {
        if let Some(h) = &self.c.borrow().jsx {
            return h.clone();
        }
        let mut got = Some(ext());
        let mut source = String::new();
        for rel in &self.p.files {
            let base = rel.rsplit('/').next().unwrap_or(rel);
            if base.starts_with("tsconfig") && rel.ends_with(".json") {
                let txt = std::fs::read(self.p.root.join(rel)).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
                if let Some(m) = txt.as_deref().and_then(|t| re!(r#""jsxImportSource"\s*:\s*"([^"]+)""#).search(t).map(|c| c.s(1).to_string())) {
                    source = m;
                    break;
                }
            }
        }
        let pkg = self.package_name();
        if !source.is_empty() && !pkg.is_empty() && (source == pkg || source.starts_with(&format!("{pkg}/"))) {
            let mut tys: Vec<(String, String)> = Vec::new();
            for n in ["jsxDEV", "jsx"] {
                for id in self.g.by_name.get(n).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if let Some(gf) = self.fx(id) {
                        let base = gf.file.rsplit('/').next().unwrap_or(&gf.file);
                        if (base.starts_with("jsx-runtime") || base.starts_with("jsx-dev-runtime")) && !gf.returns.is_empty() {
                            let e = (gf.module.clone(), gf.returns.clone());
                            if !tys.contains(&e) {
                                tys.push(e);
                            }
                        }
                    }
                }
            }
            let distinct: std::collections::HashSet<&String> = tys.iter().map(|(_, t)| t).collect();
            got = if distinct.len() == 1 {
                let (gm, ty) = tys.remove(0);
                self.instance(self.m(&gm), &ty, None)
            } else {
                None
            };
        }
        self.c.borrow_mut().jsx = Some(got.clone());
        got
    }

    /// What a Rust enum variant holds (`DirEntryInner::Raw(x)`: x is a
    /// DirEntryRaw); without the enum named, every enum's variant of that name
    /// must hold the one type.
    fn variant(&self, md: &'a Module, spec: &str, f: Option<&'a Func>) -> Option<Val> {
        let (enum_, name) = match spec.rsplit_once("::") {
            Some((a, b)) => (a, b),
            None => ("", spec),
        };
        if enum_ == "Self" {
            // `Self::IoWrite(handle) =>` in the enum's own impl
            let own = self.own_class(md, f);
            let got = own.and_then(|o| self.field(&o, &format!("::{name}"), 1));
            return if got == Some(Val::Unknown) { None } else { got };
        }
        if !enum_.is_empty() {
            let (mods, _, declared) = self.type_modules(md, enum_);
            let got = if declared { self.field(&mods, &format!("::{name}"), 1) } else { None };
            return if got == Some(Val::Unknown) { None } else { got };
        }
        let cached = self.c.borrow().variants.is_some();
        if !cached {
            let mut idx: std::collections::HashMap<String, Vec<(String, String)>> = Default::default();
            for (mn, m) in &self.g.modules {
                for (k, v) in &m.bound {
                    if let Some(rest) = k.strip_prefix("::") {
                        idx.entry(rest.to_string()).or_default().push((mn.clone(), v.clone()));
                    }
                }
            }
            self.c.borrow_mut().variants = Some(idx);
        }
        let hits = self.c.borrow().variants.as_ref().unwrap().get(name).cloned().unwrap_or_default();
        let distinct: std::collections::HashSet<&String> = hits.iter().map(|(_, v)| v).collect();
        if distinct.len() != 1 {
            return None;
        }
        self.held(self.m(&hits[0].0), None, &hits[0].1, 1, None)
    }

    /// A for-loop variable: an element of what it iterates (a declared
    /// container, or a function whose return type is a sequence).
    pub fn loop_element(&self, md: &'a Module, f: Option<&'a Func>, chain: &[String], depth: usize) -> Option<Val> {
        let key = (md.name.clone(), f.map(|x| x.id.clone()).unwrap_or_default(), chain.to_vec(), self.line.get());
        if let Some(h) = self.c.borrow().elem.get(&key) {
            return h.clone();
        }
        self.c.borrow_mut().elem.insert(key.clone(), None); // (a cycle through the same container: unknown)
        let got = self.loop_element_(md, f, chain, depth);
        self.c.borrow_mut().elem.insert(key, got.clone());
        got
    }

    fn loop_element_(&self, md: &'a Module, f: Option<&'a Func>, chain: &[String], depth: usize) -> Option<Val> {
        let last = chain.last()?;
        if last.ends_with("()") && ends(&md.file, RB_EXTS) && depth < MAX {
            // Ruby: `site.docs` reads `@docs`, its `docs[]`
            let owner = if chain.len() > 1 { self.chain(md, f, &chain[..chain.len() - 1], depth + 1, None, None) } else { None };
            if let Some(o) = &owner {
                if !o.is_ext() && *o != Val::Unknown {
                    if let Some(el) = self.element(md, f, &last[..last.len() - 2], owner.as_ref(), depth + 1) {
                        return Some(el);
                    }
                }
            }
        }
        if !last.ends_with("()") {
            // `for x in xs` / `self.items`: a declared container
            let owner = if chain.len() > 1 { self.chain(md, f, &chain[..chain.len() - 1], depth + 1, None, None) } else { None };
            if chain.len() > 1 && owner.is_none() {
                return None;
            }
            return self.element(md, f, last, owner.as_ref(), depth);
        }
        let name = last[..last.len() - 2].split('`').next().unwrap().to_string();
        let hits: Vec<&Func> = if chain.len() == 1 {
            self.bare(md, f, &name).into_iter().collect()
        } else {
            let v = self.chain(md, f, &chain[..chain.len() - 1], depth + 1, None, None);
            match v {
                Some(v) if !v.is_ext() && v != Val::Unknown && v.is_inst_or_static() => {
                    let mut h: Vec<&Func> = Vec::new();
                    if matches!(v, Val::Static(..)) {
                        for mn in v.mods() {
                            h.extend(self.in_scope(mn, &name, 0));
                        }
                    }
                    if h.is_empty() {
                        h = self.method(v.mods(), &name, None).0;
                    }
                    h
                }
                _ => return None,
            }
        };
        let mut els: Vec<(String, String)> = Vec::new();
        for gf in &hits {
            if !gf.relem.is_empty() {
                let e = (gf.module.clone(), gf.relem.clone());
                if !els.contains(&e) {
                    els.push(e);
                }
            }
        }
        if !hits.is_empty() && els.is_empty() {
            // `for seq in Offsets::new(line)`: a project iterator gives its `type Item`
            let v = self.chain(md, f, chain, depth + 1, None, None);
            if let Some(Val::Inst(mods, _)) = v {
                let got = self.field(&mods, "type Item", depth);
                return if got == Some(Val::Unknown) { None } else { got };
            }
        }
        let distinct: std::collections::HashSet<&String> = els.iter().map(|(_, e)| e).collect();
        let ids: std::collections::HashSet<&String> = hits.iter().map(|h| &h.id).collect();
        if hits.is_empty() || distinct.len() != 1 || (els.len() < ids.len() && hits.len() > 1) {
            return None;
        }
        let (gm_name, ty) = els.remove(0);
        self.g.modules.get(&gm_name).and_then(|gm| self.instance(gm, &ty, None))
    }

    /// The type parameters of the class a module's function belongs to.
    pub(super) fn class_params(&self, gm: &Module) -> Vec<String> {
        self.ancestors(&gm.name)
            .iter()
            .flat_map(|a| self.m(a).bound.get("<>").cloned().unwrap_or_default().split(',').map(|s| s.to_string()).collect::<Vec<_>>())
            .filter(|n| !n.is_empty())
            .collect()
    }

    /// The type argument a receiver was declared with for the callee's
    /// class type parameter `param` (`let s: Protected<MutableState>` and
    /// `class Protected<Value>`: Value -> MutableState), or "".
    pub(super) fn generic_arg(&self, md: &'a Module, f: Option<&'a Func>, chain: &[String], gm: &Module, param: &str) -> String {
        let names: Vec<String> = self.class_params(gm).into_iter().take(8).collect();
        if !names.iter().any(|n| n == param) {
            return String::new();
        }
        let last = chain.last().cloned().unwrap_or_default();
        let mut held = String::new();
        if let Some(f) = f {
            held = f.bound.get(&format!("{last}<>")).cloned().unwrap_or_default();
        }
        if held.is_empty() && (chain.len() == 1 || chain[0] == "self" || chain[0] == "this") {
            let own = self.own_class(md, f).unwrap_or_default();
            'o: for o in &own {
                for a in self.ancestors(o) {
                    if let Some(h) = self.m(&a).bound.get(&format!("{last}<>")).filter(|s| !s.is_empty()) {
                        held = h.clone();
                        break 'o;
                    }
                }
            }
        }
        let Some(h) = held.strip_prefix("gen:") else { return String::new() };
        let args: Vec<String> = crate::lang::lambdas::split_top(h, ',').iter().map(|a| a.trim().to_string()).collect();
        let k = names.iter().position(|n| n == param).unwrap();
        if k < args.len() {
            type_name(&args[k])
        } else {
            String::new()
        }
    }

    /// JS: g hands its parameter at argi on, whole, to a library call
    /// (`function startHTTPServer(handler) { http.createServer(handler) }`).
    pub(super) fn forwards_to_library(&self, gf: &'a Func, argi: usize, depth: usize) -> bool {
        let Some(hm) = re!(r"^[^(]*\(").match_start(&gf.code) else { return false };
        let close = crate::lang::lambdas::matching(&gf.code, hm.end() - 1);
        let params = if close > 0 { crate::lang::lambdas::split_top(&gf.code[hm.end()..close as usize], ',') } else { vec![] };
        if argi >= params.len() {
            return false;
        }
        let Some(pm) = re!(r"^\s*(?:\.\.\.)?([A-Za-z_$][\w$]*)\s*(?:[?:=,]|$)").match_start(&params[argi]) else { return false };
        let pn = esc(pm.s(1));
        let gm = self.g.modules.get(&gf.module);
        let rest = if close > 0 { &gf.code[close as usize..] } else { &gf.code[..] };
        for cm in rxd(&format!(r"((?:[A-Za-z_$][\w$]*\.)*[A-Za-z_$][\w$]*)\s*\((?:[^()]*?[(,]\s*)?{pn}\s*[,)]")).find_iter(rest) {
            let ch = crate::lang::chains::expression(&format!("{}()", cm.s(1)), 0, 40, Default::default());
            if let Some(gm) = gm {
                if ch.len() > 1 {
                    let r = self.chain(gm, Some(self.fx(&gf.id).unwrap_or(gf)), &ch[..ch.len() - 1], depth + 1, None, None);
                    if is_ext(&r) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// A framework-given parameter ("express/handler:res"): the module its
    /// pack names ("express:lib/response"), project code when the project is
    /// that package (express itself), else the library's.
    pub(super) fn pack_param(&self, spec: &str) -> Option<Val> {
        let (pid, name) = spec.rsplit_once(':')?;
        let entry = self.g.params.get(pid)?;
        let ty = entry.get("types").and_then(|t| t.get(name)).and_then(|x| x.as_str()).unwrap_or("");
        let (pkg, path) = ty.split_once(':').unwrap_or((ty, ""));
        if pkg.is_empty() {
            return None;
        }
        if pkg == self.package_name() && self.g.modules.contains_key(path) {
            // (a class's file: an instance of it)
            let k = if self.m(path).kind == "class" { "inst" } else { "static" };
            let mods = vec![path.to_string()];
            return Some(if k == "inst" { Val::Inst(mods, true) } else { Val::Static(mods, true) });
        }
        Some(ext())
    }

    pub(super) fn package_name(&self) -> String {
        if let Some(h) = &self.c.borrow().pkgname {
            return h.clone();
        }
        let got = std::fs::read(self.p.root.join("package.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .unwrap_or_default();
        self.c.borrow_mut().pkgname = Some(got.clone());
        got
    }

    /// The pytest fixture a test's parameter names: its own class's, else
    /// its file's (module level), else the nearest conftest.py up the tree.
    /// A fixture's parameter of its own name is the one it overrides.
    pub(super) fn fixture(&self, md: &'a Module, name: &str, f: Option<&'a Func>) -> Option<&'a Func> {
        let d = super::dirname(&md.file).to_string();
        let fm = self.file_mod(md);
        let mut scopes: Vec<String> = Vec::new();
        if !md.owner.is_empty() {
            scopes.push(md.name.clone());
        }
        scopes.push(fm.name.clone());
        for d2 in parents(&d) {
            let rel = if d2.is_empty() { "conftest.py".to_string() } else { format!("{d2}/conftest.py") };
            for m in self.g.by_file.get(&rel).map(|v| v.as_slice()).unwrap_or(&[]) {
                if self.m(m).owner.is_empty() {
                    scopes.push(m.clone());
                }
            }
        }
        for mn in scopes {
            let Some(m) = self.g.modules.get(&mn) else { continue };
            let Some(gf) = self.fx(&format!("{mn}.{name}")) else { continue };
            if f.is_some_and(|f| f.id == gf.id) {
                continue;
            }
            let from = gf.start.saturating_sub(4);
            for j in from..gf.start.saturating_sub(1) {
                if j < m.raw.len() && re!(r"\s*@(?:pytest\.)?fixture\b").match_start(&m.raw[j]).is_some() {
                    return Some(gf);
                }
            }
        }
        None
    }

    pub fn instance(&self, md: &'a Module, ty: &str, self_mods: Option<&[String]>) -> Option<Val> {
        if let Some(rest) = ty.strip_prefix("opt:") {
            // Rust `Option<Foo>` / `Result<Foo, E>`: Foo once unwrapped
            let inner = if rest.is_empty() { None } else { self.instance(md, rest, self_mods) };
            return Some(Val::Wrap(inner.map(Box::new)));
        }
        if ty == "Self" {
            return self_mods.filter(|s| !s.is_empty()).map(|s| Val::Inst(s.to_vec(), true));
        }
        if md.file.ends_with(".go") {
            return match self.go_type(md, ty) {
                None => Some(ext()),
                Some(mods) if !mods.is_empty() => Some(Val::Inst(mods, true)),
                Some(_) => None,
            };
        }
        if super::CS_KEYWORD_TYPES.contains(&ty) && ends(&md.file, &[".cs", ".java"]) {
            return Some(ext()); // `int n`, `string s`: the language's own
        }
        if ends(&md.file, C_EXTS) && !self.by_type.contains_key(ty) {
            self.cpp_aliases(); // (fills the set of aliases naming std / boost types)
            if self.cpp_std(ty) {
                return Some(ext()); // a project alias of a standard-library type
            }
        }
        let (mods, exact, declared) = self.type_modules(md, ty);
        let fm = self.file_mod(md);
        if !declared && ends(&md.file, JS_EXTS) && fm.imported.contains_key(ty) {
            // `const Koa = require('../lib/application')`; `new Koa()`
            let (w, n) = fm.imported[ty].clone();
            let dm = self.defining_module(&w, &n, 0);
            if !dm.is_empty() {
                let inner = format!("{dm}.{n}");
                return Some(Val::Inst(vec![if self.g.modules.contains_key(&inner) { inner } else { dm }], true));
            }
        }
        if !declared {
            if ends(&md.file, super::DYNAMIC) && self.extended.contains(ty) {
                // `image: Image` with `ImageExtended extends Image` in the project: that subclass or the
                // engine's own. A method no project subclass defines is the library's (core: an external
                // base); one a subclass defines stays a guess (not exact)
                let ext_ = crate::project::ext_of(&md.file);
                let subs: Vec<String> = self
                    .g
                    .modules
                    .iter()
                    .filter(|(_, m_)| m_.file.ends_with(ext_) && (m_.parent == ty || m_.lib_bases.iter().any(|b| b == ty)))
                    .map(|(mn, _)| mn.clone())
                    .collect();
                return if subs.is_empty() { None } else { Some(Val::Inst(subs, false)) };
            }
            let cond = upper1(ty)
                || ty.contains('.')
                || ends(&md.file, &[".rs", ".swift"])
                || (md.file.ends_with(".py") && super::PY_BUILTIN.contains(&ty));
            return if cond { Some(Val::Ext(vec![ty.to_string()])) } else { None };
        }
        Some(Val::Inst(mods, exact))
    }

    /// A Go type of the caller's package (or an imported one): its method
    /// modules; None when it is the standard library's / another module's.
    fn go_type(&self, md: &'a Module, ty: &str) -> Option<Vec<String>> {
        let fm = self.file_mod(md);
        let mut pkg = fm.package.clone();
        let mut ty = ty.to_string();
        if let Some((q, rest)) = ty.clone().split_once('.') {
            let target = fm.aliases.get(q).cloned().unwrap_or_default();
            let Some(pk) = target.strip_prefix("pkg:") else { return None };
            pkg = pk.to_string();
            ty = rest.to_string();
        }
        let mods: Vec<String> = self
            .by_type
            .get(&ty)
            .map(|v| v.iter().filter(|mn| self.pkg_of(mn) == pkg).cloned().collect())
            .unwrap_or_default();
        if !mods.is_empty() {
            return Some(mods);
        }
        if super::GO_BUILTIN.contains(&ty.as_str()) {
            return None;
        }
        let declared = self
            .g
            .packages
            .get(&pkg)
            .map(|v| v.iter().any(|mn| self.g.modules.get(mn).is_some_and(|m| m.decls.iter().any(|d| *d == ty))))
            .unwrap_or(false);
        if declared {
            Some(vec![])
        } else {
            None
        }
    }

    pub fn own_class(&self, md: &'a Module, f: Option<&'a Func>) -> Option<Vec<String>> {
        if let Some(f) = f {
            if !f.recv.is_empty() && md.file.ends_with(".go") {
                return Some(vec![md.name.clone()]);
            }
        }
        if !md.owner.is_empty() || matches!(md.kind.as_str(), "class" | "interface" | "ext") {
            return Some(vec![md.name.clone()]);
        }
        None
    }

    pub fn returns(&self, gf: &'a Func, self_mods: Option<&[String]>, depth: usize, targs: Option<&str>) -> Option<Val> {
        if depth > MAX || gf.returns.is_empty() {
            return None;
        }
        let gm = self.g.modules.get(&gf.module)?;
        let own = |s: Option<&[String]>| -> Option<Vec<String>> {
            s.filter(|x| !x.is_empty()).map(|x| x.to_vec()).or_else(|| self.own_class(gm, Some(gf)))
        };
        if let Some(e) = gf.returns.strip_prefix("expr:") {
            let sm = own(self_mods);
            return self.chain(gm, Some(gf), &split_chain(e), depth + 1, sm.as_deref(), None);
        }
        if let Some(e) = gf.returns.strip_prefix("callret:") {
            // `ReturnType<BodyRespond>`: its call signature's result
            let (mods, _, declared) = self.type_modules(gm, e);
            let got = if declared { self.field(&mods, "()", depth + 1) } else { None };
            return if got == Some(Val::Unknown) { None } else { got };
        }
        if gf.returns.starts_with("tuple:") {
            return None; // a tuple: its elements are typed through `let (a, b) =`
        }
        if let Some(e) = gf.returns.strip_prefix("byov:") {
            // C# overloads returning different types: the call's
            let mut ovs: Vec<(Vec<String>, String)> = e
                .split('|')
                .map(|x| {
                    let (a, b) = x.split_once('=').unwrap_or((x, ""));
                    (a.split('/').map(|s| s.to_string()).collect(), b.to_string())
                })
                .collect();
            let want = targs.unwrap_or("0").to_string();
            let narrowed: Vec<_> = ovs.iter().filter(|o| o.0.get(1) == Some(&want)).cloned().collect();
            ovs = if !narrowed.is_empty() {
                narrowed
            } else if targs.is_some() {
                vec![]
            } else {
                ovs
            };
            if ovs.iter().any(|o| !o.0[0].is_empty()) {
                // extension overloads: the one on the receiver's type
                let mut decls: Vec<String> = Vec::new();
                for mn in self_mods.unwrap_or(&[]) {
                    for a in self.ancestors(mn) {
                        let m = self.m(&a);
                        let ds: Vec<&String> =
                            if !m.owner.is_empty() || m.kind == "ext" { m.decls.iter().take(1).collect() } else { m.decls.iter().rev().take(1).collect() };
                        for d in ds {
                            if !decls.contains(d) {
                                decls.push(d.clone());
                            }
                        }
                    }
                }
                let extra: Vec<String> = decls.iter().map(|d| d.split('`').next().unwrap().to_string()).collect();
                decls.extend(extra);
                let kept: Vec<_> = ovs.iter().filter(|o| decls.contains(&o.0[0])).cloned().collect();
                ovs = if !kept.is_empty() {
                    kept
                } else if self_mods.is_some_and(|s| !s.is_empty()) {
                    vec![]
                } else {
                    ovs
                };
            }
            let mut tys: Vec<String> = Vec::new();
            for (_, t) in &ovs {
                if !tys.contains(t) {
                    tys.push(t.clone());
                }
            }
            if tys.len() != 1 || tys[0].is_empty() {
                return None;
            }
            let ty = tys.remove(0);
            if ty == "Self" {
                return own(self_mods).map(|m| Val::Inst(m, true));
            }
            return self.instance(gm, &ty, self_mods);
        }
        if gf.returns == "Self" {
            return own(self_mods).map(|m| Val::Inst(m, true));
        }
        self.instance(gm, &gf.returns, self_mods)
    }
}

fn parents(d: &str) -> Vec<String> {
    let mut out = vec![d.to_string()];
    let mut d = d.to_string();
    while !d.is_empty() {
        d = d.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default();
        out.push(d.clone());
    }
    out
}
