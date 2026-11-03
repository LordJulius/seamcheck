//! How Python prints the values of the runtime rules: `json.dumps(sort_keys=True)`
//! (messages and subjects), `repr()` / `str()` (names and ids in messages),
//! float `repr` and the `:g` format (a finding's `t=`).

use super::expr::V;

/// Python's `json.dumps(v, sort_keys=True)` (ASCII-escaped, `NaN` / `Infinity` bare).
pub fn dumps(v: &V) -> String {
    let mut s = String::new();
    dump(v, &mut s);
    s
}

/// A JSON string as `json.dumps` writes it (ensure_ascii).
pub fn dump_str(x: &str, out: &mut String) {
    out.push('"');
    for c in x.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The shortest digits that read back as `f`, and the decimal exponent of
/// the first one (`1.5e-5` -> ("15", -5)).
fn shortest(f: f64) -> (String, i32) {
    let s = format!("{:e}", f.abs());
    let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
    (m.replace('.', ""), e.parse().unwrap_or(0))
}

/// Python's `repr(float)` for a finite value: the shortest round-trip
/// digits, in exponent form below 1e-4 and from 1e16 on.
fn finite_repr(f: f64) -> String {
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if f == 0.0 {
        return format!("{sign}0.0");
    }
    let (digits, exp) = shortest(f);
    if (-4..16).contains(&exp) {
        let fixed = format!("{}", f.abs());
        let fixed = if fixed.contains('.') { fixed } else { format!("{fixed}.0") };
        format!("{sign}{fixed}")
    } else {
        let mant = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        format!("{sign}{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// A float as `json.dumps` writes it.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        finite_repr(f)
    }
}

/// A float as Python's `repr()` / `str()` writes it (`nan`, `inf`).
pub fn float_py(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        finite_repr(f)
    }
}

/// Python's `f"{f:g}"`: six significant digits, trailing zeros dropped,
/// exponent form below 1e-4 and from 1e6 on.
pub fn fmt_g(f: f64) -> String {
    if !f.is_finite() {
        return float_py(f);
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{:.5e}", f);
    let (m, e) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = e.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if (-4..6).contains(&exp) {
        trim(&format!("{:.*}", (5 - exp) as usize, f))
    } else {
        format!("{}e{}{:02}", trim(m), if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// Python's `repr()` of a value.
pub fn py_repr(v: &V) -> String {
    match v {
        V::Str(s) => crate::pyre::repr(s),
        V::List(xs) => format!("[{}]", xs.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        V::Tuple(xs) if xs.len() == 1 => format!("({},)", py_repr(&xs[0])),
        V::Tuple(xs) => format!("({})", xs.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        V::Set(xs) if xs.is_empty() => "set()".into(),
        V::Set(xs) => format!("{{{}}}", xs.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        V::Dict(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", crate::pyre::repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
        other => py_str(other),
    }
}

/// Python's `str()` of a value.
pub fn py_str(v: &V) -> String {
    match v {
        V::None => "None".into(),
        V::Bool(b) => (if *b { "True" } else { "False" }).into(),
        V::Int(i) => i.to_string(),
        V::Float(f) => float_py(*f),
        V::Str(s) => s.clone(),
        other => py_repr(other),
    }
}

fn dump(v: &V, out: &mut String) {
    match v {
        V::None => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Int(i) => out.push_str(&i.to_string()),
        V::Float(f) => out.push_str(&float_repr(*f)),
        V::Str(s) => dump_str(s, out),
        // (a set is no JSON: the messages' `default=str` writes its repr)
        V::Set(_) => dump_str(&py_repr(v), out),
        V::List(xs) | V::Tuple(xs) => {
            out.push('[');
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump(x, out);
            }
            out.push(']');
        }
        V::Dict(m) => {
            let mut ks: Vec<&String> = m.keys().collect();
            ks.sort();
            out.push('{');
            for (i, k) in ks.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_str(k, out);
                out.push_str(": ");
                dump(&m[*k], out);
            }
            out.push('}');
        }
    }
}
