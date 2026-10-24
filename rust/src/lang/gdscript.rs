//! GDScript adapter (lang_gdscript.py, Godot 4): one Module per .gd file.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use crate::lang::chains::{expression, receiver, type_name, Opts};
use crate::lang::python::tab_width;
use crate::lang::{Adapter, ParseCtx};
use crate::model::{Call, Func, Module};
use crate::pyre::{floor_char, Rx};
use crate::scan::blank;

pub struct GdScript;

impl Adapter for GdScript {
    fn family(&self) -> &'static str {
        "gdscript"
    }
    fn exts(&self) -> &'static [&'static str] {
        &[".gd"]
    }
    fn parse(&self, ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module> {
        parse(ctx, rel, text)
    }
}

struct Pats {
    func: Rx,
    call: Rx,
    class_name: Rx,
    extends: Rx,
    preload: Rx,
    group_var: Rx,
    connect: Rx,
    value: Rx,
    typed_var: Rx,
    infer_var: Rx,
    param: Rx,
    cast: Rx,
    returns: Rx,
    array_of: Rx,
    dict_of: Rx,
    default_param: Rx,
    ret_array: Rx,
    for_in: Rx,
    ident: Rx,
}

fn pats() -> &'static Pats {
    static P: OnceLock<Pats> = OnceLock::new();
    P.get_or_init(|| Pats {
        func: Rx::lit(r"^(\s*)(?:static\s+)?func\s+([A-Za-z_]\w*)"),
        call: Rx::lit(r"(?:\b([A-Za-z_]\w*)\s*\.\s*)?\b([A-Za-z_]\w*)\s*\("),
        class_name: Rx::lit(r"(?m)^class_name\s+(\w+)"),
        extends: Rx::lit(r#"(?m)^extends\s+(?:"([^"]+)"|(\w+))"#),
        preload: Rx::lit(
            r#"(?m)^(?:static\s+)?(?:const|var)\s+(\w+)\s*(?::\s*\w+\s*)?:?=\s*(?:preload|load)\(\s*"([^"]+\.gd)"\s*\)"#,
        ),
        group_var: Rx::lit(
            r#"\bvar\s+(\w+)\s*(?::\s*\w+\s*)?:?=\s*(?:[\w.]+\.)?get_tree\(\)\.get_first_node_in_group\(\s*""#,
        ),
        connect: Rx::lit(r"\.connect\(\s*([A-Za-z_]\w*)\s*[,)]"),
        value: Rx::lit(r"(?:[=(,\[:]|\breturn)\s*([A-Za-z_]\w*)\s*(?=[,)\]}]|$)"),
        typed_var: Rx::lit(r"\bvar\s+([A-Za-z_]\w*)\s*:\s*([A-Z][\w.]*(?:\[[^\]]*\])?)"),
        infer_var: Rx::lit(r"\bvar\s+([A-Za-z_]\w*)\s*:?=(?!=)"),
        param: Rx::lit(r"[(,]\s*([A-Za-z_]\w*)\s*:\s*([A-Z][\w.]*)"),
        // anchored where Python's `.match(text, pos)` is: a miss ends at once
        cast: Rx::lit(r"(?m)\A[^\n#]*?\sas\s+([A-Z]\w*)[ \t]*(?:#[^\n]*)?$"),
        returns: Rx::lit(r"\)\s*->\s*([A-Z][\w.]*)\s*:"),
        array_of: Rx::lit(r"(?:Array|PackedArray)\[\s*([A-Z][\w.]*)\s*\]$"),
        dict_of: Rx::lit(r"Dictionary\[\s*[\w.]+\s*,\s*([A-Z][\w.]*)\s*\]$"),
        default_param: Rx::lit(r"[(,]\s*([A-Za-z_]\w*)\s*:=\s*"),
        ret_array: Rx::lit(r"\)\s*->\s*Array\[\s*([A-Z][\w.]*)\s*\]\s*:"),
        for_in: Rx::lit(r"(?m)^[ \t]*for[ \t]+([A-Za-z_]\w*)(?:[ \t]*:[ \t]*([A-Z][\w.]*))?[ \t]+in[ \t]+"),
        ident: Rx::lit(r"[A-Za-z_]\w*"),
    })
}

const KEYWORDS: &[&str] = &[
    "if", "elif", "else", "for", "while", "match", "return", "func", "and", "or", "not", "in", "is", "as", "await",
    "assert", "preload", "load", "print", "printerr", "push_error", "push_warning", "len", "range", "str", "int",
    "float", "bool", "Vector2", "Vector3", "Vector2i", "Vector3i", "Color", "Rect2", "Rect2i", "Array", "Dictionary",
    "PackedByteArray", "PackedStringArray", "PackedVector3Array", "PackedInt32Array", "PackedFloat32Array",
    "StringName", "NodePath", "Callable", "Transform3D", "Basis", "Quaternion", "AABB", "min", "max", "abs", "clamp",
    "clampf", "clampi", "lerp", "lerpf", "minf", "maxf", "mini", "maxi", "absf", "absi", "sign", "signf", "signi",
    "floor", "ceil", "round", "floorf", "ceilf", "roundf", "floori", "ceili", "roundi", "sqrt", "pow", "sin", "cos",
    "tan", "atan2", "deg_to_rad", "rad_to_deg", "randf", "randi", "randf_range", "randi_range", "wrapi", "wrapf",
    "snappedf", "is_instance_valid", "typeof", "move_toward", "posmod", "fposmod", "exp", "log", "var_to_str",
    "str_to_var", "hash", "super", "Time", "OS", "Engine", "self", "lerp_angle", "smoothstep", "is_equal_approx",
    "is_zero_approx",
];
const INDIRECT: &[&str] = &["rpc", "rpc_id", "call", "call_deferred", "callv", "bind"];

fn keyword(s: &str) -> bool {
    static K: OnceLock<HashSet<&'static str>> = OnceLock::new();
    K.get_or_init(|| KEYWORDS.iter().copied().collect()).contains(s)
}

fn is_ws(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}
fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_ws)
}
fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_ws)
}

fn types(held: &mut indexmap::IndexMap<String, String>, text: &str) {
    let p = pats();
    let o = Opts::default();
    for m in p.typed_var.find_iter(text) {
        let t = type_name(m.s(2));
        if !t.is_empty() && !held.get(m.s(1)).map(|s| s.starts_with("group:")).unwrap_or(false) {
            held.insert(m.s(1).to_string(), format!("type:{t}"));
        }
        if let Some(em) = p.array_of.match_start(m.s(2)) {
            held.insert(format!("{}[]", m.s(1)), format!("type:{}", em.s(1)));
        }
        if let Some(dm) = p.dict_of.match_start(m.s(2)) {
            held.insert(format!("{}[]", m.s(1)), format!("type:{}", dm.s(1)));
        }
    }
    for m in p.infer_var.find_iter(text) {
        let name = m.s(1);
        if let Some(cast) = p.cast.search(&text[m.end()..]) {
            if !held.contains_key(name) {
                held.insert(name.to_string(), format!("type:{}", cast.s(1)));
                continue;
            }
        }
        let ch = expression(text, m.end(), 40, o);
        if !ch.is_empty() && !held.contains_key(name) {
            held.insert(name.to_string(), format!("expr:{}", ch.join("\x1f")));
        }
    }
}

fn module_name(rel: &str, text: &str) -> String {
    if let Some(m) = pats().class_name.search(text) {
        return m.s(1).to_string();
    }
    let base = rel.rsplit('/').next().unwrap_or(rel);
    match base.rfind('.') {
        Some(k) if k > 0 => base[..k].to_string(),
        _ => base.to_string(),
    }
}

/// os.path.normpath on a relative or absolute posix path.
fn normpath(p: &str) -> String {
    let abs = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().map(|l| *l != "..").unwrap_or(false) {
                    out.pop();
                } else if !abs {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let j = out.join("/");
    match (abs, j.is_empty()) {
        (true, _) => format!("/{j}"),
        (false, true) => ".".into(),
        _ => j,
    }
}

fn dirname(p: &str) -> &str {
    p.rsplit_once('/').map(|(a, _)| a).unwrap_or("")
}

/// The module a script path names (gdscript._res).
fn res(rel: &str, path: &str, res_of: &HashMap<String, String>) -> String {
    let get = |k: String| res_of.get(&k).cloned().unwrap_or_default();
    let mut here = dirname(rel).to_string();
    if !path.starts_with("res://") {
        let joined = if path.starts_with('/') || here.is_empty() { path.to_string() } else { format!("{here}/{path}") };
        return get(format!("res://{}", normpath(&joined)));
    }
    let tail = &path[6..];
    loop {
        let got = get(format!("res://{}{}", if here.is_empty() { String::new() } else { format!("{here}/") }, tail));
        if !got.is_empty() || here.is_empty() {
            return got;
        }
        here = dirname(&here).to_string();
    }
}

fn col(raw: &[String], lines: &[&str], i: usize, byte: usize) -> i64 {
    let l = lines[i];
    match raw.get(i) {
        Some(r) if r.len() == l.len() && r.is_char_boundary(byte) => r[..byte].chars().count() as i64,
        _ => l[..floor_char(l, byte)].chars().count() as i64,
    }
}

pub fn parse(ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module> {
    let p = pats();
    let o = Opts::for_file(rel);
    let (code, strings) = blank(text, "gdscript", false);
    let raw: Arc<Vec<String>> = Arc::new(text.split('\n').map(String::from).collect());
    let code_lines: Arc<Vec<String>> = Arc::new(code.split('\n').map(String::from).collect());
    let lines: Vec<&str> = code_lines.iter().map(|s| s.as_str()).collect();
    let n = lines.len();
    let mut module = Module::new(&module_name(rel, text), rel, raw.clone(), code_lines.clone());
    module.kind = "class".into();
    if p.class_name.search(text).is_some() {
        module.decls = vec![module.name.clone()];
    }
    if let Some(ext) = p.extends.search(text) {
        module.parent = match ext.get(1) {
            Some(g) => res(rel, g, ctx.res),
            None => ext.s(2).to_string(),
        };
    }
    for m in p.preload.find_iter(text) {
        let target = res(rel, m.s(2), ctx.res);
        if !target.is_empty() {
            module.aliases.insert(m.s(1).to_string(), target);
        }
    }
    let mut starts: Vec<(usize, usize, String)> = Vec::new();
    for (idx, ln) in lines.iter().enumerate() {
        if let Some(m) = p.func.match_start(ln) {
            starts.push((idx, tab_width(m.s(1)), m.s(2).to_string()));
        }
    }
    let mut str_by_line: HashMap<usize, Vec<String>> = HashMap::new();
    for (ln, s) in strings {
        str_by_line.entry(ln).or_default().push(s);
    }
    let mut covered: HashSet<usize> = HashSet::new();
    for (idx, indent, name) in &starts {
        let (idx, indent) = (*idx, *indent);
        let mut end = n - 1;
        let (mut hdr_end, mut depth) = (idx, 0i64);
        for j in idx..n.min(idx + 40) {
            depth += lines[j].matches('(').count() as i64 - lines[j].matches(')').count() as i64;
            hdr_end = j;
            if depth <= 0 {
                break;
            }
        }
        let header = lines[idx..=hdr_end].join(" ");
        for j in hdr_end + 1..n {
            let t = lines[j];
            if lstrip(t).is_empty() {
                continue;
            }
            let lead = &t[..t.len() - lstrip(t).len()];
            if tab_width(lead) <= indent {
                end = j - 1;
                break;
            }
        }
        while end > idx && lstrip(lines[end]).is_empty() {
            end -= 1;
        }
        let mut f = Func::new(&module.name, name, rel, idx + 1, end + 1, lines[idx..=end].join("\n"), "func");
        collect(&mut f, &lines, &raw, idx, end, &str_by_line, None, o);
        for m in p.param.find_iter(&header) {
            f.bound.insert(m.s(1).to_string(), format!("type:{}", m.s(2)));
        }
        for m in p.default_param.find_iter(&header) {
            let ch = expression(&header, m.end(), 40, o);
            if !ch.is_empty() {
                f.bound.entry(m.s(1).to_string()).or_insert(format!("expr:{}", ch.join("\x1f")));
            }
        }
        types(&mut f.bound, &f.code.clone());
        if let Some(rm) = p.returns.search(&header) {
            f.returns = type_name(rm.s(1));
        }
        if let Some(em) = p.ret_array.search(&header) {
            f.relem = em.s(1).to_string();
        }
        let fcode = f.code.clone();
        for lm in p.for_in.find_iter(&fcode) {
            if let Some(ty) = lm.get(2) {
                f.bound.entry(lm.s(1).to_string()).or_insert(format!("type:{ty}"));
                continue;
            }
            let eol = fcode[lm.end()..].find('\n').map(|k| k + lm.end());
            let src = rstrip(&fcode[lm.end()..eol.unwrap_or(fcode.len())]);
            let src = src.strip_suffix(':').unwrap_or(src);
            let ch = expression(src, 0, 40, o);
            if !ch.is_empty() && !(ch.len() == 1 && ch[0].ends_with("()") && &ch[0][..ch[0].len() - 2] == "range") {
                f.bound.entry(lm.s(1).to_string()).or_insert(format!("elem:{}", ch.join("\x1f")));
            }
        }
        module.funcs.push(f);
        covered.extend(idx..=end);
    }
    let top_lines: Vec<&str> = (0..n).map(|i| if covered.contains(&i) { "" } else { lines[i] }).collect();
    types(&mut module.bound, &top_lines.join("\n"));
    let mut top = Func::new(&module.name, "<top>", rel, 1, n, top_lines.join("\n"), "top");
    collect(&mut top, &top_lines, &raw, 0, n - 1, &str_by_line, Some(&covered), o);
    module.funcs.push(top);
    vec![module]
}

#[allow(clippy::too_many_arguments)]
fn collect(
    f: &mut Func,
    lines: &[&str],
    raw: &[String],
    a: usize,
    b: usize,
    strs: &HashMap<usize, Vec<String>>,
    skip: Option<&HashSet<usize>>,
    o: Opts,
) {
    let p = pats();
    for i in a..=b {
        if skip.map(|s| s.contains(&i)).unwrap_or(false) {
            continue;
        }
        let line = lines[i];
        let mut t = line;
        if i == a && f.kind != "top" {
            t = line.split_once('(').map(|x| x.1).unwrap_or("");
        }
        let cut = line.len() - t.len();
        for m in p.call.find_iter(t) {
            let name = m.s(2);
            let mut qual = m.get(1).unwrap_or("").to_string();
            if INDIRECT.contains(&name) && !qual.is_empty() {
                f.calls.push(Call::new(&qual, "", i + 1));
                continue;
            }
            if qual.is_empty() && keyword(name) {
                continue;
            }
            let s2 = m.span(2).unwrap().0;
            let chain = if !qual.is_empty() || rstrip(&t[..s2]).ends_with('.') {
                receiver(line, cut + s2, 40, o)
            } else {
                Vec::new()
            };
            if !chain.is_empty() && qual.is_empty() {
                qual = ".".into();
            }
            f.calls.push(Call { name: name.into(), qual, line: i + 1, chain, col: col(raw, lines, i, cut + s2), targs: -1 });
        }
        if f.kind != "top" {
            if let Some(g) = p.group_var.search(t) {
                if let Some(first) = strs.get(&(i + 1)).and_then(|v| v.first()) {
                    f.bound.insert(g.s(1).to_string(), format!("group:{first}"));
                }
            }
        }
        for m in p.connect.find_iter(t) {
            f.refs.insert(m.s(1).to_string());
        }
        for m in p.value.find_iter(t) {
            f.refs.insert(m.s(1).to_string());
        }
        for s in strs.get(&(i + 1)).map(|v| v.as_slice()).unwrap_or(&[]) {
            if p.ident.fullmatch(s).is_some() {
                f.refs.insert(s.clone());
            }
        }
    }
}
