//! The receiver-chain walk (infer.Types.chain / first / step).

use super::{
    ends, esc, ext, is_ext, lower1, re, rxd, upper1, Types, Val, C_EXTS, DYNAMIC, ES_MODULE, EXPLICIT_SELF, JS_EXTS,
    JS_FILE_EXTS, MAX, RB_EXTS,
};
use crate::lang::chains::UNWRAP;
use crate::model::{Func, Module};
use std::collections::HashSet;

const DOM_PROPS: &[&str] = &["classList", "style", "dataset"];
const DOM_QUERIES: &[&str] = &[
    "getElementsByTagName()", "getElementsByClassName()", "querySelectorAll()", "querySelector()", "getElementById()",
    "children", "childNodes", "closest()", "parentElement", "parentNode",
];
const ELEMENT_ACCESS: &[&str] = &[
    "[]", "at()", "first()", "last()", "value()", "get()", "front()", "back()", "constFirst()", "constLast()",
    "elementAt()", "getFirst()", "getLast()", "peek()", "poll()", "pop()", "find()", "detect()",
];
const SWIFT_SEQ_OPS: &[&str] = &[
    "map()", "filter()", "compactMap()", "flatMap()", "sorted()", "reversed()", "enumerated()", "joined()", "split()",
    "dropFirst()", "dropLast()", "prefix()", "suffix()", "lowercased()", "uppercased()", "trimmingCharacters()",
    "components()", "replacingOccurrences()",
];
const JS_BUILTINS: &[&str] = &[
    "Buffer", "URL", "URLSearchParams", "Promise", "Math", "JSON", "Object", "Array", "Date", "RegExp", "Map", "Set",
    "WeakMap", "WeakSet", "Error", "TypeError", "RangeError", "Symbol", "Reflect", "Intl", "Number", "String", "Boolean",
    "BigInt", "AbortController", "AbortSignal", "TextEncoder", "TextDecoder", "Uint8Array", "ArrayBuffer", "DataView",
    "Proxy", "Atomics", "WebAssembly", "Blob", "FormData", "ReadableStream", "WritableStream", "EventEmitter",
    "Function", "structuredClone",
];
const JS_GLOBALS: &[&str] = &[
    "console", "window", "document", "navigator", "localStorage", "sessionStorage", "globalThis", "location", "history",
    "process", "performance", "crypto", "vi", "jest", "expect", "cy",
];
const PY_BUILTIN_CALLS: &[&str] = &[
    "vars", "dict", "list", "set", "frozenset", "tuple", "str", "bytes", "bytearray", "sorted", "reversed", "enumerate",
    "zip", "map", "filter", "range", "iter", "repr", "format", "getattr", "type", "open", "globals", "locals", "dir",
    "int", "float", "bool", "len", "id", "hash",
];
const RS_PRIMS: &str = r"u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64|bool|char|str";

fn is_ident(s: &str) -> bool {
    re!(r"^[\w$]+$").is_match(s)
}

impl<'a> Types<'a> {
    /// `self_mods or self.own_class(mod, f)`
    fn self_or_own(&self, sm: Option<&[String]>, md: &'a Module, f: Option<&'a Func>) -> Option<Vec<String>> {
        match sm {
            Some(s) if !s.is_empty() => Some(s.to_vec()),
            _ => self.own_class(md, f),
        }
    }

    /// The value a receiver chain evaluates to: ("inst" | "static",
    /// modules, exact), EXT, or None (unknown). `start`: the first segment's
    /// value, when the caller has it.
    pub fn chain(
        &self,
        md: &'a Module,
        f: Option<&'a Func>,
        chain: &[String],
        depth: usize,
        self_mods: Option<&[String]>,
        start: Option<Val>,
    ) -> Option<Val> {
        if chain.is_empty() || depth > MAX {
            return None;
        }
        let mut chain: &[String] = chain;
        if md.file.ends_with(".rs") && chain.len() > 1 {
            // `grep::printer::StandardBuilder::new()`: a path ending at a type the project declares starts there
            let k = chain[..chain.len() - 1]
                .iter()
                .enumerate()
                .filter(|(_, sg)| upper1(sg) && self.by_type.contains_key(*sg))
                .map(|(i, _)| i)
                .max()
                .unwrap_or(0);
            let path_: Vec<&String> = chain[..k].iter().filter(|sg| !matches!(sg.as_str(), "crate" | "self" | "super")).collect();
            let cands_: Vec<String> = if k > 0 { self.by_type.get(&chain[k]).cloned().unwrap_or_default() } else { vec![] };
            if !path_.is_empty() && cands_.len() > 1 && start.is_none() {
                // `grep::regex::RegexMatcherBuilder`: of the crates declaring it, the one its path names
                let sg = path_[path_.len() - 1];
                let by_dir: Vec<&String> = cands_
                    .iter()
                    .filter(|mn| {
                        let f = format!("/{}", self.m(mn).file);
                        f.contains(&format!("/{sg}/")) || f.contains(&format!("/{}/", sg.replace("grep_", "")))
                    })
                    .collect();
                if by_dir.len() == 1 {
                    return self.chain(md, f, &chain[k..], depth + 1, self_mods, Some(Val::Static(vec![by_dir[0].clone()], true)));
                }
            }
            chain = &chain[k..];
        }
        if chain.len() > 1 && DOM_PROPS.contains(&chain[chain.len() - 1].as_str()) && ends(&md.file, JS_EXTS) {
            return Some(ext()); // `el.classList`, `el.style`: the DOM's, whatever `el` is
        }
        if start.is_none()
            && chain.len() > 1
            && (chain[0] == "window" || chain[0] == "globalThis")
            && ends(&md.file, JS_EXTS)
            && !f.is_some_and(|f| f.bound.contains_key(&chain[0]))
        {
            // `window.h`: a member the project adds to the global Window (`declare global { interface Window { h: ... } }`)
            let wins: Vec<String> = self
                .by_type
                .get("Window")
                .map(|v| v.iter().filter(|mn| self.m(mn).kind == "interface").cloned().collect())
                .unwrap_or_default();
            let got = if wins.is_empty() { None } else { self.field(&wins, &chain[1], depth) };
            if let Some(got) = got {
                if got != Val::Unknown {
                    return self.chain(md, f, &chain[1..], depth + 1, self_mods, Some(got));
                }
            }
        }
        let mut cur: Option<Val> = match start {
            Some(s) => Some(s),
            None => self.first(md, f, &chain[0], depth, self_mods),
        };
        let mut before: Option<Val> = None; // the value the previous segment was read from
        for k in 1..chain.len() {
            let seg = chain[k].as_str();
            let prev = if md.file.ends_with(".rb") && chain[k - 1].ends_with("()") {
                &chain[k - 1][..chain[k - 1].len() - 2]
            } else {
                &chain[k - 1][..]
            };
            if ELEMENT_ACCESS.contains(&seg) && re!(r"^[@\w$]+$").is_match(prev) {
                // `m_players[id]`, `xs.get(0)`, `list.first()`: an element of a declared container
                // (Ruby: `site.collections["x"]`, the reader's `@collections`)
                let owner = if k > 1 { before.as_ref() } else { None };
                if let Some(el) = self.element(md, f, prev, owner, depth) {
                    before = cur;
                    cur = Some(el);
                    continue;
                }
            }
            if ELEMENT_ACCESS.contains(&seg) && seg != "[]" {
                if let Some(Val::Inst(cm, _)) = &cur {
                    if self.method(cm, &seg[..seg.len() - 2], None).0.is_empty() {
                        if let Some(el_t) = self.collection_element(cm) {
                            // `els.get(0)` on `class Elements extends Nodes<Element>`: an Element
                            let nv = self.instance(self.m(&el_t.0), &el_t.1, None);
                            before = cur;
                            cur = nv;
                            continue;
                        }
                    }
                }
            }
            before = cur.clone();
            if cur.is_none()
                && DOM_QUERIES.contains(&seg)
                && seg.ends_with("()")
                && ends(&md.file, JS_FILE_EXTS)
                && self.g.by_name.get(&seg[..seg.len() - 2]).map_or(true, |v| v.is_empty())
            {
                cur = Some(ext()); // `el.getElementsByTagName('a')`: whatever el is, that finds DOM nodes
                continue;
            }
            if cur.is_none()
                && SWIFT_SEQ_OPS.contains(&seg)
                && md.file.ends_with(".swift")
                && !self.g.by_name.get(&seg[..seg.len() - 2]).is_some_and(|v| v.iter().any(|id| self.fx(id).is_some_and(|g| g.file.ends_with(".swift"))))
            {
                cur = Some(ext()); // `xs.map { ... }`: whatever xs is, the standard library's Array
                continue;
            }
            if cur.is_none() || is_ext(&cur) {
                if cur.is_some() {
                    let from = chain.iter().position(|x| x == seg).unwrap_or(0);
                    let dom = ends(&md.file, JS_FILE_EXTS) && chain.iter().any(|s| DOM_QUERIES.contains(&s.as_str()));
                    if chain[from..].iter().any(|s| s == "[]") && !dom {
                        return None; // an element of a library collection (`shapes[0]`): could be a project object
                    }
                }
                return if cur.is_none() { None } else { Some(ext()) };
            }
            cur = self.step(md, cur.as_ref().unwrap(), seg, depth);
        }
        cur
    }

    pub fn first(&self, md: &'a Module, f: Option<&'a Func>, seg: &str, depth: usize, self_mods: Option<&[String]>) -> Option<Val> {
        let p = self.g;
        let fm = self.file_mod(md);
        if seg == "new this()" && ends(&fm.file, JS_EXTS) {
            if let Some(own_) = self.self_or_own(self_mods, md, f) {
                return Some(Val::Inst(own_, true)); // `new this(first)` in a static method: an instance of the class
            }
        }
        if matches!(seg, "this" | "self" | "$this" | "Self" | "static") {
            let own = self.self_or_own(self_mods, md, f);
            if own.is_none() {
                if let Some(h) = f.and_then(|f| f.bound.get(seg)).filter(|h| h.starts_with("mod:")) {
                    return self.held(md, f, h, depth + 1, None); // a Python closure's captured `self`
                }
            }
            let own = own?;
            return Some(if seg == "Self" || seg == "static" { Val::Static(own, true) } else { Val::Inst(own, true) });
        }
        if fm.file.ends_with(".rs") && (seg == "super" || seg == "crate") {
            // (Rust's `self` is the receiver: `self.x()`)
            if seg == "super" {
                if let Some(f) = f {
                    let upto = fm.raw[..f.start.min(fm.raw.len())].join("\n");
                    if re!(r"(?m)^\s*(?:pub\s+)?mod\s+\w+\s*\{").search(&upto).is_some() {
                        return Some(Val::Static(vec![fm.name.clone()], true)); // `super::shell_quote()` in an inline `mod tests`
                    }
                }
            }
            return None;
        }
        if fm.file.ends_with(".rs") && rxd(&format!("^(?:{RS_PRIMS})$")).is_match(seg) {
            return Some(ext()); // `u32::from(c)`: a primitive's
        }
        let fext = crate::project::ext_of(&fm.file);
        if seg == "super" || (seg == "base" && fext == ".cs") || (seg == "parent" && fext == ".php") || (seg == "super()" && fext == ".py") {
            // (C#'s `base`, PHP's `parent::`, Python's `super().x()`; elsewhere `base` is a name)
            let own = self.own_class(md, f)?;
            // (the next ancestor that isn't another part of the same type: a C# partial class's other file)
            let prim = self.primary(&own[0]);
            let sup: Vec<String> = self.ancestors(&own[0]).into_iter().skip(1).filter(|b| self.primary(b) != prim).take(1).collect();
            return if !sup.is_empty() {
                Some(Val::Inst(sup, true))
            } else if self.external_base(&own[0]) {
                Some(ext())
            } else {
                None
            };
        }
        if ends(&fm.file, C_EXTS) && !f.is_some_and(|f| f.bound.contains_key(seg)) && !fm.bound.contains_key(seg) {
            if seg == "std" || seg == "boost" {
                return Some(ext()); // (before a project file named std.h)
            }
            if self.cpp_namespaces().contains(seg) && !self.by_type.contains_key(seg) {
                return Some(Val::Ns); // `details::registry::instance()`: a project namespace, then its class
            }
        }
        if seg == "typeof()" && fm.file.ends_with(".cs") {
            return Some(Val::Ext(vec!["Type".into()])); // `typeof(X).ShouldBeThrownBy(...)`
        }
        if let Some(r) = seg.strip_prefix("new ") {
            return self.instance(md, &r[..r.len().saturating_sub(2)], self_mods);
        }
        if (seg.starts_with('$') || seg.starts_with('%')) && fm.file.ends_with(".gd") {
            return self.scene_node(&fm.file, &seg[1..]); // `$HUD`: the node the script's scene names so
        }
        if seg.starts_with("<as:") {
            return self.instance(md, &seg[4..seg.len() - 1], self_mods); // `(x as Foo)`: a Foo
        }
        if seg == "described_class()" && ends(&fm.file, RB_EXTS) {
            // RSpec: the class the (outermost) `describe Foo do` names
            let raw = fm.raw.join("\n");
            return match re!(r"(?m)^\s*(?:RSpec\.)?describe\s*\(?\s*((?:::)?[A-Z][\w:]*)").search(&raw) {
                Some(dm) => self.chain(fm, None, &[dm.s(1).trim_start_matches(':').to_string()], depth + 1, None, None),
                None => None,
            };
        }
        {
            let head = seg.split('(').next().unwrap_or("");
            let head = head.strip_prefix("new ").unwrap_or(head);
            let head0 = seg.split('(').next().unwrap_or("");
            if JS_BUILTINS.contains(&head)
                && ends(&fm.file, JS_EXTS)
                && !self.by_type.contains_key(head)
                && !fm.imported.contains_key(head0)
                && !f.is_some_and(|f| f.bound.contains_key(seg))
            {
                return Some(ext()); // `Buffer.from(x)`, `new URL(u)`, `JSON.parse(s)`: the runtime's
            }
        }
        if seg.ends_with("!()") && fm.file.ends_with(".rs") {
            return self.rs_macro_value(fm, &seg[..seg.len() - 3]);
        }
        if seg.ends_with("()") {
            let name = &seg[..seg.len() - 2];
            let held = f.and_then(|f| f.bound.get(name)).cloned().unwrap_or_default();
            if let Some(h) = held.strip_prefix("call:") {
                // a closure: calling it gives its body's value
                return self.chain(md, f, &h.split('\x1f').map(|x| x.to_string()).collect::<Vec<_>>(), depth + 1, self_mods, None);
            }
            if !held.is_empty() && !held.starts_with("lam:") && ends(&fm.file, JS_EXTS) && depth < MAX {
                // `api(url)` with `api: KyInstance`: its call signature's
                if let Some(Val::Inst(mods, _)) = self.held(md, f, &held, depth + 1, self_mods) {
                    if let Some(got) = self.field(&mods, "()", depth + 1) {
                        if got != Val::Unknown {
                            return Some(got);
                        }
                    }
                }
            }
            if self.lib_results().contains(name) {
                return Some(ext()); // `hc<App>(url)`: a runtime proxy (packs' [[returns]])
            }
            let mut gf = self.bare(md, f, name);
            let cpp_ = ends(&fm.file, &[".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx"]);
            if (upper1(name) || (cpp_ && gf.is_none() && self.type_modules(md, name).2))
                && gf.map_or(true, |g| g.returns.is_empty() && g.name == name)
            {
                if let Some(got) = self.instance(md, name, self_mods) {
                    return Some(got); // `ConfigHandler().value()`: a constructor (ahead of a library base's own methods)
                }
            }
            if gf.is_none() && !fm.file.ends_with(".py") {
                if let Some(own) = self.own_class(md, f) {
                    let hits = self.method(&own, name, None).0;
                    if hits.len() > 1
                        && hits.iter().map(|h| h.returns.as_str()).collect::<HashSet<_>>().len() == 1
                        && !matches!(hits[0].returns.as_str(), "" | "Self")
                        && !hits[0].returns.starts_with("expr:")
                        && !hits[0].returns.starts_with("byov:")
                    {
                        gf = Some(hits[0]); // `parse(url)`, open with overrides: each returns the one declared type
                    }
                }
            }
            if let Some(g) = gf {
                let got = self.returns(g, None, depth + 1, None);
                return if got.is_some() { got } else { self.rb_named(md, name) };
            }
            if md.file.ends_with(".rb") {
                let own = self.own_class(md, f).unwrap_or_default();
                let mut got: Vec<Option<Val>> = Vec::new();
                for h in self.method(&own, name, None).0 {
                    let r = self.returns(h, Some(&[h.module.clone()]), depth + 1, None);
                    if !got.contains(&r) {
                        got.push(r);
                    }
                }
                if got.len() == 1 {
                    return got.pop().unwrap(); // Ruby: a mixin's `site` that every includer's `site` returns alike
                }
            }
            if fm.external.contains(name) || (fm.file.ends_with(".py") && PY_BUILTIN_CALLS.contains(&name)) {
                return Some(ext()); // (Python's `vars(x)`, `sorted(x)`: the language's own values)
            }
            let own = self.own_class(md, f);
            if let Some(own) = &own {
                if !fm.file.ends_with(".py") && own.iter().any(|o| self.external_base(o)) && self.method(own, name, None).0.is_empty() {
                    return Some(ext()); // inherited from a library supertype (Qt's `sourceModel()`)
                }
            }
            if upper1(name) {
                return self.instance(md, name, self_mods); // `Foo(...)`: a constructor
            }
            if ends(&fm.file, JS_FILE_EXTS)
                && self.es_module(&fm.file)
                && !fm.imported.contains_key(name)
                && !f.is_some_and(|f| f.bound.contains_key(name))
                && !self.g.by_name.get(name).is_some_and(|v| {
                    v.iter().any(|id| self.fx(id).is_some_and(|g| g.file == fm.file || self.m(&g.module).owner.is_empty()))
                })
            {
                // `fetch(url)` in an ES module: not its own, not imported, and no project module exports
                // a function of that name (a lost import stays a guess): the runtime's global
                return Some(ext());
            }
            let got = self.rb_named(md, name);
            if got.is_none()
                && ends(&fm.file, RB_EXTS)
                && !fm.imported.contains_key(name)
                && !f.is_some_and(|f| f.bound.contains_key(name))
                && self.method(&self.own_class(md, f).unwrap_or_default(), name, None).0.is_empty()
                && self.g.by_name.get(&format!("{name}=")).map_or(true, |v| v.is_empty())
                && self.bare_reaches_nothing(fm, name)
            {
                return Some(ext()); // Ruby: nothing in scope has that name, so it is a library's
            }
            return got;
        }
        if seg == "<literal>" {
            return Some(ext()); // a string / array / number literal: the language's own type
        }
        if seg == "<range>" {
            return Some(Val::Ext(vec!["Range".into()]));
        }
        if matches!(seg, "(...)" | "[]" | "?" | "!!") {
            return None;
        }
        if fm.bound.get(seg).map(|s| s.as_str()) == Some("qmlns:") {
            return Some(Val::Ns);
        }
        if let Some(f) = f {
            let line = self.line.get();
            if line != 0 {
                if let Some(h) = f.bound.get(&format!("{seg}@{line}")) {
                    // a binding of that line only (a match arm)
                    return self.held(md, Some(f), h, depth + 1, self_mods);
                }
            }
        }
        if let Some(f) = f {
            if !f.bound.contains_key(seg)
                && ends(&fm.file, JS_EXTS)
                && rxd(&format!(r"[^{{]*?\(\s*(?:[^()]*,\s*)?\.\.\.{}\s*(?::[^()]*)?\)", esc(seg))).match_start(&f.code).is_some()
            {
                return Some(ext()); // `concat(first, ...targets)`: a rest parameter is an Array
            }
        }
        if let Some(f) = f {
            let mut held: Option<String> = f.bound.get(seg).cloned();
            let line = self.line.get();
            let pre = format!("{seg}\0from");
            if line != 0 && ends(&fm.file, JS_EXTS) && f.bound.keys().any(|k| k.starts_with(&pre)) {
                // reassigned at the declaration's level before this line: the latest such value
                let mut after: Vec<(i64, String)> = f
                    .bound
                    .iter()
                    .filter(|(k, _)| k.starts_with(&pre))
                    .map(|(k, v)| (k.split("\0from").nth(1).and_then(|n| n.parse().ok()).unwrap_or(0), v.clone()))
                    .filter(|(n, _)| (*n as usize) < line)
                    .collect();
                if !after.is_empty() {
                    after.sort();
                    held = after.pop().map(|x| x.1);
                }
            }
            if let Some(h) = &held {
                if h.starts_with("lam:")
                    && line != 0
                    && ends(&fm.file, JS_EXTS)
                    && (fm.imported.contains_key(seg) || fm.bound.contains_key(seg) || fm.aliases.contains_key(seg))
                    && f.bound.keys().any(|k| k.starts_with(&format!("{seg}@")))
                {
                    held = None; // `run((axios) => ...)`: that parameter is on the lambda's lines; here, the import
                }
            }
            let expr_self = held.as_ref().is_some_and(|h| h.starts_with("expr:") && h[5..].split('\x1f').next() == Some(seg));
            if expr_self
                && line != 0
                && line <= fm.raw.len()
                && rxd(&format!(r"\b{0}\s*=\s*(?:try[!?]?\s+|await\s+)*{0}\b", esc(seg))).search(&fm.raw[line - 1]).is_some()
            {
                held = None; // the rebinding's own line: `guard let x = x.read()` reads the outer x
            } else if expr_self {
                // `client = client.newBuilder().build()`: rebuilt from the field (or outer
                // value) of that name, so the chain starts there
                let sm = self.self_or_own(self_mods, md, Some(f));
                let base = self.first(md, None, seg, depth + 1, sm.as_deref());
                base.as_ref()?;
                let ch: Vec<String> = held.unwrap()[5..].split('\x1f').map(|x| x.to_string()).collect();
                return self.chain(md, Some(f), &ch, depth + 1, self_mods, base);
            }
            if let Some(h) = held.filter(|h| !h.is_empty()) {
                let got = self.held(md, Some(f), &h, depth + 1, self_mods);
                if got.is_some() || !ends(&fm.file, RB_EXTS) {
                    return got;
                }
                // (Ruby: a value we can't type says nothing; the name may still)
            }
        }
        // a bare name is a field only where `this` is implicit: never in Python,
        // JS/TS or PHP (`cli` in a Flask method is the module, not self.cli)
        let own = if ends(&fm.file, EXPLICIT_SELF) { None } else { self.self_or_own(self_mods, md, f) };
        if let Some(own) = own {
            let got = self.field(&own, seg, depth);
            if got == Some(Val::Unknown) {
                return None;
            }
            if got.is_some() {
                return got;
            }
        }
        let mut held = md.bound.get(seg).filter(|s| !s.is_empty()).or_else(|| fm.bound.get(seg)).cloned().unwrap_or_default();
        if held == format!("expr:{seg}") {
            held.clear(); // a file-named JS class's `this.context = Object.create(context)`: the import
        }
        if !held.is_empty() && !held.starts_with("group:") {
            return self.held(fm, None, &held, depth + 1, None);
        }
        if fm.file.ends_with(".go") && !fm.package.is_empty() {
            // a package-level var another file of the package declares
            if let Some(pm) = self.go_package_var(&fm.package, seg) {
                return self.held(pm, None, &pm.bound[seg], depth + 1, None);
            }
        }
        if fm.external.contains(seg) {
            return Some(ext());
        }
        if upper1(seg) && self.by_type.contains_key(seg) && !fm.file.ends_with(".py") {
            // a type the project declares, whatever module the import names
            // (Rust's `use grep_searcher::SearcherBuilder` names the crate root that re-exports it)
            let (mods, exact, declared) = self.type_modules(md, seg);
            if declared {
                return Some(Val::Static(mods, exact));
            }
        }
        if fm.aliases.contains_key(seg) && fm.imported.contains_key(seg) && ends(&fm.file, JS_EXTS) {
            let (w, n) = fm.imported[seg].clone();
            if let Some((vm, vn)) = self.var_module(&w, &n, 0) {
                return self.held(vm, None, &vm.bound[&vn], depth + 1, None); // `import axios from './index'`
            }
        }
        if let Some(target) = fm.aliases.get(seg) {
            if let Some(pk) = target.strip_prefix("pkg:") {
                return Some(Val::Pkg(vec![pk.to_string()]));
            }
            if fm.file.ends_with(".rs") {
                if let Some(tm) = p.modules.get(target) {
                    if tm.bound.contains_key(seg) {
                        // `use crate::directories::PROJECT_DIRS;`: the static there, not its module
                        return self.held(tm, None, &tm.bound[seg], depth + 1, None);
                    }
                }
            }
            if p.modules.contains_key(target) {
                return Some(Val::Static(vec![target.clone()], true));
            }
        }
        if fm.file.ends_with(".py") && !f.is_some_and(|f| f.bound.contains_key(seg)) && !fm.bound.contains_key(seg) {
            // a function of this file used as a value: one defined inside the function first
            // (`@click.group()\n def cli():` in a test), else the module's own
            let defs: Vec<&Func> = fm.funcs.iter().filter(|g| g.name == seg && g.kind != "top").collect();
            let inner: Vec<&&Func> = defs.iter().filter(|g| f.is_some_and(|f| f.id != g.id && f.start < g.start && g.start <= f.end)).collect();
            let outer: Vec<&&Func> = defs
                .iter()
                .filter(|g| !fm.funcs.iter().any(|o| o.id != g.id && o.kind != "top" && o.start < g.start && g.start <= o.end))
                .collect();
            let pick: Vec<&Func> = if !inner.is_empty() {
                vec![**inner.last().unwrap()]
            } else if outer.len() == 1 {
                vec![*outer[0]]
            } else {
                vec![]
            };
            if inner.is_empty() && depth < MAX {
                if let Some(f) = f.filter(|f| f.kind != "top") {
                    // a decorated def inside this function the parser kept as part of it
                    let raw_ = fm.raw[(f.start - 1).min(fm.raw.len())..f.end.min(fm.raw.len())].join("\n");
                    if let Some(dm_) = rxd(&format!(
                        r"(?m)^[ \t]*@([^\n]+)\n(?:[ \t]*@[^\n]+\n)*[ \t]*(?:async[ \t]+)?def[ \t]+{}[ \t]*\(",
                        esc(seg)
                    ))
                    .search(&raw_)
                    {
                        let ch_ = crate::lang::chains::expression(&format!("{} ", dm_.s(1).trim()), 0, 40, Default::default());
                        if !ch_.is_empty()
                            && ch_.last().unwrap().ends_with("()")
                            && !fm.external.contains(ch_[0].split('(').next().unwrap_or(""))
                        {
                            let got_ = self.chain(fm, None, &ch_, depth + 1, None, None);
                            if let Some(g @ Val::Inst(..)) = got_ {
                                return Some(g);
                            }
                        }
                    }
                }
            }
            if !pick.is_empty() && !fm.imported.contains_key(seg) {
                return self.py_function_value(pick[0]);
            }
        }
        if let Some((w, n)) = fm.imported.get(seg) {
            let (w, n) = (w.clone(), n.clone());
            if let Some((vm, vn)) = self.var_module(&w, &n, 0) {
                // an imported module-level variable (`from flask import current_app`)
                return self.held(vm, None, &vm.bound[&vn], depth + 1, None);
            }
            let dm = self.defining_module(&w, &n, 0);
            if !dm.is_empty() && fm.file.ends_with(".py") {
                if let Some(func) = self.fx(&format!("{dm}.{n}")) {
                    return self.py_function_value(func);
                }
            }
            if !dm.is_empty() {
                let inner = format!("{dm}.{n}");
                return Some(Val::Static(vec![if p.modules.contains_key(&inner) { inner } else { dm }], true));
            }
        }
        if upper1(seg) || self.by_type.contains_key(seg) {
            let (mods, exact, declared) = self.type_modules(md, seg);
            if declared {
                return Some(Val::Static(mods, exact));
            }
        }
        if p.modules.contains_key(seg) {
            return Some(Val::Static(vec![seg.to_string()], true));
        }
        let mut short: Option<Vec<String>> = if !(lower1(seg) && ends(&fm.file, &[".rb", ".rake", ".ru", ".py", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"])) {
            p.short.get(seg).cloned()
        } else {
            None
        };
        if let Some(s) = &mut short {
            // (a Swift file's `LSNocilla` is not the Objective-C header of that name)
            s.retain(|mn| crate::graph::link::shares_family(&self.m(mn).file, &fm.file));
        }
        if let Some(s) = short.filter(|s| !s.is_empty()) {
            // (a lowercase Ruby / JS name is a value, never the file `site.rb` / `app.js`: those come by import)
            let one = s.len() == 1;
            return Some(Val::Static(s, one));
        }
        if fm.file.ends_with(".gd") {
            if let Some(al) = self.autoload(&fm.file, seg) {
                return Some(Val::Inst(vec![al], true)); // project.godot's [autoload] GameState="*res://gamestate.gd"
            }
        }
        if upper1(seg) && !fm.file.ends_with(".ex") && !fm.file.ends_with(".exs") {
            return Some(ext()); // a type or module the project doesn't declare (GDScript: the engine's class)
        }
        if fm.file.ends_with(".qml") && !self.qml_names.contains(seg) && !matches!(seg, "parent" | "model" | "modelData" | "index") {
            return Some(ext()); // a name no project QML file declares: the window's / framework's
        }
        if fm.file.ends_with(".rs") && self.rs_external().contains(seg) {
            return Some(ext()); // `std::...`, `serde_json::from_str`: a crate the project depends on
        }
        if ends(&fm.file, C_EXTS) && !self.by_type.contains_key(seg) && self.cpp_std_known(seg) {
            return Some(ext()); // `log_clock::now()` with `using log_clock = std::chrono::system_clock;`
        }
        if (seg == "std" || seg == "boost") && ends(&fm.file, &[".cpp", ".cc", ".cxx", ".h", ".hpp", ".hh", ".c"]) {
            return Some(ext()); // `std::unique_ptr<X509>(...)`: the standard library's
        }
        if fm.file.ends_with(".cs") && super::CS_KEYWORD_TYPES.contains(&seg) && !f.is_some_and(|f| f.bound.contains_key(seg)) {
            return Some(ext()); // `string.Join(...)`, `object.ReferenceEquals(a, b)`: System's
        }
        if fm.file.ends_with(".py") && crate::graph::pylink::is_py_builtin(seg) {
            return Some(ext()); // `int.__new__`, `object.__repr__`: Python's own
        }
        if JS_GLOBALS.contains(&seg) && ends(&fm.file, JS_EXTS) {
            return Some(ext()); // `console`, `window`, `document`: the runtime's (no project name took it)
        }
        self.rb_named(md, seg)
    }

    fn cpp_std_known(&self, seg: &str) -> bool {
        self.cpp_aliases();
        self.cpp_std(seg)
    }

    /// Ruby: nothing in scope has that name (core says so of the call too).
    fn bare_reaches_nothing(&self, fm: &'a Module, name: &str) -> bool {
        self.in_scope(&fm.name, name, 0).is_empty() && !fm.uses.iter().any(|w| !self.in_scope(w, name, 0).is_empty())
    }

    /// JS `delegate(proto, 'response').method(name)`: the value of the
    /// property a member is forwarded to, or None.
    pub fn forward(&self, md: &'a Module, cur: &Val, name: &str, depth: usize) -> Option<Val> {
        for mn in cur.mods() {
            for a in self.ancestors(mn) {
                let h = self.m(&a).bound.get(name).cloned().unwrap_or_default();
                if let Some(d) = h.strip_prefix("deleg:") {
                    let got = self.step(md, cur, d, depth + 1);
                    return got.filter(|g| g.is_inst_or_static());
                }
            }
        }
        None
    }

    pub fn step(&self, md: &'a Module, cur: &Val, seg: &str, depth: usize) -> Option<Val> {
        let mut cur = cur.clone();
        if matches!(cur, Val::Static(..) | Val::Inst(..)) && ends(&md.file, JS_EXTS) && depth < MAX {
            let name = seg.strip_suffix("()").unwrap_or(seg);
            if let Some(fw) = self.forward(md, &cur, name, depth) {
                cur = fw;
            }
        }
        let kind = cur.kind();
        let exact = cur.exact();
        let mods: Vec<String> = cur.mods().to_vec();
        let mut seg = seg;
        if seg.starts_with("new ") && seg.ends_with("()") {
            seg = &seg[4..]; // `new express.Router()`: the member, constructed
        }
        if let Val::Wrap(inner) = &cur {
            // a Rust Option / Result: unwrapping gives the value, any other method is std's
            if UNWRAP.contains(&seg) || matches!(seg, "unwrap_unchecked()" | "unwrap_or_else()" | "unwrap_or_default()") {
                return inner.as_ref().map(|b| (**b).clone());
            }
            if matches!(seg, "as_ref()" | "as_mut()" | "as_deref()" | "as_deref_mut()" | "cloned()" | "copied()" | "clone()" | "take()" | "ok()" | "by_ref()") {
                return Some(cur);
            }
            return Some(ext());
        }
        if seg == "?" || seg == "!!" {
            return Some(cur);
        }
        if seg == "[]" || seg == "(...)" {
            return None;
        }
        if seg == "constructor" && kind == "inst" && ends(&md.file, JS_EXTS) {
            return Some(Val::Static(mods, exact)); // `headers.constructor.accessor(...)`: the class itself
        }
        if kind == "ns" {
            // a QML module's namespace: `KDEConnect.Type`
            let name = seg.strip_suffix("()").unwrap_or(seg);
            if ends(&md.file, C_EXTS) && self.cpp_namespaces().contains(name) && !self.by_type.contains_key(name) && !seg.ends_with("()") {
                return Some(cur); // `fmt::detail::`: a namespace inside a namespace
            }
            let (mods_, ex, declared) = self.type_modules(md, name);
            return if declared { Some(Val::Static(mods_, ex)) } else { Some(ext()) };
        }
        if kind == "pkg" {
            if let Some(n) = seg.strip_suffix("()") {
                let hits = self.in_scope(&format!("pkg:{}", mods[0]), n, 0);
                return if hits.len() == 1 { self.returns(hits[0], None, depth + 1, None) } else { None };
            }
            let got: Vec<String> =
                self.by_type.get(seg).map(|v| v.iter().filter(|mn| self.pkg_of(mn) == mods[0]).cloned().collect()).unwrap_or_default();
            return if got.is_empty() { None } else { Some(Val::Static(got, true)) };
        }
        if seg.ends_with("{}") && ends(&md.file, RB_EXTS) {
            // Ruby `x.redis { |conn| }`: what the project method redis yields to its block
            let n = &seg[..seg.len() - 2];
            let mut hits = self.method(&mods, n, None).0;
            if kind == "static" {
                let mut h: Vec<&Func> = Vec::new();
                for mn in &mods {
                    h.extend(self.in_scope(mn, n, 0));
                }
                if !h.is_empty() {
                    hits = h;
                }
            }
            let mut got: Vec<Option<Val>> = Vec::new();
            for h in hits {
                let r = self.yielded(h, depth + 1);
                if !got.contains(&r) {
                    got.push(r);
                }
            }
            return if got.len() == 1 { got.pop().unwrap() } else { None };
        }
        if seg.ends_with("()") {
            let mut name = &seg[..seg.len() - 2];
            let mut targs: Option<&str> = None;
            if let Some((n, t)) = name.split_once('`') {
                // C# `Handle<E>()`: the overloads taking one type argument
                name = n;
                targs = Some(t);
            }
            let mut hits: Vec<&Func>;
            if kind == "static" {
                let mut nested: Vec<String> = mods
                    .iter()
                    .map(|mn| format!("{mn}.{name}"))
                    .filter(|x| self.g.modules.contains_key(x) && self.nested(x))
                    .collect();
                if nested.is_empty() {
                    nested = self.py_classes(&mods, name); // (not the file `Pool.Shared.cs` beside `Pool.cs`)
                }
                if !nested.is_empty() {
                    // `Request.Builder()`: builds the nested class (`httpx.Client()`: the class the package exports)
                    let one = nested.len() == 1;
                    return Some(Val::Inst(nested, one));
                }
                hits = Vec::new();
                for mn in &mods {
                    hits.extend(self.in_scope(mn, name, 0));
                }
                if hits.is_empty() && ends(&md.file, JS_EXTS) {
                    if let Some(g_) = self.func_ref(&mods, name, depth + 1) {
                        hits = vec![g_]; // `z.string()`: the function `string` names (`ZodString.create`)
                    }
                }
                if hits.is_empty() {
                    hits = self.method(&mods, name, None).0;
                }
            } else {
                hits = self.method(&mods, name, None).0;
            }
            if hits.is_empty() && matches!(kind, "inst" | "static") && ends(&md.file, JS_EXTS) {
                if let Some(g_) = self.expando_fn(&mods, name) {
                    hits = vec![g_]; // `axios.create()`: `instance.create = function create(...)`
                }
            }
            if md.file.ends_with(".cs") && hits.len() > 1 {
                let want = targs.unwrap_or("0");
                let narrowed: Vec<&Func> = hits.iter().copied().filter(|g| g.targs.is_empty() || g.targs.split(',').any(|x| x == want)).collect();
                if !narrowed.is_empty() {
                    hits = narrowed;
                }
            }
            if !hits.is_empty() && kind == "static" && hits.iter().all(|g| g.returns == "Self") && ends(&md.file, JS_EXTS) {
                return Some(cur); // `res.status(404)` on the module object `res`: `return this` gives that object back
            }
            // (Rust / Swift / Kotlin: an associated function's `Self` is the type it is called on,
            // `TimeFilter::after(s) -> Option<Self>`)
            let sm: Option<Vec<String>> = if kind == "inst" || (kind == "static" && ends(&md.file, &[".rs", ".swift", ".kt", ".kts"])) {
                Some(mods.clone())
            } else {
                None
            };
            if hits.len() == 1 {
                return self.returns(hits[0], sm.as_deref(), depth + 1, targs);
            }
            if !hits.is_empty() && hits.iter().map(|g| g.returns.as_str()).collect::<HashSet<_>>().len() == 1 && !hits[0].returns.is_empty() {
                // overloads that all return the same type: whichever runs, that is the value
                return self.returns(hits[0], sm.as_deref(), depth + 1, targs);
            }
            if !hits.is_empty()
                && kind == "inst"
                && hits.iter().filter(|g| !g.returns.is_empty()).map(|g| g.returns.as_str()).collect::<HashSet<_>>() == HashSet::from(["Self"])
                && md.file.ends_with(".swift")
            {
                return Some(cur); // `request.validate()`: each overload declared returns `Self` (the rest unread)
            }
            if hits.is_empty() && kind == "inst" && ends(&md.file, JS_EXTS) && depth < MAX {
                // a TS member signature `get: (u) => ResponsePromise`
                if let Some(got) = self.field(&mods, &format!("{name}()"), depth) {
                    if got != Val::Unknown {
                        return Some(got);
                    }
                }
            }
            if hits.is_empty() && kind == "static" && ends(&md.file, JS_EXTS) && depth < MAX {
                let alt = self.js_default_reexport(&mods);
                if !alt.is_empty() {
                    // `require('express')` is index.js, `module.exports = require('./lib/express')`
                    return self.step(md, &Val::Static(alt, exact), seg, depth + 1);
                }
            }
            if hits.is_empty() {
                if kind == "static"
                    && ends(&md.file, JS_EXTS)
                    && mods.iter().any(|mn| {
                        let m = self.m(mn);
                        m.bound.get(name).is_some_and(|b| b.starts_with("reexport:"))
                            && is_ext(&self.held(m, None, &m.bound[name], depth + 1, None))
                    })
                {
                    return Some(ext()); // `express.Router()`: the `router` package's, re-exported
                }
                if kind == "static" && seg == "new()" && ends(&md.file, &[".gd", ".rb", ".rake", ".ru"]) {
                    return Some(Val::Inst(mods, exact)); // `Foo.new()` (GDScript, Ruby): a Foo (no `new` of its own)
                }
                if UNWRAP.contains(&seg) {
                    return Some(cur); // `.unwrap()` / `.get()` on an Option: the value inside
                }
                if mods.iter().any(|mn| self.external_base(mn)) {
                    return Some(ext());
                }
            }
            return None;
        }
        // a field / property / nested type
        if kind == "static" && ends(&md.file, JS_EXTS) && !seg.starts_with(['(', '<', '[']) && !seg.is_empty() && depth < MAX {
            if let Some(val) = self.ts_value(&mods, seg, depth) {
                return Some(val);
            }
        }
        if kind == "static" {
            let mut inner: Vec<String> = mods
                .iter()
                .map(|mn| format!("{mn}.{seg}"))
                .filter(|x| self.g.modules.contains_key(x) && self.nested(x))
                .collect();
            if inner.is_empty() {
                inner = self.py_classes(&mods, seg);
            }
            if !inner.is_empty() {
                let one = inner.len() == 1;
                return Some(Val::Static(inner, one));
            }
            for mn in &mods {
                let m = self.m(mn);
                if let Some(held) = m.bound.get(seg).filter(|s| !s.is_empty()) {
                    return self.held(m, None, held, depth + 1, Some(&[mn.clone()]));
                }
                if m.owner.is_empty() {
                    if let Some((vm, vn)) = self.var_module(mn, seg, 0) {
                        // `flask.g`: the package re-exports globals.g
                        return self.held(vm, None, &vm.bound[&vn], depth + 1, None);
                    }
                }
            }
            if ends(&md.file, JS_EXTS) {
                for mn in &mods {
                    // `z.coerce` with `export * as coerce from "./coerce.js"` (through `export *`)
                    let dm = self.defining_module(mn, seg, 0);
                    if !dm.is_empty() && dm != *mn && self.m(&dm).owner.is_empty() && !self.m(mn).bound.contains_key(seg) {
                        return Some(Val::Static(vec![dm], true));
                    }
                }
            }
            if ends(&md.file, JS_EXTS) {
                // a module's object given a property elsewhere (`ctx.response = ...`)
                let got = self.field(&mods, seg, depth);
                return if got == Some(Val::Unknown) { None } else { got };
            }
            return None;
        }
        let mut got = self.field(&mods, seg, depth);
        if got.is_none() {
            got = self.getter(&mods, seg, depth);
        }
        if got == Some(Val::Unknown) {
            return None;
        }
        if got.is_none() && seg.starts_with("__") && seg.ends_with("__") && md.file.ends_with(".py") {
            return Some(ext()); // `self.__dict__`, `cls.__name__`: Python's own attributes
        }
        if got.is_none() && UNWRAP.contains(&seg) {
            return Some(cur);
        }
        if got.is_none() && mods.iter().any(|mn| self.external_base(mn)) {
            return Some(ext());
        }
        got
    }

    /// The function a bare call `name()` reaches (one, or None).
    pub fn bare(&self, md: &'a Module, f: Option<&'a Func>, name: &str) -> Option<&'a Func> {
        let fm = self.file_mod(md);
        let own = self.own_class(md, f);
        if let Some(own) = &own {
            if !fm.file.ends_with(".py") {
                let (hits, _) = self.method(own, name, None);
                if hits.len() == 1 {
                    return Some(hits[0]);
                }
            }
        }
        let mut hits = self.in_scope(&fm.name, name, 0);
        if hits.is_empty() {
            if let Some((w, n)) = fm.imported.get(name) {
                hits = self.in_scope(w, n, 0);
            }
        }
        if hits.is_empty() {
            for w in &fm.uses {
                hits = self.in_scope(w, name, 0);
                if !hits.is_empty() {
                    break;
                }
            }
        }
        if hits.is_empty() && !fm.package.is_empty() {
            hits = self.in_scope(&format!("pkg:{}", fm.package), name, 0);
        }
        if hits.len() == 1 {
            Some(hits[0])
        } else {
            None
        }
    }

    pub(super) fn lib_results(&self) -> HashSet<String> {
        if let Some(h) = &self.c.borrow().libres {
            return h.clone();
        }
        let got: HashSet<String> = self
            .g
            .params
            .values()
            .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("returns"))
            .flat_map(|e| e.get("functions").and_then(|f| f.as_array()).cloned().unwrap_or_default())
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect();
        self.c.borrow_mut().libres = Some(got.clone());
        got
    }

    pub(super) fn is_ident(s: &str) -> bool {
        is_ident(s)
    }

    pub(super) fn unused_consts() {
        let _ = (ES_MODULE, DYNAMIC);
    }
}
