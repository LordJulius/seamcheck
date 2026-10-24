//! The brace languages (lang_other.parse_brace): JS / TS / QML, Rust,
//! Kotlin, Java, C#, Go, C / C++, Swift, PHP. One walker reads them all:
//! function spans by brace matching on blanked code, classes as modules of
//! their own (`<file>.<Class>`, or the file's module when the class is named
//! like the file), declared types per line and per class, each function's
//! calls with their receiver chains, and what each import binds.
//!
//! Positions are byte offsets (the blanked code keeps the text's bytes in
//! place); `Call.col` is converted to Python's character column at the end.

mod classes;
mod extra;
mod imports;
mod pats;
mod returns;
mod typed;
mod util;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

use self::classes::{classes, Cls};
use self::extra::{bind_closure, bind_lambdas, class_initializers, js_reassignments, macro_defined, pack_params, qml_types, top_calls};
use self::pats::{call_rx, call_skip, not_func, CPP_EXTS};
use self::returns::{kotlin_body, loop_bindings, own_return, return_text, returns, rs_arms};
use self::typed::{cpp_not_type, elements, go_kw, go_new, kt_field, kt_new, ts_new, typed};
use self::util::*;
use crate::lang::assign::assignments;
use crate::lang::chains::{elem_type, expression, generic_name, receiver, top_commas, type_name, Opts};
use crate::lang::lambdas::{self, matching, param_types, split_top};
use crate::lang::{Adapter, ParseCtx};
use crate::model::{Call, Func, Module};
use crate::scan::blank;

pub struct Brace;

impl Adapter for Brace {
    /// (each extension's own family: project::brace_family)
    fn family(&self) -> &'static str {
        "brace"
    }
    fn exts(&self) -> &'static [&'static str] {
        pats::EXTS
    }
    fn parse(&self, _ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module> {
        parse(rel, text)
    }
}

/// Line starts of a text, for `line_of`.
pub struct Idx {
    pub starts: Vec<usize>,
}

impl Idx {
    pub fn new(text: &str) -> Idx {
        let mut starts = vec![0];
        for (k, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(k + 1);
            }
        }
        Idx { starts }
    }
    /// The 0-based line holding byte `pos`.
    pub fn of(&self, pos: usize) -> usize {
        self.starts.partition_point(|&s| s <= pos) - 1
    }
}

const TRAILING_SKIP: &[&str] = &[
    "get", "set", "init", "apply", "run", "with", "use", "synchronized", "async", "launch", "if", "else", "for", "while",
    "guard", "switch", "do", "catch", "defer", "repeat", "when", "try", "func", "fun", "willSet", "didSet", "return",
    "in", "class", "struct", "enum", "extension", "protocol", "object", "companion", "interface", "finally", "lazy",
    "inline", "where", "throws", "rethrows", "await", "some", "any", "private", "public", "internal", "open", "val",
    "var", "let", "is", "as", "deinit", "subscript", "operator", "actor", "mutating", "nonmutating", "override",
    "static", "final", "unowned", "weak", "convenience", "required", "dynamic", "indirect",
];

const TS_BUILTIN_TYPES: &[&str] = &[
    "Map", "Set", "WeakMap", "WeakSet", "Array", "ReadonlyArray", "ReadonlyMap", "ReadonlySet", "Record", "Promise",
    "string", "number", "boolean", "bigint", "symbol", "object", "unknown", "any", "never", "Uint8Array", "Uint16Array",
    "Uint32Array", "Float32Array", "Float64Array", "ArrayBuffer", "DataView", "Date", "RegExp", "Error", "Function",
    "Blob", "File", "URL", "Headers", "Request", "Response", "HTMLElement", "Element", "Node", "Event", "MouseEvent",
    "KeyboardEvent", "PointerEvent", "Iterable",
];

fn rs_wrapped(s: &str) -> bool {
    re!(r"^\s*(?:&\s*)?(?:(?:std::)?(?:option|result|io|fmt)::)?(?:Option|Result)\s*<").match_start(s).is_some()
}

fn literal(tk: &str, first: &str) -> Option<&'static str> {
    Some(match (tk, first) {
        ("js", "[") | ("swift", "[") | ("rust", "[") => "Array",
        ("js", "\"") | ("js", "'") | ("js", "`") | ("swift", "\"") => "String",
        ("rust", "\"") => "str",
        ("rust", "v") => "Vec",
        _ => return None,
    })
}

fn field_assign(tk: &str) -> Option<&'static crate::pyre::Rx> {
    Some(match tk {
        "kotlin" => re!(r"\b(?:val|var)\s+([A-Za-z_]\w*)\s*=(?!=)"),
        "swift" => re!(r"\b(?:let|var)\s+([A-Za-z_]\w*)\s*=(?!=)"),
        "js" => re!(r"^\s*(?:(?:private|public|protected|readonly|static|declare|override|export|const|let|var)\s+)*#?([A-Za-z_$][\w$]*)\s*[?!]?\s*(?::[^=;\n]+)?=(?![=>])"),
        "php" => re!(r"(?:private|public|protected|var)\s+(?:readonly\s+)?(?:\??[\\\w]+\s+)?(\$\w+)\s*=(?!=)"),
        "jvm" => re!(r"\bvar\s+([A-Za-z_]\w*)\s*=(?!=)"),
        _ => return None,
    })
}

fn word_only(s: &str) -> bool {
    re!(r"^[\w$]+$").is_match(s)
}

/// A field's declared types as a set kept in insertion order.
type Fields = IndexMap<String, IndexMap<String, Vec<String>>>;

struct P<'a> {
    rel: &'a str,
    ext: &'a str,
    raw: Arc<Vec<String>>,
    lines: Arc<Vec<String>>,
    stem: String,
    plain: HashSet<String>,
    classes: Vec<Cls>,
    /// every module of the file; the first is the file's own
    all: IndexMap<String, Module>,
}

impl<'a> P<'a> {
    fn new_sub(&self, name: &str, decls: Vec<String>) -> Module {
        let file = &self.all[0];
        let mut m = Module::new(name, self.rel, self.raw.clone(), self.lines.clone());
        m.owner = file.name.clone();
        m.decls = decls;
        m.aliases = file.aliases.clone();
        m.uses = file.uses.clone();
        m.imported = file.imported.clone();
        m
    }

    fn ckey(&self, c: &Cls) -> String {
        if self.ext == ".cs" && c.arity > 0 {
            format!("{}`{}", c.name, c.arity)
        } else {
            c.name.clone()
        }
    }

    /// Classes whose body holds `pos`, outermost first.
    fn enclosing(&self, pos: usize) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.classes.len())
            .filter(|&i| self.classes[i].span.map(|(a, b)| a < pos && pos <= b).unwrap_or(false))
            .collect();
        v.sort_by_key(|&i| self.classes[i].span.unwrap().0);
        v
    }

    /// The module a chain of nested classes lands in (`class_module`): its index in `all`.
    fn class_module(&mut self, chain: &[usize]) -> usize {
        let names: Vec<String> =
            chain.iter().filter(|&&i| !self.classes[i].name.is_empty()).map(|&i| self.ckey(&self.classes[i])).collect();
        if names.is_empty() {
            return 0;
        }
        let n0 = names[0].split('`').next().unwrap_or("").to_string();
        let file_cls = stem_key(if !self.plain.contains(&n0) { &n0 } else { &names[0] }) == self.stem;
        let last_c = &self.classes[*chain.last().unwrap()];
        let home = if names.len() == 1 && file_cls {
            0
        } else {
            let path = if file_cls { &names[1..] } else { &names[..] };
            let mname = format!("{}.{}", self.all[0].name, path.join("."));
            match self.all.get_index_of(&mname) {
                Some(i) => i,
                None => {
                    let arity = if !last_c.name.is_empty() { last_c.arity } else { 0 };
                    let last_ = names.last().unwrap().split('`').next().unwrap_or("").to_string();
                    let d = if arity > 0 { format!("{last_}`{arity}") } else { last_ };
                    let m = self.new_sub(&mname, vec![d]);
                    self.all.insert(mname, m);
                    self.all.len() - 1
                }
            }
        };
        let k = chain.len();
        let mut last = if !last_c.name.is_empty() { chain[k - 1] } else if k > 1 { chain[k - 2] } else { chain[k - 1] };
        if self.classes[last].kw == "companion" {
            last = if k > 1 { chain[k - 2] } else { last };
        }
        let lname = names.last().unwrap().clone();
        let bases = self.classes[last].bases.clone();
        let lkind = self.classes[last].kind.clone();
        {
            let hm = &mut self.all[home];
            for b in bases {
                if !hm.bases.contains(&b) && b != lname {
                    hm.bases.push(b);
                }
            }
        }
        if home != 0
            && lkind == "ext"
            && !self.classes.iter().any(|c| self.ckey(c) == lname && c.kind != "ext")
        {
            self.all[home].kind = "ext".into();
        } else if home != 0 {
            let hm = &mut self.all[home];
            if hm.kind.is_empty() {
                hm.kind = if lkind == "interface" { "interface".into() } else { "class".into() };
            }
        } else {
            let arity = if !last_c.name.is_empty() { last_c.arity } else { 0 };
            let plain_has = |s: &str| self.plain.contains(s);
            let last_ = lname.split('`').next().unwrap_or("").to_string();
            let keep_plain = plain_has(&last_);
            let fm = &mut self.all[0];
            if fm.kind.is_empty() {
                fm.kind = "class".into();
            }
            if arity > 0 {
                fm.decls.retain(|d| *d != last_ || keep_plain);
                fm.decls.push(format!("{last_}`{arity}"));
            } else if !fm.decls.contains(&lname) {
                fm.decls.push(lname);
            }
        }
        home
    }
}

/// One file's modules (lang_other.parse_brace).
pub fn parse(rel: &str, text: &str) -> Vec<Module> {
    let base = basename(rel);
    let ext_owned = match base.rfind('.') {
        Some(k) => base[k..].to_string(),
        None => format!(".{base}"),
    };
    let ext = ext_owned.as_str();
    let Some((fam, fpats)) = pats::lang(ext) else { return Vec::new() };
    let o = Opts::for_file(rel);
    let (code0, strings) = blank(text, fam, false);
    let mut code = code0;
    if ext == ".cs" {
        // preprocessor lines (`#pragma warning disable` between a signature and its `=>`)
        code = re!(r"(?m)^[ \t]*#[ \t]*(?:pragma|region|endregion|nullable|if|elif|else|endif|define|undef|warning|error|line)\b[^\n]*")
            .sub_with(&code, |m| " ".repeat(m.group0().len()));
    }
    if matches!(ext, ".swift" | ".kt" | ".kts") {
        // `default`, `is`: a quoted identifier is the identifier (padded to keep the bytes in place)
        code = re!(r"`([^`\n]+)`").sub_with(&code, |m| {
            let id = re!(r"\W").sub("_", m.s(1));
            let pad = m.group0().len() - id.len() - 2;
            format!(" {id} {}", " ".repeat(pad))
        });
    }
    let code = code.as_str();
    let raw: Arc<Vec<String>> = Arc::new(text.split('\n').map(String::from).collect());
    let lines: Arc<Vec<String>> = Arc::new(code.split('\n').map(String::from).collect());
    let idx = Idx::new(code);
    let line_start = &idx.starts;
    let modname = match rel.rfind('.') {
        Some(k) => &rel[..k],
        None => rel,
    };
    let mut fm = Module::new(modname, rel, raw.clone(), lines.clone());
    let go = ext == ".go";
    let swift = ext == ".swift";
    let php = ext == ".php";
    let cpp = CPP_EXTS.contains(&ext);
    let qml = ext == ".qml";
    let kotlin = fam == "kotlin";
    // (C++: only definitions declare a type; `class QMouseEvent;` names one defined elsewhere)
    if !cpp {
        let pat = if go {
            re!(r"\btype\s+([A-Za-z_]\w*)(?:\[[^\]]*\])?\s+(?!=)")
        } else if swift {
            re!(r"\b(?:class|struct|enum|protocol|actor|typealias)\s+([A-Z]\w*)")
        } else {
            re!(r"\b(?:class|object|interface|enum|struct|record|trait|type)\s+([A-Z][\w]*)")
        };
        let mut d: Vec<String> = pat.find_iter(code).iter().map(|m| m.s(1).to_string()).collect();
        d.sort();
        d.dedup();
        fm.decls = d;
    }
    let mut type_imports: Vec<String> = Vec::new();
    if fam == "js" {
        // `type X` declares only as `type X = ...`, and an alias of a runtime type is that library type
        let mut aliases: HashMap<String, String> = HashMap::new();
        for m in re!(r"\btype\s+([A-Z]\w*)\s*(?:<[^=;]*?>)?\s*=\s*[|&]?\s*([A-Za-z_$][\w$]*)").find_iter(code) {
            aliases.insert(m.s(1).to_string(), m.s(2).to_string());
        }
        let mut others: HashSet<String> =
            re!(r"\b(?:class|interface|enum)\s+([A-Z]\w*)").find_iter(code).iter().map(|m| m.s(1).to_string()).collect();
        for m in re!(r"\btype\s+([A-Z][\w$]*)\s*(?:<[^=;{]*(?:=[^;{]*)?>)?\s*=\s*\{").find_iter(code) {
            others.insert(m.s(1).to_string());
        }
        fm.decls.retain(|d| {
            others.contains(d) || aliases.get(d).map(|a| !TS_BUILTIN_TYPES.contains(&a.as_str())).unwrap_or(false)
        });
        imports::js_imports(&mut fm, rel, text, &lines, &idx, &mut type_imports);
        for em in re!(r"(?m)^[ \t]*(?:module\.)?exports\.([A-Za-z_$][\w$]*)\s*=\s*([A-Za-z_$][\w$]*)(?:\.([A-Za-z_$][\w$]*))?\s*;?[ \t]*$")
            .find_iter(code)
        {
            if em.get(3).is_none() {
                // `exports.Router = Router`: whatever that name is here
                fm.bound.entry(em.s(1).to_string()).or_insert(format!("reexport:{}", em.s(2)));
            }
        }
        imports::js_reexports(&mut fm, rel, text, &lines, &idx);
        // the `delegates` package: `delegate(proto, 'response').method('redirect').access('body')`
        for dm in re!(r#"\bdelegate\(\s*[\w$.]+\s*,\s*['"]([\w$]+)['"]\s*\)((?:\s*\.\s*(?:method|access|getter|setter|fluent)\(\s*['"][\w$]+['"]\s*\))+)"#)
            .find_iter(text)
        {
            for nm in re!(r#"\(\s*['"]([\w$]+)['"]"#).find_iter(dm.s(2)) {
                fm.bound.entry(nm.s(1).to_string()).or_insert(format!("deleg:{}", dm.s(1)));
            }
        }
        if text.contains("require(") || text.contains("module.exports") {
            imports::cjs(&mut fm, rel, text, &lines, &idx);
        }
    } else if kotlin || ext == ".java" {
        imports::kt_imports(&mut fm, text, &lines, &idx);
    } else if ext == ".cs" {
        imports::cs_imports(&mut fm, code);
    } else if go {
        imports::go_imports(&mut fm, rel, text, code);
    } else if ext == ".rs" {
        imports::rs_imports(&mut fm, rel, code);
        let name = fm.name.clone();
        // `mod convert { pub fn string(...) }` inline: `convert::string(v)` is this file's
        for im in re!(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z_]\w*)\s*\{").find_iter(code) {
            if im.s(1) != "tests" && im.s(1) != "test" {
                fm.bound.entry(im.s(1).to_string()).or_insert(format!("mod:{name}"));
            }
        }
        // `pub static PROJECT_DIRS: Lazy<BatProjectDirs> = ...`: a module-level value of that type
        for sm in re!(r"(?m)^(?:pub(?:\([^)]*\))?\s+)?(?:static|const)\s+(?:mut\s+)?([A-Z_][A-Z0-9_]*)\s*:\s*([^=;]+?)\s*=")
            .find_iter(code)
        {
            let t = type_name(sm.s(2));
            if !t.is_empty() {
                fm.bound.entry(sm.s(1).to_string()).or_insert(format!("type:{t}"));
            }
        }
        // `type Range = Match;`: another name for Match
        for am in re!(r"(?m)^(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Z]\w*)\s*(?:<[^=;]*>)?\s*=\s*([^;]+);").find_iter(code) {
            let t = type_name(am.s(2));
            if !t.is_empty() && t != am.s(1) {
                fm.bound.entry(am.s(1).to_string()).or_insert(format!("type:{t}"));
            }
        }
    } else if swift {
        imports::swift_imports(&mut fm, rel, code);
    } else if php {
        imports::php_imports(&mut fm, code);
    } else if cpp {
        for gm in re!(r"\bQ_GLOBAL_STATIC(?:_WITH_ARGS)?\s*\(\s*([\w:<>]+)\s*,\s*(\w+)").find_iter(code) {
            fm.bound.entry(gm.s(2).to_string()).or_insert(format!("type:{}", type_name(gm.s(1))));
        }
        // `#include "x.h"` (core resolves it): its free functions are in scope
        let d = dir_of(rel);
        for im in re!(r#"(?m)^[ \t]*#[ \t]*include[ \t]*"([^"]+)""#).find_iter(text) {
            fm.uses.push(format!("inc:{d}\0{}", im.s(1)));
        }
    }
    let _ = type_imports;

    // --- function spans -------------------------------------------------------------
    let n = code.len();
    let cb = code.as_bytes();
    let mut seen: HashSet<(usize, String)> = HashSet::new();
    let mut found: Vec<(usize, usize, String, usize, isize)> = Vec::new();
    for pat in fpats {
        let ts_start = pat.tsstart;
        for m in pat.rx.find_iter(code) {
            let ng = if ts_start { 2 } else { 1 };
            let mut name = m.s(ng).to_string();
            if pat.dflt {
                name = "default".into();
            }
            if not_func(&name) && !(fam == "rust" && name == "new") {
                continue;
            }
            let npos = m.span(ng).map(|s| s.0).unwrap_or(m.start());
            let start_line = idx.of(npos);
            if seen.contains(&(start_line, name.clone())) {
                continue;
            }
            if !ts_start {
                seen.insert((start_line, name.clone()));
            }
            let (mend, mstart) = (m.end(), m.start());
            let mut k = mend;
            let last_ch = cb[mend - 1];
            if last_ch == b'{' {
                k -= 1;
            }
            let mut depth: i64 = if last_ch == b'(' { 1 } else { 0 };
            let (mut body_start, mut body_end): (Option<usize>, Option<usize>) = (None, None);
            if qml && name.starts_with("on") {
                // a handler: `onX: {` block, or the rest of the line
                let nl = find_from(code, "\n", mend);
                let nl = if nl < 0 { n } else { nl as usize };
                let brace = find_in(code, "{", mend, nl);
                if brace >= 0 && strip(sl(code, mend, brace as usize)).is_empty() {
                    k = brace as usize;
                    depth = 0;
                    body_start = Some(brace as usize);
                } else {
                    body_end = Some(if nl > 0 { nl - 1 } else { mend });
                    k = n;
                }
            }
            if kotlin {
                let (bs, be) = kotlin_body(code, text, mend, n);
                body_start = bs;
                body_end = be;
                if bs.is_none() && be.is_none() {
                    body_end = Some(mend);
                }
                k = n; // skip the generic scan
            }
            let limit = ahead(text, mend, 4000); // a signature is never longer than this
            while k < limit {
                let ch = cb[k];
                if ch == b'(' || ch == b'[' {
                    depth += 1;
                } else if ch == b')' || ch == b']' {
                    depth -= 1;
                } else if depth <= 0 && ch == b'{' {
                    if fam == "js"
                        && !qml
                        && matches!(prev_char(rstrip(&code[..k]), rstrip(&code[..k]).len()), ":" | "&" | "|" | "," | "<")
                        && sl(code, mend - 1, k).contains(')')
                    {
                        let e = matching(code, k); // a type literal (`): R & { get(): R } {`), not the body
                        if e > 0 {
                            k = e as usize + 1;
                            continue;
                        }
                    }
                    body_start = Some(k);
                    break;
                } else if depth <= 0 && ch == b';' {
                    body_end = Some(k); // a declaration without a body (trait/interface) or an expression body
                    break;
                } else if depth <= 0
                    && ch == b'\n'
                    && sl(code, mstart, k).contains("=>")
                    && !{
                        let w = rb(code, text, k, 8);
                        let w = if k - w.len() < mstart { sl(code, mstart, k) } else { w };
                        rstrip(w).ends_with("=>")
                    }
                    && ext != ".cs"
                {
                    body_end = Some(k);
                    break;
                } else if depth <= 0
                    && ch == b'\n'
                    && swift
                    && !starts_any(lstrip(rw(code, text, k + 1, 79)), &["{", "->", "throws", "rethrows", "async", "where"])
                    && !ends_any(rstrip(sl(code, mstart, k)), &["->", ",", "throws", "async", "where", "&", ":"])
                {
                    body_end = Some(k); // a protocol requirement: no body
                    break;
                }
                k += 1;
            }
            if let Some(bs) = body_start {
                let (mut d, mut k2) = (0i64, bs);
                while k2 < n {
                    if cb[k2] == b'{' {
                        d += 1;
                    } else if cb[k2] == b'}' {
                        d -= 1;
                        if d == 0 {
                            break;
                        }
                    }
                    k2 += 1;
                }
                body_end = Some(k2);
            }
            if cpp && body_start.is_none() {
                continue; // a declaration, prototype or `Type var(args);`: not a function
            }
            if cpp {
                let s1 = m.span(1).unwrap().0;
                let ls = line_begin(code, s1);
                let prev = rstrip(&code[..ls]);
                if rstrip(sl(code, ls, s1)).ends_with(',')
                    || (ends_any(prev, &[",", "(", "&&", "||", "="]) && !ends_any(prev, &["==", "!=", "<=", ">="]))
                {
                    continue; // an argument line of a call, not a definition
                }
            }
            if qml && name.starts_with("on") && body_start.is_none() {
                let e = find_from(code, "\n", mend); // `onX: expr` on one line
                body_end = Some(if e < 0 { n - 1 } else { e as usize });
            }
            if ts_start {
                let close = matching(code, mend - 1);
                let after: Option<&str> = match (close > 0, body_start) {
                    (true, Some(bs)) => Some(strip(sl(code, close as usize + 1, bs))),
                    _ => None,
                };
                let skip = match after {
                    None => true,
                    Some(a) => (!a.is_empty() && !a.starts_with(':')) || count_nl(code, mend, body_start.unwrap()) > 40,
                };
                if skip {
                    continue; // a call statement (`foo(\n  a,\n)`), not a method
                }
            }
            seen.insert((start_line, name.clone()));
            let be = body_end.unwrap_or_else(|| ahead(text, mend, 200).min(n.saturating_sub(1)));
            found.push((start_line, idx.of(be), name, npos, body_start.map(|b| b as isize).unwrap_or(-1)));
        }
    }
    if cpp {
        found = macro_defined(code, text, &idx, found);
    }
    let mut tests: HashSet<(usize, String)> = HashSet::new();
    if fam == "js" {
        // it("...") / test("...") blocks: anonymous callbacks, named by their label
        let mut first_str: HashMap<usize, String> = HashMap::new();
        for (ln, st) in &strings {
            first_str.entry(ln - 1).or_insert(st.clone());
        }
        for m in re!(r"(?m)^[ \t]*(?:it|test)(?:\.(?:only|skip|todo|concurrent|each\([^)]*\)))?[ \t]*\(").find_iter(code) {
            let start_line = idx.of(m.start());
            let label = first_str.get(&start_line).cloned().unwrap_or_else(|| format!("line {}", start_line + 1));
            let (mut k, mut depth) = (m.end() - 1, 0i64);
            while k < n {
                let c = cb[k];
                if c == b'(' || c == b'[' || c == b'{' {
                    depth += 1;
                } else if c == b')' || c == b']' || c == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                k += 1;
            }
            let name = format!("test {label}");
            found.push((start_line, idx.of(k), name.clone(), m.start(), -1));
            tests.insert((start_line, name));
        }
    }
    let jsx = ext == ".tsx" || ext == ".jsx";
    let tk: &str = if cpp {
        "cpp"
    } else if ext == ".java" || ext == ".cs" {
        "jvm"
    } else if go {
        "go"
    } else if swift {
        "swift"
    } else if php {
        "php"
    } else {
        fam
    };

    // --- which class each function belongs to ----------------------------------------
    let cls_list = if go || qml { Vec::new() } else { classes(code, text, ext, fam) };
    let mut plain: HashSet<String> = HashSet::new();
    if ext == ".cs" {
        // a generic class goes by `Name`N`, not by the plain name another class may hold
        plain = cls_list.iter().filter(|c| c.arity == 0).map(|c| c.name.clone()).collect();
        let gen: HashSet<String> =
            cls_list.iter().filter(|c| c.arity > 0).map(|c| c.name.clone()).filter(|x| !plain.contains(x)).collect();
        fm.decls.retain(|d| !gen.contains(d));
    }
    let mut cls_list = cls_list;
    for c in cls_list.iter_mut() {
        match c.span {
            Some((a, b)) => {
                c.a = idx.of(a);
                c.b = idx.of(b);
            }
            None => {
                c.a = idx.of(c.pos);
                c.b = c.a;
            }
        }
    }
    let mut fspans: Vec<(usize, usize, String, usize)> = found.iter().map(|f| (f.0, f.1, f.2.clone(), f.3)).collect();
    fspans.sort_by(|x, y| (x.0, std::cmp::Reverse(x.1)).cmp(&(y.0, std::cmp::Reverse(y.1))));
    let stem = stem_key(splitext0(basename(rel)));
    let mut all = IndexMap::new();
    all.insert(fm.name.clone(), fm);
    let mut p = P {
        rel,
        ext,
        raw: raw.clone(),
        lines: lines.clone(),
        stem: stem.clone(),
        plain,
        classes: cls_list,
        all,
    };

    // every class gets its module, methods or not (fields, an interface)
    for ci in 0..p.classes.len() {
        if p.classes[ci].name.is_empty() || p.classes[ci].kw == "companion" {
            continue;
        }
        let mut chain = p.enclosing(p.classes[ci].pos);
        chain.push(ci);
        let home = p.class_module(&chain);
        let c = &p.classes[ci];
        if matches!(c.kw.as_str(), "class" | "struct" | "interface" | "record" | "protocol" | "trait") {
            let w = rw(code, text, c.pos, 300);
            let after = match w.find(c.name.as_str()) {
                Some(k) => &w[k + c.name.len()..],
                None => w,
            };
            if let Some(gp) = re!(r"\s*<([^<>]*(?:<[^<>]*>[^<>]*)*)>").match_start(after) {
                // `final class Protected<Value>`: its type parameters, in order
                let names: Vec<String> = split_top(gp.s(1), ',')
                    .iter()
                    .map(|g| re!(r"[\s:=]").split(strip(g))[0].to_string())
                    .collect();
                if !names.is_empty() && names.iter().all(|x| re!(r"^[A-Z]\w*$").is_match(x)) {
                    p.all[home].bound.entry("<>".into()).or_insert(names.join(","));
                }
            }
        }
    }
    if ext == ".cs" {
        // a class with a module of its own is not the file's
        let own: HashSet<String> = p.all.values().skip(1).flat_map(|m| m.decls.iter().cloned()).collect();
        let stemmed: HashSet<String> =
            p.classes.iter().filter(|c| stem_key(&c.name) == stem).map(|c| c.name.clone()).collect();
        p.all[0].decls.retain(|d| !own.contains(d) || stemmed.contains(d.split('`').next().unwrap_or("")));
    }

    // Go methods by receiver, C++ `Foo::bar` bodies by their class, the rest by class body
    let mut spans: IndexMap<(String, String), Vec<(usize, usize, usize, isize)>> = IndexMap::new();
    let mut owners: HashMap<(String, String), (usize, Option<usize>)> = HashMap::new();
    let mut sorted_found = found.clone();
    sorted_found.sort();
    for (a_, b_, name, npos, bpos) in &sorted_found {
        let (a_, b_, npos, bpos) = (*a_, *b_, *npos, *bpos);
        let mut recv = String::new();
        let (mut home, mut cls): (usize, Option<usize>) = (0, None);
        if go {
            if let Some(rm) = re!(r"^func\s*\(\s*(?:\w+\s+)?\*?\s*([A-Za-z_]\w*)").match_start(&lines[a_]) {
                recv = rm.s(1).to_string();
            }
        } else if cpp {
            let pat = rxd(&format!(r"\b([A-Za-z_]\w*)\s*(?:<[^<>]*>)?\s*::\s*~?{}\s*\(", esc(name)));
            if let Some(cm) = pat.search(&lines[a_]) {
                if p.enclosing(npos).is_empty() {
                    recv = cm.s(1).to_string(); // `Foo::bar(` outside a class body
                }
            }
        }
        if !recv.is_empty() && cpp && stem_key(&recv) == stem {
            // `Foo::bar` in Foo.cpp: the file is the class
            let fm = &mut p.all[0];
            if !fm.decls.contains(&recv) {
                fm.decls.push(recv.clone());
            }
            if fm.kind.is_empty() {
                fm.kind = "class".into();
            }
        } else if !recv.is_empty() {
            let mname = format!("{}.{recv}", p.all[0].name);
            home = match p.all.get_index_of(&mname) {
                Some(i) => i,
                None => {
                    let mut m = p.new_sub(&mname, vec![recv.clone()]);
                    m.kind = if cpp { "ext".into() } else { String::new() };
                    p.all.insert(mname, m);
                    p.all.len() - 1
                }
            };
        } else if !tests.contains(&(a_, name.clone())) {
            // the module a function at npos belongs to: its innermost class, unless a function holds it
            let chain = p.enclosing(npos);
            if !chain.is_empty() {
                let inner = *chain.last().unwrap();
                let member = rb(code, text, npos, 5) == "this.";
                let ispan0 = p.classes[inner].span.unwrap().0;
                let local = fspans
                    .iter()
                    .any(|(fa, fb, _, fp)| *fp < npos && *fa <= a_ && a_ <= *fb && *fp > ispan0 && (*fa, *fp) != (a_, npos) && !member);
                if !local {
                    home = p.class_module(&chain);
                    cls = Some(inner);
                }
            }
            if fam == "js" && p.all[home].kind == "interface" && bpos < 0 {
                continue; // an interface's member `applyOp: (op: Op) => void;` is a type, not a function
            }
        }
        let key = (p.all[home].name.clone(), name.clone());
        spans.entry(key.clone()).or_default().push((a_, b_, npos, bpos));
        owners.insert(key, (home, cls));
    }

    // --- declared types per line, and fields --------------------------------------------
    let mut typed_by_line: HashMap<usize, Vec<(String, String)>> = HashMap::new();
    for (i, ln) in lines.iter().enumerate() {
        if ln.contains(':') || tk != fam || ln.contains('=') || php {
            let mut got = typed(tk, ln);
            got.extend(elements(tk, ln));
            if !got.is_empty() {
                typed_by_line.insert(i, got);
            }
        }
    }
    let mut inside = vec![false; lines.len()];
    for f in &found {
        for i in f.0..(f.1 + 1).min(lines.len()) {
            inside[i] = true;
        }
    }
    let mut fields: Fields = IndexMap::new();
    let enum_lines: HashSet<usize> = p
        .classes
        .iter()
        .filter(|c| c.kw == "enum" && c.span.is_some())
        .flat_map(|c| (c.a + 1)..(c.b + 1))
        .collect();
    let mut line_class: HashMap<usize, usize> = HashMap::new();
    for ci in 0..p.classes.len() {
        if p.classes[ci].name.is_empty() || p.classes[ci].span.is_none() {
            continue;
        }
        let mut chain = p.enclosing(p.classes[ci].pos);
        chain.push(ci);
        let home = p.class_module(&chain);
        for i in p.classes[ci].a..=p.classes[ci].b {
            line_class.insert(i, home); // (nested classes come later and win)
        }
    }
    if fam == "js" {
        // an interface member typed by an object literal (`h: {\n  app: App;\n}`): a type of its own
        for (i, ln) in lines.iter().enumerate() {
            let Some(om) = re!(r"^\s*(?:readonly\s+)?([A-Za-z_$][\w$]*)\s*\??\s*:\s*\{\s*$").match_start(ln) else {
                continue;
            };
            let Some(&owner) = line_class.get(&i) else { continue };
            if p.all[owner].kind != "interface" || inside[i] {
                continue;
            }
            let g1end = om.span(1).unwrap().1;
            let brace = g1end + ln[g1end..].find('{').unwrap();
            let e = matching(code, line_start[i] + brace);
            if e < 0 {
                continue;
            }
            let sub = format!("{}.{}", p.all[owner].name, om.s(1));
            let sub_i = match p.all.get_index_of(&sub) {
                Some(si) => si,
                None => {
                    let own = if p.all[owner].owner.is_empty() { p.all[0].name.clone() } else { p.all[owner].owner.clone() };
                    let mut m = p.new_sub(&sub, Vec::new());
                    m.owner = own;
                    m.kind = "interface".into();
                    p.all.insert(sub.clone(), m);
                    p.all.len() - 1
                }
            };
            let oname = p.all[owner].name.clone();
            add(fields.entry(oname).or_default().entry(om.s(1).to_string()).or_default(), format!("\0mod:{sub}"));
            for j in (i + 1)..=idx.of(e as usize) {
                line_class.insert(j, sub_i);
            }
        }
    }
    for ci in 0..p.classes.len() {
        // Kotlin primary constructors: `class Foo(private val repo: Repo)`
        if p.classes[ci].name.is_empty() || fam != "kotlin" {
            continue;
        }
        let mut chain = p.enclosing(p.classes[ci].pos);
        chain.push(ci);
        let home = p.class_module(&chain);
        let hname = p.all[home].name.clone();
        let hdr = p.classes[ci].hdr.clone();
        for m in kt_field().find_iter(&hdr) {
            add(fields.entry(hname.clone()).or_default().entry(m.s(1).to_string()).or_default(), m.s(2).to_string());
        }
    }
    let mut expr_fields: IndexMap<String, IndexMap<String, Vec<String>>> = IndexMap::new();
    let fpat = field_assign(tk);
    let mut ts_enum_lines: HashSet<usize> = HashSet::new();
    if fam == "js" {
        // TS `enum Kind { ZodString = "ZodString" }`: members, not fields of the file
        for em in re!(r"\benum\s+[A-Za-z_$][\w$]*\s*\{").find_iter(code) {
            let e = matching(code, em.end() - 1);
            if e > 0 {
                for i in idx.of(em.end())..=idx.of(e as usize) {
                    ts_enum_lines.insert(i);
                }
            }
        }
    }
    let file_name = p.all[0].name.clone();
    for (i, ln) in lines.iter().enumerate() {
        let ln = ln.as_str();
        let home = line_class.get(&i).copied().unwrap_or(0);
        let hname = p.all[home].name.clone();
        fields.entry(hname.clone()).or_default();
        if let Some(fpat) = fpat {
            if !inside[i] && !ts_enum_lines.contains(&i) {
                // `private val client = rule.newClient()`: a property's value says its type
                for m in fpat.find_iter(ln) {
                    let mut o_ = line_start[i] + m.end();
                    let w = rw(code, text, o_, 40);
                    o_ += w.len() - lstrip(w).len();
                    if fam == "js"
                        && home == 0
                        && cb.get(o_) == Some(&b'{')
                        && re!(r"\s*(?:export\s+)?(?:const|let|var)\b").match_start(ln).is_some()
                    {
                        let e = matching(code, o_);
                        let sub = format!("{file_name}.{}", m.s(1));
                        if e > 0 && !p.all.contains_key(&sub) {
                            // `export const coerce = { string: (arg) => ZodString.create(arg), ... }`
                            let mut om = p.new_sub(&sub, Vec::new());
                            om.kind = "interface".into();
                            let inner = sl(code, o_ + 1, e as usize);
                            for part in split_top(inner, ',') {
                                if let Some(km) = re!(r"\s*([A-Za-z_$][\w$]*)\s*:\s*\(*\s*(?:async\s+)?(?:\([^()]*\)|[A-Za-z_$][\w$]*)\s*=>\s*")
                                    .match_start(&part)
                                {
                                    let ch = expression(&part, km.end(), 40, o);
                                    if !ch.is_empty() {
                                        om.methods.push(km.s(1).to_string());
                                        om.bound.insert(format!("{}()", km.s(1)), format!("expr:{}", join(&ch)));
                                    }
                                }
                            }
                            if !om.methods.is_empty() || strip(inner).is_empty() {
                                p.all.insert(sub.clone(), om);
                                add(
                                    fields.entry(hname.clone()).or_default().entry(m.s(1).to_string()).or_default(),
                                    format!("\0mod:{sub}"),
                                );
                                continue;
                            }
                        }
                    }
                    let at = line_start[i] + m.end();
                    let ch = expression(code, at, 40, o);
                    if !ch.is_empty() && fam == "js" && (ch[0] == "require()" || ch[0] == "import()") {
                        continue; // `var express = require('..')`: the import handling knows what that is
                    }
                    if !ch.is_empty()
                        && fam == "js"
                        && re!(r"\s*(?:new\s+)?[\w$.<>]+\s*\((?:[^()]|\([^()]*\))*?(?:=>|\bfunction\b|\{)")
                            .match_start(rw(code, text, at, 400))
                            .is_some()
                    {
                        continue; // `create((set) => ({ applyOp }))`: a library object holding project callbacks
                    }
                    let key = if php { m.s(1).trim_start_matches('$').to_string() } else { m.s(1).to_string() };
                    if !ch.is_empty() {
                        expr_fields.entry(hname.clone()).or_default().entry(key).or_insert(ch);
                    } else if let Some(lit) = literal(tk, first(lstrip(rw(code, text, at, 40)))) {
                        // `let sample = """...`: a String
                        add(fields.entry(hname.clone()).or_default().entry(key).or_default(), lit.to_string());
                    }
                }
            }
        }
        if fam == "js" && !inside[i] && ln.contains('{') {
            // `const { h } = window;` at the top of a file
            let t = if rstrip(ln).ends_with(';') { ln.to_string() } else { format!("{ln};") };
            for (nm, held) in assignments("js", &t, o) {
                if held.starts_with("expr:") && held.split('\x1f').count() > 1 {
                    let segs: Vec<String> = held[5..].split('\x1f').map(String::from).collect();
                    expr_fields.entry(hname.clone()).or_default().entry(nm).or_insert(segs);
                }
            }
        }
        if fam == "js" && inside[i] && ln.contains("this.") {
            // `this.context = Object.create(context)` in the constructor
            let in_ctor = found.iter().any(|f| f.0 <= i && i <= f.1 && f.2 == "constructor");
            if in_ctor {
                for m in re!(r"^[ \t]*this\.([A-Za-z_$][\w$]*)[ \t]*=(?![=>])[ \t]*").find_iter(ln) {
                    let o_ = line_start[i] + m.end();
                    if cb.get(o_) == Some(&b'{') {
                        let e = matching(code, o_);
                        if e > 0 {
                            // `this.interceptors = { request: new InterceptorManager() }`: an object of those members
                            let sub = format!("{hname}.{}", m.s(1));
                            let has_field = fields.get(&hname).map(|f| f.contains_key(&sub)).unwrap_or(false);
                            if !p.all.contains_key(&sub) && !has_field {
                                let own = if p.all[home].owner.is_empty() { file_name.clone() } else { p.all[home].owner.clone() };
                                let mut om = p.new_sub(&sub, Vec::new());
                                om.owner = own;
                                om.kind = "interface".into();
                                for part in split_top(sl(code, o_ + 1, e as usize), ',') {
                                    let Some(km) = re!(r"\s*([A-Za-z_$][\w$]*)\s*:").match_start(&part) else { continue };
                                    let ch = expression(&part, km.end(), 40, o);
                                    if !ch.is_empty() && ch[0] != "require()" && ch[0] != "import()" {
                                        om.bound.insert(km.s(1).to_string(), format!("expr:{}", join(&ch)));
                                    }
                                }
                                if !om.bound.is_empty() {
                                    p.all.insert(sub.clone(), om);
                                    add(
                                        fields.entry(hname.clone()).or_default().entry(m.s(1).to_string()).or_default(),
                                        format!("\0mod:{sub}"),
                                    );
                                }
                            }
                            continue;
                        }
                    }
                    let ch = expression(code, o_, 40, o);
                    if !ch.is_empty() {
                        let c0 = &ch[0];
                        let fmod = &p.all[0];
                        if (c0 == "this"
                            || upper1(c0)
                            || c0.starts_with("new ")
                            || fmod.imported.contains_key(c0)
                            || fmod.aliases.contains_key(c0))
                            && c0 != "require()"
                            && c0 != "import()"
                        {
                            expr_fields.entry(hname.clone()).or_default().entry(m.s(1).to_string()).or_insert(ch);
                        }
                    }
                }
            }
        }
        let pats_: Vec<&crate::pyre::Rx> = if fam == "kotlin" {
            if inside[i] {
                vec![]
            } else {
                vec![kt_field(), kt_new()]
            }
        } else if fam == "js" {
            let param_field = re!(r"\b(?:private|public|protected|readonly)\s+([a-z_$][\w$]*)\s*\??\s*:\s*([A-Z][\w$]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>)?)(?![\w$.<\[])");
            let mut v = vec![param_field];
            if !inside[i] {
                v.push(re!(r"(?m)^\s*(?:(?:private|public|protected|readonly|static|declare)\s+)*([a-z_$][\w$]*)\s*[?!]?\s*:\s*([A-Z][\w$]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>)?)(?![\w$.<\[])\s*[;=]"));
                v.push(ts_new());
                v.push(re!(r"(?m)^\s*(?:export\s+)?(?:declare\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*:\s*([A-Z][\w$]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>)?)(?![\w$.<\[])"));
                v.push(re!(r"(?m)^\s*(?:(?:private|public|protected|readonly|static|declare)\s+)*([a-z_$][\w$]*)\s*[?!]?\s*:\s*InstanceType\s*<\s*typeof\s+([A-Z][\w$]*)\s*>\s*[;,]?\s*$"));
            }
            v
        } else {
            vec![]
        };
        let php_field = re!(r"(?:^\s*|[(,]\s*)(?:(?:public|private|protected|static|readonly|var)\s+)+\??([\\\w]+)\s+(\$\w+)");
        let got = |fields: &mut Fields, k: &str, v: String| {
            add(fields.entry(hname.clone()).or_default().entry(k.to_string()).or_default(), v);
        };
        if !inside[i] && matches!(tk, "cpp" | "jvm" | "kotlin" | "js" | "swift") {
            for (nm, ty) in elements(tk, ln) {
                got(&mut fields, &nm, ty); // `QHash<QString, Player*> m_players;`: its elements
            }
        }
        if fam == "js" {
            if !inside[i] && home != 0 && p.all[home].kind == "interface" && re!(r"^\s*(?:<[^()]*?>)?\s*\(").match_start(ln).is_some() {
                // a call signature: `<T>(data: T): Response`
                let o_ = line_start[i] + ln.find('(').unwrap();
                let e = matching(code, o_);
                if e > 0 {
                    if let Some(rm) = re!(r"\s*:\s*([^;{}\n]+)").match_start(rw(code, text, e as usize + 1, 299)) {
                        let t = type_name(rm.s(1));
                        if !t.is_empty() {
                            got(&mut fields, "()", t);
                        }
                    }
                }
            }
            let mm = if home != 0 && p.all[home].kind == "interface" {
                re!(r"^\s*(?:readonly\s+)?([A-Za-z_$][\w$]*)\s*\??\s*(?:(<[^()]*?>)?\s*\(|:\s*(?:<[^()]*?>\s*)?\()").match_start(ln)
            } else {
                None
            };
            if let Some(mm) = mm {
                if !inside[i] || line_class.get(&i) == Some(&home) {
                    // a member signature: `text(): Promise<string>;`, `get: <T>(url: Input) => ResponsePromise<T>;`
                    let nm = mm.s(1).to_string();
                    if !p.all[home].methods.contains(&nm) {
                        p.all[home].methods.push(nm.clone());
                    }
                    let o_ = line_start[i] + mm.end() - 1 + ln[mm.end() - 1..].find('(').unwrap();
                    let e = matching(code, o_);
                    if e > 0 {
                        if let Some(rm) = re!(r"\s*(?::|=>)\s*([^;{}\n]+)").match_start(rw(code, text, e as usize + 1, 299)) {
                            let t = type_name(rm.s(1));
                            if !t.is_empty() && t != "Promise" && t != "void" {
                                got(&mut fields, &format!("{nm}()"), t);
                            }
                        }
                    }
                }
            }
        } else if ext == ".cs" && !inside[i] {
            for m in re!(r"^\s*(?:\[[^\]\n]*\]\s*)*(?:(?:public|private|protected|internal|static|readonly|virtual|override|abstract|sealed|new|required|const|volatile|unsafe|extern|partial)\s+)*([A-Z][\w.]*(?:<[^;={}()]*>)?\??)\s+([A-Za-z_]\w*)\s*(?:\{\s*(?:get|set|init|private|protected|internal)\b|=>|=(?![=>])|;)")
                .find_iter(ln)
            {
                got(&mut fields, m.s(2), generic_name(m.s(1).trim_end_matches('?')));
            }
        } else if tk == "jvm" && !inside[i] {
            for m in re!(r"^\s*(?:(?:public|private|protected|internal|static|readonly|final|volatile|transient)\s+)*([A-Z]\w*)(?:<[^>;]*>)?\??\s+([a-z_]\w*)\s*(?:=[^;]*)?;")
                .find_iter(ln)
            {
                got(&mut fields, m.s(2), m.s(1).to_string());
            }
        } else if swift && !inside[i] {
            for m in re!(r"\b(?:let|var)\s+([A-Za-z_]\w*)\s*:\s*\(?(?:any\s+)?[A-Z][\w.]*\s*<(.+)>\s*[?!]?\)?\s*(?:[={]|$)").find_iter(ln) {
                got(&mut fields, &format!("{}<>", m.s(1)), format!("\0gen:{}", m.s(2))); // `Protected<MutableState>`
            }
            for m in re!(r"^\s*(?:(?:private|public|internal|fileprivate|open|static|class|lazy|weak|unowned|final|override|nonisolated)(?:\([^)]*\))?\s+)*(?:let|var)\s+([A-Za-z_]\w*)\s*(?::\s*\(?\s*(?:any\s+|some\s+)?(\[[^\]=\n]*\][?!]?|[A-Z][\w.]*)|=\s*([A-Z]\w*)\s*\()")
                .find_iter(ln)
            {
                let src = if !m.s(2).is_empty() { m.s(2) } else { m.s(3) };
                let t = type_name(src);
                if !t.is_empty() {
                    got(&mut fields, m.s(1), t);
                }
            }
        } else if php {
            for m in php_field.find_iter(ln) {
                let t = type_name(&m.s(1).replace('\\', "."));
                if !t.is_empty() && (!inside[i] || ln.contains("__construct")) {
                    got(&mut fields, m.s(2).trim_start_matches('$'), t);
                }
            }
            if inside[i] && ln.contains("$this->") {
                // `$this->repo = $repo;`: the parameter's declared type
                for m in re!(r"\$this->(\w+)\s*=\s*(\$\w+)\s*;").find_iter(ln) {
                    if let Some(fnd) = found.iter().find(|f| f.0 <= i && i <= f.1) {
                        let mut ts: Vec<String> = Vec::new();
                        for j in fnd.0..=i {
                            for (v, t) in typed_by_line.get(&j).map(|x| x.as_slice()).unwrap_or(&[]) {
                                if v == m.s(2) {
                                    add(&mut ts, t.clone());
                                }
                            }
                        }
                        if ts.len() == 1 {
                            for t in ts {
                                got(&mut fields, m.s(1), t);
                            }
                        }
                    }
                }
            }
        } else if tk == "go" && !inside[i] {
            if home == 0 {
                // `var echoCmd = &cobra.Command{`: a package-level value
                for m in go_new().find_iter(ln) {
                    if !go_kw(m.s(1)) {
                        let t = if !m.s(2).is_empty() { m.s(2) } else { m.s(3) };
                        got(&mut fields, m.s(1), t.to_string());
                    }
                }
            }
            for m in re!(r"^\s*(?:var\s+)?([A-Za-z_]\w*)\s+\*?((?:[A-Za-z_]\w*\.)?[A-Za-z_]\w*)\s*(?:`[^`]*`)?\s*$").find_iter(ln) {
                if !go_kw(m.s(1)) && !go_kw(m.s(2)) {
                    got(&mut fields, m.s(1), m.s(2).to_string());
                }
            }
        } else if tk == "rust" && !inside[i] && (home != 0 || line_class.contains_key(&i)) {
            if let Some(im) = re!(r"^\s*type\s+Item\s*=\s*([^;]+);").match_start(ln) {
                let t = type_name(im.s(1));
                if !t.is_empty() {
                    got(&mut fields, "type Item", t); // what iterating X gives
                }
            }
            if enum_lines.contains(&i) {
                // `Walkdir(walkdir::DirEntry),`: what that variant holds
                if let Some(vm) = re!(r"^\s*([A-Z]\w*)\s*\(\s*([^()]+?)\s*,?\s*\)\s*,?\s*$").match_start(ln) {
                    let t = type_name(vm.s(2));
                    if split_top(vm.s(2), ',').len() == 1 && !t.is_empty() {
                        got(&mut fields, &format!("::{}", vm.s(1)), t);
                    }
                }
            }
            for m in re!(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_]\w*)\s*:\s*([^,<]+?(?:<.*>)?[^,<]*?),?\s*$").find_iter(ln) {
                let t = type_name(m.s(2));
                if !t.is_empty() {
                    let v = if rs_wrapped(m.s(2)) { format!("opt:{t}") } else { t };
                    got(&mut fields, m.s(1), v);
                }
            }
        } else if cpp && !inside[i] {
            if let Some(qm) = re!(r"^\s*Q_PROPERTY\(\s*(?:const\s+)?((?:[\w:]+)(?:<[^()]*?>)?)\s*[\*&]?\s*([A-Za-z_]\w*)\s+(?:READ|MEMBER|BINDABLE)\b").match_start(ln) {
                let t = type_name(qm.s(1));
                if !t.is_empty() && !allcaps(&t) {
                    got(&mut fields, qm.s(2), t); // what QML reads as `obj.name`
                }
            }
            for m in re!(r"^\s*(?:(?:mutable|static|const|inline|volatile)\s+)*(?:::)?((?:\w+::)*[A-Za-z_]\w*(?:<[^;()]*>)?)\s*[\*&]?\s*([a-z_]\w*)\s*(?:=[^;]*|\{[^;]*\})?;")
                .find_iter(ln)
            {
                let g1 = m.s(1);
                let base_ = { let l = last_part(g1, "::"); l.split('<').next().unwrap_or(l) };
                if cpp_not_type(base_) || cpp_not_type(m.s(2)) {
                    continue; // (`return x;`, `delete p;`, `int n;`)
                }
                let mut t = type_name(g1);
                if g1.starts_with("std::") && g1.contains('<') {
                    t = g1.split('<').next().unwrap_or("").replace("::", "."); // a library container
                }
                if !t.is_empty() && !allcaps(&t) && t != "Q_OBJECT" && t != "Q_PROPERTY" {
                    got(&mut fields, m.s(2), t);
                }
            }
        }
        for pat_ in &pats_ {
            for m in pat_.find_iter(ln) {
                let g2 = m.s(2);
                let t = if g2.contains('<') { type_name(g2) } else { g2.to_string() };
                if !t.is_empty() {
                    got(&mut fields, m.s(1), t);
                }
            }
        }
        if php {
            for m in php_field.find_iter(ln) {
                let t = last_part(m.s(1), "\\");
                if !matches!(
                    t.to_lowercase().as_str(),
                    "string" | "int" | "float" | "bool" | "array" | "mixed" | "callable" | "iterable" | "object" | "void" | "null" | "false" | "true" | "self" | "static"
                ) {
                    got(&mut fields, m.s(2).trim_start_matches('$'), t.to_string());
                }
            }
        }
    }
    for (mname, fl) in &fields {
        let Some(ti) = p.all.get_index_of(mname) else { continue };
        let target = &mut p.all[ti];
        for (k, v) in fl {
            if v.len() == 1 {
                let x = &v[0];
                let val = match x.strip_prefix('\0') {
                    Some(r) => r.to_string(),
                    None => format!("type:{x}"),
                };
                target.bound.insert(k.clone(), val);
            }
        }
    }
    for (mname, fl) in &expr_fields {
        let Some(ti) = p.all.get_index_of(mname) else { continue };
        let target = &mut p.all[ti];
        for (k, ch) in fl {
            target.bound.entry(k.clone()).or_insert(format!("expr:{}", join(ch)));
        }
    }
    if go {
        // a type's methods see the fields of its struct
        let b = p.all[0].bound.clone();
        for m in p.all.values_mut().skip(1) {
            m.bound = b.clone();
        }
    }

    // --- functions ------------------------------------------------------------------
    let go_test = go && rel.ends_with("_test.go");
    let mut cpp_ns: Vec<(usize, usize, String)> = Vec::new();
    if cpp {
        for nm in re!(r"\bnamespace\s+((?:\w+::)*\w+)?\s*\{").find_iter(code) {
            let e = matching(code, nm.end() - 1);
            let mut e = if e > 0 { e as usize } else { code.len() };
            if !nm.s(1).is_empty() {
                // `}  // namespace dragonbox`: where it says it ends
                let pat = rxd(&format!(
                    r"(?m)^[ \t]*}}[ \t;]*//[ \t]*(?:end[ \t]+(?:of[ \t]+)?)?namespace[ \t]+{}\b",
                    esc(nm.s(1))
                ));
                if let Some(cm) = pat.search_at(text, nm.end()) {
                    if cm.start() < e {
                        e = cm.start();
                    }
                }
            }
            cpp_ns.push((nm.end() - 1, e, nm.s(1).to_string()));
        }
        // `SPDLOG_NAMESPACE_BEGIN` ... `SPDLOG_NAMESPACE_END`: a macro opening the library's namespace
        for bm in re!(r"(?m)^[ \t]*([A-Z][A-Z0-9]*)_(?:NAMESPACE_BEGIN|BEGIN_NAMESPACE)\b[ \t]*$").find_iter(code) {
            let pat = rxd(&format!(r"(?m)^[ \t]*{}_(?:NAMESPACE_END|END_NAMESPACE)\b", bm.s(1)));
            let end = pat.search_at(code, bm.end()).map(|m| m.start()).unwrap_or(code.len());
            cpp_ns.push((bm.end(), end, bm.s(1).to_lowercase()));
        }
        cpp_ns.sort();
    }
    let decl_pos: HashSet<usize> = found.iter().map(|f| f.3).collect();
    let mut entries: Vec<((String, String), Vec<(usize, usize, usize, isize)>)> = spans.into_iter().collect();
    entries.sort_by(|a, b| a.1[0].cmp(&b.1[0]));
    for ((hname, name), ss) in entries {
        let multi_js = fam == "js" && ss.len() > 1;
        let mut best = ss[0];
        let key_of = |s: &(usize, usize, usize, isize)| {
            let top_level = multi_js && !lines[s.0].chars().next().map(|c| c.is_whitespace()).unwrap_or(false);
            (top_level, s.1 as i64 - s.0 as i64)
        };
        for s in &ss[1..] {
            if key_of(s) > key_of(&best) {
                best = *s;
            }
        }
        let (a_, b_, npos, bpos) = best;
        let (home, cls) = owners[&(hname.clone(), name.clone())];
        let recv = if (go || (cpp && p.all[home].kind == "ext")) && home != 0 {
            p.all[home].decls.first().cloned().unwrap_or_else(|| panic!("list index out of range"))
        } else {
            String::new()
        };
        // JUnit / kotlin.test `@Test`, xUnit / NUnit / MSTest `[Fact]`, Rust `#[test]`
        let test_attr = re!(r"@Test\b|\[(?:Fact|Theory|Test|TestMethod|TestCase|DataTestMethod)\b|#\[(?:[\w:]+::)?test\b");
        let mut annotated = matches!(fam, "kotlin" | "c" | "rust")
            && (a_.saturating_sub(3)..=a_).any(|j| test_attr.search(&raw[j]).is_some());
        if go_test && recv.is_empty() && re!(r"^(?:Test|Benchmark|Example|Fuzz)(?:[A-Z_0-9]|$)").is_match(&name) {
            annotated = true; // func TestX(t *testing.T), BenchmarkX, ExampleX, FuzzX
        }
        if (swift || php)
            && ((name.starts_with("test")
                && (cls.is_some() || p.all[home].kind == "class")
                && re!(r"(?:^|/)[Tt]ests?/").search(rel).is_some())
                || (a_.saturating_sub(3)..=a_).any(|j| re!(r"@Test\b|@test\b|#\[Test\]").search(&raw[j]).is_some()))
        {
            annotated = true; // XCTest / PHPUnit `testX` methods, swift-testing `@Test`, PHPUnit `#[Test]` / `@test`
        }
        let is_test = tests.contains(&(a_, name.clone()));
        let fcode = lines[a_..=b_.min(lines.len() - 1)].join("\n");
        let mut f = Func::new(&p.all[home].name, &name, rel, a_ + 1, b_ + 1, fcode, if is_test || annotated { "test" } else { "func" });
        f.recv = recv.clone();
        if cpp && home == 0 && !cpp_ns.is_empty() {
            // a free function's namespace
            let mut path: Vec<String> = Vec::new();
            for (s0, s1, nm) in &cpp_ns {
                if *s0 < npos && npos < *s1 && !nm.is_empty() && path.last() != Some(nm) {
                    path.push(nm.clone());
                }
            }
            f.ns = path.join("::");
        }
        if fam == "js" {
            pack_params(&mut f);
            // `new ReadableStream({ start(controller) {...} })`: an options object's method
            let ob = rfind_before(code, "{", npos);
            if ob >= 0 {
                let ob = ob as usize;
                if let Some(nm) = re!(r"\bnew\s+([A-Za-z_$][\w$.]*)\s*(?:<[^()]*>)?\s*\(\s*$").search(rb(code, text, ob, 200)) {
                    if matching(code, ob) > npos as isize && re!(r"[;{}]").search(sl(code, ob + 1, npos)).is_none() {
                        let pk = find_from(code, "(", npos);
                        let pe = if pk >= 0 { matching(code, pk as usize) } else { -1 };
                        let prms = if pe > 0 { split_top(sl(code, pk as usize + 1, pe as usize), ',') } else { Vec::new() };
                        let cls_ = last_part(nm.s(1), ".");
                        for prm in prms {
                            if let Some(pm) = re!(r"\s*([A-Za-z_$][\w$]*)\s*[,:=)]?").match_start(&prm) {
                                if !f.bound.contains_key(pm.s(1)) {
                                    f.bound.insert(pm.s(1).to_string(), format!("expr:new {cls_}()\x1f(...)"));
                                }
                            }
                        }
                    }
                }
            }
        }
        let sig_end = if bpos >= 0 { bpos as usize } else { ahead(text, npos, 400) };
        let sig = sl(code, npos + name.len(), sig_end).to_string();
        if matches!(tk, "swift" | "php" | "kotlin" | "jvm" | "rust" | "go" | "js" | "cpp") && sig.contains('\n') {
            for (nm, ty) in typed(tk, &sig.replace('\n', " ")) {
                f.bound.entry(nm).or_insert(format!("type:{ty}")); // parameters on lines of their own
            }
        }
        let mut by_ov: Vec<String> = Vec::new();
        if kotlin {
            let pat = rxd(&format!(r"\bfun\s+(?:<[^>]*>\s*)?([A-Z]\w*)(?:<[^>]*>)?\??\.{}\s*\(", esc(&name)));
            if let Some(rm) = pat.search(&lines[a_]) {
                f.recv = rm.s(1).to_string();
            }
        } else if ext == ".cs" {
            // overloads share one function: each one's type-parameter count, receiver, return type
            let mut recvs: Vec<String> = Vec::new();
            let mut targs: Vec<usize> = Vec::new();
            for &(_sa, _sb, np, bp) in &ss {
                let after = rw(code, text, np + name.len(), 300);
                let gm = re!(r"\s*<([^<>()]*(?:<[^<>()]*>[^<>()]*)*)>\s*\(").match_start(after);
                let t = gm.map(|g| 1 + g.s(1).matches(',').count()).unwrap_or(0);
                if !targs.contains(&t) {
                    targs.push(t);
                }
                let sig_ = sl(code, np + name.len(), if bp >= 0 { bp as usize } else { ahead(text, np, 400) });
                let mut r = String::new();
                if let Some(rm) = re!(r"\s*(?:<[^>]*>)?\s*\(\s*this\s+([\w.<>\[\]?]+)\s+\w+").match_start(after) {
                    // an extension method: `static bool IsPrimitive(this Type t)`
                    r = last_part(&generic_name(rm.s(1)), ".").to_string();
                    if let Some(wm) = rxd(&format!(r"\bwhere\s+{}\s*:\s*([\w.<>]+)", esc(&r))).search(sig_) {
                        r = last_part(&generic_name(wm.s(1)), ".").to_string();
                    }
                    if !recvs.contains(&r) {
                        recvs.push(r.clone());
                    }
                }
                let cname = cls.map(|c| p.classes[c].name.clone()).unwrap_or_default();
                let rt = returns(tk, sl(code, line_begin(code, np), np), sig_, &name, &cname);
                let ov = format!("{r}/{t}={rt}");
                if !by_ov.contains(&ov) {
                    by_ov.push(ov);
                }
            }
            targs.sort();
            f.targs = targs.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(",");
            if !recvs.is_empty() {
                f.recv = recvs.join("|");
            }
        }
        if matches!(tk, "jvm" | "kotlin" | "js" | "swift" | "rust") {
            // each overload's parameter types (a callback's type for its lambda)
            let mut ovs: Vec<String> = Vec::new();
            for &(_sa, _sb, np, _bp) in &ss {
                let s0 = np + name.len();
                let k = find_in(code, "(", s0, ahead(text, s0, 300));
                let e = if k >= 0 { matching(code, k as usize) } else { -1 };
                if e <= 0 {
                    continue;
                }
                let (k, e) = (k as usize, e as usize);
                let gm = re!(r"\s*<([^<>()]*(?:<[^<>()]*>[^<>()]*)*)>\s*\(").match_start(sl(code, s0, k + 1));
                let mut pts = param_types(tk, &cs(code, text, k + 1, e));
                if tk == "rust" {
                    // `F` -> its bound: `<F: FnMut(Match) -> bool>` / `where F: FnMut(...)`
                    let tail = rw(code, text, e, 400);
                    let tail = tail.split('{').next().unwrap_or("");
                    let hay = format!("{}{}", sl(code, np, k), tail);
                    let mut bounds: HashMap<String, String> = HashMap::new();
                    for bm in re!(r"\b([A-Z]\w*)\s*:\s*((?:Fn|FnMut|FnOnce)\s*\([^)]*\)(?:\s*->\s*[^,{>]+)?)").find_iter(&hay) {
                        bounds.insert(bm.s(1).to_string(), bm.s(2).to_string());
                    }
                    pts = pts
                        .into_iter()
                        .map(|t| {
                            let key = strip(&re!(r"^&\s*(?:mut\s+)?").sub("", &t)).to_string();
                            bounds.get(&key).cloned().unwrap_or(t)
                        })
                        .collect();
                }
                let n_t = gm.map(|g| 1 + g.s(1).matches(',').count()).unwrap_or(0);
                let ov = format!("{n_t}{}{}", lambdas::TARGS, pts.join(&lambdas::PARAM.to_string()));
                if !ovs.contains(&ov) {
                    ovs.push(ov);
                }
            }
            f.ptypes = ovs.join(&lambdas::OV.to_string());
        }
        // the declared return type
        let line_start_pos = line_start[a_];
        let cls_name = match cls.map(|c| p.classes[c].name.clone()).filter(|s| !s.is_empty()) {
            Some(c) => c,
            None => {
                if home != 0 || !recv.is_empty() {
                    p.all[home].decls.last().cloned().unwrap_or_else(|| panic!("list index out of range"))
                } else {
                    String::new()
                }
            }
        };
        if !is_test {
            let prefix = sl(code, line_start_pos, npos);
            f.returns = returns(tk, prefix, &sig, &name, &cls_name);
            let rtext = return_text(tk, prefix, &sig);
            f.relem = elem_type(&rtext);
            if fam == "js" && f.returns.is_empty() && ss.len() > 1 {
                // `get res(): Response` beside `set res(r)`: the getter's
                for &(_sa, _sb, np, _bp) in &ss {
                    let w = rw(code, text, np + name.len(), 200);
                    if let Some(gm) = re!(r"\s*\(\s*\)\s*:\s*([^{;=]+?)\s*\{").match_start(w) {
                        if re!(r"\bget\s+$").search(rb(code, text, np, 12)).is_some() {
                            f.returns = type_name(gm.s(1));
                            break;
                        }
                    }
                }
            }
            if fam == "js" {
                if let Some(crm) = re!(r"^ReturnType\s*<\s*([A-Z][\w$]*)\s*>$").match_start(strip(&rtext)) {
                    f.returns = format!("callret:{}", crm.s(1)); // what calling a BodyRespond gives
                }
            }
            if tk == "rust" && rs_wrapped(&rtext) {
                f.returns = format!("opt:{}", f.returns); // `-> Option<Foo>`
            } else if (tk == "rust" || tk == "swift" || ext == ".cs")
                && rtext.starts_with('(')
                && matching(&rtext, 0) == rtext.len() as isize - 1
            {
                let mut parts = split_top(&rtext[1..rtext.len() - 1], ',');
                if ext == ".cs" {
                    parts = parts.iter().map(|x| re!(r"\s+[A-Za-z_]\w*\s*$").sub("", strip(x))).collect();
                }
                if parts.len() > 1 {
                    let tys: Vec<String> = parts
                        .iter()
                        .map(|x| {
                            let pre = if rs_wrapped(x) { "opt:" } else { "" };
                            format!("{pre}{}", type_name(last_part(x, ":")))
                        })
                        .collect();
                    f.returns = format!("tuple:{}", tys.join("|"));
                }
            }
            if ext == ".cs" {
                let distinct: HashSet<&str> = by_ov.iter().map(|x| x.split_once('=').map(|s| s.1).unwrap_or("")).collect();
                if distinct.len() > 1 {
                    f.returns = format!("byov:{}", by_ov.join("|")); // overloads that return different types
                }
            }
            if f.returns.is_empty() && bpos >= 0 && re!(r"\breturn\s+(?:this|self|\$this)\s*[;\n}]").search(&f.code).is_some() {
                f.returns = "Self".into(); // a builder step: `return this`
            }
            if f.returns.is_empty()
                && bpos >= 0
                && fam == "js"
                && !cls_name.is_empty()
                && re!(r"\breturn\s+(?:[\w$.]+\s+instanceof\s+this\s*\?\s*[\w$]+\s*:\s*)?new\s+this\s*\(").search(&f.code).is_some()
            {
                f.returns = format!("expr:new {cls_name}()");
            }
            if f.returns.is_empty() && bpos >= 0 {
                f.returns = own_return(code, bpos as usize, o);
            }
            if f.returns.is_empty() && bpos < 0 && fam == "js" {
                // `(input) => createRequest(input)`: that expression
                if let Some(am) = re!(r"\)\s*(?::[^=;{}]+?)?=>\s*").search(&sig) {
                    let ch = expression(&sig, am.end(), 40, o);
                    let next = first(&sig[am.end()..]);
                    if !ch.is_empty() && !"({[".contains(next) {
                        f.returns = format!("expr:{}", join(&ch));
                    }
                }
            }
            if f.returns.is_empty() && tk == "kotlin" && bpos < 0 {
                let em = re!(r"\)\s*=\s*").search(&sig);
                let builder = em
                    .as_ref()
                    .map(|em| re!(r"\s*(?:apply|also)\s*\{|\s*this\b(?!\s*\.)").match_start(&sig[em.end()..]).is_some())
                    .unwrap_or(false);
                if builder {
                    f.returns = "Self".into(); // `fun header(...) = apply { ... }`
                } else if let Some(em) = em {
                    let ch = expression(&sig, em.end(), 40, o);
                    if !ch.is_empty() {
                        f.returns = format!("expr:{}", join(&ch));
                    }
                }
            }
        }
        let mut sizes: HashMap<String, isize> = HashMap::new();
        let mut done: HashSet<usize> = HashSet::new();
        for &(sa, sb, _, _) in &ss {
            let name_col = lines[sa].find(name.as_str()).unwrap_or(0);
            let body = lines[sa..=sb.min(lines.len() - 1)].join("\n");
            let rbody = raw[sa..=sb.min(lines.len() - 1)].join("\n");
            for (nm, held) in assignments(tk, &body, o) {
                if tk == "rust" && held.starts_with(&format!("expr:{nm}\x1f")) {
                    // `let te = te.normalize_line(true);`: a shadowing rebind
                    f.bound.entry(format!("{nm}\0rebind")).or_insert(held);
                    continue;
                }
                f.bound.entry(nm).or_insert(held);
            }
            if fam == "js" {
                js_reassignments(&mut f, &lines, sa, sb, o);
            }
            for (nm, held) in loop_bindings(tk, &body, o) {
                f.bound.entry(nm).or_insert(held);
            }
            if go {
                // `st, ok := any(f).(SchemaTyper)`: the asserted type
                for am in re!(r"(?m)\b([a-z_]\w*)(?:\s*,\s*\w+)?\s*:?=\s*[^;\n]*?\.\(\s*\*?((?:\w+\.)?[A-Z]\w*)\s*\)\s*$").find_iter(&body) {
                    f.bound.entry(am.s(1).to_string()).or_insert(format!("type:{}", am.s(2)));
                }
                // a table-driven test: `tests := []struct{ ... }{...}`
                for tm in re!(r"\b([a-zA-Z_]\w*)\s*:?=\s*\[\]struct\s*\{").find_iter(&body) {
                    let e = matching(&body, tm.end() - 1);
                    let sub = format!("{}.{name}${}", file_name, tm.s(1));
                    if e < 0 || p.all.contains_key(&sub) {
                        continue;
                    }
                    let mut sm = p.new_sub(&sub, vec![format!("{name}${}", tm.s(1))]);
                    sm.package = p.all[0].package.clone();
                    sm.kind = "class".into();
                    for fl in re!(r"(?m)^\s*([a-zA-Z_]\w*(?:\s*,\s*[a-zA-Z_]\w*)*)\s+\*?((?:\w+\.)?[A-Za-z_]\w*)\s*$")
                        .find_iter(sl(&body, tm.end(), e as usize))
                    {
                        for fname in fl.s(1).split(',') {
                            sm.bound.insert(strip(fname).to_string(), format!("type:{}", fl.s(2)));
                        }
                    }
                    if !sm.bound.is_empty() {
                        p.all.insert(sub.clone(), sm);
                        f.bound.entry(format!("{}[]", tm.s(1))).or_insert(format!("mod:{sub}"));
                    }
                }
            }
            if tk == "rust" || tk == "swift" || ext == ".cs" {
                // `let (ig, err) = b.add_child(p);`: each name is that element of the tuple
                for tm in re!(r"\b(?:let|var)\s+\(\s*([^()=]*?)\s*\)\s*(?::[^=]+)?=(?!=)").find_iter(&body) {
                    let ch = expression(&body, tm.end(), 40, o);
                    if ch.last().map(|s| s.ends_with("()")).unwrap_or(false) {
                        for (k, nm) in split_top(tm.s(1), ',').iter().enumerate() {
                            let nm = re!(r"^(?:mut\s+|ref\s+)").sub("", strip(nm));
                            if re!(r"^[a-z_]\w*$").is_match(&nm) && nm != "_" {
                                f.bound.entry(nm).or_insert(format!("tuple:{k}\x1e{}", join(&ch)));
                            }
                        }
                    }
                }
            }
            if tk == "rust" {
                // `Raw(ref x) => x.ino()`: x, on that arm's lines, is what the variant holds
                for (nm, ln, held) in rs_arms(&body, &rbody, sa) {
                    f.bound.entry(format!("{nm}@{ln}")).or_insert(held);
                }
            }
            if fam == "js" {
                // `mixin(app, proto)` / `Object.assign(app, proto)`: app has proto's members
                for mm in re!(r"\b(?:Object\.assign|mixin|merge|extend|_\.extend|_\.assign)\s*\(\s*([A-Za-z_$][\w$]*)\s*,\s*([A-Za-z_$][\w$]*)\s*[,)]")
                    .find_iter(&body)
                {
                    f.bound.insert(mm.s(1).to_string(), format!("expr:{}", mm.s(2)));
                }
            }
            let mut off = 0usize;
            for i in sa..=sb.min(lines.len() - 1) {
                let ln = lines[i].as_str();
                if done.contains(&i) {
                    off += ln.len() + 1; // (a merged span's lines count once)
                    continue;
                }
                done.insert(i);
                for (nm, ty) in typed_by_line.get(&i).map(|x| x.as_slice()).unwrap_or(&[]) {
                    let held = f.bound.get(nm).cloned().unwrap_or_default();
                    let segs: Vec<&str> = match held.strip_prefix("expr:") {
                        Some(h) => h.split('\x1f').collect(),
                        None => Vec::new(),
                    };
                    if segs.len() > 2 && segs[0] == last_part(ty, ".") && matches!(segs[1], "new()" | "builder()" | "default()") {
                        continue; // `Foo::new()\n  .a()\n  .build()`: the chain's end, not a Foo
                    }
                    if segs.len() > 1 && segs[0] == format!("new {ty}()") {
                        continue; // `var x = new Builder().Add().Build()`: the chain's end
                    }
                    f.bound.insert(nm.clone(), format!("type:{ty}"));
                }
                for m in call_rx().find_iter(ln) {
                    let nm = m.s(2);
                    let s2 = m.span(2).unwrap().0;
                    if (i == sa && ((nm == name && s2 <= name_col) || (s2 < name_col && fam != "js")))
                        || decl_pos.contains(&(line_start[i] + s2))
                    {
                        continue;
                    }
                    let chain = receiver(&body, off + s2, 40, o);
                    let before = rstrip(&ln[..s2]);
                    let mut q = if !m.s(1).is_empty() {
                        m.s(1).to_string()
                    } else if (before.ends_with('.') || before.ends_with("->")) && !before.ends_with("...") {
                        ".".into()
                    } else {
                        String::new()
                    };
                    if q == "." && chain.is_empty() && before.ends_with("->") && matches!(prev_char(ln, s2), " " | "\t" | "") {
                        q = String::new(); // `x == null -> knownNotNull(d)`: a Kotlin `when` arrow
                    }
                    if !chain.is_empty() && !q.is_empty() && q != "." {
                        let last = chain.last().unwrap();
                        q = if word_only(last) { last.clone() } else { ".".into() };
                    }
                    if nm == "new" && q.is_empty() && ext == ".cs" {
                        bind_lambdas(&mut f, &mut sizes, fam, &body, &rbody, off + m.end() - 1, "new", &[], sa as isize, -1);
                    }
                    if call_skip(nm) && (q.is_empty() || (not_func(nm) && nm != "new")) {
                        continue;
                    }
                    let tail = strip(&m.group0()[m.span(2).unwrap().1 - m.start()..]).to_string();
                    let mut c = Call::new(nm, &q, i + 1);
                    c.chain = if q.is_empty() { Vec::new() } else { chain.clone() };
                    c.col = s2 as i64;
                    f.calls.push(c);
                    if matches!(tk, "jvm" | "kotlin" | "js" | "swift" | "rust") {
                        let targs = if tail.starts_with('<') && tail.contains('>') {
                            1 + top_commas(&tail[1..tail.rfind('>').unwrap()]) as i64
                        } else {
                            -1
                        };
                        let ch: &[String] = if q.is_empty() { &[] } else { &chain };
                        bind_lambdas(&mut f, &mut sizes, fam, &body, &rbody, off + m.end() - 1, nm, ch, sa as isize, targs);
                    }
                    if ext == ".cs" {
                        let t = if tail.starts_with('<') { 1 + top_commas(&tail[1..tail.rfind('>').unwrap()]) as i64 } else { 0 };
                        f.calls.last_mut().unwrap().targs = t;
                    }
                }
                if tk == "kotlin" || tk == "swift" {
                    // `xs.map { $0.name }`, `x.let { it.go() }`: a trailing closure, no parentheses
                    for tm in re!(r"(\.)?\b([A-Za-z_]\w*)[ \t]*\{").find_iter(ln) {
                        let g2 = tm.s(2);
                        if TRAILING_SKIP.contains(&g2) || upper1(g2) {
                            continue;
                        }
                        let s2 = tm.span(2).unwrap().0;
                        if tm.get(1).is_some() {
                            let tch = receiver(&body, off + s2, 40, o);
                            if !tch.is_empty() {
                                bind_closure(&mut f, &mut sizes, fam, &body, &rbody, off + tm.end() - 1, g2, &tch, sa as isize);
                            }
                            if !tch.is_empty() && tk == "kotlin" {
                                // (a call with only a trailing lambda is a call: `lock.withLock { }`)
                                let last = tch.last().unwrap();
                                let q = if word_only(last) { last.clone() } else { ".".into() };
                                let mut c = Call::new(g2, &q, i + 1);
                                c.chain = tch.clone();
                                c.col = s2 as i64;
                                f.calls.push(c);
                            }
                        } else {
                            let pre = re!(r"(?:\b(?:await|try[?!]?|return|throw)\s*)+$").sub("", &ln[..s2]);
                            if re!(r"[\w)\]>?]\s*$").search(&pre).is_none() {
                                // `performEvent { $0... }`: a bare call
                                bind_closure(&mut f, &mut sizes, fam, &body, &rbody, off + tm.end() - 1, g2, &[], sa as isize);
                                if tk == "kotlin" && !(i == sa && s2 < name_col) {
                                    let mut c = Call::new(g2, "", i + 1);
                                    c.col = s2 as i64;
                                    f.calls.push(c);
                                }
                            }
                        }
                    }
                }
                if jsx {
                    // `<Widget ...>` renders (calls) the component Widget
                    for m in re!(r"<(?:([A-Za-z_$][\w$]*)\.)?([A-Z][\w$]*)(?=[\s/>]|$)").find_iter(ln) {
                        f.calls.push(Call::new(m.s(2), m.s(1), i + 1));
                    }
                }
                off += ln.len() + 1;
            }
        }
        p.all[home].funcs.push(f);
    }

    // a nested function sees what its enclosing function bound
    let mut every: Vec<(usize, usize)> = Vec::new();
    for (mi, m) in p.all.values().enumerate() {
        for fi in 0..m.funcs.len() {
            every.push((mi, fi));
        }
    }
    let span_of = |all: &IndexMap<String, Module>, (mi, fi): (usize, usize)| {
        let g = &all[mi].funcs[fi];
        (g.start, g.end)
    };
    every.sort_by_key(|&k| {
        let (s, e) = span_of(&p.all, k);
        (s, std::cmp::Reverse(e))
    });
    for gi in 0..every.len() {
        let (gs, ge) = span_of(&p.all, every[gi]);
        let mut inner: Option<(usize, usize)> = None;
        let mut inner_start = 0usize;
        for &o_ in &every {
            if o_ == every[gi] {
                continue;
            }
            let (os, oe) = span_of(&p.all, o_);
            if os <= gs && ge <= oe && (os, oe) != (gs, ge) && (inner.is_none() || os > inner_start) {
                inner = Some(o_);
                inner_start = os;
            }
        }
        if let Some((omi, ofi)) = inner {
            let outer: Vec<(String, String)> = p.all[omi].funcs[ofi]
                .bound
                .iter()
                .filter(|(_, v)| !v.starts_with("fixture:"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let (gmi, gfi) = every[gi];
            let g = &mut p.all[gmi].funcs[gfi];
            let mut merged: IndexMap<String, String> = IndexMap::new();
            for (k, v) in outer {
                merged.insert(k, v);
            }
            for (k, v) in std::mem::take(&mut g.bound) {
                merged.insert(k, v);
            }
            g.bound = merged;
        }
    }
    if qml {
        qml_types(&mut p.all[0], code, text, o);
    }
    if go {
        // `type Engine struct { RouterGroup ... }`: an embedded type's methods are promoted (a base)
        for sm in re!(r"(?m)^type\s+([A-Za-z_]\w*)(?:\[[^\]]*\])?\s+struct\s*\{").find_iter(code) {
            let e = matching(code, sm.end() - 1);
            let end = if e > 0 { e as usize } else { code.len() };
            let emb: Vec<String> = re!(r"(?m)^[ \t]*\*?((?:[A-Za-z_]\w*\.)?[A-Z]\w*)[ \t]*(?:`[^`\n]*`)?[ \t]*$")
                .find_iter(sl(code, sm.end(), end))
                .iter()
                .map(|em| last_part(em.s(1), ".").to_string())
                .collect();
            if !emb.is_empty() {
                let tname = format!("{file_name}.{}", sm.s(1));
                if !p.all.contains_key(&tname) {
                    let m = p.new_sub(&tname, vec![sm.s(1).to_string()]);
                    p.all.insert(tname.clone(), m);
                }
                let tm = p.all.get_mut(&tname).unwrap();
                for b in emb {
                    if !tm.bases.contains(&b) {
                        tm.bases.push(b);
                    }
                }
            }
        }
        // `type Window interface { Top() int ... }`: a module listing the methods
        for im in re!(r"(?m)^type\s+([A-Za-z_]\w*)(?:\[[^\]]*\])?\s+interface\s*\{").find_iter(code) {
            let (mut k, mut d) = (im.end(), 1i64);
            while k < cb.len() && d != 0 {
                d += (cb[k] == b'{') as i64;
                d -= (cb[k] == b'}') as i64;
                k += 1;
            }
            let names: Vec<String> =
                re!(r"(?m)^\s*([A-Za-z_]\w*)\s*\(").find_iter(sl(code, im.end(), k)).iter().map(|m| m.s(1).to_string()).collect();
            let iname = format!("{file_name}.{}", im.s(1));
            if !p.all.contains_key(&iname) {
                let m = p.new_sub(&iname, vec![im.s(1).to_string()]);
                p.all.insert(iname.clone(), m);
            }
            let imod = p.all.get_mut(&iname).unwrap();
            imod.kind = "interface".into();
            imod.methods = names;
        }
    }
    let mut top = Func::new(&file_name, "<top>", rel, 1, lines.len(), String::new(), "top");
    if (fam == "js" && ext != ".qml" && !rel.ends_with(".d.ts")) || php || ext == ".java" || ext == ".cs" {
        // (PHP: a page script's code; Java / C#: field and static initializers)
        top_calls(&mut top, &p.all, code, &lines, &idx, tk, fam, o);
        if ext == ".java" || ext == ".cs" {
            // (`@Deprecated(since = "9")`, `[Obsolete("x")]`, `object Map();`: no calls)
            let decl_like = re!(r"(?<![.\w])(?!(?:return|new|await|throw|yield|else|case|in|is|as|out|ref|not|and|or)\s)[A-Za-z_][\w<>,\[\]?.]*\s+$");
            top.calls.retain(|c| {
                !(0 < c.line && c.line <= lines.len() && {
                    let l = &lines[c.line - 1];
                    let pre = sl(l, 0, c.col.max(0) as usize);
                    rstrip(pre).ends_with('@') || lstrip(l).starts_with('[') || decl_like.search(pre).is_some()
                })
            });
            top.calls.retain(|c| extra::field_initializer(&lines, c));
            class_initializers(&mut top, &mut p.all, code, &idx, rel);
        }
    }
    if fam == "js" {
        // module-level callbacks (`app.get('/', function (req, res) {` at the top of a file)
        top.code = code.to_string();
        pack_params(&mut top);
        top.code = String::new();
        let packed: Vec<(String, String)> =
            top.bound.iter().filter(|(_, v)| v.starts_with("pack:")).map(|(k, v)| (k.clone(), v.clone())).collect();
        if !packed.is_empty() {
            for m in p.all.values_mut() {
                for g in m.funcs.iter_mut() {
                    for (k, v) in &packed {
                        g.bound.entry(k.clone()).or_insert(v.clone());
                    }
                }
            }
        }
    }
    if qml {
        // `Button {` instantiates Button.qml: a use of that component
        for (i, ln) in lines.iter().enumerate() {
            if let Some(cm) = re!(r"[ \t]*([A-Z]\w*)[ \t]*\{").match_start(ln) {
                top.calls.push(Call::new(cm.s(1), cm.s(1), i + 1));
                top.refs.insert(cm.s(1).to_string());
            }
        }
    }
    p.all[0].funcs.push(top);
    if !go {
        let sub: HashSet<String> =
            p.all.values().skip(1).filter(|m| m.kind != "ext").flat_map(|m| m.decls.iter().cloned()).collect();
        let fmod = &mut p.all[0];
        let mut d: Vec<String> = fmod.decls.iter().filter(|x| !sub.contains(*x)).cloned().collect();
        d.sort();
        d.dedup();
        fmod.decls = d;
    }
    // Python counts a call's column in characters
    let mut out: Vec<Module> = p.all.into_values().collect();
    for m in out.iter_mut() {
        for f in m.funcs.iter_mut() {
            for c in f.calls.iter_mut() {
                if c.col > 0 {
                    if let Some(r) = raw.get(c.line - 1) {
                        c.col = chars_to(r, c.col as usize) as i64;
                    }
                }
            }
        }
    }
    out
}

