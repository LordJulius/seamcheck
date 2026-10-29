//! Argument counts of overloaded declarations (infer._arities and friends).

use super::{esc, rxd, OVERLOADED};
use crate::model::Module;
use crate::pyre::Rx;

const DECL_MODS: &str = r"(?:(?:public|protected|private|static|final|abstract|synchronized|native|default|override|virtual|open|internal|suspend|inline|operator|async|sealed|new|extern|unsafe|partial|readonly|const|fun)\s+)*";

/// What may stand before a declared name on its line: annotations, modifiers,
/// a return type (infer._DECL_HEAD).
fn decl_head() -> &'static Rx {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Rx::lit(&format!(
            r"^(?!\s*(?:return|throw|new|else|case|yield|await|assert|if|while|for|switch|do|try|catch|delete|goto|co_return|co_await|sizeof|typeof)\b)\s*(?:@\w+(?:\([^()]*\))?\s*)*{DECL_MODS}(?:<[^<>]*>\s*)?(?:[A-Za-z_][\w.:]*(?:<[^()]*>)?[\[\]?*& ]*\s+)?(?:[A-Za-z_][\w:]*::)?$"
        ))
    })
}

/// The argument counts each declaration of `name` in m's file takes:
/// [(fewest, most)], `...` / `params` / `vararg` taking any more. [] when
/// none can be read (then nothing is ruled out).
pub fn arities(m: &Module, name: &str) -> Vec<(usize, usize)> {
    if !OVERLOADED.iter().any(|e| m.file.ends_with(e)) {
        return vec![];
    }
    let text = m.code.join("\n");
    let mut out = Vec::new();
    for dm in rxd(&format!(r"\b{}\s*(?:<[^()<>]*>)?\s*\(", esc(name))).find_iter(&text) {
        let ls = text[..dm.start()].rfind('\n').map(|x| x + 1).unwrap_or(0);
        let head = &text[ls..dm.start()];
        if decl_head().match_start(head).is_none() || head.trim().is_empty() {
            continue; // (a call: `x.name(`, `return name(`, `= name(`)
        }
        let close = paren_close(&text, dm.end() - 1);
        if close < 0 {
            continue;
        }
        let close = close as usize;
        let after_s = crate::pyre::take_chars(&text[close + 1..], 120).trim_start();
        if rxd(r"(?:const\s*)?(?:throws\b[^{;]*)?(?:noexcept\s*)?(?:override\s*)?(?:\{|;|:|=|->|where\b|$)")
            .match_start(after_s)
            .is_none()
        {
            continue;
        }
        let params = &text[dm.end()..close];
        if params.trim().is_empty() {
            out.push((0, 0));
            continue;
        }
        let parts = top_split(params);
        let n = parts.len();
        if parts.iter().any(|p| p.contains("...") || rxd(r"\s*(?:params|vararg)\b").match_start(p).is_some()) {
            out.push((n - 1, 1 << 20));
            continue;
        }
        let defaults = parts.iter().filter(|p| p.contains('=')).count();
        out.push((n - defaults, n));
    }
    out
}

/// `text[k]` opens a bracket: the index of its partner within 2000 characters, or -1.
pub fn paren_close(text: &str, k: usize) -> isize {
    let mut depth = 0i32;
    let end = text.len().min(k + 2000);
    let b = text.as_bytes();
    for j in k..end {
        match b[j] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return j as isize;
                }
            }
            _ => {}
        }
    }
    -1
}

/// Split on the commas at depth 0 (generics `<A, B>` included).
pub fn top_split(s: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (Vec::new(), 0i32, String::new());
    for ch in s.chars() {
        if "([{<".contains(ch) {
            depth += 1;
        } else if ")]}>".contains(ch) {
            depth -= 1;
        }
        if ch == ',' && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    out.push(cur);
    out
}
