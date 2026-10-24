//! Ruby adapter (lang_ruby.py): one Module per file, and one per class or
//! module (`<file>.<A::B>`, nested names spelled out), like Python's classes.
//!
//! Ruby writes down less than other languages: a call needs no parentheses,
//! a bare name is a method call unless the method assigned it first, a
//! constant means whatever the lexical scope (then the superclass) finds
//! wherever it was defined, and no parameter has a type. So the parse
//! records what each file says, and `link` (run once every file is parsed)
//! resolves it across the project.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::lang::{Adapter, ParseCtx};
use crate::model::{Call, Func, Module};
use crate::pyre::Rx;
use crate::scan::blank;

pub mod blocks;
pub mod calls;
pub mod fields;
pub mod link;
pub mod pats;
pub mod recv;
pub mod util;

use blocks::Blk;
use pats::*;
use util::*;

pub struct Ruby;

impl Adapter for Ruby {
    fn family(&self) -> &'static str {
        "ruby"
    }
    fn exts(&self) -> &'static [&'static str] {
        &[".rb", ".rake", ".ru"]
    }
    fn parse(&self, _ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module> {
        parse(rel, text)
    }
}

/// The modules being built and the functions they hold: Python shares each
/// Func between the module's list and the dicts that find it again, so here
/// they live in one arena and a module holds indexes.
pub struct St {
    pub ms: Vec<Module>,
    pub mod_funcs: Vec<Vec<usize>>,
    pub funcs: Vec<Func>,
}

impl St {
    pub fn add(&mut self, mi: usize, f: Func) -> usize {
        self.funcs.push(f);
        let fi = self.funcs.len() - 1;
        self.mod_funcs[mi].push(fi);
        fi
    }
}

fn rx_private_def() -> &'static Rx {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    R.get_or_init(|| Rx::lit(r"^\s*private\s+def\b"))
}

/// Modules of one Ruby file (lang_ruby.parse).
pub fn parse(rel: &str, text: &str) -> Vec<Module> {
    let p = pats();
    let (code, strings) = blank(text, "ruby", false);
    let raw: Arc<Vec<String>> = Arc::new(text.split('\n').map(String::from).collect());
    let lines_a: Arc<Vec<String>> = Arc::new(code.split('\n').map(String::from).collect());
    let lines: &[String] = &lines_a;
    let nl = lines.len();
    let mut starts = vec![0usize];
    for ln in &lines[..nl - 1] {
        let last = *starts.last().unwrap();
        starts.push(last + ln.len() + 1);
    }

    let mut fm = Module::new(&modname(rel), rel, raw.clone(), lines_a.clone());
    requires(&mut fm, rel, lines, &raw);
    let mut blocks = blocks::blocks(&code, &starts);
    blocks::blockless_classes(&mut blocks, lines);
    let mut first_str: HashMap<usize, String> = HashMap::new();
    for (ln, st) in &strings {
        first_str.entry(ln.wrapping_sub(1)).or_insert_with(|| st.clone());
    }

    let mut st = St { ms: vec![fm], mod_funcs: vec![vec![]], funcs: vec![] };
    // classes and modules: their full names, from the enclosing ones
    let mut mod_idx: IndexMap<String, usize> = IndexMap::new();
    let mut home: HashMap<usize, usize> = HashMap::new(); // block index -> the module its methods go to
    for b in &blocks {
        let outer = if b.parent >= 0 { home.get(&(b.parent as usize)).copied().unwrap_or(0) } else { 0 };
        if b.kind == "class" || b.kind == "module" {
            let written = &b.name;
            let enclosing = if outer != 0 { st.ms[outer].decls[0].clone() } else { String::new() };
            let full = if let Some(w) = written.strip_prefix("::") {
                w.to_string()
            } else if !enclosing.is_empty() {
                format!("{enclosing}::{written}")
            } else {
                written.clone()
            };
            let mname = format!("{}.{}", st.ms[0].name, full);
            let ci = match mod_idx.get(&mname) {
                Some(&c) => c,
                None => {
                    let mut cm = Module::new(&mname, rel, raw.clone(), lines_a.clone());
                    cm.owner = st.ms[0].name.clone();
                    cm.decls = vec![full];
                    cm.kind = "class".into();
                    st.ms.push(cm);
                    st.mod_funcs.push(vec![]);
                    let c = st.ms.len() - 1;
                    mod_idx.insert(mname, c);
                    c
                }
            };
            if !b.base.is_empty() {
                st.ms[ci].bases.push(format!("S{enclosing}\0{}", b.base));
            }
            if written.trim_start_matches(':').contains("::") && !st.ms[ci].bases.iter().any(|x| x == "C\0") {
                st.ms[ci].bases.push("C\0".into());
            }
            home.insert(b.i, ci);
        } else if b.parent >= 0 {
            home.insert(b.i, outer);
        }
    }
    // each line's owner: the innermost def / test block, else the class body
    let mut owner_blk = vec![-1i64; nl];
    let mut body_blk = vec![-1i64; nl]; // the innermost block of any kind
    for b in &blocks {
        for i in b.a..=b.b {
            body_blk[i] = b.i as i64;
            if matches!(b.kind, "class" | "module" | "def" | "test") {
                owner_blk[i] = b.i as i64;
            }
        }
    }
    let test_file = pats::test_file(rel);
    let mut fkey: HashMap<(String, String), usize> = HashMap::new();
    let mut func_of_blk: IndexMap<usize, usize> = IndexMap::new();
    let mut private_from: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for b in &blocks {
        if b.kind == "class" || b.kind == "module" {
            let ci = home[&b.i];
            for i in b.a + 1..b.b {
                if body_blk[i] != b.i as i64 {
                    continue;
                }
                if let Some(pm) = p.private.start(&lines[i]) {
                    private_from.entry(st.ms[ci].name.clone()).or_default().push((i, pm.s(1).to_string()));
                }
                if let Some(mx) = p.mixin.start(&lines[i]) {
                    for c in split_commas(mx.s(2)) {
                        if c != "self" {
                            let d = st.ms[ci].decls[0].clone();
                            st.ms[ci].bases.push(format!("M{d}\0{c}"));
                        }
                    }
                }
            }
        }
    }
    for (i, ln) in lines.iter().enumerate() {
        let mx = if body_blk[i] == -1 { p.mixin.start(ln) } else { None };
        if let Some(mx) = mx {
            if mx.s(1) == "include" {
                for c in split_commas(mx.s(2)) {
                    st.ms[0].bases.push(format!("G\0{c}"));
                }
            }
        }
    }
    for b in &blocks {
        if b.kind != "def" && b.kind != "test" {
            continue;
        }
        let mi = if b.parent >= 0 { home.get(&(b.parent as usize)).copied().unwrap_or(0) } else { 0 };
        let (a, z) = (b.a, b.b);
        let name: String;
        let mut kind: &str;
        if b.kind == "test" {
            let label = first_str.get(&a).cloned().unwrap_or_else(|| format!("line {}", a + 1));
            name = format!("test {label}");
            kind = "test";
        } else {
            name = b.name.clone();
            kind = if test_file && name.starts_with("test_") && mi != 0 { "test" } else { "func" };
            if kind == "func" && b.on.is_empty() {
                let marks: Vec<&str> = private_from
                    .get(&st.ms[mi].name)
                    .map(|v| v.iter().filter(|(i, _)| *i < a).map(|(_, w)| w.as_str()).collect())
                    .unwrap_or_default();
                if (!marks.is_empty() && *marks.last().unwrap() != "public") || rx_private_def().match_start(&lines[a]).is_some() {
                    kind = "private";
                }
            }
        }
        let mname = st.ms[mi].name.clone();
        let key = (mname.clone(), name.clone());
        let fi = match fkey.get(&key) {
            None => {
                let f = Func::new(&mname, &name, rel, a + 1, z + 1, lines[a..=z].join("\n"), kind);
                let fi = st.add(mi, f);
                fkey.insert(key, fi);
                fi
            }
            Some(&fi) => {
                let f = &mut st.funcs[fi];
                if z - a > f.end - f.start {
                    f.start = a + 1;
                    f.end = z + 1;
                    f.code = lines[a..=z].join("\n");
                }
                fi
            }
        };
        func_of_blk.insert(b.i, fi);
    }
    let mut tops: IndexMap<String, usize> = IndexMap::new();
    let all_mods: Vec<usize> = std::iter::once(0).chain(mod_idx.values().copied()).collect();
    for &mi in &all_mods {
        let name = st.ms[mi].name.clone();
        let top = Func::new(&name, "<top>", rel, 1, nl, String::new(), "top");
        let fi = st.add(mi, top);
        tops.insert(name, fi);
    }
    calls::attrs(&mut st, &blocks, &home, &body_blk, lines, &raw, &mut fkey);
    calls::lets(&mut st, &blocks, &home, &code, &starts, lines, &mut fkey);
    let scopes = calls::scopes(&mut st, &blocks, &home, lines, &mut fkey);

    // calls, locals and bindings, function by function
    let mut ends: HashMap<usize, usize> = HashMap::new();
    for b in &blocks {
        if b.kind == "do" {
            if let Some(e) = b.end_pos {
                ends.insert(e, b.i);
            }
        }
    }
    let mut owned: IndexMap<i64, Vec<usize>> = IndexMap::new();
    for (i, &ob) in owner_blk.iter().enumerate() {
        owned.entry(ob).or_default().push(i);
    }
    for (ob, idx) in &owned {
        let b = if *ob >= 0 { Some(&blocks[*ob as usize]) } else { None };
        let fi = match b {
            None => tops[&st.ms[0].name],
            Some(b) if b.kind == "class" || b.kind == "module" => tops[&st.ms[home[&b.i]].name],
            Some(b) => func_of_blk[&b.i],
        };
        if st.funcs[fi].kind == "top" {
            let mut mine = vec![false; nl];
            for &i in idx {
                mine[i] = true;
            }
            st.funcs[fi].code = (0..nl).map(|i| if mine[i] { lines[i].as_str() } else { "" }).collect::<Vec<_>>().join("\n");
        }
        calls::collect(&mut st.funcs[fi], b, &code, &starts, idx, lines, &raw, &ends, &blocks);
    }
    for &fi in &scopes {
        // (the lambda's calls were collected as the class body's)
        let (module, start) = (st.funcs[fi].module.clone(), st.funcs[fi].start);
        if let Some(&ti) = tops.get(&module) {
            let mine: Vec<Call> = st.funcs[ti].calls.iter().filter(|c| c.line == start).cloned().collect();
            st.funcs[ti].calls.retain(|c| c.line != start);
            st.funcs[fi].calls = mine;
        }
    }
    // globals reach nothing in the project: `$stdout.puts`
    {
        static G: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
        let g = G.get_or_init(|| Rx::lit(r"(?<![\w$])\$([A-Za-z_]\w*)"));
        let mut seen: Vec<&str> = Vec::new();
        for gname in g.findall(&code) {
            if !seen.contains(&gname) {
                seen.push(gname);
                st.ms[0].aliases.insert(format!("${gname}"), "ext:".into());
            }
        }
    }
    let mut class_funcs: Vec<usize> = func_of_blk.values().copied().collect();
    class_funcs.extend(tops.values().copied());
    fields::class_fields(&mut st, &mod_idx, &class_funcs);
    fields::setup_fields(&mut st, &blocks, &func_of_blk, &code, &starts);
    // names in a class resolve through its file: share its requires
    let (al, us, re) = (st.ms[0].aliases.clone(), st.ms[0].uses.clone(), st.ms[0].reexports.clone());
    for &mi in mod_idx.values() {
        st.ms[mi].aliases = al.clone();
        st.ms[mi].uses = us.clone();
        st.ms[mi].reexports = re.clone();
    }
    // the funcs move into their modules, in the order they were added
    let mut slots: Vec<Option<Func>> = std::mem::take(&mut st.funcs).into_iter().map(Some).collect();
    for (mi, m) in st.ms.iter_mut().enumerate() {
        m.funcs = st.mod_funcs[mi].iter().map(|&fi| slots[fi].take().expect("a func is in one module")).collect();
    }
    st.ms
}

/// `require "x/y"`, `require_relative "z"`, `autoload :K, "x/k"`: kept as
/// "rb:<dir>\0<spec>\0<relative>" for link() to find in the project.
fn requires(fm: &mut Module, rel: &str, lines: &[String], raw: &[String]) {
    let p = pats();
    let d = match rel.rfind('/') {
        Some(k) => &rel[..k],
        None => "",
    };
    for (i, ln) in lines.iter().enumerate() {
        if !ln.contains("require") && !ln.contains("autoload") && !ln.contains("load") {
            continue;
        }
        if let Some(m) = p.require.start(&raw[i]) {
            fm.uses.push(format!("rb:{d}\0{}\0{}", m.s(2), if m.s(1) == "require_relative" { "1" } else { "" }));
            continue;
        }
        if let Some(m) = p.autoload.start(&raw[i]) {
            fm.uses.push(format!("rb:{d}\0{}\0", m.s(1)));
        }
    }
}

pub fn join_chain(ch: &[String]) -> String {
    ch.join("\x1f")
}
