//! Class-like declarations (lang_other._classes): name, bases as written,
//! header text, and the body's span.

use super::util::{allcaps, ends_any, last_part, lstrip, re, rstrip, rw, sl, strip};
use crate::lang::chains::generic_name;
use crate::lang::lambdas::matching;

#[derive(Debug, Clone)]
pub struct Cls {
    pub name: String,
    pub kw: String,
    pub bases: Vec<String>,
    pub hdr: String,
    pub pos: usize,
    /// the body's `{` and its `}` (byte offsets); None: no body
    pub span: Option<(usize, usize)>,
    pub kind: String,
    pub arity: usize,
    /// the span's lines (0-based), set by the parser
    pub a: usize,
    pub b: usize,
}

const BASE_SKIP: &[&str] = &[
    "public", "private", "protected", "virtual", "internal", "extends", "implements", "where", "class", "struct", "new",
    "final", "sealed", "open", "abstract", "override", "in", "out", "reified", "const", "Sendable", "Equatable",
    "Hashable", "Codable", "Identifiable", "CustomStringConvertible",
];

fn base_skip(s: &str) -> bool {
    BASE_SKIP.contains(&s)
}

/// ES5 classes: a capitalized function defining methods on `this` (`_js_constructors`):
/// (start, end, "function", name).
fn js_constructors(code: &str) -> Vec<(usize, usize, String, String)> {
    let mut out = Vec::new();
    for m in re!(r"(?<![\w$.])(function)\s+([A-Z][\w$]*)\s*\(").find_iter(code) {
        let close = matching(code, m.end() - 1);
        let b: isize = if close > 0 { super::util::find_from(code, "{", close as usize) } else { -1 };
        let e = if b > 0 && strip(sl(code, (close + 1) as usize, b as usize)).is_empty() {
            matching(code, b as usize)
        } else {
            -1
        };
        if e > 0
            && re!(r"\bthis\.[\w$]+\s*=\s*(?:async\s+)?(?:function\b|\([^()]*\)\s*=>|[\w$]+\s*=>)")
                .search_range(code, b as usize, e as usize)
                .is_some()
        {
            out.push((m.start(), m.end(), m.s(1).to_string(), m.s(2).to_string()));
        }
    }
    out
}

/// (start, end, group 1, group 2, how)
type Hit = (usize, usize, String, String, &'static str);

fn hits(code: &str, ext: &str, fam: &str) -> Vec<Hit> {
    let mut out: Vec<Hit> = Vec::new();
    let caps = |r: &crate::pyre::Rx, how: &'static str, out: &mut Vec<Hit>| {
        for m in r.find_iter(code) {
            out.push((m.start(), m.end(), m.s(1).to_string(), m.s(2).to_string(), how));
        }
    };
    if fam == "rust" {
        caps(re!(r"\bimpl\s*(?:<[^{}]*?>)?\s*(?:([\w:]+)(?:<[^{}]*?>)?\s+for\s+)?([\w:]+)"), "impl", &mut out);
        caps(re!(r"(?<![\w.])(?:pub(?:\([^)]*\))?\s+)?(struct|enum|trait|union)\s+([A-Za-z_]\w*)"), "type", &mut out);
        return out;
    }
    caps(
        re!(r"(?<![\w.$@])(?:(?:data|sealed|abstract|open|inner|value|enum|annotation|final|public|private|protected|internal|static|partial|export|default|readonly|ref|unsafe|pub(?:\([^)]*\))?|fileprivate|indirect|declare|fun)\s+)*(class|interface|struct|record|object|trait|protocol|extension|union)\s+([A-Za-z_$][\w$]*)"),
        "type",
        &mut out,
    );
    if ext == ".swift" || ext == ".php" {
        // Swift / PHP enums have methods; a Swift actor is a class
        caps(
            re!(r"(?<![\w.$@])(?:(?:public|private|internal|fileprivate|indirect|final)\s+)*(enum|actor)\s+([A-Z]\w*)"),
            "type",
            &mut out,
        );
    }
    if fam == "kotlin" {
        for m in re!(r"\bcompanion\s+object\b(?:\s+\w+)?").find_iter(code) {
            out.push((m.start(), m.end(), String::new(), String::new(), "companion"));
        }
    }
    if fam == "js" && ext != ".qml" {
        for (s, e, g1, g2) in js_constructors(code) {
            out.push((s, e, g1, g2, "type"));
        }
        // TS `type ResponsePromise<T> = { text: () => ... } & Promise<R>`: an object type, read as an interface
        caps(
            re!(r"(?<![\w.$])(?:export\s+)?(?:declare\s+)?(type)\s+([A-Z][\w$]*)(?=\s*(?:<[^=;{]*(?:=[^;{]*)?>)?\s*=\s*\{)"),
            "type",
            &mut out,
        );
    }
    out
}

/// Python `re.split(pat, s, maxsplit=1)` with a group-free pattern.
fn split1<'a>(r: &crate::pyre::Rx, s: &'a str) -> Vec<&'a str> {
    match r.search(s) {
        Some(m) => vec![&s[..m.start()], &s[m.end()..]],
        None => vec![s],
    }
}

/// Class-like declarations in file order of the patterns (`_classes`).
pub fn classes(code: &str, text: &str, ext: &str, fam: &str) -> Vec<Cls> {
    let cpp = super::pats::CPP_EXTS.contains(&ext);
    let b = code.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    for (mstart, mend, g1, g2, how) in hits(code, ext, fam) {
        let (mut name, mut bases, kw): (String, Vec<String>, String);
        if how == "impl" {
            name = last_part(&g2, "::").to_string();
            bases = if g1.is_empty() { Vec::new() } else { vec![last_part(&g1, "::").to_string()] };
            kw = "impl".into();
        } else if how == "companion" {
            name = String::new();
            bases = Vec::new();
            kw = "companion".into();
        } else {
            kw = g1.clone();
            name = g2.clone();
            if let Some(qn) = re!(r"(?:\s*::\s*[A-Za-z_]\w*)+").match_start(rw(code, text, mend, 200)) {
                // `struct Device::DevicePrivate {`: the nested type's own name
                name = strip(last_part(qn.group0(), "::")).to_string();
            }
            if ext == ".swift" && kw == "extension" {
                if let Some(sq) = re!(r"(?:\.[A-Za-z_]\w*)+").match_start(rw(code, text, mend, 200)) {
                    // `extension NameSpecification.Element {`: the nested type's own name
                    name = last_part(sq.group0(), ".").to_string();
                }
            }
            if allcaps(&name) {
                // `class KDECONNECTCORE_EXPORT NetworkPacket`: an export macro, then the name
                if let Some(nm2) = re!(r"\s+([A-Za-z_]\w*)").match_start(rw(code, text, mend, 200)) {
                    if !allcaps(nm2.s(1)) {
                        name = nm2.s(1).to_string();
                    }
                }
            }
            if kw == "object" && fam != "kotlin" {
                continue;
            }
            if kw == "protocol" && ext != ".swift" {
                continue;
            }
            bases = Vec::new();
        }
        // header: to the body's `{` at depth 0, or to the end of the declaration
        let (mut k, mut depth, mut body, mut steps): (usize, i64, Option<usize>, usize) = (mend, 0, None, 0);
        while k < n && steps < 20000 {
            let ch = b[k];
            if ch == b'(' || ch == b'<' || ch == b'[' {
                depth += 1;
            } else if ch == b')' || ch == b'>' || ch == b']' {
                if !(ch == b'>' && k > 0 && b[k - 1] == b'-') {
                    depth -= 1;
                }
            } else if depth <= 0 && ch == b'{' {
                body = Some(k);
                break;
            } else if depth <= 0 && ch == b';' {
                break;
            } else if depth <= 0
                && ch == b'\n'
                && (fam == "kotlin" || fam == "js")
                && !matches!(super::util::first(lstrip(rw(code, text, k + 1, 199))), "{" | ":" | "," | "")
            {
                let nxt = lstrip(rw(code, text, k + 1, 199));
                if !ends_any(rstrip(&code[..k]), &[",", ":", "("])
                    && re!(r"(?:extends|implements)\b").match_start(nxt).is_none()
                    && !(fam == "kotlin"
                        && re!(r"(?:@\w+\s*|(?:private|internal|public|protected)\s+)*constructor\b|@\w+\s*$|@\w+\s*\n")
                            .match_start(nxt)
                            .is_some())
                {
                    break; // (Kotlin: `class Builder\n  @JvmOverloads\n  constructor(...) {` goes on)
                }
            } else if ch == b'='
                && depth <= 0
                && (fam == "js" || fam == "c")
                && how == "type"
                && !(kw == "type" && lstrip(rw(code, text, k + 1, 39)).starts_with('{'))
            {
                break; // `type X = ...` / a declaration, not a class body
            }
            if super::util::lead(text, k) {
                steps += 1;
            }
            k += 1;
        }
        // (the loop counts characters: step over a multi-byte one whole)
        let hdr_end = body.unwrap_or(k);
        let hdr = sl(code, mend, hdr_end).to_string();
        if how == "type" {
            let h = re!(r"(?s)\bwhere\b.*").sub("", &hdr);
            let h = re!(r"<[^<>]*(?:<[^<>]*>[^<>]*)*>").sub("", &h); // generic arguments
            let mut h2 = h.clone();
            while h2.contains('(') {
                let h3 = re!(r"\([^()]*\)").sub("", &h2);
                if h3 == h2 {
                    break;
                }
                h2 = h3;
            }
            let tail = split1(re!(r"\bextends\b|\bimplements\b|:"), &h2);
            if tail.len() == 2 {
                bases = re!(r"[A-Za-z_$][\w$.:\\]*")
                    .find_iter(tail[1])
                    .iter()
                    .map(|m| last_part(last_part(last_part(m.group0(), "::"), "."), "\\").to_string())
                    .filter(|b| !base_skip(b))
                    .collect();
                if ext == ".cs" {
                    // `: Policy<TResult>, IAsyncPolicy`: a generic base by its arity
                    let nowhere = re!(r"(?s)\bwhere\b.*").sub("", &hdr);
                    let raw_tail = split1(re!(r":"), &nowhere);
                    if raw_tail.len() == 2 {
                        let t = raw_tail[1];
                        let (mut depth, mut last, mut parts) = (0i64, 0usize, Vec::new());
                        let tc = format!("{t},");
                        for (k2, ch2) in tc.char_indices() {
                            depth += "<([".contains(ch2) as i64;
                            depth -= ">)]".contains(ch2) as i64;
                            if ch2 == ',' && depth == 0 {
                                parts.push(sl(t, last, k2).to_string());
                                last = k2 + 1;
                            }
                        }
                        let parts: Vec<String> =
                            parts.iter().map(|x| re!(r"\s*\((?:[^()]|\([^()]*\))*\)\s*$").sub("", x)).collect();
                        let gb: Vec<String> = parts
                            .iter()
                            .filter_map(|x| {
                                let g = generic_name(x);
                                (!g.is_empty()).then(|| last_part(&g, ".").to_string())
                            })
                            .filter(|b| !base_skip(b.split('`').next().unwrap_or("")))
                            .collect();
                        if !gb.is_empty() {
                            bases = gb;
                        }
                    }
                }
            }
        }
        let mut span = None;
        if let Some(bd) = body {
            let (mut d, mut j) = (0i64, bd);
            while j < n {
                if b[j] == b'{' {
                    d += 1;
                } else if b[j] == b'}' {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                j += 1;
            }
            span = Some((bd, j));
            if kw == "type" {
                // `type X = {...} & express.Express & Promise<R>`: the other parts are its bases
                let mut k_ = j + 1;
                while let Some(im) =
                    re!(r"\s*&\s*([A-Za-z_$][\w$.]*)(?:<(?:[^<>]|<(?:[^<>]|<[^<>]*>)*>)*>)?").match_start(rw(code, text, k_, 400))
                {
                    bases.push(last_part(im.s(1), ".").to_string());
                    k_ += im.end();
                }
            }
            if ext == ".php" {
                // `use SomeTrait;` in the body: the trait's methods are the class's
                for tm in re!(r"(?m)^[ \t]*use[ \t]+([\w\\, \t]+)[ \t]*[;{]").find_iter(sl(code, bd, j)) {
                    for t in tm.s(1).split(',') {
                        if !strip(t).is_empty() {
                            bases.push(last_part(strip(t), "\\").to_string());
                        }
                    }
                }
            }
        } else if cpp {
            continue; // `class QMouseEvent;`: a forward declaration names a type defined elsewhere
        }
        let around = format!("{}{}", super::util::rb(code, text, mstart, 40), sl(code, mstart, mend));
        let kind = if kw == "extension" || kw == "impl" || re!(r"\bpartial\b").search(&around).is_some() {
            "ext"
        } else {
            "class"
        };
        let mut kind = kind.to_string();
        if matches!(kw.as_str(), "interface" | "protocol" | "trait" | "type") {
            kind = "interface".into();
        }
        let arity = if ext == ".cs" {
            re!(r"\s*<([^>{]*)>").match_start(&hdr).map(|am| am.s(1).matches(',').count() + 1).unwrap_or(0)
        } else {
            0
        };
        out.push(Cls { name, kw, bases, hdr, pos: mstart, span, kind, arity, a: 0, b: 0 });
    }
    out
}
