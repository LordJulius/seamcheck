//! Lambda parameters: what type a callback's parameter has, read from the
//! function it is passed to (lambdas.py).
//!
//! `services.AddPipeline("k", builder => builder.AddRetry())`: AddPipeline's
//! parameter at that position is `Action<PipelineBuilder>`, so `builder` is
//! a PipelineBuilder. The parser records, per function, each overload's
//! parameter types (`Func.ptypes`) and, per call, which lambda parameter
//! names sit in which argument (`lam:` bindings); the type inference joins
//! the two.

use std::sync::OnceLock;

use crate::pyre::Rx;

/// Func.ptypes separators: overloads ("<type-parameter count>TARGS<params>"), then parameters.
pub const OV: char = '\x1e';
pub const PARAM: char = '\x1f';
pub const TARGS: char = '\x1d';

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [OnceLock<Rx>; 24] = [const { OnceLock::new() }; 24];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

/// `text` split at `sep` outside brackets (an arrow's `>` is no bracket).
pub fn split_top(text: &str, sep: char) -> Vec<String> {
    let c: Vec<char> = text.chars().collect();
    let (mut out, mut d, mut cur, mut i) = (Vec::new(), 0i64, String::new(), 0);
    while i < c.len() {
        let ch = c[i];
        if (ch == '=' || ch == '-') && c.get(i + 1) == Some(&'>') {
            cur.push(ch);
            cur.push('>');
            i += 2;
            continue;
        }
        if "<([{".contains(ch) {
            d += 1;
        } else if ">)]}".contains(ch) {
            d = (d - 1).max(0);
        }
        if ch == sep && d == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
        i += 1;
    }
    out.push(cur);
    if out.iter().any(|s| !s.trim().is_empty()) {
        out
    } else {
        Vec::new()
    }
}

/// `code[i]` is an opening bracket: the byte index of its partner, or -1
/// (looking at most 20,000 characters on).
pub fn matching(code: &str, i: usize) -> isize {
    let mut stack: Vec<char> = Vec::new();
    for (n, (k, ch)) in code[i..].char_indices().enumerate() {
        if n >= 20000 {
            break;
        }
        match ch {
            '(' => stack.push(')'),
            '[' => stack.push(']'),
            '{' => stack.push('}'),
            ')' | ']' | '}' => {
                if stack.pop() != Some(ch) {
                    return -1;
                }
                if stack.is_empty() {
                    return (i + k) as isize;
                }
            }
            _ => {}
        }
    }
    -1
}

fn cut_default(p: &str) -> String {
    let parts = split_top(p, '='); // (`=>` stays whole: an arrow type is no default)
    parts.into_iter().next().unwrap_or_else(|| p.to_string())
}

/// The declared parameter types of one signature's parameter list.
pub fn param_types(tk: &str, inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    for p in split_top(inner, ',') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        match tk {
            "jvm" => {
                // Java / C#: `[Attr] this Foo<T> name = default`, `final @Ann Foo name`
                let p = rx(0, r"^(?:\s*\[[^\]]*\])+").sub("", p);
                let p = rx(1, r"@\w+(?:\([^)]*\))?\s*").sub("", &p);
                let p = cut_default(&p);
                let p = p.trim();
                out.push(
                    rx(2, r"(?s)^(?:(?:this|params|ref|out|in|final|scoped|readonly)\s+)*(.+?)\s*\b([A-Za-z_]\w*)$")
                        .match_start(p)
                        .map(|m| m.s(1).trim().to_string())
                        .unwrap_or_default(),
                );
            }
            "rust" => {
                // `name: Type`; a method's `self` takes no argument
                if rx(3, r"^(?:&\s*(?:'\w+\s+)?)?(?:mut\s+)?self\b").match_start(p).is_some() {
                    continue;
                }
                let parts = split_top(p, ':');
                out.push(if parts.len() > 1 { parts[1..].join(":").trim().to_string() } else { String::new() });
            }
            "kotlin" | "js" | "swift" => {
                // `name: Type = default` (TS `name?: Type`, Swift `label name: Type`)
                let parts = split_top(p, ':');
                out.push(if parts.len() > 1 { cut_default(&parts[1..].join(":")).trim().to_string() } else { String::new() });
            }
            _ => out.push(String::new()),
        }
    }
    out
}

/// Java's functional interfaces: how many leading type arguments are parameters.
fn java_fn(name: &str) -> Option<usize> {
    Some(match name {
        "Consumer" | "Predicate" | "Function" | "UnaryOperator" | "ToIntFunction" | "ToLongFunction"
        | "ToDoubleFunction" | "ObjIntConsumer" => 1,
        "BiConsumer" | "BiFunction" | "BiPredicate" | "BinaryOperator" => 2,
        "IntFunction" | "Supplier" | "Callable" | "Runnable" => 0,
        _ => return None,
    })
}

/// A function type's parameter types: C# `Action<A, B>` / `Func<A, R>`,
/// Java `Consumer<A>` / `BiFunction<A, B, R>`, Kotlin / Swift `(A, B) -> R`,
/// TS `(a: A) => R`, Rust `impl FnMut(Match) -> bool`.
pub fn lambda_params(ty: &str) -> Vec<String> {
    let t = ty.trim();
    let mut t = rx(4, r"^(?:@escaping\s+|@Sendable\s+|suspend\s+|noinline\s+|crossinline\s+)+").sub("", t);
    loop {
        if t.ends_with('?') {
            t = t[..t.len() - 1].trim().to_string();
        } else if t.starts_with('(')
            && matching(&t, 0) == t.len() as isize - 1
            && (t.contains("->") || t.contains("=>"))
        {
            t = t[1..t.len() - 1].trim().to_string(); // `((A) -> R)?`: the function type inside
        } else {
            break;
        }
    }
    if let Some(m) = rx(
        5,
        r"(?s)^(?:&\s*(?:'\w+\s+)?(?:mut\s+)?)?(?:Box\s*<\s*)?(?:impl|dyn)?\s*(?:Fn|FnMut|FnOnce)\s*\((.*?)\)(?:\s*->.*)?\s*>?(?:\s*\+\s*[\w']+)*$",
    )
    .match_start(&t)
    {
        return split_top(m.s(1), ',').iter().map(|a| a.trim().to_string()).collect();
    }
    if let Some(m) = rx(6, r"(?s)^(?:[\w.]+\.)?Expression\s*<(.*)>$").match_start(&t) {
        t = m.s(1).trim().to_string(); // C# `Expression<Func<A, bool>>`
    }
    if let Some(m) = rx(7, r"(?s)^(?:System\.)?(Action|Func)\s*<(.*)>$").match_start(&t) {
        let mut args: Vec<String> = split_top(m.s(2), ',').iter().map(|a| a.trim().to_string()).collect();
        if m.s(1) != "Action" {
            args.pop();
        }
        return args;
    }
    if let Some(m) = rx(8, r"(?s)^(?:java\.util\.function\.)?(\w+)\s*<(.*)>$").match_start(&t) {
        if let Some(n) = java_fn(m.s(1)) {
            let args: Vec<String> = split_top(m.s(2), ',')
                .iter()
                .map(|a| rx(9, r"^\?\s*(?:super|extends)\s+").sub("", a.trim()))
                .collect();
            return if n > 0 { args.into_iter().take(n).collect() } else { Vec::new() };
        }
    }
    if t.starts_with('(') {
        // `(A, B) -> R`, `(a: A) => R`
        let k = matching(&t, 0);
        if k > 0
            && rx(10, r"\s*(?:async\s+)?(?:re)?(?:throws\s*(?:\([^)]*\))?\s*)?[-=]>")
                .match_start(&t[k as usize + 1..])
                .is_some()
        {
            return split_top(&t[1..k as usize], ',')
                .iter()
                .map(|p| {
                    let parts = split_top(p, ':');
                    let ty = if parts.len() > 1 { parts[1..].join(":") } else { p.clone() };
                    ty.trim().trim_end_matches('?').trim().to_string()
                })
                .collect();
        }
    }
    Vec::new()
}

/// A lambda's parameter names, "" for one this can't bind (destructuring,
/// a typed C# / Java parameter, one typed in place).
fn names(params: &str) -> Vec<String> {
    split_top(params, ',')
        .iter()
        .map(|p| rx(11, r"^([A-Za-z_$][\w$]*)\s*$").match_start(p.trim()).map(|m| m.s(1).to_string()).unwrap_or_default())
        .collect()
}

/// For each argument that is a lambda: (its index, its parameter names, ""
/// for one this can't bind).
pub fn lambda_args(fam: &str, args: &[String]) -> Vec<(usize, Vec<String>)> {
    let mut out = Vec::new();
    let mut merged: Vec<String> = Vec::new();
    let args: &[String] = if fam == "rust" {
        // `|a, b| ...` was split at its comma: put each closure back together
        let mut k = 0;
        while k < args.len() {
            let mut a = args[k].clone();
            while rx(12, r"^\s*(?:move\s+)?\|[^|]*$").match_start(&a).is_some() && k + 1 < args.len() {
                k += 1;
                a = format!("{a},{}", args[k]);
            }
            merged.push(a);
            k += 1;
        }
        &merged
    } else {
        args
    };
    for (i, a) in args.iter().enumerate() {
        let mut a = a.trim().to_string();
        if let Some(m) = rx(13, r"^(?:async\s+|static\s+)*([A-Za-z_$][\w$]*)\s*(?:=>|->)").match_start(&a) {
            if m.s(1) != "async" && m.s(1) != "static" {
                out.push((i, vec![m.s(1).to_string()]));
                continue;
            }
        }
        if let Some(m) = rx(14, r"^(?:async\s+|static\s+)*\(([^()]*)\)\s*(?::\s*[^=]+)?(?:=>|->)").match_start(&a) {
            out.push((i, names(m.s(1))));
            continue;
        }
        if fam == "js" {
            if let Some(m) = rx(15, r"^(?:async\s+)?function\b\s*\*?\s*[\w$]*\s*\(([^()]*)\)").match_start(&a) {
                out.push((i, names(m.s(1))));
                continue;
            }
        }
        if fam == "swift" {
            a = rx(16, r"^[A-Za-z_]\w*\s*:\s*(?=\{)").sub("", &a); // `upload(multipartFormData: { form in ... })`
        }
        if (fam == "kotlin" || fam == "swift") && a.starts_with('{') {
            out.push((i, brace_lambda(fam, &a)));
        }
        if fam == "rust" {
            if let Some(m) = rx(17, r"^(?:move\s+)?\|([^|]*)\|").match_start(&a) {
                let ps = if m.s(1).trim().is_empty() {
                    Vec::new()
                } else {
                    split_top(m.s(1), ',')
                        .iter()
                        .map(|n| {
                            let n = n.trim();
                            if rx(18, r"^(?:&\s*)?(?:mut\s+)?[a-z_]\w*$").match_start(n).is_some() {
                                rx(19, r"^(?:&\s*)?(?:mut\s+)?").sub("", n)
                            } else {
                                String::new()
                            }
                        })
                        .collect()
                };
                out.push((i, ps));
            }
        }
    }
    out
}

/// A Kotlin / Swift `{ a, b -> ... }` / `{ a, b in ... }` lambda's
/// parameter names; Kotlin's `{ ... }` has one, `it`.
pub fn brace_lambda(fam: &str, text: &str) -> Vec<String> {
    if let Some(m) =
        rx(20, r"^\{\s*(?:\(([^()]*)\)|([A-Za-z_]\w*(?:\s*,\s*[A-Za-z_]\w*)*))\s*(->|\bin\b)").match_start(text)
    {
        if (m.s(3) == "->") == (fam == "kotlin") {
            // (Python: `group(1) or group(2)`; empty parens leave None, and the parse fails there)
            let params = match m.get(1).filter(|s| !s.is_empty()).or(m.get(2)) {
                Some(p) => p,
                None => panic!("object of type 'NoneType' has no len()"),
            };
            return names(params);
        }
    }
    if fam == "swift" {
        // `{ $0.name < $1.name }`: shorthand parameters, as many as it uses
        let k = if text.starts_with('{') { matching(text, 0) } else { -1 };
        let body = if k > 0 { &text[..k as usize] } else { text };
        let used: Vec<usize> =
            rx(21, r"\$(\d)").find_iter(body).iter().filter_map(|c| c.s(1).parse::<usize>().ok()).collect();
        return match used.iter().max() {
            Some(&mx) => (0..=mx).map(|i| format!("${i}")).collect(),
            None => Vec::new(),
        };
    }
    if fam == "kotlin" {
        vec!["it".into()]
    } else {
        Vec::new()
    }
}

/// `T`, `TResult`, `K`: a type parameter, not a class.
pub fn generic_param(ty: &str) -> bool {
    rx(22, r"^(?:[A-Z]|T[A-Z]\w*)$").match_start(ty.trim().trim_end_matches('?')).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    // lambdas.py's docstrings, as cases
    #[test]
    fn function_types_and_lambda_names() {
        assert_eq!(lambda_params("Action<A, B>"), v(&["A", "B"]));
        assert_eq!(lambda_params("Func<A, R>"), v(&["A"]));
        assert_eq!(lambda_params("BiFunction<A, B, R>"), v(&["A", "B"]));
        assert_eq!(lambda_params("(A, B) -> R"), v(&["A", "B"]));
        assert_eq!(lambda_params("((a: A) => R)?"), v(&["A"]));
        assert_eq!(lambda_params("impl FnMut(Match) -> bool"), v(&["Match"]));
        assert_eq!(brace_lambda("kotlin", "{ a, b -> a + b }"), v(&["a", "b"]));
        assert_eq!(brace_lambda("kotlin", "{ it.x }"), v(&["it"]));
        assert_eq!(brace_lambda("swift", "{ $0.n < $1.n }"), v(&["$0", "$1"]));
        let args = split_top("x, |a, b| a + b", ',');
        assert_eq!(lambda_args("rust", &args), vec![(1, v(&["a", "b"]))]);
        assert_eq!(param_types("rust", "&self, m: Match, n: usize"), v(&["Match", "usize"]));
        assert!(generic_param("TResult") && generic_param("K") && !generic_param("Foo"));
    }
}
