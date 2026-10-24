//! `end`-closed blocks: classes, modules, defs, tests, do-blocks (lang_ruby._blocks).

use super::pats::{opens_after, pats};
use super::util::*;

#[derive(Clone, Debug, Default)]
pub struct Blk {
    /// "class", "module", "sclass", "def", "test", "do", "other"
    pub kind: &'static str,
    pub name: String,
    pub base: String,
    pub on: String,
    pub pos: usize,
    pub params: Option<(usize, usize)>,
    pub name_span: Option<(usize, usize)>,
    /// first and last line (0-based)
    pub a: usize,
    pub b: usize,
    pub parent: i64,
    pub i: usize,
    pub end_pos: Option<usize>,
    /// where an endless def's body expression starts (0: not endless)
    pub endless: usize,
    pub setup: bool,
}

pub const OPEN: usize = usize::MAX;

/// code[k] is "(": the index of its ")" (or len(code)).
pub fn match_paren(code: &str, k: usize) -> usize {
    let t = code.as_bytes();
    let n = t.len();
    let mut depth = 0i64;
    let mut k = k;
    while k < n {
        match t[k] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return k;
                }
            }
            _ => {}
        }
        k += 1;
    }
    n
}

pub fn stmt_prefix(code: &str, pos: usize) -> &str {
    let a = [rfind(code, "\n", pos), rfind(code, ";", pos)].into_iter().flatten().max().map(|x| x + 1).unwrap_or(0);
    rstrip(sub(code, a, pos))
}

/// An `if`/`unless`/`while`/`until` at pos opens a block (it leads a
/// statement or a value), rather than modifying the statement before it.
fn opens(code: &str, pos: usize) -> bool {
    let pre = stmt_prefix(code, pos);
    let mut it = pre.chars().rev();
    let Some(last) = it.next() else { return true };
    if "=([{,|&<>+-%~^".contains(last) {
        return true;
    }
    let second = it.next();
    if "?!:".contains(last) && second.map(|c| c == ' ' || c == '\t').unwrap_or(true) {
        return true;
    }
    // (\w+)$
    let word_start = pre.char_indices().rev().take_while(|&(_, c)| isalnum(c) || c == '_').last().map(|(i, _)| i);
    match word_start {
        Some(s) => opens_after(&pre[s..]) && !pre[..s].ends_with('.'),
        None => false,
    }
}

pub fn blocks(code: &str, starts: &[usize]) -> Vec<Blk> {
    let p = pats();
    let line_of = |k: usize| starts.partition_point(|&s| s <= k) - 1;
    let mut out: Vec<Blk> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut loop_do: i64 = -1;
    let mut skip_to = 0usize;
    let n = code.len();
    for m in p.tok.find_iter(code) {
        let pos = m.start();
        if pos < skip_to {
            continue;
        }
        let kw = m.s(1);
        let line = line_of(pos);
        let mut b: Blk;
        if kw == "end" {
            if let Some(top) = stack.pop() {
                out[top].b = line;
                out[top].end_pos = Some(pos);
            }
            continue;
        }
        match kw {
            "def" => {
                // unreadable: still a block `end` closes; Python raises when neither pattern reads it
                let dm = p
                    .def_
                    .at(code, pos)
                    .or_else(|| p.def_any.at(code, pos))
                    .unwrap_or_else(|| panic!("'NoneType' object has no attribute 'end'"));
                let mut k = dm.end();
                while k < n && matches!(byte_at(code, k), b' ' | b'\t') {
                    k += 1;
                }
                let params_at = k;
                if byte_at(code, k) == b'(' && k < n {
                    k = match_paren(code, k) + 1;
                }
                skip_to = k;
                let mut eq = k;
                while eq < n && matches!(byte_at(code, eq), b' ' | b'\t') {
                    eq += 1;
                }
                let endless = starts_at(code, eq, "=")
                    && !(starts_at(code, eq, "==") || starts_at(code, eq, "=~") || starts_at(code, eq, "=>"));
                let (s2, e2) = dm.span(2).unwrap_or((0, 0));
                b = Blk {
                    kind: "def",
                    name: dm.s(2).to_string(),
                    on: dm.s(1).to_string(),
                    pos,
                    params: Some((params_at, k)),
                    name_span: Some((s2, e2)),
                    ..Default::default()
                };
                if endless {
                    b.a = line;
                    b.b = line;
                    b.endless = eq + 1;
                    b.end_pos = Some(find(code, "\n", k).unwrap_or(n));
                    b.parent = stack.last().map(|&i| i as i64).unwrap_or(-1);
                    b.i = out.len();
                    out.push(b);
                    continue;
                }
            }
            "class" => {
                let sm = p.sclass.at(code, pos);
                let cm = if sm.is_some() { None } else { p.class.at(code, pos) };
                b = if sm.is_some() {
                    Blk { kind: "sclass", ..Default::default() }
                } else if let Some(cm) = cm {
                    Blk { kind: "class", name: cm.s(1).to_string(), base: cm.s(2).to_string(), ..Default::default() }
                } else {
                    Blk { kind: "other", ..Default::default() }
                };
            }
            "module" => {
                b = match p.module.at(code, pos) {
                    Some(mm) => Blk { kind: "module", name: mm.s(1).to_string(), ..Default::default() },
                    None => Blk { kind: "other", ..Default::default() },
                };
            }
            "if" | "unless" | "while" | "until" => {
                if !opens(code, pos) {
                    continue;
                }
                if kw == "while" || kw == "until" {
                    loop_do = line as i64;
                }
                b = Blk { kind: "other", ..Default::default() };
            }
            "for" => {
                loop_do = line as i64;
                b = Blk { kind: "other", ..Default::default() };
            }
            "do" => {
                if loop_do == line as i64 {
                    loop_do = -1;
                    continue;
                }
                b = do_block(code, pos);
            }
            _ => b = Blk { kind: "other", ..Default::default() },
        }
        b.a = line;
        b.pos = pos;
        b.parent = stack.last().map(|&i| i as i64).unwrap_or(-1);
        b.i = out.len();
        b.b = OPEN;
        stack.push(b.i);
        out.push(b);
    }
    let last = line_of(n);
    for b in out.iter_mut() {
        if b.b == OPEN {
            b.b = last;
            b.end_pos = Some(n);
        }
    }
    out
}

/// `Generator = Class.new(Plugin)` (no block): a class all the same, as a
/// one-line block.
pub fn blockless_classes(blocks: &mut Vec<Blk>, lines: &[String]) {
    let p = pats();
    for (i, ln) in lines.iter().enumerate() {
        let Some(m) = p.class_new.start(ln) else { continue };
        let tail = from(ln, m.end());
        if tail.contains('{') || has_do(tail) {
            continue;
        }
        let mut parent = -1i64;
        let mut best: Option<(usize, std::cmp::Reverse<usize>)> = None;
        for b in blocks.iter() {
            if b.a <= i && i <= b.b && b.end_pos.is_some() {
                let key = (b.a, std::cmp::Reverse(b.b));
                if best.map(|k| key > k).unwrap_or(true) {
                    best = Some(key);
                    parent = b.i as i64;
                }
            }
        }
        let base = match m.get(3) {
            Some(x) if !x.is_empty() => x.to_string(),
            _ => {
                if m.s(2) == "Class" {
                    String::new()
                } else {
                    m.s(2).to_string()
                }
            }
        };
        let i_ = blocks.len();
        blocks.push(Blk {
            kind: "class",
            name: m.s(1).to_string(),
            base,
            a: i,
            b: i,
            pos: 0,
            parent,
            i: i_,
            end_pos: None,
            ..Default::default()
        });
    }
}

/// `\bdo\b` somewhere in the text.
fn has_do(s: &str) -> bool {
    use std::sync::OnceLock;
    static R: OnceLock<crate::pyre::Rx> = OnceLock::new();
    R.get_or_init(|| crate::pyre::Rx::lit(r"\bdo\b")).is_match(s)
}

fn do_block(code: &str, pos: usize) -> Blk {
    let p = pats();
    let mut head = stmt_prefix(code, pos).to_string();
    let mut a: isize = rfind(code, "\n", pos).map(|x| x as isize).unwrap_or(-1);
    for _ in 0..3 {
        // `should "a, " \` / `"b" do`: a head over several lines
        let prev: isize = if a > 0 { rfind(code, "\n", a as usize).map(|x| x as isize).unwrap_or(-1) } else { -1 };
        if a <= 0 {
            break;
        }
        let seg = rstrip(sub(code, (prev + 1) as usize, a as usize));
        if !(seg.ends_with('\\') || seg.ends_with(',')) {
            break;
        }
        head = format!("{} {}", seg.trim_end_matches('\\'), lstrip(&head));
        a = prev;
    }
    if p.test_head.start(&head).is_some() {
        return Blk { kind: "test", ..Default::default() };
    }
    if let Some(dm) = p.define_method.search(&head) {
        return Blk { kind: "def", name: dm.s(1).to_string(), ..Default::default() };
    }
    if let Some(cm) = p.class_new.start(&head) {
        let base = match cm.get(3) {
            Some(x) if !x.is_empty() => x.to_string(),
            _ => {
                if cm.s(2) == "Class" {
                    String::new()
                } else {
                    cm.s(2).to_string()
                }
            }
        };
        return Blk { kind: "class", name: cm.s(1).to_string(), base, ..Default::default() };
    }
    if let Some(am) = p.anon_class.search(&head) {
        let line = code[..clamp(code, pos)].matches('\n').count() + 1;
        return Blk {
            kind: "class",
            name: format!("{}.new@{}", am.s(1), line),
            base: am.s(2).to_string(),
            ..Default::default()
        };
    }
    let setup = {
        use std::sync::OnceLock;
        static R: OnceLock<crate::pyre::Rx> = OnceLock::new();
        R.get_or_init(|| crate::pyre::Rx::lit(r"\s*(?:setup|before)\b")).match_start(&head).is_some()
    };
    Blk { kind: "do", setup, ..Default::default() }
}
