//! What a local holds, from how it is assigned (lang_other._assignments):
//! shared by the brace languages and Python.
//!
//! (local, "expr:<chain>") for `x = <chain>`; (local, "call:<chain>") for a
//! closure `x = |a| <chain>` / `x = { a -> <chain> }` (calling it gives that);
//! (local, "type:<Lit>") for a literal of the language's own type;
//! (local, "either:<chain>\x1c<chain>") for a JS ternary of two `new`s;
//! (local, "jsx:") for a JSX element. Chains join their segments with \x1f.

use std::sync::OnceLock;

use crate::lang::chains::{expression, Opts};
use crate::lang::lambdas::split_top;
use crate::pyre::{take_chars, Rx};

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [OnceLock<Rx>; 24] = [const { OnceLock::new() }; 24];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

/// `x = ...` per language (lang_other._ASSIGN); None: the language has none.
pub fn assign_pattern(tk: &str) -> Option<&'static Rx> {
    Some(match tk {
        "js" => rx(0, r"\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=;\n]+)?=(?!=)"),
        "kotlin" => rx(1, r"\b(?:val|var)\s+([A-Za-z_]\w*)\s*(?::[^=\n]+)?=(?!=)"),
        "swift" => rx(2, r"\b(?:let|var)\s+([A-Za-z_]\w*)\s*(?::[^=\n]+)?=(?!=)"),
        "rust" => rx(3, r"\blet\s+(?:mut\s+)?([A-Za-z_]\w*)\s*(?::[^=\n]+)?=(?!=)"),
        "go" => rx(4, r"(?m)(?:^|[;{]|\bvar\b)\s*([A-Za-z_]\w*)\s*(?:,\s*[A-Za-z_]\w*\s*)*(?::=|=(?!=))"),
        "jvm" => rx(5, r"\bvar\s+([A-Za-z_]\w*)\s*=(?!=)"),
        "cpp" => rx(6, r"\bauto\s*[&*]*\s*([A-Za-z_]\w*)\s*=(?!=)\s*[*&]?"),
        "php" => rx(7, r"(\$[A-Za-z_]\w*)\s*=(?![=>])"),
        "py" => rx(8, r"(?m)^[ \t]*([A-Za-z_]\w*)\s*(?::[^=\n]+)?=(?!=)"),
        _ => return None,
    })
}

/// A literal's own type, by its first character (lang_other._LITERAL).
fn literal(tk: &str, first: &str) -> Option<&'static str> {
    Some(match (tk, first) {
        ("js", "[") | ("swift", "[") | ("rust", "[") => "Array",
        ("js", "\"") | ("js", "'") | ("js", "`") | ("swift", "\"") => "String",
        ("py", "[") => "list",
        ("py", "{") => "dict",
        ("py", "\"") | ("py", "'") => "str",
        ("rust", "\"") => "str",
        ("rust", "v") => "Vec",
        _ => return None,
    })
}

fn chain(ch: &[String]) -> String {
    ch.join("\x1f")
}

fn first_char(s: &str) -> &str {
    s.char_indices().nth(1).map(|(k, _)| &s[..k]).unwrap_or(s)
}

/// Every assignment of `text` in language `tk`, in Python's order.
pub fn assignments(tk: &str, text: &str, o: Opts) -> Vec<(String, String)> {
    let Some(pat) = assign_pattern(tk) else { return Vec::new() };
    let mut out: Vec<(String, String)> = Vec::new();
    for m in pat.find_iter(text) {
        let name = m.s(1).to_string();
        let rest = take_chars(&text[m.end()..], 300).trim_start();
        let cm = match tk {
            "rust" => rx(9, r"(?:move\s*)?\|[^|]*\|\s*").match_start(rest),
            "kotlin" => rx(10, r"\{\s*(?:[\w, ]+->)?\s*").match_start(rest),
            _ => None,
        };
        if let Some(cm) = cm {
            let mut body_at = cm.end();
            if rest[body_at..].starts_with('{') && tk == "rust" {
                body_at += 1; // `|p| {\n  one.expr(p)\n}`: a block of one expression
            }
            if !rest[body_at..].starts_with('{') {
                let ch = expression(rest, body_at, 40, o);
                if !ch.is_empty() {
                    out.push((name, format!("call:{}", chain(&ch))));
                }
            }
            continue;
        }
        let mut lit = literal(tk, first_char(rest));
        if lit == Some("Vec") && !rest.starts_with("vec!") {
            lit = None; // (only `vec![...]`)
        }
        if let Some(l) = lit {
            if rx(11, r"[\[{][^\n]*?[\]}]\s*(?:\.|\[|\?\.)").match_start(rest).is_none() {
                out.push((name, format!("type:{l}"))); // `x = []`, `x = ""`: the language's own value
                continue;
            }
        }
        let ch = expression(text, m.end(), 40, o);
        if tk == "js" {
            let line_end = text[m.end()..].find('\n').map(|k| m.end() + k).unwrap_or(text.len());
            if let Some(tm) = rx(
                12,
                r#"[^?:;\n'"`]*\?\s*(new\s+[\w$.]+\s*\([^()\n]*\))\s*:\s*(new\s+[\w$.]+\s*\([^()\n]*\))\s*;?\s*$"#,
            )
            .match_start(&text[m.end()..line_end])
            {
                // `c ? new TextEncoder() : new util.TextEncoder()`: whichever runs, if the two agree
                let (a, b) = (expression(tm.s(1), 0, 40, o), expression(tm.s(2), 0, 40, o));
                if !a.is_empty() && !b.is_empty() {
                    out.push((name, format!("either:{}\x1c{}", chain(&a), chain(&b))));
                    continue;
                }
            }
        }
        if ch.len() == 1 && ["null", "nil", "None", "undefined", "NULL", "nullptr"].contains(&ch[0].as_str()) {
            continue; // `$stats = null;`: no type yet; a later assignment says it
        }
        if !ch.is_empty() {
            out.push((name, format!("expr:{}", chain(&ch))));
        }
    }
    if tk != "js" {
        return out;
    }
    // `app = app || new Koa()`, `opts ??= {}`: a parameter's default, the value when none came
    for m in rx(13, r"(?m)^[ \t]*([A-Za-z_$][\w$]*)[ \t]*(?:=[ \t]*\1[ \t]*(?:\|\||\?\?)|\|\|=|\?\?=)[ \t]*").find_iter(text) {
        let ch = expression(text, m.end(), 40, o);
        if !ch.is_empty() && !(ch.len() == 1 && (ch[0] == "null" || ch[0] == "undefined")) {
            out.push((m.s(1).to_string(), format!("expr:{}", chain(&ch))));
        }
    }
    // `const template = <p>Hello</p>`: an element of the JSX runtime
    for m in rx(14, r"\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=;\n]+)?=\s*\(?\s*<[A-Za-z>]").find_iter(text) {
        out.push((m.s(1).to_string(), "jsx:".into()));
    }
    // `var blog = express(), app = express()`: the names after the first
    let b = text.as_bytes();
    let n = b.len();
    for m in rx(15, r"\b(?:var|let|const)\s+").find_iter(text) {
        let (mut k, mut d, mut seen) = (m.end(), 0i64, 0usize);
        // the statement's end: `;`, or a line break at depth 0 not continued
        while k < n && seen < 3000 {
            let ch = b[k];
            if b"([{".contains(&ch) {
                d += 1;
            } else if b")]}".contains(&ch) {
                d -= 1;
                if d < 0 {
                    break;
                }
            } else if d == 0 && ch == b';' {
                break;
            } else if d == 0 && ch == b',' {
                let win = take_chars(&text[k + 1..], 199);
                if let Some(am) = rx(16, r"\s*([A-Za-z_$][\w$]*)\s*=(?![=>])").match_start(win) {
                    let ch_ = expression(text, k + 1 + am.end(), 40, o);
                    if !ch_.is_empty() {
                        out.push((am.s(1).to_string(), format!("expr:{}", chain(&ch_))));
                    }
                }
            } else if d == 0
                && ch == b'\n'
                && !take_chars(&text[k + 1..], 199).trim_start().starts_with(',')
                && !{
                    let before = text[..k].trim_end();
                    before.ends_with(',') || before.ends_with('=')
                }
            {
                break;
            }
            k += match ch {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            seen += 1;
        }
    }
    // `const { h, app: a } = window`: each name is that member of the value
    for m in rx(17, r"\b(?:const|let|var)\s*\{([^{}=;]*)\}\s*(?::[^=;]+)?=(?!=)").find_iter(text) {
        let ch = expression(text, m.end(), 40, o);
        let Some(last) = ch.last() else { continue };
        let probe = if last.ends_with("()") { "x" } else { last.as_str() };
        if rx(18, r"^[\w$]+$").match_start(probe).is_none() {
            continue;
        }
        for part in split_top(m.s(1), ',') {
            if let Some(pm) = rx(19, r"(?s)^\s*([A-Za-z_$][\w$]*)\s*(?::\s*([A-Za-z_$][\w$]*))?\s*(?:=.*)?$").match_start(&part) {
                let local = pm.get(2).filter(|s| !s.is_empty()).unwrap_or(pm.s(1));
                let mut full = ch.clone();
                full.push(pm.s(1).to_string());
                out.push((local.to_string(), format!("expr:{}", chain(&full))));
            }
        }
    }
    // `utils.extend(instance, Axios.prototype, ...)`: instance has Axios's methods (ahead of its own value)
    let mut proto: Vec<(String, String)> = rx(
        20,
        r"\b(?:extend|assign|mixin|setPrototypeOf)\(\s*([A-Za-z_$][\w$]*)\s*,\s*([A-Z][\w$]*)\.prototype\b",
    )
    .find_iter(text)
    .iter()
    .map(|m| (m.s(1).to_string(), format!("expr:new {}()", m.s(2))))
    .collect();
    proto.extend(out);
    proto
}
