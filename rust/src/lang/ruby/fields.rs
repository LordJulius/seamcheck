//! A class's fields from the assignments in its methods (lang_ruby._class_fields,
//! _setup_fields, _ivar_sets, _pushes).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use indexmap::IndexMap;

use super::blocks::Blk;
use super::pats::*;
use super::recv::expression;
use super::util::*;
use super::St;
use crate::model::Func;
use crate::pyre::{rx as cached, Rx};

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [OnceLock<Rx>; 8] = [const { OnceLock::new() }; 8];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

/// A test's fields from the `setup do` / `before do` blocks of the contexts
/// around it (the innermost first): each context sets up its own.
pub fn setup_fields(st: &mut St, blocks: &[Blk], func_of_blk: &IndexMap<usize, usize>, code: &str, starts: &[usize]) {
    let p = pats();
    let mut setups: HashMap<i64, Vec<usize>> = HashMap::new();
    for b in blocks {
        if b.setup && b.parent >= 0 {
            setups.entry(b.parent).or_default().push(b.i);
        }
    }
    if setups.is_empty() {
        return;
    }
    for (&bi, &fi) in func_of_blk {
        let kind = st.funcs[fi].kind.clone();
        if kind != "test" && kind != "func" {
            continue; // (a test, or a helper `def` in the describe block: an instance method of the same test class)
        }
        let mut par = blocks[bi].parent;
        while par >= 0 {
            if let Some(ss) = setups.get(&par) {
                for &si in ss {
                    let s = &blocks[si];
                    let (lo, hi) = (starts[s.a], starts[s.b.min(starts.len() - 1)]);
                    for m in p.ivar_set.find_iter_range(code, lo, hi) {
                        let ch = expression(code, m.end(), &|_, _| false);
                        if !ch.is_empty() && !(first_lower(&ch[0]) && !ch[0].ends_with("()")) {
                            st.funcs[fi].bound.entry(m.s(1).to_string()).or_insert_with(|| format!("expr:{}", ch.join("\x1f")));
                        }
                    }
                }
            }
            par = blocks[par as usize].parent;
        }
    }
}

/// `@x = Foo.new` in a class's methods (or a `setup do` block in its body):
/// the field's type, for every method. Several values are kept as "alt:" for
/// link() to check that they all build the same class; one it can't read
/// says nothing.
pub fn class_fields(st: &mut St, mod_idx: &IndexMap<String, usize>, funcs: &[usize]) {
    let mut seen: IndexMap<usize, IndexMap<String, BTreeSet<String>>> = IndexMap::new();
    for &fi in funcs {
        let Some(&mi) = mod_idx.get(&st.funcs[fi].module) else { continue };
        let accessors: HashSet<String> = st.mod_funcs[mi]
            .iter()
            .map(|&g| &st.funcs[g])
            .filter(|g| g.returns == format!("expr:@{}", g.name))
            .map(|g| g.name.clone())
            .collect();
        let f = &st.funcs[fi];
        let mut got: Vec<(String, String)> = ivar_sets(f, &accessors);
        got.extend(pushes(st, f, mi));
        for (k, v) in got {
            seen.entry(mi).or_default().entry(k).or_default().insert(v);
        }
    }
    for (mi, fields) in seen {
        for (k, vs) in fields {
            let mut vs = vs;
            if vs.len() > 1 {
                vs.remove("expr:NilClass"); // `@theme = nil` here, `@theme = Theme.new(...)` there: a Theme
            }
            if !vs.contains("?") {
                let val = if vs.len() == 1 {
                    vs.iter().next().unwrap().clone()
                } else {
                    format!("alt:{}", vs.iter().cloned().collect::<Vec<_>>().join("\x1e"))
                };
                st.ms[mi].bound.entry(k).or_insert(val);
            }
        }
    }
}

fn params_of(f: &Func) -> HashSet<String> {
    let pat = format!(r"\s*def\s+(?:\w+\.)?{}\s*\(([^)]*)\)", escape(&f.name));
    match cached(&pat).ok().and_then(|r| r.match_start(&f.code).map(|m| m.s(1).to_string())) {
        Some(g) => rx(0, r"[a-z_]\w*").findall(&g).into_iter().map(String::from).collect(),
        None => HashSet::new(),
    }
}

/// (field, value) for each `@x = <expr>` in f, and each `self.x = <expr>`
/// through an attr_accessor (the same field).
fn ivar_sets(f: &Func, accessors: &HashSet<String>) -> Vec<(String, String)> {
    let p = pats();
    let params = params_of(f);
    let mut out: Vec<(String, String)> = Vec::new();
    if f.name == "initialize" {
        // `@site = relations[:site]`: an option the callers pass by name
        let r = rx(1, r"(?m)(?<![\w@])(@\w+)\s*=\s*([a-z_]\w*)\[:(\w+)\]\s*$");
        for m in r.find_iter(&f.code) {
            if params.contains(m.s(2)) {
                out.push((m.s(1).to_string(), format!("expr:{}\x1f[:{}]", m.s(2), m.s(3))));
            }
        }
    }
    let opt_tail = rx(2, r"\s*[a-z_]\w*\[:\w+\]\s*$");
    let mut sets: Vec<(String, usize)> = Vec::new();
    for m in p.ivar_set.find_iter(&f.code) {
        let e = m.end();
        let stop = find(&f.code, "\n", e).unwrap_or(f.code.len());
        // (python: code.find("\n", end) == -1 slices to the last character)
        let stop = if find(&f.code, "\n", e).is_none() { f.code.len().saturating_sub(1) } else { stop };
        if opt_tail.match_start(sub(&f.code, e, stop)).is_none() {
            sets.push((m.s(1).to_string(), e));
        }
    }
    for m in p.self_set.find_iter(&f.code) {
        if accessors.contains(m.s(1)) {
            sets.push((format!("@{}", m.s(1)), m.end()));
        }
    }
    for (name, at) in sets {
        let is_local = |n: &str, _p: usize| f.bound.contains_key(n) || params.contains(n);
        let ch = expression(&f.code, at, &is_local);
        if ch.is_empty() {
            out.push((name, "?".into()));
            continue;
        }
        if let Some(held) = f.bound.get(&ch[0]).filter(|h| h.starts_with("expr:")) {
            // `@site = site` with site's type known
            let mut segs: Vec<String> = held[5..].split('\x1f').map(String::from).collect();
            segs.extend(ch[1..].iter().cloned());
            out.push((name, format!("expr:{}", segs.join("\x1f"))));
        } else if first_lower(&ch[0]) && !ch[0].ends_with("()") {
            // `@site = doc.site`: as written, for the constructor's callers to type (_ctor_params)
            if f.name == "initialize" && params.contains(&ch[0]) {
                out.push((name, format!("expr:{}", ch.join("\x1f"))));
            } else {
                out.push((name, "?".into()));
            }
        } else {
            out.push((name, format!("expr:{}", ch.join("\x1f"))));
        }
    }
    out
}

/// ("@docs[]", <element>) for each `docs << doc` / `@docs.push(Foo.new)` in
/// f: what the collection holds.
fn pushes(st: &St, f: &Func, mi: usize) -> Vec<(String, String)> {
    let p = pats();
    let own: HashMap<&str, &Func> = st.mod_funcs[mi].iter().map(|&g| (st.funcs[g].name.as_str(), &st.funcs[g])).collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for pm in p.push.find_iter(&f.code) {
        let coll = pm.s(1);
        let (cls, var) = (pm.get(2), pm.get(3));
        let val: String;
        if let Some(cls) = cls {
            val = format!("expr:{cls}\x1fnew()");
        } else {
            let held = f.bound.get(var.unwrap_or("")).map(|s| s.as_str()).unwrap_or("");
            if !(held.starts_with("expr:") && held[5..].chars().next().map(upper).unwrap_or(false) && held.ends_with("\x1fnew()")) {
                let c = if coll.starts_with('@') { coll.to_string() } else { format!("@{coll}") };
                out.push((format!("{c}[]"), "?".into()));
                continue;
            }
            val = held.to_string();
        }
        if coll.starts_with('@') {
            out.push((format!("{coll}[]"), val));
        } else if !f.bound.contains_key(coll) && own.get(coll).map(|g| g.code.contains(&format!("@{coll}"))).unwrap_or(false) {
            out.push((format!("{coll}[]"), val.clone()));
            out.push((format!("@{coll}[]"), val));
        }
    }
    // `@collections ||= names.each_with_object({}) do |name, hsh| hsh[name] = Collection.new(...)`
    let acc_pat = format!(
        r"(?s)(@{})\s*\|\|=.*?\.each_with_object\(\s*(?:\{{\s*\}}|\[\s*\]|Hash\.new|\{{\}})\s*\)\s*(?:do|\{{)\s*\|[^|,]*,\s*([a-z_]\w*)\s*\|",
        escape(&f.name)
    );
    let acc = cached(&acc_pat).ok().and_then(|r| r.search(&f.code).map(|m| (m.s(1).to_string(), m.s(2).to_string())));
    let hm = rx(3, r"(?<![\w@.:])(@?[a-z_]\w*)\[[^\]\n]+\]\s*=\s*([A-Z][\w:]*)\.new\b");
    for m in hm.find_iter(&f.code) {
        let (coll, cls) = (m.s(1), m.s(2));
        if coll.starts_with('@') {
            out.push((format!("{coll}[]"), format!("expr:{cls}\x1fnew()")));
        } else if let Some((a1, a2)) = &acc {
            if coll == a2 {
                out.push((format!("{}[]", f.name), format!("expr:{cls}\x1fnew()")));
                out.push((format!("{a1}[]"), format!("expr:{cls}\x1fnew()")));
            }
        }
    }
    out
}
