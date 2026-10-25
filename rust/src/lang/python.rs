//! Python adapter (lang_other.parse_python and its `_py_*` helpers): one
//! Module per file, one per class (`<file>.<Class>`, its parent the first
//! base), functions from indentation on the blanked code.
//!
//! Offsets: the Rust scan keeps bytes in place, so every position here is a
//! byte offset into the blanked text; `Call.col` is a character column, read
//! off the raw line (the blanked and the raw line share their bytes).

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;

use crate::lang::assign::assignments;
use crate::lang::chains::{expression, receiver, type_name, Opts};
use crate::lang::{Adapter, ParseCtx};
use crate::model::{Call, Func, Module};
use crate::pyre::{floor_char, Rx};
use crate::scan::blank;

pub struct Python;

impl Adapter for Python {
    fn family(&self) -> &'static str {
        "python"
    }
    fn exts(&self) -> &'static [&'static str] {
        &[".py"]
    }
    fn parse(&self, _ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module> {
        parse_python(rel, text)
    }
}

struct Pats {
    block: Rx,
    call: Rx,
    import: Rx,
    from: Rx,
    new: Rx,
    ann: Rx,
    self_attr: Rx,
    value: Rx,
    class_head: Rx,
    fixture: Rx,
    module_var: Rx,
    attr_ann: Rx,
    attr_new: Rx,
    self_assign: Rx,
    ret: Rx,
    ret_fix: Rx,
    with: Rx,
    super_dot: Rx,
    deco: Rx,
    cm_deco: Rx,
    cm_sub: Rx,
    alt_skip: Rx,
    ret_ann: Rx,
    ret_quoted: Rx,
    param: Rx,
    param_quoted: Rx,
    star: Rx,
    fixture_param: Rx,
    paren: Rx,
    word: Rx,
}

fn pats() -> &'static Pats {
    static P: OnceLock<Pats> = OnceLock::new();
    P.get_or_init(|| Pats {
        block: Rx::lit(r"^(\s*)(?:async\s+)?(def|class)\s+([A-Za-z_]\w*)"),
        call: Rx::lit(r"(?:\b([A-Za-z_]\w*)\s*\.\s*)?\b([A-Za-z_]\w*)\s*\("),
        import: Rx::lit(r"(?m)^[ \t]*import[ \t]+([\w. \t,]+)$"),
        from: Rx::lit(r"(?m)^[ \t]*from[ \t]+(\.*[\w.]*)[ \t]+import[ \t]+(\([^)]*\)|[^\n]+)"),
        new: Rx::lit(
            r"(?m)(?:^|[;(,]|\bself\.)[ \t]*([A-Za-z_]\w*)[ \t]*(?::[^=\n]*)?=[ \t]*((?:[a-z_]\w*\.)*[A-Z]\w*)[ \t]*\(",
        ),
        ann: Rx::lit(r"(?m)^[ \t]*([A-Za-z_]\w*)[ \t]*:[ \t]*([^=\n#]+?)[ \t]*(?:=|#|$)"),
        self_attr: Rx::lit(
            r"\bself\.([A-Za-z_]\w*)[ \t]*(?::[ \t]*([A-Z]\w*)(?![\w.\[])[^=\n]*)?(?:=[ \t]*((?:[a-z_]\w*\.)*[A-Z]\w*)[ \t]*\()?",
        ),
        value: Rx::lit(r"(?m)(?:[=(,\[:]|\breturn)\s*(?:self\.|cls\.)?([A-Za-z_]\w*)\s*(?=[,)\]}]|$)"),
        class_head: Rx::lit(r"\s*class\s+\w+\s*\(([^)]*)\)"),
        fixture: Rx::lit(r"\s*@(?:pytest\.)?fixture\b"),
        module_var: Rx::lit(r"([A-Za-z_]\w*)\s*:\s*([^=]+?)\s*(?:=|$)"),
        attr_ann: Rx::lit(r"\s*([A-Za-z_]\w*)\s*:\s*([^=\n#]+?)\s*(?:=|#|$)"),
        attr_new: Rx::lit(r"\s*([a-z_]\w*)\s*=\s*((?:[a-z_]\w*\.)*[A-Z]\w*)\s*\("),
        self_assign: Rx::lit(r"\bself\.([A-Za-z_]\w*)\s*(?::[^=\n]*)?=(?!=)"),
        ret: Rx::lit(r"(?m)^[ \t]*return[ \t]+(.+)$"),
        ret_fix: Rx::lit(r"(?m)^[ \t]*(?:return|yield)[ \t]+(.+)$"),
        with: Rx::lit(
            r"\bwith[ \t]+((?:[^:\n()]|\((?:[^()]|\([^()]*\))*\))+?)\s+as[ \t]+([A-Za-z_]\w*)[ \t]*[:,)]",
        ),
        super_dot: Rx::lit(r"\bsuper\(\s*\)\s*\.$"),
        deco: Rx::lit(r"^\s*@((?:[A-Za-z_]\w*\.)*)([A-Za-z_]\w*)\s*(?:#.*)?$"),
        cm_deco: Rx::lit(r"\s*@(?:contextlib\.)?(?:async)?contextmanager\b"),
        cm_sub: Rx::lit(r"^(?:[\w.]+\.)?(?:Async)?(?:Iterator|Generator)\[(.*)\]$"),
        alt_skip: Rx::lit(r"\s*(?:None\b|(?:t\.|typing\.)?Callable\b)"),
        ret_ann: Rx::lit(r"\)\s*->\s*([^:]+?)\s*:\s*$"),
        ret_quoted: Rx::lit(r#"\)\s*->\s*['"]([^'"]+)['"]\s*:"#),
        param: Rx::lit(r"(?s)\s*([A-Za-z_]\w*)\s*:\s*([^=]+?)\s*(?:=.*)?$"),
        param_quoted: Rx::lit(r#"\s*[A-Za-z_]\w*\s*:\s*['"]([^'"]+)['"]"#),
        star: Rx::lit(r"[(,]\s*(\*\*?)([A-Za-z_]\w*)"),
        fixture_param: Rx::lit(r"^[a-z_]\w*$"),
        paren: Rx::lit(r"\(([^)]*)\)"),
        word: Rx::lit(r"^\w+$"),
    })
}

fn skip_word(s: &str) -> bool {
    static K: OnceLock<HashSet<&'static str>> = OnceLock::new();
    K.get_or_init(|| {
        [
            "if", "elif", "while", "for", "return", "and", "or", "not", "in", "is", "print", "len", "range", "str",
            "int", "float", "bool", "list", "dict", "set", "tuple", "isinstance", "super", "lambda", "min", "max",
            "abs", "sorted", "enumerate", "zip", "any", "all", "sum", "map", "filter", "getattr", "setattr",
            "hasattr", "type", "repr", "round", "with", "assert", "yield", "await", "except", "def", "class", "del",
            "raise", "import", "from", "as", "async", "else", "try", "finally", "global", "nonlocal", "pass", "case",
            "match", "issubclass", "iter", "next", "id", "hash", "vars", "dir", "callable", "format", "frozenset",
            "bytes", "bytearray", "object", "reversed", "divmod", "pow", "chr", "ord", "hex", "oct", "bin", "input",
            "globals", "locals", "memoryview", "property", "staticmethod", "classmethod", "slice", "complex",
            "breakpoint", "aiter", "anext",
        ]
        .into_iter()
        .collect()
    })
    .contains(s)
}

// --- Python's str helpers (isspace includes \x1c-\x1f) ---

fn is_ws(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}
fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_ws)
}
fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_ws)
}
fn strip(s: &str) -> &str {
    s.trim_matches(is_ws)
}

/// `len(s.expandtabs(4))`
pub(crate) fn tab_width(s: &str) -> usize {
    let mut col = 0;
    for c in s.chars() {
        match c {
            '\t' => col += 4 - col % 4,
            '\n' | '\r' => col = 0,
            _ => col += 1,
        }
    }
    col
}

/// Python's `s[a:b]` on byte positions that may be off a character boundary.
fn sl(s: &str, a: usize, b: usize) -> &str {
    let b = floor_char(s, b.min(s.len()));
    let a = floor_char(s, a.min(b));
    &s[a..b]
}

fn modname(rel: &str) -> String {
    rel.rsplit_once('.').map(|(a, _)| a).unwrap_or(rel).to_string()
}

fn py_test_file(rel: &str) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let slashed = format!("/{rel}");
    base.starts_with("test_")
        || base.ends_with("_test.py")
        || base == "tests.py"
        || base == "conftest.py"
        || slashed.contains("/tests/")
        || slashed.contains("/test/")
}

fn imports(m: &mut Module, rel: &str, code: &str) {
    let p = pats();
    let d = if rel.contains('/') { rel.rsplit_once('/').unwrap().0 } else { "" };
    let where_ = |spec: &str| format!("py:{d}\0{spec}");
    for c in p.import.find_iter(code) {
        for part in c.s(1).split(',') {
            let bits: Vec<&str> = part.split_whitespace().collect();
            if bits.is_empty() {
                continue;
            }
            let path = bits[0];
            if bits.len() == 3 && bits[1] == "as" {
                m.aliases.insert(bits[2].to_string(), where_(path));
                continue;
            }
            let head = path.split('.').next().unwrap_or("");
            m.aliases.entry(head.to_string()).or_insert_with(|| where_(head));
            if path.contains('.') {
                let tail = path.rsplit('.').next().unwrap_or("");
                m.aliases.entry(tail.to_string()).or_insert_with(|| where_(path));
            }
        }
    }
    for c in p.from.find_iter(code) {
        let spec = c.s(1);
        let names = strip(c.s(2)).trim_matches(|ch| ch == '(' || ch == ')');
        for part in names.split(',') {
            let bits: Vec<&str> = part.split_whitespace().collect();
            if bits.is_empty() {
                continue;
            }
            if bits[0] == "*" {
                m.uses.push(where_(spec));
                m.reexports.push(where_(spec));
                continue;
            }
            let local = if bits.len() == 3 && bits[1] == "as" { bits[2] } else { bits[0] };
            m.imported.insert(local.to_string(), (where_(spec), bits[0].to_string()));
        }
    }
}

struct Block {
    start: usize,
    end: usize,
    indent: usize,
    class: bool,
    name: String,
}

/// The file's modules (parse_python).
pub fn parse_python(rel: &str, text: &str) -> Vec<Module> {
    let p = pats();
    let o = Opts::for_file(rel);
    let (code, _strings) = blank(text, "python", false);
    let raw: Arc<Vec<String>> = Arc::new(text.split('\n').map(String::from).collect());
    let code_lines: Arc<Vec<String>> = Arc::new(code.split('\n').map(String::from).collect());
    let lines: Vec<&str> = code_lines.iter().map(|s| s.as_str()).collect();
    let n = lines.len();
    let mut fm = Module::new(&modname(rel), rel, raw.clone(), code_lines.clone());
    imports(&mut fm, rel, &code);
    // lines that start inside a triple-quoted string
    let mut in_str: HashSet<usize> = HashSet::new();
    let mut open: Option<&str> = None;
    for (i, ln) in lines.iter().enumerate() {
        if open.is_some() {
            in_str.insert(i);
        }
        let b = ln.as_bytes();
        let mut k = 0;
        while k + 3 <= b.len() {
            let q = if &b[k..k + 3] == b"\"\"\"" {
                Some("\"\"\"")
            } else if &b[k..k + 3] == b"'''" {
                Some("'''")
            } else {
                None
            };
            match q {
                Some(q) => {
                    open = if open == Some(q) { None } else { open.or(Some(q)) };
                    k += 3;
                }
                None => k += 1,
            }
        }
    }
    let mut blocks: Vec<Block> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        let Some(m) = p.block.match_start(ln) else { continue };
        let indent = tab_width(m.s(1));
        let mut end = n - 1;
        for j in i + 1..n {
            let t = lines[j];
            if strip(t).is_empty() || in_str.contains(&j) {
                continue;
            }
            let lead = &t[..t.len() - lstrip(t).len()];
            if tab_width(lead) <= indent && !(lstrip(t).starts_with(')') || lstrip(t).starts_with(']')) {
                end = j - 1;
                break;
            }
        }
        while end > i && strip(lines[end]).is_empty() {
            end -= 1;
        }
        blocks.push(Block { start: i, end, indent, class: m.s(2) == "class", name: m.s(3).to_string() });
    }
    let enclosing = |k: usize| -> isize {
        let (a, ind) = (blocks[k].start, blocks[k].indent);
        for j in (0..k).rev() {
            let bj = &blocks[j];
            if bj.start < a && a <= bj.end && bj.indent < ind {
                return j as isize;
            }
        }
        -1
    };
    let outer: Vec<isize> = (0..blocks.len()).map(enclosing).collect();
    let test_file = py_test_file(rel);
    let fm_name = fm.name.clone();
    let mut mods: IndexMap<String, Module> = IndexMap::new();
    mods.insert(String::new(), fm);
    let mut owner_of: IndexMap<usize, String> = IndexMap::new();
    for (k, b) in blocks.iter().enumerate() {
        if !b.class {
            continue;
        }
        let mut chain = vec![b.name.clone()];
        let mut j = outer[k];
        while j >= 0 {
            if blocks[j as usize].class {
                chain.push(blocks[j as usize].name.clone());
            }
            j = outer[j as usize];
        }
        chain.reverse();
        let mname = format!("{}.{}", fm_name, chain.join("."));
        owner_of.insert(k, mname.clone());
        if mods.contains_key(&mname) {
            continue;
        }
        let mut cm = Module::new(&mname, rel, raw.clone(), code_lines.clone());
        cm.owner = fm_name.clone();
        cm.decls = vec![b.name.clone()];
        let head = lines[b.start..=b.end.min(b.start + 5)].iter().map(|x| strip(x)).collect::<Vec<_>>().join(" ");
        if let Some(bm) = p.class_head.match_start(&head) {
            let bases: Vec<String> = bm
                .s(1)
                .split(',')
                .map(|x| strip(x).to_string())
                .filter(|x| !x.is_empty() && !x.contains('='))
                .filter(|x| !["object", "ABC", "Protocol", "Generic"].contains(&x.as_str()) && !x.starts_with("Generic["))
                .collect();
            if !bases.is_empty() {
                cm.parent = format!("py:{}", bases[0].split('[').next().unwrap_or(""));
                cm.bases = bases.iter().map(|x| format!("py:{}", strip(x.split('[').next().unwrap_or("")))).collect();
            }
        }
        mods.insert(mname, cm);
    }
    let mut inside = vec![false; n];
    // (module, name) -> (module key, index in its funcs)
    let mut funcs: IndexMap<(String, String), (String, usize)> = IndexMap::new();
    for (k, b) in blocks.iter().enumerate() {
        if b.class {
            continue;
        }
        let (a, bb, name) = (b.start, b.end, b.name.as_str());
        for x in inside.iter_mut().take(bb + 1).skip(a) {
            *x = true;
        }
        let o_ = outer[k];
        let in_class = o_ >= 0 && blocks[o_ as usize].class;
        let mname = if in_class { owner_of[&(o_ as usize)].clone() } else { fm_name.clone() };
        let is_test = test_file && name.starts_with("test") && (o_ < 0 || in_class);
        let kind = if is_test {
            "test"
        } else if name.starts_with('_') && !name.starts_with("__") {
            "private"
        } else {
            "func"
        };
        let key = (mname.clone(), name.to_string());
        let code_span = lines[a..=bb].join("\n");
        let loc = match funcs.get(&key) {
            Some(l) => l.clone(),
            None => {
                let f = Func::new(&mname, name, rel, a + 1, bb + 1, code_span.clone(), kind);
                let mkey = if mods.contains_key(&mname) && mname != fm_name { mname.clone() } else { String::new() };
                let m = mods.get_mut(&mkey).unwrap();
                m.funcs.push(f);
                let l = (mkey, m.funcs.len() - 1);
                funcs.insert(key, l.clone());
                l
            }
        };
        let f = &mut mods.get_mut(&loc.0).unwrap().funcs[loc.1];
        if f.start != a + 1 || f.end != bb + 1 {
            // `@property x` and its `@x.setter`: one function, the longest span
            if bb - a > f.end - f.start {
                f.start = a + 1;
                f.end = bb + 1;
                f.code = code_span;
            }
        }
        if mname == fm_name && o_ >= 0 && !blocks[o_ as usize].class {
            // a closure in a method: its `self` is the method's
            let mut j = o_;
            while j >= 0 && !blocks[j as usize].class {
                j = outer[j as usize];
            }
            if j >= 0 {
                if let Some(ow) = owner_of.get(&(j as usize)).filter(|s| !s.is_empty()) {
                    f.bound.entry("self".into()).or_insert_with(|| format!("mod:{ow}"));
                }
            }
        }
        let mut nested = vec![false; bb - a + 1];
        for b2 in &blocks {
            if !b2.class && a < b2.start && b2.start <= bb {
                for x in b2.start..=b2.end {
                    if x >= a && x <= bb {
                        nested[x - a] = true;
                    }
                }
            }
        }
        collect(f, &lines, &raw, a, bb, true, Some((&nested, a)), o);
        let fixture = (a.saturating_sub(3)..a).any(|j| p.fixture.match_start(lines[j]).is_some());
        types(f, &lines, &raw, a, bb, test_file, fixture, o);
    }
    let mut top = Func::new(&fm_name, "<top>", rel, 1, n, String::new(), "top");
    let mut fm_bound: Vec<(String, String)> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        let ls = lstrip(ln);
        if !inside[i] && !strip(ln).is_empty() && !(ls.starts_with("class ") || ls.starts_with("import ") || ls.starts_with("from "))
        {
            collect(&mut top, &lines, &raw, i, i, false, None, o);
            let first_ws = ln.chars().next().map(is_ws).unwrap_or(false);
            if !first_ws {
                if let Some(am) = p.module_var.match_start(ln) {
                    let t = type_name(am.s(2));
                    if !t.is_empty() {
                        fm_bound.push((am.s(1).to_string(), format!("type:{t}")));
                    }
                }
                for (nm, held) in assignments("py", ln, o) {
                    fm_bound.push((nm, held));
                }
            } else {
                for (nm, held) in assignments("py", strip(ln), o) {
                    top.bound.entry(nm).or_insert(held);
                }
            }
        }
    }
    {
        let fm = mods.get_mut("").unwrap();
        for (k, v) in fm_bound {
            fm.bound.entry(k).or_insert(v);
        }
        fm.funcs.push(top);
    }
    // declared types: the class's `self.x = Foo()` / `self.x: Foo` for its methods
    for (k, b) in blocks.iter().enumerate() {
        if !b.class {
            continue;
        }
        let mname = owner_of[&k].clone();
        let (fm_aliases, fm_imported) = {
            let fm = &mods[""];
            (fm.aliases.clone(), fm.imported.clone())
        };
        let cm = mods.get_mut(&mname).unwrap();
        let mut seen: IndexMap<String, Vec<String>> = IndexMap::new();
        let add = |seen: &mut IndexMap<String, Vec<String>>, k: &str, v: &str| {
            let e = seen.entry(k.to_string()).or_default();
            if !e.iter().any(|x| x == v) {
                e.push(v.to_string());
            }
        };
        for i in b.start..=b.end {
            for m in p.self_attr.find_iter(lines[i]) {
                let ty = m.get(2).filter(|s| !s.is_empty()).or(m.get(3)).unwrap_or("");
                if !ty.is_empty() {
                    add(&mut seen, m.s(1), ty);
                }
            }
        }
        let mut body_ind: Option<usize> = None;
        for i in b.start + 1..=b.end {
            let ln = lines[i];
            if strip(ln).is_empty() {
                continue;
            }
            let ind_i = ln.chars().count() - lstrip(ln).chars().count();
            if body_ind.is_none() {
                body_ind = Some(ind_i);
            }
            if Some(ind_i) != body_ind {
                continue;
            }
            if let Some(am) = p.attr_ann.match_start(ln) {
                let ty = type_name(am.s(2));
                if !ty.is_empty() && !(ty.starts_with("ClassVar") || ty.starts_with("t.ClassVar")) {
                    add(&mut seen, am.s(1), &ty);
                    continue;
                }
            }
            if let Some(vm) = p.attr_new.match_start(ln) {
                add(&mut seen, vm.s(1), vm.s(2));
            }
        }
        for (nm, tys) in &seen {
            if tys.len() == 1 {
                cm.bound.entry(nm.clone()).or_insert_with(|| format!("type:{}", tys[0]));
            }
        }
        // `self.app = app` with `app: Flask` a parameter, `self.x = self.make()`
        for gi in 0..cm.funcs.len() {
            let g_code = cm.funcs[gi].code.clone();
            for m in p.self_assign.find_iter(&g_code) {
                let ch = expression(&g_code, m.end(), 40, o);
                if ch.is_empty() || cm.bound.contains_key(m.s(1)) {
                    continue;
                }
                let held = if ch.len() == 1 { cm.funcs[gi].bound.get(&ch[0]).cloned().unwrap_or_default() } else { String::new() };
                if held.starts_with("type:") {
                    cm.bound.insert(m.s(1).to_string(), held);
                } else if ch[0] == "self"
                    || ch[0] == "cls"
                    || ch[0].chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                    || ch[0].ends_with("()")
                    || fm_aliases.contains_key(&ch[0])
                    || fm_imported.contains_key(&ch[0])
                {
                    cm.bound.insert(m.s(1).to_string(), format!("expr:{}", ch.join("\x1f")));
                }
            }
        }
    }
    let (al, im, us) = {
        let fm = &mods[""];
        (fm.aliases.clone(), fm.imported.clone(), fm.uses.clone())
    };
    let mut out = Vec::new();
    let mut first: Option<Module> = None;
    for (k, mut m) in mods {
        if k.is_empty() {
            first = Some(m);
        } else {
            m.aliases = al.clone();
            m.imported = im.clone();
            m.uses = us.clone();
            out.push(m);
        }
    }
    out.insert(0, first.unwrap());
    out
}

/// The character column of a byte position in a line (raw and blanked lines
/// share their bytes).
fn char_col(raw: &[String], lines: &[&str], i: usize, byte: usize) -> i64 {
    let l = lines[i];
    match raw.get(i) {
        Some(r) if r.len() == l.len() && r.is_char_boundary(byte) => r[..byte].chars().count() as i64,
        _ => sl(l, 0, byte).chars().count() as i64,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect(f: &mut Func, lines: &[&str], raw: &[String], a: usize, b: usize, first: bool, skip: Option<(&[bool], usize)>, o: Opts) {
    let p = pats();
    let body = lines[a..=b].join("\n");
    let mut off = 0usize;
    for i in a..=b {
        let t0 = lines[i];
        if let Some((s, base)) = skip {
            if s[i - base] {
                off += t0.len() + 1;
                continue;
            }
        }
        let mut cut = 0usize;
        let mut t = t0;
        if (first && i == a) || p.block.match_start(t).is_some() {
            cut = t.find('(').map(|k| k + 1).unwrap_or(t.len());
            t = &t[cut..];
        }
        for m in p.call.find_iter(t) {
            let name = m.s(2);
            let mut q = m.get(1).unwrap_or("").to_string();
            if q.is_empty() {
                let before = rstrip(&t[..m.start()]);
                if p.super_dot.search(before).is_some() {
                    q = "super".into();
                } else if before.ends_with('.') {
                    q = ".".into();
                } else if skip_word(name) {
                    continue;
                }
            }
            let s2 = m.span(2).unwrap().0;
            let mut chain: Vec<String> = if q == "super" {
                vec!["super()".into()]
            } else if !q.is_empty() {
                receiver(&body, off + cut + s2, 40, o)
            } else {
                Vec::new()
            };
            if chain.len() == 1 && chain[0] == "(...)" {
                let before = rstrip(&t[..m.start()]);
                if before.ends_with("}.") || before.ends_with("].") {
                    chain = vec!["<literal>".into()];
                }
            }
            if !chain.is_empty() && q != "." {
                let last = chain.last().unwrap();
                q = if p.word.is_match(last) { last.clone() } else { ".".into() };
            }
            let col = char_col(raw, lines, i, cut + s2);
            f.calls.push(Call { name: name.into(), qual: q, line: i + 1, chain, col, targs: -1 });
        }
        if let Some(dm) = p.deco.match_start(t) {
            if !skip_word(dm.s(2)) {
                let g1 = dm.s(1);
                let q = if g1.is_empty() {
                    String::new()
                } else {
                    g1.trim_end_matches('.').rsplit('.').next().unwrap_or("").to_string()
                };
                let s2 = dm.span(2).unwrap().0;
                let chain = if q.is_empty() { Vec::new() } else { receiver(&body, off + cut + s2, 40, o) };
                let col = char_col(raw, lines, i, cut + s2);
                f.calls.push(Call { name: dm.s(2).into(), qual: q, line: i + 1, chain, col, targs: -1 });
            }
        }
        if f.kind != "top" {
            for m in p.new.find_iter(t) {
                f.bound.insert(m.s(1).to_string(), format!("type:{}", m.s(2)));
            }
            for m in p.ann.find_iter(t) {
                let ty = type_name(m.s(2));
                if !ty.is_empty() {
                    f.bound.entry(m.s(1).to_string()).or_insert(format!("type:{ty}"));
                }
            }
        }
        for m in p.value.find_iter(t) {
            f.refs.insert(m.s(1).to_string());
        }
        off += t0.len() + 1;
    }
}

/// What a Python function returns, what its locals are assigned, and in a
/// test file each plain parameter: the pytest fixture of that name.
#[allow(clippy::too_many_arguments)]
fn types(f: &mut Func, lines: &[&str], raw: &[String], a: usize, b: usize, test_file: bool, fixture: bool, o: Opts) {
    let p = pats();
    let mut head = lines[a].to_string();
    let mut k = a;
    while !head.contains("):") && !head.replace(' ', "").contains(")->") && k < b {
        k += 1;
        head.push(' ');
        head.push_str(strip(lines[k]));
    }
    let k_end = k;
    let mut ann: String = p.ret_ann.search(&head).map(|m| m.s(1).to_string()).unwrap_or_default();
    if matches!(strip(&ann).chars().next(), Some('\'') | Some('"')) {
        // `-> "Color"`: a forward reference (blanked in code)
        let rhead = (a..=k).map(|j| strip(raw.get(j).map(|s| s.as_str()).unwrap_or(""))).collect::<Vec<_>>().join(" ");
        if let Some(rrm) = p.ret_quoted.search(&rhead) {
            ann = rrm.s(1).to_string();
        }
    }
    if !ann.is_empty() && (a.saturating_sub(3)..a).any(|j| p.cm_deco.match_start(lines[j]).is_some()) {
        let s = p.cm_sub.sub(r"\1", strip(&ann));
        ann = s.split(',').next().unwrap_or("").to_string();
    }
    if !ann.is_empty() && ann.contains('|') {
        let (mut alts, mut d, mut cur) = (Vec::<String>::new(), 0i64, String::new());
        for ch in ann.chars() {
            d += "[(".contains(ch) as i64;
            d -= "])".contains(ch) as i64;
            if ch == '|' && d == 0 {
                alts.push(std::mem::take(&mut cur));
            } else {
                cur.push(ch);
            }
        }
        alts.push(cur);
        let mut named: Vec<String> = Vec::new();
        for x in &alts {
            if !strip(x).is_empty() && p.alt_skip.match_start(x).is_none() {
                let t = type_name(x);
                if !t.is_empty() && !named.contains(&t) {
                    named.push(t);
                }
            }
        }
        if named.len() == 1 {
            ann = named.pop().unwrap();
        }
    }
    if !ann.is_empty() {
        let t = type_name(&ann);
        if !t.is_empty() {
            f.returns = t;
        }
    }
    let paren = head.find('(').map(|i| i + 1).unwrap_or(0);
    let sig = &head[paren..];
    let (mut depth, mut last) = (0i64, 0usize);
    let mut parts: Vec<&str> = Vec::new();
    for (kk, ch) in sig.char_indices() {
        depth += "([{".contains(ch) as i64;
        depth -= ")]}".contains(ch) as i64;
        if depth < 0 || (ch == ',' && depth == 0) {
            parts.push(&sig[last..kk]);
            last = kk + 1;
            if depth < 0 {
                break;
            }
        }
    }
    // the same head from the raw lines, at the same offsets
    let mut rsig = String::new();
    {
        let mut rhead = raw.get(a).cloned().unwrap_or_default();
        for j in a + 1..=k_end {
            let lead = lines[j].len() - lstrip(lines[j]).len();
            let end = rstrip(lines[j]).len();
            rhead.push(' ');
            rhead.push_str(sl(raw.get(j).map(|s| s.as_str()).unwrap_or(""), lead, end));
        }
        if rhead.len() == head.len() {
            rsig = sl(&rhead, paren, rhead.len()).to_string();
        }
    }
    let mut off = 0usize;
    for prm in &parts {
        let pm = p.param.match_start(prm);
        let mut ann_ = pm.as_ref().map(|m| m.s(2).to_string()).unwrap_or_default();
        if pm.is_some() && !rsig.is_empty() && matches!(strip(&ann_).chars().next(), Some('\'') | Some('"')) {
            let rp = sl(&rsig, off, off + prm.len());
            if let Some(qm) = p.param_quoted.match_start(rp) {
                ann_ = qm.s(1).to_string();
            }
        }
        if let Some(pm) = &pm {
            let ty = type_name(&ann_);
            if !ty.is_empty() {
                f.bound.entry(pm.s(1).to_string()).or_insert(format!("type:{ty}"));
            }
        }
        off += prm.len() + 1;
    }
    for m in p.star.find_iter(&head) {
        f.bound.insert(m.s(2).to_string(), if m.s(1) == "**" { "type:dict".into() } else { "type:tuple".into() });
    }
    let body = lines[a..=b].join("\n");
    if f.returns.is_empty() {
        let pat = if fixture { &p.ret_fix } else { &p.ret };
        let rets: Vec<String> = pat.find_iter(&body).iter().map(|m| m.s(1).to_string()).collect();
        if !rets.is_empty() {
            if rets.iter().all(|r| strip(r) == "self" || strip(r) == "cls") {
                f.returns = "Self".into();
            } else {
                let ch = expression(rets.last().unwrap(), 0, 40, o);
                if !ch.is_empty() {
                    f.returns = format!("expr:{}", ch.join("\x1f"));
                }
            }
        }
    }
    for (nm, held) in assignments("py", &body, o) {
        let first = held.split('\x1f').next().unwrap_or("");
        if nm != f.name || (first != format!("expr:{}", f.name) && first != format!("expr:{}()", f.name)) {
            f.bound.entry(nm).or_insert(held);
        }
    }
    for m in p.with.find_iter(&body) {
        let ch = expression(&format!("{}\n", m.s(1)), 0, 40, o);
        if !ch.is_empty() {
            f.bound.entry(m.s(2).to_string()).or_insert(format!("expr:{}", ch.join("\x1f")));
        }
    }
    if test_file {
        if let Some(pm) = p.paren.search(&head) {
            for prm in pm.s(1).split(',') {
                let prm = strip(prm);
                if p.fixture_param.is_match(prm) && !["self", "cls", "request"].contains(&prm) {
                    f.bound.entry(prm.to_string()).or_insert(format!("fixture:{prm}"));
                }
            }
        }
    }
}
