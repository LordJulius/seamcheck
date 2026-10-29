//! Slint adapter (lang_slint.py, `.slint`): one Module per file.
//!
//! Each `component` / `global` is a Func named for it (its body is the element
//! tree, so the near-duplicate check reads whole element bodies), whose calls
//! are the elements it instantiates (`Press { }`, `rail := Rail { }`,
//! `inherits Base`) and the functions it calls. A `function` inside a
//! component or global is a Func named `Owner.name`; `Shell.navigate()` from
//! another file reaches `Shell.navigate` through `import { Shell } from
//! "state.slint"`.
//!
//! What Rust and the Slint compiler see, not the file: an `export`ed window,
//! global, struct or enum and a `public function` are the generated Rust API
//! (`slint::include_modules!`), so they are roots, never dead. A plain
//! exported component is a library part: dead when nothing instantiates it.
//! Callbacks and properties are interface, not code.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use crate::lang::lambdas::matching;
use crate::lang::{Adapter, ParseCtx};
use crate::model::{Call, Func, Module};
use crate::pyre::Rx;
use crate::scan::blank;

pub struct Slint;

impl Adapter for Slint {
    fn family(&self) -> &'static str {
        "slint"
    }
    fn exts(&self) -> &'static [&'static str] {
        &[".slint"]
    }
    fn parse(&self, _ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module> {
        parse(rel, text)
    }
}

const ID: &str = r"[A-Za-z_][\w-]*";

struct Pats {
    decl: Rx,
    type_decl: Rx,
    func: Rx,
    export_list: Rx,
    element: Rx,
    id_element: Rx,
    call: Rx,
    cap: Rx,
    import: Rx,
}

fn pats() -> &'static Pats {
    static P: OnceLock<Pats> = OnceLock::new();
    P.get_or_init(|| Pats {
        decl: Rx::lit(&format!(r"(?m)^[ \t]*(export[ \t]+)?(component|global)[ \t]+({ID})(?:[ \t]+inherits[ \t]+({ID}))?[ \t]*\{{")),
        type_decl: Rx::lit(&format!(r"(?m)^[ \t]*(?:export[ \t]+)?(?:struct|enum)[ \t]+({ID})")),
        func: Rx::lit(&format!(r"(?<![\w-])((?:(?:pure|public|protected)[ \t]+)*)function[ \t]+({ID})[ \t]*\(")),
        export_list: Rx::lit(r#"(?m)^[ \t]*export[ \t]*\{([^}]*)\}[ \t]*(?:from[ \t]*"([^"]+)")?[ \t]*;?"#),
        element: Rx::lit(&format!(r"(?m)(?:^|[;{{}}:])[ \t]*(?:{ID}[ \t]*:=[ \t]*)?([A-Z][\w-]*(?:\.[A-Z][\w-]*)?)[ \t]*\{{")),
        id_element: Rx::lit(&format!(r"(?<![\w-])({ID})[ \t]*:=[ \t]*([A-Z][\w-]*)[ \t]*\{{")),
        call: Rx::lit(&format!(r"(?:(?<![\w.-])({ID})[ \t]*\.[ \t]*)?(?<![\w-])({ID})[ \t]*\(")),
        cap: Rx::lit(r"(?<![\w.-])([A-Z][\w-]*)"),
        import: Rx::lit(r#"(?m)^[ \t]*import[ \t]*\{([^}]*)\}[ \t]*from[ \t]*"([^"]+)"[ \t]*;"#),
    })
}

/// functions the language provides
const BUILTIN: &[&str] = &[
    "min", "max", "abs", "round", "floor", "ceil", "sqrt", "pow", "log", "exp", "sin", "cos", "tan", "asin", "acos", "atan",
    "atan2", "mod", "clamp", "rgb", "rgba", "hsv", "cubic-bezier", "linear-gradient", "radial-gradient", "conic-gradient",
    "darker", "brighter", "transparentize", "mix", "with-alpha", "to-float", "to-string", "parse-int", "is-float",
    "animation-tick", "debug", "init", "if", "for", "return", "image-url", "@image-url", "@tr", "@linear-gradient",
    "@radial-gradient", "@conic-gradient", "@children", "@debug", "fabs", "sign", "lerp", "length", "set-selection-offsets",
    "select-all", "clear-selection", "copy", "cut", "paste", "focus", "show", "hide", "close", "clear-focus", "has-focus",
    "ceiling", "trunc", "fract", "lines", "deg",
];
const KEYWORDS: &[&str] = &[
    "if", "for", "in", "else", "return", "animate", "states", "transitions", "when", "out", "in-out", "pure", "public",
    "protected", "callback", "property", "function", "component", "global", "struct", "enum", "import", "export", "inherits",
    "root", "self", "parent",
];

fn modname(rel: &str) -> String {
    rel.rsplit_once('.').map(|x| x.0).unwrap_or(rel).to_string()
}

fn where_(rel: &str, spec: &str) -> String {
    let d = rel.rsplit_once('/').map(|x| x.0).unwrap_or("");
    format!("sl:{d}\0{spec}")
}

fn first_upper(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_uppercase())
}

pub fn parse(rel: &str, text: &str) -> Vec<Module> {
    let p = pats();
    let (code, _) = blank(text, "slint", false);
    let raw: Vec<String> = text.split('\n').map(String::from).collect();
    let lines: Vec<String> = code.split('\n').map(String::from).collect();
    let mut md = Module::new(&modname(rel), rel, Arc::new(raw), Arc::new(lines.clone()));
    let mut starts = vec![0usize];
    for ln in &lines[..lines.len().saturating_sub(1)] {
        starts.push(starts.last().unwrap() + ln.len() + 1);
    }
    let line_of = |pos: usize| -> usize { starts.partition_point(|&x| x <= pos).max(1) };

    // `import` paths are strings, which the blanked code lost: read them from the text
    for m in p.import.find_iter(text) {
        let nl = text[..m.start()].matches('\n').count();
        if !lines.get(nl).is_some_and(|l| l.trim().starts_with("import")) {
            continue; // inside a comment
        }
        let w = where_(rel, m.s(2));
        for part in m.s(1).split(',') {
            let part = part.trim();
            let (src, local) = match part.split_once(" as ") {
                Some((a, b)) => (a, b),
                None => (part, ""),
            };
            if !src.trim().is_empty() {
                let key = if local.is_empty() { src } else { local }.trim().to_string();
                md.imported.insert(key, (w.clone(), src.trim().to_string()));
            }
        }
    }
    let local_types: BTreeSet<String> = p.type_decl.find_iter(&code).iter().map(|m| m.s(1).to_string()).collect();
    md.decls = local_types.iter().cloned().collect();
    let mut top = Func::new(&md.name, "<top>", rel, 1, lines.len(), String::new(), "top");

    let mut decls: Vec<(crate::pyre::Caps, usize)> = Vec::new();
    for m in p.decl.find_iter(&code) {
        let close = matching(&code, m.end() - 1);
        if close < 0 {
            continue;
        }
        decls.push((m, close as usize));
    }
    let declared: BTreeSet<String> = decls.iter().map(|(m, _)| m.s(3).to_string()).collect();
    let mut all: BTreeSet<String> = md.decls.iter().cloned().collect();
    all.extend(declared.iter().cloned());
    md.decls = all.into_iter().collect();
    for m in p.export_list.find_iter(&code) {
        if m.get(2).is_none() {
            // `export { A, B }` of this file's own names
            for part in m.s(1).split(',') {
                let src = part.trim().split(" as ").next().unwrap_or("").trim();
                if declared.contains(src) || local_types.contains(src) {
                    top.refs.insert(src.to_string());
                }
            }
        }
    }
    for tm in p.type_decl.find_iter(&code) {
        if crate::pyre::rx(r"[ \t]*export\b").unwrap().match_start(&code[tm.start()..tm.end()]).is_some() {
            top.refs.insert(tm.s(1).to_string());
        }
    }

    for (m, close) in &decls {
        let exported = m.get(1).is_some_and(|s| !s.is_empty());
        let (kind, name, base) = (m.s(2), m.s(3).to_string(), m.get(4));
        let body0 = m.end() - 1;
        let owner_name = name.clone();
        if (kind == "global" && exported) || (exported && matches!(base, Some("Window" | "Dialog" | "PopupWindow"))) {
            top.refs.insert(name.clone()); // the generated Rust API
        }
        // (name, start offset, end offset, name offset, public, body brace)
        let mut funcs: Vec<(String, usize, usize, usize, bool, usize)> = Vec::new();
        for fm in p.func.find_iter_range(&code, body0, *close) {
            let Some(fb) = code[fm.end()..*close].find('{').map(|x| x + fm.end()) else { continue };
            let fe = matching(&code, fb);
            if fe < 0 {
                continue;
            }
            funcs.push((fm.s(2).to_string(), fm.start(), fe as usize, fm.span(2).unwrap().0, fm.s(1).contains("public"), fb));
        }
        // the owner's own text: its function bodies are their own Funcs
        let mut own: Vec<u8> = code.as_bytes()[m.start()..=*close].to_vec();
        for (_n, fs, fe, _no, _pub, _fb) in &funcs {
            for k in (fs - m.start())..=(fe - m.start()) {
                if own[k] != b'\n' {
                    own[k] = b' ';
                }
            }
        }
        let own_code = String::from_utf8_lossy(&own).into_owned();
        let fnames: HashSet<String> = funcs.iter().map(|f| f.0.clone()).collect();
        let mut ids: HashMap<String, String> = HashMap::new();
        for i in p.id_element.find_iter_range(&code, body0, *close) {
            ids.insert(i.s(1).to_string(), i.s(2).to_string());
        }
        let mut cf = Func::new(&md.name, &name, rel, line_of(m.start()), line_of(*close), code[m.start()..=*close].to_string(), "func");
        let ctx = Ctx { line_of: &line_of, owner: &owner_name, fnames: &fnames, ids: &ids, declared: &declared, local_types: &local_types };
        collect(&mut cf, &own_code[body0 - m.start()..], body0, &ctx, &mut md, base.map(|s| s.to_string()));
        md.funcs.push(cf);
        for (n, fs, fe, _no, public, fb) in &funcs {
            let fc = code[*fs..=*fe].to_string();
            let mut f = Func::new(&md.name, &format!("{name}.{n}"), rel, line_of(*fs), line_of(*fe), fc, "func");
            collect(&mut f, &code[*fb..=*fe], *fb, &ctx, &mut md, None);
            if *public {
                top.refs.insert(f.name.clone()); // callable from Rust: `ui.invoke_name()`
            }
            md.funcs.push(f);
        }
    }
    md.funcs.push(top);
    vec![md]
}

struct Ctx<'a> {
    line_of: &'a dyn Fn(usize) -> usize,
    owner: &'a str,
    fnames: &'a HashSet<String>,
    ids: &'a HashMap<String, String>,
    declared: &'a BTreeSet<String>,
    local_types: &'a BTreeSet<String>,
}

fn collect(f: &mut Func, text: &str, base: usize, c: &Ctx, md: &mut Module, inherits: Option<String>) {
    let p = pats();
    let at = |i: usize| (c.line_of)(base + i);
    if let Some(inh) = inherits {
        f.calls.push(Call::new(&inh, "", at(0)));
    }
    for m in p.element.find_iter(text) {
        let mut el = m.s(1).to_string();
        if el.contains('.') {
            el = el.rsplit('.').next().unwrap().to_string(); // `Shell.Sub {`: a namespaced element, its last part
        }
        f.calls.push(Call::new(&el, "", at(m.span(1).unwrap().0)));
        f.refs.insert(el);
    }
    for m in p.cap.find_iter(text) {
        let nm = m.s(1);
        if c.declared.contains(nm) || c.local_types.contains(nm) || md.imported.contains_key(nm) {
            f.refs.insert(nm.to_string());
        }
    }
    for m in p.call.find_iter(text) {
        let (qual, name) = (m.s(1), m.s(2));
        if (KEYWORDS.contains(&name) && qual.is_empty()) || (qual.is_empty() && BUILTIN.contains(&name)) {
            continue;
        }
        let line = at(m.span(2).unwrap().0);
        if qual == "root" || qual == "self" || qual.is_empty() || qual == c.owner {
            if c.fnames.contains(name) {
                f.calls.push(Call::new(&format!("{}.{name}", c.owner), "", line));
            }
            continue;
        }
        if let Some(ty) = c.ids.get(qual) {
            if c.declared.contains(ty) || md.imported.contains_key(ty) {
                f.calls.push(Call::new(&format!("{ty}.{name}"), "", line));
                if let Some((w, orig)) = md.imported.get(ty).cloned() {
                    md.imported.insert(format!("{ty}.{name}"), (w, format!("{orig}.{name}")));
                }
                continue;
            }
        }
        if first_upper(qual) && (c.declared.contains(qual) || md.imported.contains_key(qual)) {
            f.calls.push(Call::new(&format!("{qual}.{name}"), "", line));
            if let Some((w, orig)) = md.imported.get(qual).cloned() {
                md.imported.insert(format!("{qual}.{name}"), (w, format!("{orig}.{name}")));
            }
            continue;
        }
    }
}
