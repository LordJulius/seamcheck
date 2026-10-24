//! What a file imports, per language (lang_other._js_imports, _cjs,
//! _js_reexports, _rs_imports, _kt_imports, _cs_imports, _swift_imports,
//! _php_imports, _go_imports). JS / TS specifiers are kept as
//! "js:<file dir>\0<spec>", Rust paths as "rs:<file>\0<a::b>", Go's as
//! "go:<path>" for core to resolve once every file is parsed.

use super::util::{basename, dir_of, re, strip};
use super::Idx;
use crate::lang::lambdas::matching;
use crate::model::Module;

/// The match at `pos` is code, not inside a comment (blanked there): `_real`.
fn real(lines: &[String], idx: &Idx, pos: usize) -> bool {
    let ln = idx.of(pos);
    ln < lines.len() && strip(&lines[ln]).starts_with("import")
}

/// text[pos:] starts `word` as code (the blanked line keeps it): `_code_at`.
fn code_at(lines: &[String], idx: &Idx, pos: usize, word: &str) -> bool {
    let ln = idx.of(pos);
    let col = pos - idx.starts[ln];
    ln < lines.len() && lines[ln].as_bytes().get(col..col + word.len()) == Some(word.as_bytes())
}

/// Python `s.partition(sep)`: (head, tail) with tail "" when absent.
fn partition<'a>(s: &'a str, sep: &str) -> (&'a str, &'a str) {
    match s.find(sep) {
        Some(k) => (&s[..k], &s[k + sep.len()..]),
        None => (s, ""),
    }
}

fn or<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() {
        b
    } else {
        a
    }
}

/// CommonJS (`_cjs`): `const x = require("./x")`, `const { a, b: c } = require(...)`,
/// a require inside an expression, and what the file exports.
pub fn cjs(m_: &mut Module, rel: &str, text: &str, lines: &[String], idx: &Idx) {
    let d = dir_of(rel);
    let base = basename(rel);
    let mut seen = std::collections::HashSet::new();
    let req = re!(
        r#"(?:\b(?:const|let|var)\s+|,\s*)([A-Za-z_$][\w$]*|\{[^}]*\})\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)\s*(\.\s*([A-Za-z_$][\w$]*))?"#
    );
    for m in req.find_iter(text) {
        let rpos = m.start() + text[m.start()..].find("require").unwrap_or(0);
        if !code_at(lines, idx, rpos, "require") {
            continue; // (a doc comment's `var express = require('express')`)
        }
        seen.insert(rpos);
        let wh = format!("js:{d}\0{}", m.s(2));
        let local = m.s(1);
        if let Some(inner) = local.strip_prefix('{') {
            let inner = &inner[..inner.len().saturating_sub(1)];
            for part in inner.split(',') {
                let (src, alias) = partition(strip(part), ":");
                if !strip(src).is_empty() {
                    m_.imported.entry(strip(or(alias, src)).to_string()).or_insert((wh.clone(), strip(src).to_string()));
                }
            }
        } else if !m.s(4).is_empty() {
            m_.imported.entry(local.to_string()).or_insert((wh.clone(), m.s(4).to_string()));
        } else {
            m_.aliases.entry(local.to_string()).or_insert(wh.clone());
            m_.imported.entry(local.to_string()).or_insert((wh, "default".into()));
        }
    }
    for m in re!(r#"\brequire\(\s*['"](\.[^'"]*)['"]\s*\)"#).find_iter(text) {
        if !seen.contains(&m.start()) && code_at(lines, idx, m.start(), "require") {
            m_.imported
                .entry(format!("\0require:{}", m.s(1)))
                .or_insert((format!("js:{d}\0{}", m.s(1)), "default".into()));
        }
    }
    let exp = re!(
        r#"(?m)^[ \t]*(?:exports[ \t]*=[ \t]*)?module\.exports[ \t]*=[ \t]*(?:exports[ \t]*=[ \t]*)?(?:require\(\s*['"]([^'"]+)['"]\s*\)|([A-Za-z_$][\w$]*)[ \t]*;?[ \t]*$|class[ \t]+([A-Za-z_$][\w$]*))"#
    );
    for m in exp.find_iter(text) {
        if !m.s(1).is_empty() {
            m_.imported.insert("default".into(), (format!("js:{d}\0{}", m.s(1)), "default".into()));
        } else if !m.s(2).is_empty() || !m.s(3).is_empty() {
            m_.imported.insert("default".into(), (format!("js:{d}\0./{base}"), or(m.s(2), m.s(3)).to_string()));
        }
    }
}

fn reexport_skip(s: &str) -> String {
    re!(r"^\s*type\s+").sub("", s)
}

/// ES imports (`_js_imports`); the names of `import type` / `{ type X }`
/// imports go to `type_imports`.
pub fn js_imports(m_: &mut Module, rel: &str, text: &str, lines: &[String], idx: &Idx, type_imports: &mut Vec<String>) {
    let d = dir_of(rel);
    for m in re!(r#"(?m)^[ \t]*import[ \t]+type[ \t]*\{([^}]*)\}\s*from\s*["']([^"']+)["']"#).find_iter(text) {
        if real(lines, idx, m.start()) {
            // `import type {KyInstance} from './types/ky.js'`: types only (no dependency)
            for part in m.s(1).split(',') {
                let (src, local) = partition(strip(part), " as ");
                if !strip(src).is_empty() {
                    let key = strip(or(local, src)).to_string();
                    m_.imported.entry(key.clone()).or_insert((format!("js:{d}\0{}", m.s(2)), strip(src).to_string()));
                    super::util::add(type_imports, key);
                }
            }
        }
    }
    for m in re!(r#"(?m)^[ \t]*import[ \t]+(?!type\b)([^;'"]*?)\s+from\s+["']([^"']+)["']"#).find_iter(text) {
        if !real(lines, idx, m.start()) {
            continue;
        }
        let mut clause = strip(m.s(1)).to_string();
        let wh = format!("js:{d}\0{}", m.s(2));
        if let Some(named) = re!(r"\{([^}]*)\}").search(&clause) {
            for part in named.s(1).split(',') {
                let typed = re!(r"^\s*type\s+").match_start(strip(part)).is_some();
                let part = reexport_skip(strip(part));
                if part.is_empty() {
                    continue;
                }
                let (src, local) = partition(&part, " as ");
                let key = strip(or(local, src)).to_string();
                m_.imported.insert(key.clone(), (wh.clone(), strip(src).to_string()));
                if typed {
                    // `import { type Foo }`: for reading types, erased at runtime (no dependency)
                    super::util::add(type_imports, key);
                }
            }
            clause = format!("{}{}", &clause[..named.start()], &clause[named.end()..]);
        }
        if let Some(ns) = re!(r"\*\s+as\s+([A-Za-z_$][\w$]*)").search(&clause) {
            m_.aliases.insert(ns.s(1).to_string(), wh.clone());
            clause = format!("{}{}", &clause[..ns.start()], &clause[ns.end()..]);
        }
        if let Some(def) = re!(r"\s*([A-Za-z_$][\w$]*)").match_start(&clause) {
            // a default export: a function (called bare) or an object (X.method)
            m_.imported.insert(def.s(1).to_string(), (wh.clone(), def.s(1).to_string()));
            m_.aliases.insert(def.s(1).to_string(), wh.clone());
        }
    }
}

/// A barrel's `export * from` / `export { a as b } from`, a file's own
/// `export { X as Y }`, `export * as ns from`, `export default { ...x }`
/// (`_js_reexports`).
pub fn js_reexports(m_: &mut Module, rel: &str, text: &str, lines: &[String], idx: &Idx) {
    let d = dir_of(rel);
    let own = format!("js:{d}\0./{}", basename(rel));
    for m in re!(r"(?m)^[ \t]*export[ \t]*\{([^}]*)\}[ \t]*;?[ \t]*$").find_iter(text) {
        let ln = idx.of(m.start());
        if ln < lines.len() && strip(&lines[ln]).starts_with("export") {
            for part in m.s(1).split(',') {
                let p = reexport_skip(strip(part));
                let (src, local) = partition(&p, " as ");
                let (src, local) = (strip(src), strip(local));
                if local == "default" && !src.is_empty() {
                    // `export { axios as default }`: what a default import gets
                    m_.imported.entry("default".into()).or_insert((own.clone(), src.to_string()));
                }
                if !local.is_empty() && !src.is_empty() && local != "default" {
                    m_.imported.entry(local.to_string()).or_insert((own.clone(), src.to_string()));
                    m_.bound.insert(format!("\0export:{local}"), format!("export:{src}"));
                }
            }
        }
    }
    for m in re!(r#"(?m)^[ \t]*export[ \t]+\*[ \t]+as[ \t]+([A-Za-z_$][\w$]*)[ \t]+from[ \t]*["']([^"']+)["']"#)
        .find_iter(text)
    {
        if strip(&lines[idx.of(m.start())]).starts_with("export") {
            m_.aliases.entry(m.s(1).to_string()).or_insert(format!("js:{d}\0{}", m.s(2)));
        }
    }
    for m in re!(r#"(?m)^[ \t]*export[ \t]+(?!type\b)(\*|\{[^}]*\})[ \t]*from[ \t]*["']([^"']+)["']"#).find_iter(text) {
        let ln = idx.of(m.start());
        if ln >= lines.len() || !strip(&lines[ln]).starts_with("export") {
            continue; // inside a comment
        }
        let wh = format!("js:{d}\0{}", m.s(2));
        if m.s(1) == "*" {
            m_.reexports.push(wh);
            continue;
        }
        let g = m.s(1);
        for part in g[1..g.len() - 1].split(',') {
            let part = reexport_skip(strip(part));
            if !part.is_empty() {
                let (src, local) = partition(&part, " as ");
                m_.imported.entry(strip(or(local, src)).to_string()).or_insert((wh.clone(), strip(src).to_string()));
            }
        }
    }
    for m in re!(r"(?m)^[ \t]*(?:export[ \t]+default|module\.exports[ \t]*=)[ \t]*\{").find_iter(text) {
        // `export default { ...utils, ...platform }`: what those modules define, this object has
        let close = matching(text, m.end() - 1);
        let body = if close > 0 { super::util::sl(text, m.end(), close as usize) } else { "" };
        for sm in re!(r"(?:^|[{,])\s*\.\.\.\s*([A-Za-z_$][\w$]*)\s*(?=[,}]|$)").find_iter(body) {
            let nm = sm.s(1);
            let wh = match m_.aliases.get(nm).filter(|s| !s.is_empty()) {
                Some(w) => w.clone(),
                None => match m_.imported.get(nm) {
                    Some((w, n)) if n == "default" || n == nm => w.clone(),
                    _ => String::new(),
                },
            };
            if !wh.is_empty() && !m_.reexports.contains(&wh) {
                m_.reexports.push(wh);
            }
        }
    }
}

/// `a::{b, c::d as e, self, f::*}` -> ([path...], local name); a glob's local name is "*" (`_rs_tree`).
pub fn rs_tree(spec: &str, prefix: &[String]) -> Vec<(Vec<String>, String)> {
    let spec = strip(spec);
    let mut out = Vec::new();
    if let Some(k) = spec.find('{') {
        let (head, rest) = (&spec[..k], &spec[k + 1..]);
        let body = match rest.rfind('}') {
            Some(e) => &rest[..e],
            None => {
                // (Python's rest[:-1])
                let mut it = rest.char_indices();
                match it.next_back() {
                    Some((e, _)) => &rest[..e],
                    None => "",
                }
            }
        };
        let mut base: Vec<String> = prefix.to_vec();
        base.extend(strip(head).trim_end_matches(':').split("::").filter(|x| !x.is_empty()).map(String::from));
        let (mut depth, mut cur) = (0i64, String::new());
        for ch in body.chars().chain(std::iter::once(',')) {
            if ch == ',' && depth == 0 {
                if !strip(&cur).is_empty() {
                    out.extend(rs_tree(&cur, &base));
                }
                cur.clear();
                continue;
            }
            depth += (ch == '{') as i64;
            depth -= (ch == '}') as i64;
            cur.push(ch);
        }
        return out;
    }
    let (path, alias) = partition(spec, " as ");
    let mut parts: Vec<String> = prefix.to_vec();
    parts.extend(path.split("::").map(strip).filter(|x| !x.is_empty()).map(String::from));
    if parts.is_empty() {
        return out;
    }
    if parts.last().map(|s| s == "self").unwrap_or(false) {
        parts.pop();
        if parts.is_empty() {
            return out;
        }
    }
    let local = if strip(alias).is_empty() { parts.last().unwrap().clone() } else { strip(alias).to_string() };
    out.push((parts, if local == "*" { "*".into() } else { local }));
    out
}

/// Rust `use` declarations (`_rs_imports`).
pub fn rs_imports(m_: &mut Module, rel: &str, code: &str) {
    let w = |parts: &[String]| format!("rs:{rel}\0{}", parts.join("::"));
    for m in re!(r"(?m)^[ \t]*(pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([^;]+);").find_iter(code) {
        let spec = re!(r"\s+").sub(" ", m.s(2));
        for (parts, local) in rs_tree(&spec, &[]) {
            if local == "*" {
                let p = w(&parts[..parts.len() - 1]);
                m_.uses.push(p.clone());
                if m.get(1).is_some() {
                    m_.reexports.push(p);
                }
                continue;
            }
            if local == "_" {
                continue;
            }
            m_.aliases.insert(local.clone(), w(&parts));
            if parts.len() > 1 {
                m_.imported.insert(local, (w(&parts[..parts.len() - 1]), parts.last().unwrap().clone()));
            }
        }
    }
    for q in ["super", "crate"] {
        m_.aliases.entry(q.into()).or_insert(w(&[q.to_string()]));
    }
}

/// Kotlin / Java package and imports (`_kt_imports`).
pub fn kt_imports(m_: &mut Module, text: &str, lines: &[String], idx: &Idx) {
    if let Some(pm) = re!(r"(?m)^[ \t]*package[ \t]+([\w.]+)").search(text) {
        m_.package = pm.s(1).to_string();
    }
    for m in re!(r"(?m)^[ \t]*import[ \t]+(?:static[ \t]+)?([\w.]+?)(\.\*)?(?:[ \t]+as[ \t]+(\w+))?[ \t]*;?[ \t]*$")
        .find_iter(text)
    {
        if !real(lines, idx, m.start()) {
            continue;
        }
        let (path, star, alias) = (m.s(1), m.get(2).is_some(), m.s(3));
        if star {
            m_.uses.push(format!("pkg:{path}"));
            continue;
        }
        let (pkg, last) = match path.rfind('.') {
            Some(k) => (&path[..k], &path[k + 1..]),
            None => ("", path),
        };
        if super::util::lower1(last) {
            // a top-level function (or property)
            m_.imported.insert(or(alias, last).to_string(), (format!("pkg:{pkg}"), last.to_string()));
        } else {
            // a type: its file is in scope for method-name matches, and its package says which one
            m_.uses.push(format!("type:{last}"));
            m_.imported.entry(or(alias, last).to_string()).or_insert((format!("pkg:{pkg}"), last.to_string()));
        }
    }
}

/// C#: the namespace is the package, `using A.B;` brings it in (`_cs_imports`).
pub fn cs_imports(m_: &mut Module, code: &str) {
    if let Some(nm) = re!(r"(?m)^[ \t]*namespace[ \t]+([\w.]+)").search(code) {
        m_.package = nm.s(1).to_string();
    }
    for m in re!(r"(?m)^[ \t]*(?:global[ \t]+)?using[ \t]+(?:static[ \t]+)?([\w.]+)[ \t]*;").find_iter(code) {
        m_.uses.push(format!("pkg:{}", m.s(1)));
    }
    for m in re!(r"(?m)^[ \t]*(?:global[ \t]+)?using[ \t]+([A-Za-z_]\w*)[ \t]*=[ \t]*([\w.]+)[ \t]*;").find_iter(code) {
        // `using Assert = Tests.XUnitAssert;`: that type, by another name
        m_.bound.insert(m.s(1).to_string(), format!("type:{}", super::util::last_part(m.s(2), ".")));
    }
}

/// Swift: a SwiftPM target is one scope; `import X` brings X's in (`_swift_imports`).
pub fn swift_imports(m_: &mut Module, rel: &str, code: &str) {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut found = None;
    for (i, p) in parts[..parts.len() - 1].iter().enumerate() {
        if matches!(*p, "Sources" | "Tests" | "Source") && i + 2 < parts.len() {
            found = Some(parts[i + 1].to_string());
            break;
        }
    }
    m_.package = found.unwrap_or_else(|| if parts.len() > 1 { parts[0].to_string() } else { String::new() });
    for m in re!(r"(?m)^[ \t]*(?:@\w+(?:\([^)\n]*\))?[ \t]+)*import[ \t]+(?:(?:class|struct|enum|protocol|func|var|let|typealias)[ \t]+)?(\w+)")
        .find_iter(code)
    {
        m_.uses.push(format!("pkg:{}", m.s(1)));
    }
}

/// PHP: the namespace is the package; `use A\B\C;` names type C (`_php_imports`).
pub fn php_imports(m_: &mut Module, code: &str) {
    if let Some(nm) = re!(r"(?m)^[ \t]*namespace[ \t]+([\w\\]+)[ \t]*[;{]").search(code) {
        m_.package = nm.s(1).trim_matches('\\').replace('\\', ".");
    }
    for m in re!(r"(?m)^[ \t]*use[ \t]+(?:function[ \t]+|const[ \t]+)?([^;]+);").find_iter(code) {
        if m.s(1).contains('(') || m.s(1).contains('$') {
            continue; // a closure's `use ($x)`, a trait's `use Foo;` inside a class
        }
        let spec = strip(&re!(r"\s+").sub(" ", m.s(1))).to_string();
        let items: Vec<String> = match re!(r"([\w\\]*)\\\{([^}]*)\}").match_start(&spec) {
            Some(g) => g
                .s(2)
                .split(',')
                .filter(|x| !strip(x).is_empty())
                .map(|x| format!("{}\\{}", g.s(1), strip(x)))
                .collect(),
            None => spec.split(',').map(String::from).collect(),
        };
        for it in items {
            let Some(am) = re!(r"\s*\\?([\w\\]+)(?:\s+as\s+(\w+))?\s*$").match_start(&it) else { continue };
            let full = am.s(1).replace('\\', ".");
            let (ns, name) = match full.rfind('.') {
                Some(k) => (&full[..k], &full[k + 1..]),
                None => ("", full.as_str()),
            };
            let local = or(am.s(2), name).to_string();
            if !ns.is_empty() {
                m_.aliases.entry(local.clone()).or_insert(format!("pkg:{ns}"));
            }
            if !am.s(2).is_empty() && am.s(2) != name {
                m_.bound.insert(local, format!("type:{name}"));
            }
        }
    }
}

/// Go: package "<dir>#<name>", each import's package name (`_go_imports`).
pub fn go_imports(m_: &mut Module, rel: &str, text: &str, code: &str) {
    let d = dir_of(rel);
    if let Some(pm) = re!(r"(?m)^package\s+(\w+)").search(code) {
        m_.package = format!("{d}#{}", pm.s(1));
    }
    let mut specs: Vec<(Option<String>, String)> = re!(r#"(?m)^import\s+(?:([\w.]+)\s+)?"([^"]+)""#)
        .find_iter(text)
        .iter()
        .map(|m| (m.get(1).map(String::from), m.s(2).to_string()))
        .collect();
    for blk in re!(r"(?m)^import\s*\(([^)]*)\)").find_iter(text) {
        for m in re!(r#"(?m)^\s*(?:([\w.]+)\s+)?"([^"]+)""#).find_iter(blk.s(1)) {
            specs.push((m.get(1).map(String::from), m.s(2).to_string()));
        }
    }
    for (alias, path) in specs {
        let alias = alias.unwrap_or_default();
        if alias == "_" {
            continue;
        }
        if alias == "." {
            m_.uses.push(format!("go:{path}"));
            continue;
        }
        let trimmed = path.trim_end_matches('/');
        let mut last = trimmed.rsplit('/').next().unwrap_or("").to_string();
        if alias.is_empty() && re!(r"^v\d+$").is_match(&last) && path.contains('/') {
            // module/v2: the package is the one before
            let parts: Vec<&str> = trimmed.split('/').collect();
            if parts.len() < 2 {
                panic!("list index out of range");
            }
            last = parts[parts.len() - 2].to_string();
        }
        let name = if alias.is_empty() {
            let l = last.strip_prefix("go-").unwrap_or(&last);
            let l = l.strip_suffix("-go").unwrap_or(l);
            re!(r"[^\w]").sub("_", l)
        } else {
            alias.clone()
        };
        let v = if alias.is_empty() { format!("go?:{path}") } else { format!("go:{path}") };
        m_.aliases.insert(name, v);
    }
}
