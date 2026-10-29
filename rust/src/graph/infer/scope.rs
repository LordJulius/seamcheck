//! core's scope helpers the type walk shares with the resolver: `_in_scope`,
//! `_defining_module`, `_scope`, the `_js_*` readers and `_cpp_namespaces`.

use std::collections::HashSet;

use super::{ends, esc, re, rxd, Types, JS_FILE_EXTS};
use crate::lang::chains::{self, Opts};
use crate::model::{Func, Module};

impl<'a> Types<'a> {
    /// `name` in a module, or in any file of a "pkg:" package. A module that
    /// only re-exports the name (an `index.ts` barrel, a package `__init__.py`)
    /// leads on to where it is defined (core._in_scope).
    pub fn in_scope(&self, where_: &str, name: &str, depth: usize) -> Vec<&'a Func> {
        let g = self.g;
        if let Some(pk) = where_.strip_prefix("pkg:") {
            return g.packages.get(pk).map(|ms| ms.iter().filter_map(|mn| g.lookup(mn, name)).collect()).unwrap_or_default();
        }
        if where_.starts_with("type:") {
            return Vec::new(); // a type is only a scope for method names (see scope)
        }
        if name == "default" && depth < 4 {
            if let Some(dm) = g.modules.get(where_) {
                let (w, n) = dm.imported.get("default").cloned().unwrap_or((String::new(), "default".into()));
                if n != "default" && dm.imported.contains_key("default") {
                    // `module.exports = class Application` beside a `static get default()`: the export, not that getter
                    return self.in_scope(&w, &n, depth + 1);
                }
            }
        }
        if let Some(t) = g.lookup(where_, name) {
            if !self.js_call_arg_member(t) && !self.js_member_assigned(t) {
                return vec![t];
            }
        }
        let Some(m) = g.modules.get(where_) else { return Vec::new() };
        if depth >= 8 {
            return Vec::new(); // (zod/mini -> external -> parse -> core/index -> core/parse -> a factory)
        }
        if let Some((w, n)) = m.imported.get(name) {
            let hits = self.in_scope(w, n, depth + 1);
            if !hits.is_empty() {
                return hits;
            }
        }
        for w in &m.reexports {
            let hits: Vec<&Func> = self.in_scope(w, name, depth + 1).into_iter().filter(|h| !self.js_class_member(h)).collect();
            if !hits.is_empty() {
                return hits;
            }
        }
        let held = m.bound.get(name).map(|s| s.as_str()).unwrap_or("");
        if ends(&m.file, JS_FILE_EXTS) && re!(r"^expr:[A-Za-z_$][\w$]*$").fullmatch(held).is_some() && &held[5..] != name {
            return self.in_scope(where_, &held[5..], depth + 1); // `export const __getDirectAgent = getDirectAgent`
        }
        if ends(&m.file, JS_FILE_EXTS) && m.owner.is_empty() {
            if let Some(f) = self.js_factory_made(m, name, depth) {
                return vec![f];
            }
        }
        Vec::new()
    }

    /// `export const safeParse: <T>(...) => R = core._safeParse(ZodRealError)` where the
    /// project's `_safeParse` is `(_Err) => (schema, value) => {...}` (or builds a closure
    /// and returns it): calling safeParse runs that closure, in the factory's body.
    fn js_factory_made(&self, m: &Module, name: &str, depth: usize) -> Option<&'a Func> {
        let key = (m.name.clone(), name.to_string());
        if let Some(h) = self.c.borrow().js_factory.get(&key) {
            return h.as_ref().and_then(|id| self.fx(id));
        }
        self.c.borrow_mut().js_factory.insert(key.clone(), None);
        let code = m.code.join("\n");
        let dm = rxd(&format!(r"(?m)^[ \t]*(?:export[ \t]+)?const[ \t]+{}\b", esc(name))).search(&code)?;
        let b = code.as_bytes();
        let (mut k, mut d, n) = (dm.end(), 0i32, code.len());
        while k < n {
            let c = b[k];
            if matches!(c, b'(' | b'[' | b'{' | b'<') {
                d += 1;
            } else if matches!(c, b')' | b']' | b'}' | b'>') && !(c == b'>' && b[k - 1] == b'=') {
                d -= 1;
            } else if c == b'='
                && d <= 0
                && !matches!(b.get(k + 1), Some(b'=') | Some(b'>'))
                && !matches!(b[k - 1], b'=' | b'!' | b'<' | b'>')
            {
                break;
            } else if c == b';' && d <= 0 {
                return None;
            }
            k += 1;
        }
        let ch = if k < n { chains::expression(&code, k + 1, 40, Opts::default()) } else { vec![] };
        if ch.is_empty() || !ch.last().unwrap().ends_with("()") || ch.len() > 2 {
            return None;
        }
        let fname = &ch.last().unwrap()[..ch.last().unwrap().len() - 2];
        let hits: Vec<&Func> = if ch.len() == 1 {
            self.in_scope(&m.name, fname, depth + 1)
        } else {
            let w = m
                .aliases
                .get(&ch[0])
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| m.imported.get(&ch[0]).map(|x| x.0.clone()))
                .unwrap_or_default();
            if self.g.modules.contains_key(&w) {
                self.in_scope(&w, fname, depth + 1)
            } else {
                vec![]
            }
        };
        if hits.len() != 1 || !ends(&hits[0].file, JS_FILE_EXTS) {
            return None;
        }
        let gf = hits[0];
        let gm = self.g.modules.get(&gf.module);
        let head = gm.map(|gm| {
            let a = gf.start - 1;
            gm.code[a.min(gm.code.len())..(gf.start + 1).min(gm.code.len())].join("\n")
        });
        let head = head.unwrap_or_default();
        let curried =
            re!(r"=>\s*(?:async\s+)?(?:\([^()]*\)|[A-Za-z_$][\w$]*)\s*(?::[^=;{]+)?=>").search(&head).is_some();
        let local = re!(r"^expr:([A-Za-z_$][\w$]*)$").fullmatch(&gf.returns).map(|c| c.s(1).to_string());
        let closure = local.as_ref().is_some_and(|l| {
            rxd(&format!(
                r"\b(?:const|let)\s+{}\s*(?::[^=;]+)?=\s*(?:async\s+)?(?:\([^()]*\)\s*(?::[^=;{{]+)?=>|function\b)",
                esc(l)
            ))
            .search(&gf.code)
            .is_some()
        });
        if curried || closure {
            self.c.borrow_mut().js_factory.insert(key, Some(gf.id.clone()));
            return Some(gf);
        }
        None
    }

    /// The module that defines the object a named import refers to
    /// (`import { API } from "./helpers"`, through re-exports): `API.create()`
    /// is that module's `create` (core._defining_module).
    pub fn defining_module(&self, where_: &str, name: &str, depth: usize) -> String {
        let g = self.g;
        let Some(m) = g.modules.get(where_) else { return String::new() };
        if depth > 4 {
            return String::new();
        }
        let sub = format!("{where_}.{name}");
        if g.modules.contains_key(&sub) {
            return sub; // a class with a module of its own
        }
        if m.decls.iter().any(|d| d == name) || g.funcs.contains_key(&sub) {
            return where_.to_string();
        }
        if let Some((w, n)) = m.imported.get(name) {
            return self.defining_module(w, n, depth + 1);
        }
        if let Some(a) = m.aliases.get(name) {
            if ends(&m.file, JS_FILE_EXTS) && g.modules.contains_key(a) {
                return a.clone(); // `import * as z from "./external.js"; export { z }`: that module's namespace
            }
        }
        for w in &m.reexports {
            let hit = self.defining_module(w, name, depth + 1);
            if !hit.is_empty() {
                return hit;
            }
        }
        if name == "default" && ends(&m.file, &[".js", ".cjs", ".mjs"]) && m.owner.is_empty() {
            return where_.to_string(); // `require('./context')` of `module.exports = { inspect() {...} }`
        }
        String::new()
    }

    /// The modules a file can see: itself, what it imports, its package (core._scope).
    pub fn scope(&self, m: &'a Module) -> HashSet<String> {
        let g = self.g;
        let fm = self.file_mod(m);
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
        // a file seen is all of it: its classes' modules too
        let files: HashSet<&str> = out.iter().filter_map(|w| g.modules.get(w)).map(|m| m.file.as_str()).collect();
        for f in files {
            out.extend(g.by_file.get(f).cloned().unwrap_or_default());
        }
        out
    }

    /// `inst.validate = (...) => ...` inside a function: a property of that object, not a
    /// name in the module (a CommonJS `exports.x = ...` / `module.exports.x = ...` is one).
    pub fn js_member_assigned(&self, gf: &Func) -> bool {
        let Some(m) = self.g.modules.get(&gf.module) else { return false };
        ends(&gf.file, JS_FILE_EXTS)
            && gf.start > 0
            && gf.start <= m.raw.len()
            && self.js_property_fn(gf)
            && re!(r"\s*(?:module\.)?exports\.").match_start(&m.raw[gf.start - 1]).is_none()
    }

    /// A method of the class a JS / TS file is named for (`doc.ts`'s `class Doc { compile() }`):
    /// `export * from "./doc.js"` hands on `Doc`, not its methods.
    pub fn js_class_member(&self, gf: &Func) -> bool {
        let Some(m) = self.g.modules.get(&gf.module) else { return false };
        m.kind == "class"
            && m.owner.is_empty()
            && ends(&gf.file, JS_FILE_EXTS)
            && gf.start > 0
            && gf.start <= m.raw.len()
            && m.raw[gf.start - 1].starts_with([' ', '\t'])
    }

    /// A JS method of an object literal handed to a call (`buildURL(url, params, {
    /// encode(v) {...} })`): a member of that object, never a name in the module's scope.
    pub fn js_call_arg_member(&self, gf: &Func) -> bool {
        if !ends(&gf.file, JS_FILE_EXTS) {
            return false;
        }
        if let Some(h) = self.c.borrow().js_arg_members.get(&gf.id) {
            return *h;
        }
        let m = self.g.modules.get(&gf.module);
        let mut got = false;
        if let Some(m) = m {
            if m.owner.is_empty()
                && m.kind.is_empty()
                && gf.start > 0
                && gf.start <= m.raw.len()
                && rxd(&format!(
                    r"\s+(?:async\s+)?\*?\s*{}\s*(?:\(|:\s*(?:async\s+)?function\b)",
                    esc(&gf.name)
                ))
                .match_start(&m.raw[gf.start - 1])
                .is_some()
            {
                let text = m.code[..gf.start - 1].join("\n");
                let b = text.as_bytes();
                let mut depth = 0i32;
                let mut k = text.len() as isize - 1;
                while k >= 0 {
                    let ch = b[k as usize];
                    if matches!(ch, b')' | b']' | b'}') {
                        depth += 1;
                    } else if matches!(ch, b'(' | b'[' | b'{') {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    k -= 1;
                }
                let before = if k >= 0 && b[k as usize] == b'{' { text[..k as usize].trim_end() } else { "" };
                got = before.ends_with(',') || before.ends_with('(');
            }
        }
        self.c.borrow_mut().js_arg_members.insert(gf.id.clone(), got);
        got
    }

    /// A JS function stored as a property (`req.setTimeout = () => {}`): never called bare.
    pub fn js_property_fn(&self, gf: &Func) -> bool {
        let Some(m) = self.g.modules.get(&gf.module) else { return false };
        gf.start > 0
            && gf.start <= m.raw.len()
            && ends(&gf.file, JS_FILE_EXTS)
            && rxd(&format!(r"^\s*[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*\.{}\s*=", esc(&gf.name)))
                .match_start(&m.raw[gf.start - 1])
                .is_some()
    }

    /// A JS/TS function declared on its own (`function close(s) {`, `const close = (s) =>`),
    /// not a class or object member, and never stored as a property or exported
    /// under its name in an object (`{ close }`, `x.close = close`).
    pub fn js_plain_function(&self, gf: &Func) -> bool {
        let Some(m) = self.g.modules.get(&gf.module) else { return false };
        if gf.start == 0 || gf.start > m.raw.len() {
            return false;
        }
        let n = esc(&gf.name);
        if rxd(&format!(
            r"^\s*(?:export\s+)?(?:default\s+)?(?:return\s+)?(?:async\s+)?(?:function\s*\*?\s*{n}\s*[(<]|(?:const|let|var)\s+{n}\s*=)"
        ))
        .match_start(&m.raw[gf.start - 1])
        .is_none()
        {
            return false;
        }
        if (!m.owner.is_empty() || !m.kind.is_empty())
            && !m.funcs.iter().any(|h| h.id != gf.id && h.kind != "top" && h.start < gf.start && gf.start <= h.end)
        {
            return false; // (a class's member; a `function setHeader(...)` inside one of its methods is a plain local)
        }
        if !self.c.borrow().js_stored.contains_key(&m.name) {
            let text = m.code.join("\n");
            let mut set: HashSet<String> = HashSet::new();
            for x in re!(r"[{,]\s*([A-Za-z_$][\w$]*)\s*(?=[,}])").find_iter(&text) {
                set.insert(x.s(1).to_string());
            }
            for x in re!(r"[.\w$\]]\s*=\s*([A-Za-z_$][\w$]*)\s*[;\n]").find_iter(&text) {
                set.insert(x.s(1).to_string());
            }
            for x in re!(r":\s*([A-Za-z_$][\w$]*)\s*[,}\n]").find_iter(&text) {
                set.insert(x.s(1).to_string());
            }
            self.c.borrow_mut().js_stored.insert(m.name.clone(), set);
        }
        !self.c.borrow().js_stored[&m.name].contains(&gf.name)
    }

    /// `Object.prototype.on = function () {` inside a test's body (a prototype-pollution
    /// check): a builtin's prototype patched while that function runs, out of reach of
    /// calls made anywhere else.
    pub fn js_scoped_patch(&self, gf: &Func, caller: &Func) -> bool {
        let Some(m) = self.g.modules.get(&gf.module) else { return false };
        if gf.start == 0
            || gf.start > m.raw.len()
            || rxd(&format!(
                r"^\s*(?:Object|Array|String|Function|Number|Boolean)\.prototype\.{}\s*=",
                esc(&gf.name)
            ))
            .match_start(&m.raw[gf.start - 1])
            .is_none()
        {
            return false;
        }
        let around: Vec<&Func> = self
            .g
            .funcs
            .keys()
            .filter_map(|id| self.fx(id))
            .filter(|h| h.file == gf.file && h.kind != "top" && h.id != gf.id && h.start < gf.start && gf.start <= h.end)
            .collect();
        !around.is_empty() && !around.iter().any(|h| caller.file == h.file && h.start <= caller.start && caller.end <= h.end)
    }

    /// Every namespace part a free function is declared in (core._cpp_namespaces).
    pub fn cpp_namespaces(&self) -> HashSet<String> {
        if let Some(h) = &self.c.borrow().cpp_ns {
            return h.clone();
        }
        let got: HashSet<String> = self
            .g
            .funcs
            .keys()
            .filter_map(|id| self.fx(id))
            .filter(|f| !f.ns.is_empty())
            .flat_map(|f| f.ns.split("::").map(|s| s.to_string()).collect::<Vec<_>>())
            .collect();
        self.c.borrow_mut().cpp_ns = Some(got.clone());
        got
    }
}
