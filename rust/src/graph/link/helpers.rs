//! The per-language readers `_build_edges` leans on (core.py's `_argc`,
//! `_cpp_namespace_call`, C macros, Swift implicit members, `_typed_targets`
//! and friends).

use std::collections::{HashMap, HashSet};

use super::Resolver;
use crate::graph::infer::{self, ends, esc, is_ext, paren_close, re, rxd, top_split, upper1, Val};
use crate::model::{Call, Func, Module};
use crate::project::ext_of;

const C_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "return", "sizeof", "do", "else", "case", "typeof", "__typeof__", "defined", "_Generic",
    "offsetof", "__builtin_expect", "__attribute__",
];
const STD_NAMESPACES: &[&str] = &["std", "boost", "absl", "Qt", "QtConcurrent", "chrono", "filesystem", "ranges", "views", "this_thread"];

const SWIFT_SEQ: &[&str] = &["Sequence", "Collection", "BidirectionalCollection", "RandomAccessCollection", "Equatable", "Hashable"];

/// What Swift's standard types conform to (an extension of the protocol is theirs too).
pub(super) fn swift_conforms(ty: &str) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = match ty {
        "String" => vec!["StringProtocol", "RangeReplaceableCollection", "Comparable", "CustomStringConvertible"],
        "Substring" => vec!["StringProtocol", "RangeReplaceableCollection", "Comparable"],
        "Array" => vec!["MutableCollection", "RangeReplaceableCollection", "ExpressibleByArrayLiteral"],
        "ArraySlice" => vec!["MutableCollection", "RangeReplaceableCollection"],
        "Range" | "ClosedRange" => vec![],
        "Dictionary" => return vec!["Sequence", "Collection", "Equatable", "Hashable"],
        "Set" => return vec!["Sequence", "Collection", "SetAlgebra", "Equatable", "Hashable"],
        "Int" => return vec!["BinaryInteger", "FixedWidthInteger", "SignedInteger", "Numeric", "Comparable", "Equatable", "Hashable"],
        "Double" => return vec!["BinaryFloatingPoint", "FloatingPoint", "Numeric", "Comparable", "Equatable", "Hashable"],
        "Character" => return vec!["Comparable", "Equatable", "Hashable"],
        _ => return vec![],
    };
    v.extend(SWIFT_SEQ);
    v
}

/// Browser globals a project augments (`declare global { interface Window { h: ... } }`).
pub(super) const DOM_GLOBALS: &[&str] = &[
    "Window", "Document", "Navigator", "HTMLElement", "Element", "Node", "EventTarget", "Event", "GlobalThis",
    "WorkerGlobalScope", "ServiceWorkerGlobalScope", "Console", "Location", "History", "HTMLCanvasElement",
    "CanvasRenderingContext2D", "Performance", "Storage", "ImportMeta",
];

pub(super) const RS_STD_METHODS: &[&str] = &[
    "unwrap", "expect", "unwrap_or", "unwrap_or_else", "unwrap_or_default", "unwrap_err", "is_none", "is_some", "is_ok",
    "is_err", "ok", "err", "map_err", "ok_or", "ok_or_else", "and_then", "as_ref", "as_mut", "as_deref", "clone", "cloned",
    "copied", "to_string", "to_owned", "into", "iter", "iter_mut", "into_iter", "collect", "push", "push_str", "pop", "len",
    "is_empty", "as_bytes", "as_str", "as_slice", "to_vec", "extend", "borrow", "borrow_mut", "lock", "take", "chars",
    "bytes", "trim", "split", "join", "starts_with", "ends_with", "filter", "filter_map", "enumerate", "zip", "rev", "any",
    "all", "fold", "sum", "count", "last", "first", "to_str", "to_path_buf", "display", "fmt", "eq", "cmp", "partial_cmp",
    "hash", "deref", "drop", "default", "map", "map_or", "map_or_else", "flat_map", "flatten", "skip", "skip_while",
    "take_while", "peekable", "windows", "chunks", "position", "rposition", "max_by", "min_by", "max_by_key",
    "min_by_key", "sorted", "is_some_and", "is_none_or", "inspect", "step_by", "cycle", "product", "partition",
    "as_deref_mut", "transpose", "zip_with", "char_indices", "lines", "split_whitespace", "trim_end", "trim_start",
    "to_lowercase", "to_uppercase", "to_ascii_lowercase", "repeat", "retain", "dedup", "truncate",
];

/// The positions of the commas at bracket depth 0.
pub(super) fn top_commas(text: &str) -> Vec<usize> {
    let (mut out, mut d) = (Vec::new(), 0i32);
    for (i, ch) in text.char_indices() {
        if "([{".contains(ch) {
            d += 1;
        }
        if ")]}".contains(ch) {
            d -= 1;
        }
        if ch == ',' && d == 0 {
            out.push(i);
        }
    }
    out
}

/// A Swift signature's parameters: (external label, type text).
fn params(sig: &str) -> Vec<(String, String)> {
    let Some(a) = sig.find('(') else { return vec![] };
    let b_ = sig.as_bytes();
    let (mut d, mut b) = (0i32, a);
    for (i, &ch) in b_.iter().enumerate().skip(a) {
        b = i;
        if ch == b'(' || ch == b'[' {
            d += 1;
        }
        if ch == b')' || ch == b']' {
            d -= 1;
        }
        if d == 0 {
            break;
        }
    }
    let body = &sig[a + 1..b.max(a + 1)];
    let mut out = Vec::new();
    let mut last = 0usize;
    let mut cuts = top_commas(body);
    cuts.push(body.len());
    for cut in cuts {
        let p = body[last.min(body.len())..cut].trim();
        last = cut + 1;
        if let Some(pm) = re!(r"(\w+)(?:\s+\w+)?\s*:\s*(?:inout\s+|@\w+\s+)*([^=]+)").match_start(p) {
            let t = pm.s(2).trim();
            out.push((pm.s(1).to_string(), t.strip_suffix("...").unwrap_or(t).to_string()));
        }
    }
    out
}

/// The lines of every `func name(` in the type block `g` is in: overloads
/// share one Func (`add(name:value:)`, `add(_ header:)`).
fn overload_starts(raw: &[String], g: &Func) -> Vec<usize> {
    let type_head = re!(r"(?:\w+\s+)*(?:struct|class|enum|extension|protocol|actor)\s");
    let owner = |mut i: isize| -> isize {
        while i >= 0 && type_head.match_start(&raw[i as usize]).is_none() {
            i -= 1;
        }
        i
    };
    let top = owner(g.start as isize - 1);
    let pat = rxd(&format!(r"\s*(?:@\w+\s+)*(?:\w+\s+)*func\s+{}\s*[(<]", esc(&g.name)));
    let mut starts = vec![g.start];
    if top < 0 {
        return starts;
    }
    for i in (top as usize + 1)..raw.len() {
        if type_head.match_start(&raw[i]).is_some() {
            break;
        }
        if i + 1 != g.start && pat.match_start(&raw[i]).is_some() {
            starts.push(i + 1);
        }
    }
    starts
}

/// `enum <m's type> { case name(...) }`: name is one of its cases (in the
/// enum's own body, not another type's in the same file).
fn enum_case(m: &Module, name: &str) -> bool {
    if m.decls.is_empty() {
        return false;
    }
    let text = m.raw.join("\n");
    let tn = m.decls[0].rsplit('.').next().unwrap();
    for em in rxd(&format!(r"\benum\s+{}\b[^{{]*\{{", esc(tn))).find_iter(&text) {
        let b = text.as_bytes();
        let (mut d, mut k) = (1i32, em.end());
        while k < b.len() && d != 0 {
            if b[k] == b'{' {
                d += 1;
            }
            if b[k] == b'}' {
                d -= 1;
            }
            k += 1;
        }
        let body = &text[em.end()..k.min(text.len())];
        if rxd(&format!(r"\bcase\s+(?:[^\n]*,\s*)?{}\b", esc(name))).search(body).is_some() {
            return true;
        }
    }
    false
}

fn char_to_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|x| x.0).unwrap_or(s.len())
}

impl<'a> Resolver<'a> {
    /// How many arguments a call passes, where overloads go by count (Java, C#,
    /// Kotlin, C++); None elsewhere or when it can't be read (core._argc).
    pub(super) fn argc(&self, md: &Module, c: &Call) -> Option<usize> {
        if !ends(&md.file, infer::OVERLOADED) || !(0 < c.line && c.line <= md.code.len()) {
            return None;
        }
        let hi = (c.line + 30).min(md.code.len());
        let text = md.code[c.line - 1..hi].join("\n");
        let col = if c.col >= 0 { c.col as usize } else { 0 };
        let from = char_to_byte(&text, col);
        let k = text[from..].find('(').map(|x| x + from)?;
        // text[c.col:k]
        let between: &str = if c.col >= 0 { &text[from..k] } else { "" };
        if !between.trim_matches([' ', '\t']).is_empty() && rxd(r"\w*\s*(?:<[^()]*>)?\s*").fullmatch(between).is_none() {
            return None;
        }
        let close = paren_close(&text, k);
        if close < 0 {
            return None;
        }
        let inner = &text[k + 1..close as usize];
        if inner.trim().is_empty() {
            return Some(0);
        }
        let inner = re!(r"(?<=\w)\s*<[\w\s,.?<>\[\]]*>(?=\s*[(\[{>),]|\s*$)").sub("", inner);
        Some(top_split(&inner.replace(['<', '>'], " ")).len())
    }

    /// `detail::write(...)`, `details::os::localtime(...)`, `std::move(...)`: a call
    /// qualified by a namespace path. True when the path names namespaces (not a
    /// class or a variable): `targets` gets the free functions of that name in a
    /// namespace ending with that path (one overload set: exact), none for `std::`.
    pub(super) fn cpp_namespace_call(&self, f: &Func, c: &Call, targets: &mut Vec<(&'a Func, bool)>) -> bool {
        let g = self.g;
        let ident = re!(r"^[A-Za-z_]\w*$");
        if !c.chain.iter().all(|s| ident.is_match(s)) || f.bound.contains_key(c.chain.last().unwrap()) {
            return false;
        }
        let own = self.t.cpp_namespaces();
        if STD_NAMESPACES.contains(&c.chain[0].as_str()) && !own.contains(&c.chain[0]) {
            return true; // the standard library's (or boost's): no project function
        }
        if !c.chain.iter().all(|s| own.contains(s)) {
            return false; // a class (`Foo::make()`) or a variable: the type layer decides
        }
        let path = c.chain.join("::");
        let by_name: Vec<&Func> = g.by_name.get(&c.name).map(|v| v.iter().filter_map(|id| g.func(id)).collect()).unwrap_or_default();
        let mut hits: Vec<&Func> =
            by_name.iter().copied().filter(|h| !h.ns.is_empty() && (h.ns == path || h.ns.ends_with(&format!("::{path}")))).collect();
        if hits.is_empty() {
            // `details::m_formatter<Padder>(...)`: a class's constructor, the class named in that namespace
            hits = by_name
                .iter()
                .copied()
                .filter(|h| h.module.rsplit('.').next() == Some(c.name.as_str()) && !g.modules[&h.module].owner.is_empty())
                .collect();
            let mods: HashSet<&String> = hits.iter().map(|h| &h.module).collect();
            if mods.len() == 1 {
                targets.extend(hits.iter().map(|h| (*h, true)));
                return true;
            }
            return false;
        }
        let sure = hits.iter().map(|h| h.ns.as_str()).collect::<HashSet<_>>().len() == 1; // (overloads in one namespace, over several headers: one function)
        targets.extend(hits.into_iter().map(|h| (h, sure)));
        true
    }

    /// Function-like macros in the project's C files: name -> the names their body calls.
    fn c_macros(&mut self) -> &HashMap<String, Vec<String>> {
        if self.c_macros.is_none() {
            let mut got: HashMap<String, Vec<String>> = HashMap::new();
            for (rel, mods) in &self.g.by_file {
                if !(rel.ends_with(".c") || rel.ends_with(".h")) || mods.is_empty() {
                    continue;
                }
                let raw = self.g.modules[&mods[0]].raw.join("\n");
                for m in re!(r"(?m)^[ \t]*#[ \t]*define[ \t]+([A-Za-z_]\w*)\(([^)]*)\)((?:[^\n]*\\\n)*[^\n]*)").find_iter(&raw) {
                    let ps: HashSet<String> = m.s(2).split(',').map(|p| p.trim().to_string()).collect();
                    let body = m.s(3).replace("\\\n", " ");
                    let names: Vec<String> = re!(r"\b([A-Za-z_]\w*)\s*\(")
                        .find_iter(&body)
                        .iter()
                        .map(|x| x.s(1).to_string())
                        .filter(|n| !ps.contains(n) && !C_KEYWORDS.contains(&n.as_str()))
                        .collect();
                    got.entry(m.s(1).to_string()).or_insert(names);
                }
            }
            self.c_macros = Some(got);
        }
        self.c_macros.as_ref().unwrap()
    }

    /// The project functions a call to macro `name` runs (through macros it uses, 3 deep).
    pub(super) fn c_macro_targets(&mut self, caller_file: &str, name: &str, depth: usize) -> Vec<&'a Func> {
        let g = self.g;
        let mut out: Vec<&'a Func> = Vec::new();
        let names: Vec<String> = if depth < 3 { self.c_macros().get(name).cloned().unwrap_or_default() } else { vec![] };
        for n in names {
            let cfuncs: Vec<&Func> = g
                .by_name
                .get(&n)
                .map(|v| v.iter().filter_map(|id| g.func(id)).filter(|h| h.kind != "top" && (h.file.ends_with(".c") || h.file.ends_with(".h"))).collect())
                .unwrap_or_default();
            let own: Vec<&Func> = cfuncs.iter().copied().filter(|h| h.file == caller_file).collect();
            let glob_: Vec<&Func> = cfuncs.iter().copied().filter(|h| !self.c_static(h) || h.file.ends_with(".h")).collect();
            if let Some(o) = own.first() {
                out.push(o);
            } else if !glob_.is_empty() && glob_.iter().map(|h| &h.file).collect::<HashSet<_>>().len() == 1 {
                out.push(glob_[0]);
            } else if cfuncs.is_empty() {
                out.extend(self.c_macro_targets(caller_file, &n, depth + 1));
            }
        }
        let mut seen = HashSet::new();
        out.retain(|h| seen.insert(h.id.clone()));
        out
    }

    /// A C function declared `static` (file-local).
    pub(super) fn c_static(&self, h: &Func) -> bool {
        let Some(m) = self.g.modules.get(&h.module) else { return false };
        let lo = h.start.saturating_sub(2).min(m.raw.len());
        let hi = h.start.min(m.raw.len());
        let head = m.raw[lo..hi.max(lo)].join(" ");
        re!(r"\bstatic\b").search(&head).is_some()
    }

    /// The .c files a C file `#include`s (a test that includes the unit it tests).
    pub(super) fn c_included(&self, rel: &str) -> HashSet<String> {
        let g = self.g;
        let (mut got, mut todo, mut seen): (HashSet<String>, Vec<String>, HashSet<String>) = (HashSet::new(), vec![rel.to_string()], HashSet::from([rel.to_string()]));
        while let Some(cur) = todo.pop() {
            // (through headers: a test's `common.h` includes `../cJSON.c`)
            let Some(m) = g.by_file.get(&cur).and_then(|v| v.first()).map(|n| &g.modules[n]) else { continue };
            for inc in &m.c_includes {
                let mut inc = inc.clone();
                if !g.by_file.contains_key(&inc) {
                    let tail = inc.rsplit('/').next().unwrap_or(&inc).to_string();
                    if let Some(r) = g.by_file.keys().find(|r| r.ends_with(&format!("/{tail}"))) {
                        inc = r.clone();
                    }
                }
                if seen.contains(&inc) {
                    continue;
                }
                seen.insert(inc.clone());
                if inc.ends_with(".c") {
                    got.insert(inc.clone());
                }
                todo.push(inc);
            }
        }
        got
    }

    /// `g` is a method a call reaches only through a receiver (Go's `func (c *Command) X`,
    /// Rust's `fn x(&self)`, a JS / TS / PHP class's method), not a free function.
    pub(super) fn needs_receiver(&self, h: &Func) -> bool {
        if h.file.ends_with(".go") {
            return !h.recv.is_empty();
        }
        if h.file.ends_with(".rs") {
            let head = h.code.split('{').next().unwrap_or("");
            return re!(r"\(\s*(?:&\s*)?(?:'\w+\s+)?(?:mut\s+)?self\b").search(head).is_some() || !self.g.modules[&h.module].owner.is_empty();
        }
        !self.g.modules[&h.module].owner.is_empty()
    }

    /// The directory of the crate root a Rust file belongs to: the nearest ancestor
    /// holding main.rs / lib.rs (an integration test binary's `tests/builder/main.rs`).
    pub(super) fn rs_crate_dir(&self, rel: &str) -> String {
        let mut d = rel.rsplit_once('/').map(|x| x.0).unwrap_or("").to_string();
        while !d.is_empty() {
            if self.g.by_file.contains_key(&format!("{d}/main.rs")) || self.g.by_file.contains_key(&format!("{d}/lib.rs")) {
                return format!("{d}/");
            }
            d = d.rsplit_once('/').map(|x| x.0).unwrap_or("").to_string();
        }
        String::new()
    }

    /// The engine class a GDScript receiver is declared as (`var x: Camera3D`),
    /// or "" (untyped, or a project script's type).
    pub(super) fn gd_declared(&self, md: &'a Module, f: &'a Func, chain: &[String]) -> String {
        let g = self.g;
        let held: String = if chain.len() == 1 {
            f.bound.get(&chain[0]).filter(|s| !s.is_empty()).or_else(|| md.bound.get(&chain[0])).cloned().unwrap_or_default()
        } else {
            let v = self.t.chain(md, Some(f), &chain[..chain.len() - 1], 0, None, None);
            match v {
                Some(Val::Inst(mods, _)) if mods.len() == 1 => g.modules[&mods[0]].bound.get(chain.last().unwrap()).cloned().unwrap_or_default(),
                _ => return String::new(),
            }
        };
        let ty = held.strip_prefix("type:").unwrap_or("");
        if !ty.is_empty() && !g.modules.contains_key(ty) && g.short.get(ty).map_or(true, |v| v.is_empty()) {
            ty.to_string()
        } else {
            String::new()
        }
    }

    /// A script's `extends` chain reaches the engine class `engine`.
    pub(super) fn gd_extends(&self, module: &str, engine: &str) -> bool {
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

    /// C#: the simple names of the types a file's bare calls may name through
    /// `using static X;` (the file's own, `global using static`, and a .csproj's
    /// `<Using Include="X" Static="true"/>`).
    pub(super) fn cs_static_usings(&mut self, md: &Module) -> HashSet<String> {
        if self.cs_global_static.is_none() {
            let mut glob: HashSet<String> = HashSet::new();
            for m in self.g.modules.values() {
                if m.file.ends_with(".cs") && m.owner.is_empty() {
                    for x in re!(r"(?m)^[ \t]*global[ \t]+using[ \t]+static[ \t]+([\w.]+)").find_iter(&m.code.join("\n")) {
                        glob.insert(x.s(1).rsplit('.').next().unwrap().to_string());
                    }
                }
            }
            let mut stack = vec![self.p.root.clone()];
            while let Some(dir) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        if !n.starts_with('.') && !matches!(n.as_str(), "node_modules" | "bin" | "obj") {
                            stack.push(e.path());
                        }
                    } else if n.ends_with(".csproj") || n.ends_with(".props") {
                        if let Ok(b) = std::fs::read(e.path()) {
                            let t = String::from_utf8_lossy(&b).into_owned();
                            for x in re!(r#"<Using\s+Include="([\w.]+)"\s+Static="[Tt]rue""#).find_iter(&t) {
                                glob.insert(x.s(1).rsplit('.').next().unwrap().to_string());
                            }
                        }
                    }
                }
            }
            self.cs_global_static = Some(glob);
        }
        let fm = self.t.file_mod(self.g.modules.get(&md.name).unwrap());
        let own: HashSet<String> = re!(r"(?m)^[ \t]*using[ \t]+static[ \t]+([\w.]+)")
            .find_iter(&fm.code.join("\n"))
            .iter()
            .map(|x| x.s(1).rsplit('.').next().unwrap().to_string())
            .collect();
        let mut r = self.cs_global_static.clone().unwrap();
        r.extend(own);
        r
    }

    /// The JS / TS files some module imports (or re-exports) from.
    pub(super) fn js_imported_files(&mut self) -> &HashSet<String> {
        if self.js_imported.is_none() {
            let g = self.g;
            let mut got: HashSet<String> = HashSet::new();
            for m in g.modules.values() {
                if !ends(&m.file, infer::JS_FILE_EXTS) {
                    continue;
                }
                for (w, _) in m.imported.values() {
                    if let Some(wm) = g.modules.get(w) {
                        got.insert(wm.file.clone());
                    }
                }
                for w in &m.reexports {
                    if let Some(wm) = g.modules.get(w) {
                        got.insert(wm.file.clone());
                    }
                }
            }
            self.js_imported = Some(got);
        }
        self.js_imported.as_ref().unwrap()
    }

    /// The functions `x.name()` reaches when x is declared `ty`, and whether ty
    /// is a project type at all (core._typed_targets).
    pub(super) fn typed_targets(&mut self, md: &'a Module, ty: &str, name: &str) -> (Vec<&'a Func>, bool) {
        let g = self.g;
        if md.file.ends_with(".go") {
            return self.go_typed(md, ty, name);
        }
        let mut mods: Vec<String> = Vec::new();
        if md.file.ends_with(".py") {
            let cls = crate::graph::pylink::py_class(g, md, ty, 0);
            if !cls.is_empty() {
                mods.push(cls);
            }
        }
        if let Some(a) = md.aliases.get(ty) {
            mods.push(a.clone());
        }
        if g.modules.contains_key(ty) {
            mods.push(ty.to_string());
        }
        mods.extend(g.short.get(ty).cloned().unwrap_or_default());
        let mut hits: Vec<&'a Func> = Vec::new();
        let mut seen: HashSet<&String> = HashSet::new();
        for mn in &mods {
            if !seen.insert(mn) {
                continue;
            }
            if let Some(t) = g.lookup(mn, name) {
                if !hits.iter().any(|h| h.id == t.id) {
                    hits.push(t);
                }
            }
        }
        if hits.is_empty() {
            if self.ext_cache.is_none() {
                let mut c: HashMap<(String, String), Vec<String>> = HashMap::new();
                for id in g.funcs.keys() {
                    if let Some(h) = g.func(id) {
                        if !h.recv.is_empty() {
                            c.entry((h.recv.clone(), h.name.clone())).or_default().push(h.id.clone());
                        }
                    }
                }
                self.ext_cache = Some(c);
            }
            hits = self
                .ext_cache
                .as_ref()
                .unwrap()
                .get(&(ty.to_string(), name.to_string()))
                .map(|v| v.iter().filter_map(|id| g.func(id)).collect())
                .unwrap_or_default();
        }
        (hits, !mods.is_empty())
    }

    /// Go `x.M()` with x declared a type of the caller's own package: that
    /// type's method M, from whichever file of the package defines it.
    fn go_typed(&mut self, md: &'a Module, ty: &str, name: &str) -> (Vec<&'a Func>, bool) {
        let g = self.g;
        let fm = self.t.file_mod(md);
        if self.go_types.is_none() {
            let mut idx: HashMap<(String, String), Vec<String>> = HashMap::new();
            for m in g.modules.values() {
                if !m.owner.is_empty() && m.file.ends_with(".go") {
                    if let Some(o) = g.modules.get(&m.owner) {
                        if !o.package.is_empty() && !m.decls.is_empty() {
                            idx.entry((o.package.clone(), m.decls[0].clone())).or_default().push(m.name.clone());
                        }
                    }
                }
            }
            self.go_types = Some(idx);
        }
        let mut pkg = fm.package.clone();
        let mut ty = ty.to_string();
        if let Some((q, rest)) = ty.clone().split_once('.') {
            // `w tui.Window`: a type of an imported package
            let target = md.aliases.get(q).cloned().unwrap_or_default();
            let Some(pk) = target.strip_prefix("pkg:") else { return (vec![], false) };
            pkg = pk.to_string();
            ty = rest.to_string();
        }
        let hits: Vec<&'a Func> = self
            .go_types
            .as_ref()
            .unwrap()
            .get(&(pkg.clone(), ty.clone()))
            .map(|v| v.iter().filter_map(|mn| g.func(&format!("{mn}.{name}"))).collect())
            .unwrap_or_default();
        if !hits.is_empty() {
            return (hits, true);
        }
        let declared = g.packages.get(&pkg).is_some_and(|v| v.iter().any(|mn| g.modules.get(mn).is_some_and(|m| m.decls.iter().any(|d| *d == ty))));
        (vec![], declared) // declared here and missed: an embedded type's method (a guess); else a builtin
    }

    /// What a Swift implicit member `.name(...)` is, from its context:
    /// ("none", []) for a pattern, an enum case or a library type's member;
    /// ("exact", funcs) for the expected type's static function; ("?", []) when
    /// the context doesn't say the type.
    pub(super) fn implicit_member(&mut self, md: &'a Module, f: &'a Func, c: &Call) -> (&'static str, Vec<&'a Func>) {
        let g = self.g;
        if !(0 < c.line && c.line <= md.raw.len()) {
            return ("?", vec![]);
        }
        let line = &md.raw[c.line - 1];
        let code_: &Vec<String> = if !md.code.is_empty() && md.code.len() == md.raw.len() { &md.code } else { &md.raw };
        // the text before the member, from a few lines up (a call's arguments span lines)
        let lo = c.line.saturating_sub(12);
        let mut before = code_[lo.min(c.line - 1)..c.line - 1].join("\n");
        if c.line - 1 > lo.min(c.line - 1) || !before.is_empty() {
            before.push('\n');
        }
        {
            let cur = &code_[c.line - 1];
            let upto = (c.col - 1).max(0) as usize;
            before.push_str(crate::pyre::take_chars(cur, upto));
        }
        let case_re = re!(r"\bcase\s+(?:let\s+|var\s+)?$");
        let line_case = re!(r"\s*(?:case|if\s+case|guard\s+case|for\s+case)\b").match_start(line).is_some()
            && !before.split("case").last().unwrap_or("").contains('=');
        if case_re.search(&before).is_some() || line_case {
            return ("none", vec![]); // `case .value(let v):` matches, it builds nothing
        }
        let mut ty = String::new();
        let lm = re!(r"(?:^|[(,]\s*)(\w+)\s*:\s*$").search(&before);
        let elem = re!(r"(?:\breturn\s*\[|=\s*\[|\b(\w+)\s*\.\s*(?:append|insert)\s*\(|\[\s*)\s*$").search(&before);
        if let Some(elem) = &elem {
            // an element: of the returned / declared array, or of the one appended to
            let ret = elem.group0().contains("return");
            let mut src = if ret {
                let lo = f.start.saturating_sub(1);
                md.raw[lo.min(md.raw.len())..(f.start + 4).min(md.raw.len())].join("\n")
            } else {
                String::new()
            };
            let dm = if let Some(nm) = elem.get(1) {
                src = md.raw.join("\n");
                rxd(&format!(r"\b{}\s*:\s*\[([\w.]+)\]", esc(nm))).search(&src).map(|x| x.s(1).to_string())
            } else if ret {
                re!(r"->\s*\[([\w.]+)\]").search(&src).map(|x| x.s(1).to_string())
            } else {
                re!(r"\b(?:let|var)\s+\w+\s*:\s*\[([\w.]+)\]\s*=\s*\[\s*$").search(&before).map(|x| x.s(1).to_string())
            };
            if let Some(d) = dm {
                ty = d;
            }
        }
        if !ty.is_empty() {
        } else if re!(r"\breturn\s*$").search(&before).is_some() {
            ty = if !f.returns.is_empty() && !f.returns.starts_with("expr:") { f.returns.clone() } else { String::new() };
        } else if re!(r"\bself\s*=\s*$").search(&before).is_some() {
            let own = self.t.own_class(md, Some(f));
            ty = own.and_then(|o| g.modules[&o[0]].decls.first().cloned()).unwrap_or_default();
        } else if let Some(tm) = re!(r"\b(?:let|var)\s+\w+\s*:\s*([\w.]+)[?!]?\s*=\s*$").search(&before) {
            ty = tm.s(1).to_string();
        } else if lm.is_some() || re!(r",\s*$").search(&before).is_some() {
            // a labelled argument: the parameter of that label in the callee's signature
            // (start just after the `(` or `,` the label follows, so that paren is the call's own)
            let sub = lm.as_ref().map(|l| l.group0().len() - if l.group0().starts_with(['(', ',']) { 1 } else { 0 }).unwrap_or(0);
            let (mut depth, text) = (0i32, before.as_bytes());
            let mut k = before.len() as isize - sub as isize;
            while k > 0 {
                k -= 1;
                let ch = text[k as usize];
                if ch == b')' {
                    depth += 1;
                } else if ch == b'(' {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
            }
            let label = lm.as_ref().map(|l| l.s(1).to_string()).unwrap_or_default();
            let idx = if k >= 0 { top_commas(&before[k as usize + 1..]).len() } else { 0 };
            ty = self.param_type(md, &before, k, &label, idx, Some(f), c.line);
        } else if let Some(am) = re!(r"\(\s*$").search(&before) {
            ty = self.param_type(md, &before, am.start() as isize, "", 0, Some(f), c.line); // the first, unlabelled argument
        }
        let mut verdicts: Vec<(&'static str, Vec<&'a Func>)> = Vec::new();
        let tys: Vec<&str> = if ty.is_empty() { vec![] } else { ty.split('|').collect() };
        for t in tys {
            // overloads: the parameter type that has the member
            let t = crate::lang::chains::type_name(t);
            if t.is_empty() || t == "Self" {
                verdicts.push(("?", vec![]));
                continue;
            }
            let mut nested: Option<String> = None; // `Value` inside `Element`: the enclosing types' own nested `Value` first
            let mut o = md.name.clone();
            while !o.is_empty() && nested.is_none() {
                if g.modules.contains_key(&format!("{o}.{t}")) {
                    nested = Some(format!("{o}.{t}"));
                }
                o = g.modules.get(&o).map(|x| x.owner.clone()).unwrap_or_default();
            }
            let (tmods, ex, declared) = match nested {
                Some(n) => (vec![n], true, true),
                None => self.t.type_modules(md, &t),
            };
            if !declared {
                verdicts.push(("lib", vec![])); // a library type's member
                continue;
            }
            if !ex {
                verdicts.push(("?", vec![]));
                continue;
            }
            let hits: Vec<&Func> = self.t.method(&tmods, &c.name, None).0.into_iter().filter(|h| super::is_static(g, h)).collect();
            if !hits.is_empty() {
                verdicts.push(("exact", hits));
            } else if tmods.iter().any(|m_| enum_case(&g.modules[m_], &c.name)) {
                verdicts.push(("none", vec![])); // an enum case: `case option(String)`
            } else {
                verdicts.push(("miss", vec![])); // this overload's type has no such member
            }
        }
        let mut found: Vec<(&'static str, Vec<&'a Func>)> = Vec::new();
        for v in &verdicts {
            if v.0 == "exact" || v.0 == "none" {
                let key: Vec<&String> = v.1.iter().map(|h| &h.id).collect();
                if !found.iter().any(|x| x.0 == v.0 && x.1.iter().map(|h| &h.id).collect::<Vec<_>>() == key) {
                    found.push(v.clone());
                }
            }
        }
        if found.len() == 1 && verdicts.iter().all(|v| matches!(v.0, "exact" | "none" | "miss")) {
            return found.remove(0);
        }
        if !verdicts.is_empty() && verdicts.iter().all(|v| v.0 == "lib") {
            return ("none", vec![]);
        }
        ("?", vec![])
    }

    /// The declared type of the parameter `label` (or the first, unlabelled
    /// one) of the call whose `(` is at text[k]: `Element(value: .option(x))`,
    /// `session.request(.status(401))`. "" when the callees disagree.
    #[allow(clippy::too_many_arguments)]
    fn param_type(&mut self, md: &'a Module, text: &str, k: isize, label: &str, index: usize, f: Option<&'a Func>, line_no: usize) -> String {
        let g = self.g;
        if k <= 0 {
            return String::new();
        }
        let k = k as usize;
        let Some(callee) = re!(r"([A-Za-z_]\w*)\s*$").search(&text[..k]) else { return String::new() };
        let name = callee.s(1).to_string();
        let prefix = &text[..callee.span(1).unwrap().0];
        let callee_line = line_no as isize - (text.matches('\n').count() as isize - prefix.matches('\n').count() as isize);
        let callee_col = prefix.chars().count() as isize - (prefix.rfind('\n').map(|x| prefix[..x + 1].chars().count() as isize).unwrap_or(0));
        let mut cands: Vec<&'a Func>;
        if name == "init" && re!(r"\b(?:self|Self|super)\s*\.\s*init\s*$").search(&text[..k]).is_some() {
            let own = f.and_then(|f| self.t.own_class(md, Some(f)));
            // `self.init(base: .message(x))`: this type's own init
            cands = own
                .map(|o| self.t.ancestors(&o[0]).iter().flat_map(|a| g.modules[a].funcs.iter().filter(|h| h.name == "init").collect::<Vec<_>>()).collect())
                .unwrap_or_default();
        } else if upper1(&name) {
            let (tmods, ex, _) = self.t.type_modules(md, &name);
            cands = if ex {
                tmods.iter().flat_map(|m_| self.t.ancestors(m_)).flat_map(|a| g.modules[&a].funcs.iter().filter(|h| h.name == "init").collect::<Vec<_>>()).collect()
            } else {
                vec![]
            };
        } else {
            cands = g.by_name.get(&name).map(|v| v.iter().filter_map(|id| g.func(id)).collect()).unwrap_or_default();
            // the callee through its own receiver: `session.request(` is Session's request
            let outer = if line_no != 0 {
                f.and_then(|f| f.calls.iter().find(|c2| c2.line as isize == callee_line && c2.name == name && c2.col as isize == callee_col))
            } else {
                None
            };
            if let Some(o) = outer.filter(|o| !o.chain.is_empty()) {
                let v = self.t.chain(md, f, &o.chain, 0, None, None);
                if let Some(v) = v.filter(|v| !v.is_ext() && v.is_inst_or_static()) {
                    let hit = self.t.method(v.mods(), &name, None).0;
                    if !hit.is_empty() {
                        cands = hit;
                    }
                }
            } else if outer.is_some() {
                if let Some(own) = self.t.own_class(md, f) {
                    let hits = self.t.method(&own, &name, None).0;
                    if !hits.is_empty() {
                        cands = hits;
                    }
                }
            }
        }
        let mut found: Vec<String> = Vec::new();
        for gf in cands.iter().take(12) {
            let Some(gm) = g.modules.get(&gf.module) else { continue };
            for start in overload_starts(&gm.raw, gf) {
                let lo = (start - 1).min(gm.raw.len());
                let sig = gm.raw[lo..(start + 6).min(gm.raw.len())].join(" ");
                let ps = params(if let Some(i) = sig.find(&gf.name) { &sig[i..] } else { &sig });
                let hit: Option<String> = if !label.is_empty() {
                    ps.iter().find(|(l, _)| l == label).map(|(_, t)| t.clone())
                } else if index < ps.len() && ps[index].0 == "_" {
                    Some(ps[index].1.clone())
                } else {
                    None
                };
                if let Some(h) = hit {
                    if !found.contains(&h) {
                        found.push(h);
                    }
                }
            }
        }
        if found.is_empty() && !label.is_empty() && (name == "init" || upper1(&name)) {
            // a struct's memberwise init (`Element(value: .option(x))`, `self.init(base: .message(t))`):
            // the parameter is the stored property of that name
            let owners: Vec<String> = if name == "init" {
                f.and_then(|f| self.t.own_class(md, Some(f))).unwrap_or_default()
            } else {
                let (tmods, ex, _) = self.t.type_modules(md, &name);
                if ex { tmods } else { vec![] }
            };
            'o: for o in owners {
                for a in self.t.ancestors(&o) {
                    let held = g.modules[&a].bound.get(label).cloned().unwrap_or_default();
                    if let Some(t) = held.strip_prefix("type:") {
                        found.push(t.to_string());
                        break 'o;
                    }
                }
            }
        }
        found.sort();
        found.join("|") // (overloads: each parameter type; the member settles which)
    }

    /// A Swift call's first argument label against `g`'s first parameter:
    /// `append(contentsOf:)` is the library's, not the project's `append(optional:)`.
    pub(super) fn swift_labels_match(&self, md: &Module, c: &Call, gf: &Func) -> bool {
        if !(0 < c.line && c.line <= md.raw.len()) {
            return true;
        }
        let line = &md.raw[c.line - 1];
        let from = char_to_byte(line, (c.col.max(0) as usize) + c.name.chars().count());
        let am = rxd(r"\s*\(\s*(?:(\w+)\s*:(?!:))?").match_start(&line[from.min(line.len())..]);
        let Some(gm) = self.g.modules.get(&gf.module) else { return true };
        let sig = gm.raw[(gf.start - 1).min(gm.raw.len())..(gf.start + 3).min(gm.raw.len())].join(" ");
        let ps = params(&sig[sig.find(&gf.name).unwrap_or(0)..]);
        if am.is_none() || ps.is_empty() {
            return true;
        }
        let want = am.unwrap().get(1).filter(|s| !s.is_empty()).unwrap_or("_").to_string();
        ps[0].0 == want
    }

    /// `ext_of`-free helper for callers outside (keeps `ext_of` imported).
    pub(super) fn ext(&self, rel: &str) -> String {
        ext_of(rel).to_string()
    }

    pub(super) fn is_ext_val(v: &Option<Val>) -> bool {
        is_ext(v)
    }
}
