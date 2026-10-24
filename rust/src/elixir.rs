//! Elixir adapter, the part phase 1 needs (lang_elixir.py): one module per
//! `defmodule` (nested ones included) and each function's span and kind.
//! Calls and references (the call graph) come with phase 2.

use crate::pyre::Rx;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Func {
    pub module: String,
    pub name: String,
    pub file: String,
    pub start: usize,
    pub end: usize,
    /// "func", "private", "test", "top"
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub file: String,
    pub funcs: Vec<Func>,
}

impl Module {
    /// The innermost function holding `line` (Module.func_at).
    pub fn func_at(&self, line: usize) -> Option<&Func> {
        let mut best: Option<&Func> = None;
        for f in &self.funcs {
            if f.start <= line && line <= f.end && best.map(|b| f.start >= b.start).unwrap_or(true) {
                best = Some(f);
            }
        }
        best
    }
}

struct Pats {
    defmod: Rx,
    def: Rx,
    test: Rx,
    test_name: Rx,
    do_end: Rx,
    head_open: Rx,
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
    })
}

fn indent(s: &str) -> usize {
    s.len() - s.trim_start().len()
}

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

/// Modules and function spans of one file. `code`: the blanked text; `raw`: the original.
pub fn parse(rel: &str, raw: &str, code: &str) -> Vec<Module> {
    let p = pats();
    let lines: Vec<&str> = code.split('\n').collect();
    let raws: Vec<&str> = raw.split('\n').collect();
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
        spans.push((nm, a, lines.len() - 1, ai as isize));
    }
    if spans.is_empty() {
        let base = rel.rsplit('/').next().unwrap_or(rel);
        let base = match base.rfind('.') {
            Some(k) => &base[..k],
            None => base,
        };
        spans.push((base.to_string(), 0, lines.len() - 1, -2));
    }
    let mut out = Vec::new();
    for (name, a, b, ind) in &spans {
        let (a, b, ind) = (*a, *b, *ind);
        let mut module = Module { name: name.clone(), file: rel.to_string(), funcs: Vec::new() };
        let nested: Vec<(usize, usize)> =
            spans.iter().filter(|(n2, x, y, _)| *x > a && *y <= b && n2 != name).map(|(_, x, y, _)| (*x, *y)).collect();
        let mut i = a + 1;
        while i < b {
            if nested.iter().any(|&(x, y)| x <= i && i <= y) {
                i += 1;
                continue;
            }
            let m = p.def.match_start(lines[i]);
            let mut t_m = if m.is_some() { None } else { p.test.match_start(lines[i]) };
            let mut head = i;
            if t_m.is_some() && !p.do_end.is_match(lines[i]) {
                match head_end(&lines, i, b) {
                    Some(h) => head = h,
                    None => t_m = None,
                }
            }
            if m.is_none() && t_m.is_none() {
                i += 1;
                continue;
            }
            let (dind, kind, fname) = if let Some(t) = &t_m {
                let fname = if t.s(2) == "test" || t.s(2) == "property" {
                    let label = match p.test_name.match_start(raws.get(i).copied().unwrap_or("")) {
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
                (t.s(1).len(), "test".to_string(), fname)
            } else {
                let m = m.as_ref().unwrap();
                let d = m.s(2);
                let kind = if d.ends_with('p') && d != "defdelegate" { "private" } else { "func" };
                (m.s(1).len(), kind.to_string(), m.s(3).to_string())
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
                    || (tind == dind
                        && !["end", "rescue", "catch", "after", "else"].iter().any(|k| ts.starts_with(k)))
                {
                    break;
                }
                end = j;
                if tind == dind && ts == "end" {
                    break;
                }
            }
            let f = Func {
                module: name.clone(),
                name: fname,
                file: rel.to_string(),
                start: i + 1,
                end: end + 1,
                kind,
            };
            if let Some(prev) = module.funcs.iter_mut().find(|g| g.name == f.name) {
                prev.end = prev.end.max(f.end);
            } else {
                module.funcs.push(f);
            }
            i = end + 1;
        }
        module.funcs.push(Func {
            module: name.clone(),
            name: "<top>".into(),
            file: rel.to_string(),
            start: a + 1,
            end: b + 1,
            kind: "top".into(),
        });
        let _ = ind;
        out.push(module);
    }
    out
}
