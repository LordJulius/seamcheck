//! Elixir adapter (lang_elixir.py): one Module per `defmodule` (nested
//! modules included), its functions (ExUnit `test` / `setup` blocks too, kind
//! "test"; clauses of one function merged into the first), its aliases,
//! imports and `use`s, and each function's calls.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use crate::lang::{Adapter, ParseCtx};
use crate::model::{Call, Func, Module};
use crate::pyre::Rx;
use crate::scan::blank;

pub struct Elixir;

impl Adapter for Elixir {
    fn family(&self) -> &'static str {
        "elixir"
    }
    fn exts(&self) -> &'static [&'static str] {
        &[".ex", ".exs"]
    }
    fn parse(&self, _ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module> {
        parse(rel, text)
    }
}

struct Pats {
    defmod: Rx,
    def: Rx,
    test: Rx,
    test_name: Rx,
    do_end: Rx,
    head_open: Rx,
    alias_open: Rx,
    alias: Rx,
    import: Rx,
    use_: Rx,
    qcall: Rx,
    lcall: Rx,
    pipe: Rx,
    capture: Rx,
    child: Rx,
    ident: Rx,
}

fn pats() -> &'static Pats {
    static P: OnceLock<Pats> = OnceLock::new();
    P.get_or_init(|| Pats {
        defmod: Rx::lit(r"^(\s*)defmodule\s+([A-Z][\w.]*)\s+do\b"),
        def: Rx::lit(r"^(\s*)(defp?|defmacrop?|defguardp?|defdelegate)\s+([a-z_]\w*[?!]?)"),
        test: Rx::lit(r"^(\s*)(test|property|setup_all|setup)\b"),
        test_name: Rx::lit(r#"^\s*(?:test|property)\s+(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)')"#),
        do_end: Rx::lit(r"\bdo\s*$"),
        head_open: Rx::lit(r"[,({\[]\s*$"),
        alias_open: Rx::lit(r"^\s*alias\s.*\{[^}]*$"),
        alias: Rx::lit(r"^\s*alias\s+([A-Z]\w*(?:\.[A-Z]\w*)*)(?:\.\{([^}]*)\})?(?:\s*,\s*as:\s*([A-Z]\w*))?"),
        import: Rx::lit(r"^\s*import\s+([A-Z]\w*(?:\.[A-Z]\w*)*|__MODULE__)"),
        use_: Rx::lit(r"^\s*use\s+([A-Z]\w*(?:\.[A-Z]\w*)*|__MODULE__)(?:\s*,\s*:(\w+))?"),
        qcall: Rx::lit(r"\b([A-Z][\w.]*|__MODULE__)\.([a-z_]\w*[?!]?)\s*\("),
        lcall: Rx::lit(r"(?<![.\w:&~])([a-z_]\w*[?!]?)\s*\("),
        pipe: Rx::lit(r"\|>\s*(?:([A-Z][\w.]*|__MODULE__)\.)?([a-z_]\w*[?!]?)"),
        capture: Rx::lit(r"&(?:([A-Z][\w.]*|__MODULE__)\.)?([a-z_]\w*[?!]?)/\d"),
        child: Rx::lit(r"\bstart_supervised!?\(\s*\{?\s*([A-Z][\w.]*|__MODULE__)"),
        ident: Rx::lit(r"[a-z_]\w*[?!]?"),
    })
}

const KEYWORDS: &[&str] = &[
    "def", "defp", "defmacro", "defmacrop", "defmodule", "defstruct", "defimpl", "defprotocol", "defguard", "if",
    "unless", "case", "cond", "fn", "for", "with", "quote", "unquote", "receive", "try", "raise", "rescue", "catch",
    "after", "else", "do", "end", "when", "and", "or", "not", "in", "import", "require", "use", "alias", "is_nil",
    "is_atom", "is_binary", "is_list", "is_map", "is_integer", "is_float", "is_number", "is_tuple", "is_function",
    "is_boolean", "is_pid", "length", "hd", "tl", "elem", "put_elem", "map_size", "tuple_size", "byte_size",
    "to_string", "inspect", "send", "self", "spawn", "throw", "exit", "super", "match?", "sigil_r", "sigil_s",
    "sigil_w", "sigil_c", "abs", "max", "min", "round", "trunc", "div", "rem", "apply", "struct", "update_in",
    "put_in", "get_in", "then", "tap", "binding", "dbg",
];

fn keyword(s: &str) -> bool {
    static K: OnceLock<HashSet<&'static str>> = OnceLock::new();
    K.get_or_init(|| KEYWORDS.iter().copied().collect()).contains(s)
}

fn indent(s: &str) -> usize {
    s.len() - s.trim_start().len()
}

/// Modules of one Elixir file (lang_elixir.parse).
pub fn parse(rel: &str, text: &str) -> Vec<Module> {
    let p = pats();
    let (code, strings) = blank(text, "elixir", false);
    let raw: Arc<Vec<String>> = Arc::new(text.split('\n').map(String::from).collect());
    let code_lines: Arc<Vec<String>> = Arc::new(code.split('\n').map(String::from).collect());
    let lines: Vec<&str> = code_lines.iter().map(|s| s.as_str()).collect();
    let n = lines.len();
    // module spans from indentation: a module ends at the `end` at its indent
    let mut spans: Vec<(String, usize, usize, isize)> = Vec::new();
    let mut stack: Vec<(usize, usize, String)> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        if let Some(m) = p.defmod.match_start(ln) {
            let ind = m.s(1).len();
            let mut name = m.s(2).to_string();
            if let Some(top) = stack.last() {
                if !name.starts_with(&format!("{}.", top.2)) {
                    name = format!("{}.{}", top.2, name);
                }
            }
            stack.push((i, ind, name));
            continue;
        }
        if ln.trim() == "end" && !stack.is_empty() && indent(ln) == stack.last().unwrap().1 {
            let (a, ai, nm) = stack.pop().unwrap();
            spans.push((nm, a, i, ai as isize));
        }
    }
    for (a, ai, nm) in stack {
        spans.push((nm, a, n - 1, ai as isize));
    }
    if spans.is_empty() {
        let base = rel.rsplit('/').next().unwrap_or(rel);
        let base = match base.rfind('.') {
            Some(k) => &base[..k],
            None => base,
        };
        spans.push((base.to_string(), 0, n - 1, -2));
    }
    let mut str_by_line: HashMap<usize, Vec<String>> = HashMap::new();
    for (ln, s) in strings {
        str_by_line.entry(ln).or_default().push(s);
    }
    let mut out = Vec::new();
    for (name, a, b, ind) in &spans {
        let (a, b, ind) = (*a, *b, *ind);
        let mut module = Module::new(name, rel, raw.clone(), code_lines.clone());
        // aliases declared inside this module
        for i in a..=b {
            let mut ln: String = lines[i].to_string();
            if p.alias_open.is_match(&ln) {
                // alias A.{\n  B,\n  C\n}
                let mut j = i;
                while j < b && !lines[j].contains('}') {
                    j += 1;
                }
                ln = lines[i..=j].iter().map(|x| x.trim()).collect::<Vec<_>>().join(" ");
            }
            let Some(m) = p.alias.match_start(&ln) else { continue };
            let base = m.s(1).to_string();
            match m.get(2).filter(|s| !s.is_empty()) {
                Some(multi) => {
                    for part in multi.split(',') {
                        let part = part.trim();
                        if !part.is_empty() {
                            let last = part.rsplit('.').next().unwrap_or(part);
                            module.aliases.insert(last.to_string(), format!("{base}.{part}"));
                        }
                    }
                }
                None => {
                    let key = match m.get(3).filter(|s| !s.is_empty()) {
                        Some(a) => a.to_string(),
                        None => base.rsplit('.').next().unwrap_or(&base).to_string(),
                    };
                    module.aliases.insert(key, base);
                }
            }
        }
        module.aliases.insert("__MODULE__".into(), name.clone());
        for i in a..=b {
            if let Some(m) = p.import.match_start(lines[i]) {
                let q = resolve_qual(m.s(1), &module);
                module.uses.push(q);
            }
            if let Some(u) = p.use_.match_start(lines[i]) {
                if indent(lines[i]) as isize == ind + 2 {
                    // `use X` / `use X, :controller`: X's __using__ may import more (the graph expands it)
                    let mut q = resolve_qual(u.s(1), &module);
                    if let Some(w) = u.get(2).filter(|s| !s.is_empty()) {
                        q = format!("{q}:{w}");
                    }
                    module.using.push(q);
                }
            }
        }
        // functions directly inside (nested module bodies are theirs)
        let nested: Vec<(usize, usize)> =
            spans.iter().filter(|(n2, x, y, _)| *x > a && *y <= b && n2 != name).map(|(_, x, y, _)| (*x, *y)).collect();
        let in_nested = |i: usize| nested.iter().any(|&(x, y)| x <= i && i <= y);
        let mut covered: HashSet<usize> = HashSet::new();
        let mut i = a + 1;
        while i < b {
            if in_nested(i) {
                i += 1;
                continue;
            }
            let m = p.def.match_start(lines[i]);
            let mut t_m = if m.is_some() { None } else { p.test.match_start(lines[i]) };
            // the head's last line: `test "x",\n  %{conn: conn} do` spans two
            let mut head = i;
            if t_m.is_some() && !p.do_end.is_match(lines[i]) {
                match head_end(&lines, i, b) {
                    Some(h) => head = h,
                    None => t_m = None, // `setup :fun` / `test "x"` without a block: not a body
                }
            }
            if m.is_none() && t_m.is_none() {
                i += 1;
                continue;
            }
            let (dind, kind, fname) = if let Some(t) = &t_m {
                let fname = if t.s(2) == "test" || t.s(2) == "property" {
                    let label = match p.test_name.match_start(raw.get(i).map(|s| s.as_str()).unwrap_or("")) {
                        // (Python: `group(1) or group(2)`; an empty name falls through to None)
                        Some(nm) => match nm.get(1).filter(|s| !s.is_empty()).or(nm.get(2)) {
                            Some(s) => s.to_string(),
                            None => "None".to_string(),
                        },
                        None => format!("line {}", i + 1),
                    };
                    format!("test {label}")
                } else {
                    t.s(2).to_string()
                };
                (t.s(1).len(), "test", fname)
            } else {
                let m = m.as_ref().unwrap();
                let d = m.s(2);
                let kind = if d.ends_with('p') && d != "defdelegate" { "private" } else { "func" };
                (m.s(1).len(), kind, m.s(3).to_string())
            };
            let mut end = head;
            for j in head + 1..b {
                let t = lines[j];
                if t.trim().is_empty() {
                    continue;
                }
                let tind = indent(t);
                let ts = t.trim();
                if tind < dind
                    || (tind == dind && !["end", "rescue", "catch", "after", "else"].iter().any(|k| ts.starts_with(k)))
                {
                    break;
                }
                end = j;
                if tind == dind && ts == "end" {
                    break;
                }
            }
            let mut f = Func::new(name, &fname, rel, i + 1, end + 1, lines[i..=end].join("\n"), kind);
            collect(&mut f, &module, &lines, i, end, &str_by_line);
            covered.extend(i..=end);
            // multiple clauses of one function: merged into the first
            if let Some(prev) = module.funcs.iter_mut().find(|g| g.name == f.name) {
                prev.end = prev.end.max(f.end);
                prev.calls.extend(f.calls);
                prev.refs.extend(f.refs);
                prev.code.push('\n');
                prev.code.push_str(&f.code);
            } else {
                module.funcs.push(f);
            }
            i = end + 1;
        }
        let mut top = Func::new(name, "<top>", rel, a + 1, b + 1, String::new(), "top");
        // code outside every function: a script's body (test_helper.exs, a
        // seeds file) or a module body's compile-time calls
        let (lo, hi) = if ind < 0 { (a as isize, b as isize) } else { (a as isize + 1, b as isize - 1) };
        let mut j = lo;
        while j <= hi {
            let ju = j as usize;
            if !covered.contains(&ju) && !lines[ju].trim().is_empty() && !in_nested(ju) {
                collect(&mut top, &module, &lines, ju, ju, &str_by_line);
            }
            j += 1;
        }
        module.funcs.push(top);
        out.push(module);
    }
    out
}

/// The line ending a test head that goes on past its first line (a trailing
/// `,` or an open `%{`): the first one ending in `do`, within a few lines and
/// before anything at the head's own indent but a closing bracket.
fn head_end(lines: &[&str], i: usize, b: usize) -> Option<usize> {
    let p = pats();
    if !p.head_open.is_match(lines[i]) {
        return None;
    }
    let ind = indent(lines[i]);
    for j in i + 1..(i + 12).min(b) {
        let t = lines[j];
        if t.trim().is_empty() {
            continue;
        }
        let ts = t.trim_start();
        if indent(t) <= ind && !(ts.starts_with(')') || ts.starts_with('}') || ts.starts_with(']')) {
            return None;
        }
        if p.do_end.is_match(t) {
            return Some(j);
        }
    }
    None
}

fn resolve_qual(q: &str, m: &Module) -> String {
    if q.is_empty() {
        return String::new();
    }
    let (head, rest) = match q.find('.') {
        Some(k) => (&q[..k], &q[k + 1..]),
        None => (q, ""),
    };
    match m.aliases.get(head) {
        Some(a) if rest.is_empty() => a.clone(),
        Some(a) => format!("{a}.{rest}"),
        None => q.to_string(),
    }
}

/// A def line without its head (`def name(args)`): the parameters aren't
/// calls, the body after `do:` is.
fn after_head(t: &str) -> &str {
    let p = pats();
    let Some(m) = p.def.match_start(t).or_else(|| p.test.match_start(t)) else { return t };
    let rest = &t[m.end()..];
    if rest.starts_with('(') {
        let mut depth: i64 = 0;
        for (k, ch) in rest.char_indices() {
            depth += (ch == '(') as i64;
            depth -= (ch == ')') as i64;
            if depth == 0 {
                return &rest[k + 1..];
            }
        }
        return ""; // parameters continue on the next line
    }
    rest
}

fn collect(f: &mut Func, m: &Module, lines: &[&str], a: usize, b: usize, strs: &HashMap<usize, Vec<String>>) {
    let p = pats();
    for i in a..=b {
        let t = if i == a { after_head(lines[i]) } else { lines[i] };
        let ln = i + 1;
        for c in p.qcall.find_iter(t) {
            f.calls.push(Call::new(c.s(2), &resolve_qual(c.s(1), m), ln));
        }
        for c in p.lcall.find_iter(t) {
            if !keyword(c.s(1)) {
                f.calls.push(Call::new(c.s(1), "", ln));
            }
        }
        for c in p.pipe.find_iter(t) {
            let q = c.get(1).unwrap_or("");
            if q.is_empty() && keyword(c.s(2)) {
                continue;
            }
            f.calls.push(Call::new(c.s(2), &resolve_qual(q, m), ln));
        }
        for c in p.capture.find_iter(t) {
            // `&evaluate/1` / `&Mod.fun/2` handed to Enum.map & co. is a call
            f.refs.insert(c.s(2).to_string());
            f.calls.push(Call::new(c.s(2), &resolve_qual(c.get(1).unwrap_or(""), m), ln));
        }
        for c in p.child.find_iter(t) {
            f.calls.push(Call::new("start_link", &resolve_qual(c.s(1), m), ln));
        }
        for s in strs.get(&ln).map(|v| v.as_slice()).unwrap_or(&[]) {
            if p.ident.fullmatch(s).is_some() {
                f.refs.insert(s.clone());
            }
        }
    }
}
