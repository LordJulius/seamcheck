//! The functions of a file and the calls in them (lang_ruby._attrs, _lets,
//! _scopes, _collect, _declared).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use super::blocks::Blk;
use super::pats::*;
use super::recv::{expression, receiver};
use super::util::*;
use super::St;
use crate::lang::chains::type_name;
use crate::model::{Call, Func};
use crate::pyre::Rx;

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [OnceLock<Rx>; 16] = [const { OnceLock::new() }; 16];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

fn anch(k: usize, pat: &'static str) -> &'static Anch {
    static CELLS: [OnceLock<Anch>; 8] = [const { OnceLock::new() }; 8];
    CELLS[k].get_or_init(|| Anch::new(pat))
}

/// The symbols an `attr_reader :a, :b,` names (it may go on over lines).
fn attr_names(lines: &[String], i: usize, head_end: usize) -> Vec<String> {
    let mut text = from(&lines[i], head_end).to_string();
    let mut j = i;
    while rstrip(&text).ends_with(',') && j + 1 < lines.len() {
        j += 1;
        text.push(' ');
        text.push_str(&lines[j]);
    }
    rx(0, r":(\w+)").findall(&text).into_iter().map(String::from).collect()
}

/// attr_reader / attr_accessor are methods returning their field;
/// def_delegators / delegate are methods calling on through a field.
pub fn attrs(
    st: &mut St,
    blocks: &[Blk],
    home: &HashMap<usize, usize>,
    body_blk: &[i64],
    lines: &[String],
    raw: &[String],
    fkey: &mut HashMap<(String, String), usize>,
) {
    let p = pats();
    for b in blocks {
        if !matches!(b.kind, "class" | "module" | "sclass") || !home.contains_key(&b.i) {
            continue;
        }
        let ci = home[&b.i];
        for i in b.a + 1..b.b {
            if body_blk[i] != b.i as i64 {
                continue;
            }
            let ln = lines[i].as_str();
            let cname = st.ms[ci].name.clone();
            let cfile = st.ms[ci].file.clone();
            if let Some(am) = p.attr.start(ln) {
                for nm in attr_names(lines, i, am.end()) {
                    let key = (cname.clone(), nm.clone());
                    if !fkey.contains_key(&key) {
                        let mut f = Func::new(&cname, &nm, &cfile, i + 1, i + 1, ln.to_string(), "func");
                        f.returns = format!("expr:@{nm}");
                        let fi = st.add(ci, f);
                        fkey.insert(key, fi);
                    }
                }
                continue;
            }
            let dm = p.delegators.start(ln);
            let rm = if dm.is_some() { None } else { p.rails_delegate.start(ln) };
            if dm.is_some() || rm.is_some() {
                let via = if let Some(d) = &dm { d.s(1) } else { rm.as_ref().unwrap().s(2) };
                let list = if let Some(d) = &dm { d.s(2) } else { rm.as_ref().unwrap().s(1) };
                let mut names: Vec<String> =
                    rx(1, r":(\w+[?!]?)").findall(list).into_iter().map(String::from).collect();
                if dm.is_some() && lstrip(ln).starts_with("def_delegator ") {
                    names.truncate(1); // def_delegator :@x, :meth, :alias: one method
                }
                let seg = if via.starts_with('@') { via.to_string() } else { format!("{via}()") };
                for nm in names {
                    let key = (cname.clone(), nm.clone());
                    if fkey.contains_key(&key) {
                        continue;
                    }
                    let mut f = Func::new(&cname, &nm, &cfile, i + 1, i + 1, ln.to_string(), "func");
                    let qual = if seg.starts_with('@') { ".".to_string() } else { via.to_string() };
                    let mut c = Call::new(&nm, &qual, i + 1);
                    c.chain = vec![seg.clone()];
                    c.col = ln.find(&nm).map(|k| col_of(&raw[i], k)).unwrap_or(-1).max(0);
                    f.calls.push(c);
                    let fi = st.add(ci, f);
                    fkey.insert(key, fi);
                }
            }
        }
    }
}

/// Rails `scope :hits_by_ip, ->(ip) { ... }`: a class method of that name
/// whose body is the lambda (on its line).
pub fn scopes(
    st: &mut St,
    blocks: &[Blk],
    home: &HashMap<usize, usize>,
    lines: &[String],
    fkey: &mut HashMap<(String, String), usize>,
) -> Vec<usize> {
    let scope = anch(0, r"^\s*scope\s+:(\w+[?!]?)\s*,\s*(?:->|lambda)\s*(?:\([^)]*\))?\s*(?:\{|do\b)");
    let mut out = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        let Some(m) = scope.start(ln) else { continue };
        let mi = enclosing_class(blocks, home, i, |b| b.a <= i && i <= b.b && matches!(b.kind, "class" | "module"));
        let mname = st.ms[mi].name.clone();
        let key = (mname.clone(), m.s(1).to_string());
        if fkey.contains_key(&key) {
            continue;
        }
        let f = Func::new(&mname, m.s(1), &st.ms[mi].file.clone(), i + 1, i + 1, ln.clone(), "func");
        let fi = st.add(mi, f);
        fkey.insert(key, fi);
        out.push(fi);
    }
    out
}

/// The module of the innermost (latest-starting) class among the blocks that pass `keep`; the file when none.
fn enclosing_class(blocks: &[Blk], home: &HashMap<usize, usize>, _line: usize, keep: impl Fn(&Blk) -> bool) -> usize {
    let mut best: Option<&Blk> = None;
    for b in blocks.iter().filter(|b| keep(b) && matches!(b.kind, "class" | "module")) {
        if best.map(|x| b.a > x.a).unwrap_or(true) {
            best = Some(b);
        }
    }
    best.map(|b| home[&b.i]).unwrap_or(0)
}

/// RSpec `let(:site) { fixture_site }` / `subject { ... }` define a method of
/// the example group: a bare `site` calls it.
pub fn lets(
    st: &mut St,
    blocks: &[Blk],
    home: &HashMap<usize, usize>,
    code: &str,
    starts: &[usize],
    lines: &[String],
    fkey: &mut HashMap<(String, String), usize>,
) {
    let p = pats();
    for (i, ln) in lines.iter().enumerate() {
        let Some(m) = p.let_.start(ln) else { continue };
        let name = m.get(1).filter(|s| !s.is_empty()).unwrap_or("subject").to_string();
        let inner = |b: &Blk| (b.a <= i && i <= b.b && b.kind != "do") || (b.a < i && i <= b.b);
        if blocks.iter().any(|b| inner(b) && matches!(b.kind, "def" | "test")) {
            continue;
        }
        let mi = enclosing_class(blocks, home, i, inner);
        let mname = st.ms[mi].name.clone();
        let key = (mname.clone(), name.clone());
        if fkey.contains_key(&key) {
            continue;
        }
        let mut f = Func::new(&mname, &name, &st.ms[mi].file.clone(), i + 1, i + 1, ln.clone(), "func");
        let ch = if m.s(2) == "{" { expression(code, starts[i] + m.end(), &|_, _| false) } else { Vec::new() };
        if !ch.is_empty() {
            f.returns = format!("expr:{}", ch.join("\x1f"));
        }
        let fi = st.add(mi, f);
        fkey.insert(key, fi);
    }
}

// --- calls ----------------------------------------------------------------------

/// Parameter names of a def head `(a, b = 1, *c, d:, &e)`.
fn params(code: &str, span: Option<(usize, usize)>) -> Vec<String> {
    let Some((a, z)) = span else { return Vec::new() };
    let text0 = sub(code, a, z);
    let text: &str = if text0.starts_with('(') {
        let t = &text0[1..];
        match t.char_indices().next_back() {
            Some((k, _)) => &t[..k],
            None => t,
        }
    } else {
        // `def x a, b` (no parentheses): to the end of the line
        let e = find(code, "\n", a).unwrap_or(code.len());
        sub(code, a, e)
    };
    let pm = anch(1, r"\s*[*&]{0,2}([a-z_]\w*)");
    let mut out = Vec::new();
    let mut depth = 0i64;
    let mut cur = String::new();
    for ch in text.chars().chain(std::iter::once(',')) {
        if "([{".contains(ch) {
            depth += 1;
        } else if ")]}".contains(ch) {
            depth -= 1;
        }
        if ch == ',' && depth == 0 {
            if let Some(m) = pm.start(&cur) {
                out.push(m.s(1).to_string());
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    out
}

/// Local name -> the position it is first assigned at (-1: a parameter).
fn locals(b: Option<&Blk>, code: &str, spans: &[(usize, usize)]) -> HashMap<String, i64> {
    let p = pats();
    let mut loc: HashMap<String, i64> = HashMap::new();
    if let Some(b) = b {
        if b.kind == "def" {
            for nm in params(code, b.params) {
                loc.insert(nm, -1);
            }
        }
    }
    let ident = &p.lower_ident;
    let back = rx(2, r"(?<![@\w])[a-z_]\w*");
    for &(a, z) in spans {
        let seg = sub(code, a, z);
        for r in [&p.block_params, &p.for_var] {
            for m in r.find_iter(seg) {
                for nm in ident.findall(m.s(1)) {
                    loc.entry(nm.to_string()).or_insert((a + m.start()) as i64);
                }
            }
        }
        for m in p.rescue_var.find_iter(seg) {
            let at = m.span(1).map(|x| x.0).unwrap_or(0);
            loc.entry(m.s(1).to_string()).or_insert((a + at) as i64);
        }
        for m in p.multi.find_iter(seg) {
            let at = m.span(1).map(|x| x.0).unwrap_or(0);
            for nm in back.findall(m.s(1)) {
                loc.entry(nm.to_string()).or_insert((a + at) as i64);
            }
        }
        for m in p.assign.find_iter(seg) {
            let at = m.span(1).map(|x| x.0).unwrap_or(0);
            loc.entry(m.s(1).to_string()).or_insert((a + at) as i64);
        }
    }
    loc
}

fn add(f: &mut Func, name: &str, chain: Vec<String>, line: usize, col: i64) {
    let q = match chain.last() {
        Some(l) if l == "self" => "self".to_string(),
        Some(l) if first_upper(l) => l.replace("::", "."),
        Some(l) if !l.is_empty() && l.chars().all(|c| isalnum(c) || c == '_') => l.clone(),
        _ => ".".to_string(),
    };
    let mut c = Call::new(name, &q, line);
    c.chain = chain;
    c.col = col;
    f.calls.push(c);
}

fn drop2(s: &str) -> &str {
    // Python s[:-2]
    let mut it = s.char_indices().rev();
    it.next();
    match it.next() {
        Some((k, _)) => &s[..k],
        None => "",
    }
}

fn expr_spec(ch: &[String]) -> String {
    format!("expr:{}", ch.join("\x1f"))
}

/// The calls (and locals' bindings) on the lines f owns.
#[allow(clippy::too_many_arguments)]
pub fn collect(
    f: &mut Func,
    b: Option<&Blk>,
    code: &str,
    starts: &[usize],
    idx: &[usize],
    lines: &[String],
    raw: &[String],
    ends: &HashMap<usize, usize>,
    blocks: &[Blk],
) {
    let p = pats();
    let spans: Vec<(usize, usize)> = idx.iter().map(|&i| (starts[i], starts[i] + lines[i].len())).collect();
    let loc = locals(b, code, &spans);
    let is_local = |name: &str, pos: usize| -> bool { loc.get(name).map(|&q| q <= pos as i64).unwrap_or(false) };
    let mut top_seen: HashSet<String> = f.bound.keys().cloned().collect();
    let mut skip: (usize, usize) = (usize::MAX, 0);
    if let Some(b) = b {
        if b.kind == "def" {
            if let Some((_, e)) = b.name_span {
                skip = (b.pos, e); // `def site.process`: neither name is a call
            }
        }
    }
    let mm_ = anch(2, r"\s*([a-z_]\w*(?:\s*,\s*\*?[a-z_]\w*)+)\s*=\s*((?:::)?[A-Z]\w*(?:::[A-Z]\w*)*)\s*[.(]");
    let compound = anch(3, r"\s*(?:\|\||&&|\*\*|<<|>>|[-+*/%|&^])=");
    let dm_re = anch(4, r"\s*[a-z_]\w*\s*\|\|\s*");
    let lowers = &p.lower_ident;
    let lead_arg = rx(3, r"(?<![:\w@$])[a-z_]\w*");
    for &i in idx {
        let a = starts[i];
        let ln = lines[i].as_str();
        if strip(ln).is_empty() {
            continue;
        }
        let lead = lstrip(ln);
        let indent = ln.len() - lead.len();
        if let Some(am_) = p.alias.start(lead) {
            f.refs.insert(am_.s(1).to_string());
            f.refs.insert(am_.s(2).to_string());
            continue;
        }
        for m in p.word.find_iter(ln) {
            let mut name = m.s(1).to_string();
            let pcol = m.start();
            let pp = a + pcol;
            if skip.0 <= pp && pp < skip.1 {
                continue;
            }
            let mut e = a + m.end();
            let mut nxt = sub(code, e, e + 2);
            if name.ends_with('!') && nxt.starts_with('=') {
                name.pop(); // `x!= y`
                e -= 1;
                nxt = sub(code, e, e + 2);
            }
            let prev: Option<u8> = if pp > 0 { Some(byte_at(code, pp - 1)) } else { None };
            let prev2 = if pp >= 2 { sub(code, pp - 2, pp) } else { "" };
            if nxt.starts_with(':') && nxt != "::" {
                continue; // a hash key / keyword argument `name:`
            }
            if prev == Some(b':') && prev2 != "::" {
                continue; // a symbol
            }
            if prev.is_none() || prev == Some(b'@') || prev == Some(b'$') || first_upper(&name) || name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                continue;
            }
            let after = sub(code, e, e + 4).trim_start_matches([' ', '\t']);
            let assigns = after.starts_with('=') && !(after.starts_with("==") || after.starts_with("=~") || after.starts_with("=>"));
            let col = col_of(&raw[i], pcol);
            if (prev == Some(b'.') && prev2 != "..") || prev2 == "::" {
                // (`a..b`: b is a plain name)
                if !assigns {
                    // (`obj.x = v`: a setter)
                    let ch = receiver(code, pp, &is_local);
                    add(f, &name, ch, i + 1, col);
                }
                continue;
            }
            if is_keyword(&name) {
                if name == "super" {
                    if let Some(b) = b {
                        if b.kind == "def" {
                            let mut c = Call::new(&b.name, "super", i + 1);
                            c.col = col;
                            f.calls.push(c);
                        }
                    }
                }
                continue;
            }
            if assigns {
                continue;
            }
            if is_declare(&name) {
                continue;
            }
            if byte_at(code, e) != b'(' && is_local(&name, pp) {
                continue;
            }
            let mut c = Call::new(&name, "", i + 1);
            c.col = col;
            f.calls.push(c);
        }
        for m in p.symbol.find_iter(ln) {
            f.refs.insert(m.s(1).trim_start_matches('@').to_string());
        }
        for cb in p.const_block.find_iter(ln) {
            let cst = cb.s(1).trim_start_matches(':');
            let args = if !cb.s(2).is_empty() { cb.s(2) } else { cb.s(3) };
            let head = cst.split("::").next().unwrap_or("");
            if !STDLIB_YIELDS.contains(&head) && lead_arg.is_match(args) {
                continue; // it was handed a value of ours: what it yields may be ours
            }
            for nm in lowers.findall(cb.s(4)) {
                f.bound.entry(nm.to_string()).or_insert_with(|| format!("lib:{cst}"));
            }
        }
        for yb in p.call_block.find_iter(ln) {
            let (meth, nm) = (yb.s(1), yb.s(2));
            let mstart = yb.span(1).map(|x| x.0).unwrap_or(0);
            if is_iter(meth) || is_keyword(meth) || is_same_elements(meth) || is_local(meth, a + mstart) {
                continue;
            }
            let mut src = receiver(code, a + mstart, &is_local);
            if src.is_empty() {
                src = vec!["self".into()];
            }
            if src.iter().any(|s| s == "[]" || s == "(...)") {
                continue;
            }
            let mut parts = src.clone();
            parts.push(format!("{meth}{{}}"));
            let spec = expr_spec(&parts);
            let held = f.bound.get(nm).cloned();
            if first_upper(&src[0]) {
                let want = format!("lib:{}", src[0].trim_start_matches(':'));
                if held.as_deref() == Some(want.as_str()) {
                    // (kept if the constant is the project's, see _rewrite_constants)
                    f.bound.insert(nm.to_string(), format!("{want}\x1e{spec}"));
                }
            } else if held.is_none() {
                f.bound.insert(nm.to_string(), spec);
            } else if held.as_deref() != Some(spec.as_str()) && held.as_deref().unwrap().ends_with("{}") {
                f.bound.insert(nm.to_string(), "lam:?".into()); // (one name, two blocks)
            }
        }
        for im_ in p.iter_block.find_iter(ln) {
            // `docs.sort.each do |doc|`: doc is an element of docs
            let s1 = im_.span(1).map(|x| x.0).unwrap_or(0);
            let mut src = expression(code, a + s1, &is_local);
            if src.is_empty() || !is_iter(drop2(src.last().unwrap())) {
                continue;
            }
            src.pop(); // the iterator itself
            while src.len() > 1 && is_same_elements(drop2(src.last().unwrap())) {
                src.pop(); // `docs.sort.each`: the same elements
            }
            if src.is_empty() || src == ["self"] {
                continue;
            }
            if src.last().unwrap().ends_with("()") {
                let l = src.pop().unwrap();
                src.push(drop2(&l).to_string()); // (a reader: `docs` is `@docs`, read for its `docs[]`)
            }
            let nm: Vec<&str> = lowers.findall(im_.s(2)).into_iter().take(1).collect();
            if let Some(nm) = nm.first() {
                let spec = format!("elem:{}", src.join("\x1f"));
                match f.bound.get(*nm).cloned() {
                    None => {
                        f.bound.insert(nm.to_string(), spec);
                    }
                    Some(held) => {
                        if held != spec && held.starts_with("elem:") {
                            f.bound.insert(nm.to_string(), "lam:?".into()); // (one name, two collections)
                        }
                    }
                }
            }
        }
        if let Some(rb_) = p.rescue_bind.search(ln) {
            let classes: Vec<String> = match rb_.get(1) {
                Some(g) if !g.is_empty() => split_commas(g),
                _ => vec!["StandardError".to_string()],
            };
            if classes.len() == 1 {
                // `rescue Foo => e`: e is a Foo (a bare `rescue`, a StandardError)
                f.bound.entry(rb_.s(2).to_string()).or_insert_with(|| format!("expr:{}\x1fnew()", classes[0]));
            }
        }
        if let Some(im) = p.ivar_set_at.at(ln, indent) {
            let end = a + im.end();
            let win = crate::pyre::take_chars(from(code, end), 80);
            let extra = dm_re.start(win).map(|d| d.end()).unwrap_or(0);
            let ch = expression(code, end + extra, &is_local); // (`@pool = pool || default`: the default)
            if !ch.is_empty() && !(first_lower(&ch[0]) && !ch[0].ends_with("()") && !f.bound.contains_key(&ch[0])) {
                f.bound.entry(im.s(1).to_string()).or_insert_with(|| expr_spec(&ch));
            }
        }
        if let Some(mm) = mm_.start(ln) {
            // `rd, wr = IO.pipe`: each an element of a constant's value
            for nm in lowers.findall(mm.s(1)) {
                f.bound.entry(nm.to_string()).or_insert_with(|| format!("lib:{}", mm.s(2).trim_start_matches(':')));
            }
        }
        if let Some(am) = p.assign_at.at(ln, indent) {
            let g1 = am.s(1).to_string();
            let g1_end = am.span(1).map(|x| x.1).unwrap_or(0);
            if compound.start(from(ln, g1_end)).is_none() {
                let ch = expression(code, a + am.end(), &is_local);
                if f.kind != "top" {
                    if !ch.is_empty() && ch[0] != format!("{g1}()") {
                        f.bound.entry(g1).or_insert_with(|| expr_spec(&ch));
                    }
                } else if !top_seen.contains(&g1) {
                    // a script's or a class body's local: one name assigned one way; assigned
                    // again otherwise, unknown
                    let spec = if !ch.is_empty() && first_upper(&ch[0]) && ch.last().unwrap() == "new()" {
                        Some(expr_spec(&ch))
                    } else {
                        None
                    };
                    let differs = match &spec {
                        None => true,
                        Some(s) => f.bound.get(&g1).map(|h| h != s).unwrap_or(false),
                    };
                    if differs {
                        top_seen.insert(g1.clone());
                        if f.bound.get(&g1).map(|h| h.starts_with("expr:")).unwrap_or(false) {
                            f.bound.shift_remove(&g1);
                        }
                    } else {
                        f.bound.insert(g1, spec.unwrap());
                    }
                }
            }
        }
    }
    if let Some(b) = b {
        if b.kind == "def" && f.name != "<top>" {
            declared(f, b, code, raw, &is_local, ends, blocks);
        }
    }
}

fn yard_type(text: &str) -> String {
    let parts: Vec<&str> = text.split(',').map(strip).filter(|p| *p != "nil" && !p.is_empty()).collect();
    if parts.len() != 1 {
        return String::new();
    }
    let t = type_name(&parts[0].replace("::", "\0")).replace('\0', "::");
    if strip(parts[0]) == "self" || strip(parts[0]) == "Self" {
        return "Self".into();
    }
    if t.is_empty() || t == "Boolean" || t == "void" || t == "Object" || !first_upper(&t) {
        return String::new();
    }
    t
}

/// A method's return type and its parameters' types, from what the code says:
/// YARD `@return [Foo]` / `@param x [Foo]`, a Sorbet `sig`, and the last
/// expression of the body (`Foo.new(...)`, `self`, `@field`).
fn declared(
    f: &mut Func,
    b: &Blk,
    code: &str,
    raw: &[String],
    is_local: &dyn Fn(&str, usize) -> bool,
    ends: &HashMap<usize, usize>,
    blocks: &[Blk],
) {
    let p = pats();
    let a = b.a;
    let mut doc: Vec<&str> = Vec::new();
    let mut j = a as isize - 1;
    while j >= 0 {
        let r = raw[j as usize].as_str();
        let l = lstrip(r);
        let cond = l.starts_with('#')
            || l.starts_with("sig")
            || ((strip(r) == "end" || strip(r) == "}") && doc.last().map(|d| lstrip(d).starts_with("sig")).unwrap_or(false));
        if !cond {
            break;
        }
        doc.push(r);
        j -= 1;
    }
    for d in &doc {
        if let Some(rm) = p.yard_ret.start(d) {
            if f.returns.is_empty() {
                let t = yard_type(rm.s(1));
                if !t.is_empty() {
                    f.returns = if t == "Self" { t } else { format!("expr:{t}\x1fnew()") };
                }
            }
        }
        let pm = p.yard_param.start(d).map(|m| (m.s(1).to_string(), m.s(2).to_string())).or_else(|| {
            p.yard_param2.start(d).map(|m| (m.s(2).to_string(), m.s(1).to_string()))
        });
        if let Some((nm, ty)) = pm {
            let t = yard_type(&ty);
            if !t.is_empty() && t != "Self" {
                f.bound.entry(nm).or_insert_with(|| format!("expr:{t}\x1fnew()"));
            }
        }
    }
    let sig: String = doc.iter().rev().filter(|d| !lstrip(d).starts_with('#')).cloned().collect::<Vec<_>>().join(" ");
    if !sig.is_empty() {
        if let Some(sm) = p.sig_ret.search(&sig) {
            if f.returns.is_empty() && first_upper(sm.s(1)) {
                f.returns = format!("expr:{}\x1fnew()", sm.s(1));
            }
        }
        let head = sig.split(".returns").next().unwrap_or("");
        for m in p.sig_param.find_iter(head) {
            f.bound.entry(m.s(1).to_string()).or_insert_with(|| format!("expr:{}\x1fnew()", m.s(2)));
        }
    }
    if !f.returns.is_empty() {
        return;
    }
    let n = code.len();
    let ch: Vec<String>;
    let nl_after = py_find(code, "\n", b.pos as isize);
    let end_pos = b.end_pos.unwrap_or(0);
    if b.endless > 0 {
        ch = expression(code, b.endless, is_local);
    } else if end_pos > 0 && (end_pos as isize) < (if nl_after >= 0 { nl_after } else { n as isize }) {
        // `def get(uri, opts = {})     request(GET, uri, opts)     end`: one line
        let one = anch(5, r"def\s+(?:self\.)?[\w?!=]+\s*(?:\((?:[^()]|\([^()]*\))*\))?[ \t;]*");
        let seg = sub(code, b.pos, end_pos);
        let om = one.start(seg);
        match om {
            Some(om) => {
                let body = sub(code, b.pos + om.end(), end_pos);
                ch = if !strip(body).is_empty() && !body.contains(';') {
                    expression(code, b.pos + om.end(), is_local)
                } else {
                    Vec::new()
                };
            }
            None => ch = Vec::new(),
        }
    } else {
        let mut end_pos = end_pos;
        // `def request ... MockResponse.new(...)\n ensure\n body.close ... end`: the value is the
        // last statement before the def's own `ensure` / `rescue` (at the def's indentation)
        let def_ind = b.pos - (rfind(code, "\n", b.pos).map(|x| x + 1).unwrap_or(0));
        if end_pos > 0 {
            let cl = crate::pyre::rx(&format!(r"(?m)^[ \t]{{{def_ind}}}(?:ensure|rescue)\b")).unwrap();
            let from_ = nl_after.max(0) as usize;
            if let Some(c) = cl.search_range(code, from_, end_pos) {
                end_pos = c.start();
            }
        }
        let mut k = py_rfind(code, "\n", end_pos as isize);
        let mut s: isize = if k > 0 { py_rfind(code, "\n", k) + 1 } else { 0 };
        // the last statement before `end`: a line of its own, not a nested block's end
        while k > 0 && strip(py_slice(code, s, k)).is_empty() && s > py_find(code, "\n", b.pos as isize) {
            k = s - 1;
            s = py_rfind(code, "\n", k) + 1;
        }
        let body_start = py_find(code, "\n", b.pos as isize);
        // a statement over several lines (`Foo.new(a,\n b).tap(&:read)`): from its first line
        while s > body_start + 1 && {
            let seg = py_slice(code, s, k);
            seg.matches(')').count() > seg.matches('(').count()
        } {
            s = py_rfind(code, "\n", s - 1) + 1;
        }
        let mut last: String =
            if k > 0 && s > body_start { strip(py_slice(code, s, k)).to_string() } else { String::new() };
        let blk: Option<usize> = if last == "end" {
            let at = py_find(code, "end", s);
            if at >= 0 { ends.get(&(at as usize)).copied() } else { None }
        } else {
            None
        };
        if let Some(bi) = blk {
            // `Foo.new(x).tap do |f| ... end`: the value the block hangs off
            let bpos = blocks[bi].pos;
            s = py_rfind(code, "\n", bpos as isize) + 1;
            last = strip(py_slice(code, s, bpos as isize)).to_string();
        }
        let stop = rx(4, r"(?:end|else|elsif|when|in|rescue|ensure|then|\}|\])\b");
        if last.is_empty() || stop.match_start(&last).is_some() || last.ends_with("do") || last.ends_with('|') {
            return;
        }
        let mut st_ = py_find(code, &last, s);
        if last.starts_with("return ") {
            st_ += 7;
        }
        let am = anch(6, r"@{0,2}\w+\s*(?:\|\|)?=(?![=~>])\s*|[@\w.]+\[[^\]\n]*\]\s*\|\|=\s*").start(&last).map(|m| m.end());
        if let Some(e) = am {
            st_ += e as isize; // `@x ||= Foo.new(...)`, `cache[k] ||= build(k)`: the value assigned
        }
        let stu = st_.max(0) as usize;
        let mut c = expression(code, stu, is_local);
        if c.is_empty() && am.is_none() {
            let tail = strip(sub(code, stu, k.max(0) as usize));
            let fm = rx(5, r"[^(){}\n]*\|\|\s*@{0,2}[a-z_][\w.]*[?!]?(?:\([^()\n]*\))?");
            if fm.fullmatch(tail).is_some() {
                // `Thread.current[:pool] || local_pool`: a value cached elsewhere, else the default built here
                let at = rfind_range(code, "||", stu, k.max(0) as usize).map(|x| x + 2).unwrap_or(1);
                c = expression(code, at, is_local);
            }
        }
        ch = c;
    }
    if ch.is_empty() {
        return;
    }
    if ch == ["self"] || ch == ["new()"] {
        f.returns = "Self".into();
    } else if !(first_lower(&ch[0]) && !ch[0].ends_with("()") && !f.bound.contains_key(&ch[0])) {
        f.returns = expr_spec(&ch);
    }
}

