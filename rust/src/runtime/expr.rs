//! The rule expressions of `[[invariant]]` / `[[contract]]` (runtime.py's
//! `compile_expr` / `evaluate`): a small Python expression language over JSON
//! values, in a sandbox. Comparisons (chained), `and` / `or` / `not`,
//! arithmetic, `in` / `is`, `a if c else b`, subscripts and slices, `a.b` as
//! the field `b` of a dict, list / tuple / set / dict literals, list / set /
//! generator comprehensions, and calls to the helpers only. Anything else is
//! refused when the rule is compiled, as Python's AST allowlist does.

use std::cmp::Ordering;

use indexmap::IndexMap;
use serde_json::Value as Json;

/// A value inside an expression: JSON plus the shapes Python adds (tuple, set).
#[derive(Clone, Debug)]
pub enum V {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<V>),
    Tuple(Vec<V>),
    Set(Vec<V>),
    Dict(IndexMap<String, V>),
}

impl V {
    pub fn from_json(j: &Json) -> V {
        match j {
            Json::Null => V::None,
            Json::Bool(b) => V::Bool(*b),
            Json::Number(n) => match n.as_i64() {
                Some(i) => V::Int(i),
                None => V::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            Json::String(s) => V::Str(s.clone()),
            Json::Array(xs) => V::List(xs.iter().map(V::from_json).collect()),
            Json::Object(m) => V::Dict(m.iter().map(|(k, v)| (k.clone(), V::from_json(v))).collect()),
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            V::None => false,
            V::Bool(b) => *b,
            V::Int(i) => *i != 0,
            V::Float(f) => *f != 0.0,
            V::Str(s) => !s.is_empty(),
            V::List(x) | V::Tuple(x) | V::Set(x) => !x.is_empty(),
            V::Dict(m) => !m.is_empty(),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            V::None => "NoneType",
            V::Bool(_) => "bool",
            V::Int(_) => "int",
            V::Float(_) => "float",
            V::Str(_) => "str",
            V::List(_) => "list",
            V::Tuple(_) => "tuple",
            V::Set(_) => "set",
            V::Dict(_) => "dict",
        }
    }

    fn num(&self) -> Option<f64> {
        match self {
            V::Bool(b) => Some(*b as i64 as f64),
            V::Int(i) => Some(*i as f64),
            V::Float(f) => Some(*f),
            _ => None,
        }
    }

    fn int(&self) -> Option<i64> {
        match self {
            V::Bool(b) => Some(*b as i64),
            V::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Items of anything iterable (a dict iterates its keys, a string its characters).
    pub fn items(&self) -> Result<Vec<V>, String> {
        match self {
            V::List(x) | V::Tuple(x) | V::Set(x) => Ok(x.clone()),
            V::Dict(m) => Ok(m.keys().map(|k| V::Str(k.clone())).collect()),
            V::Str(s) => Ok(s.chars().map(|c| V::Str(c.to_string())).collect()),
            other => Err(format!("TypeError: '{}' object is not iterable", other.type_name())),
        }
    }
}

/// Python's `==`: numbers by value, sequences of the same kind item by item,
/// sets and dicts regardless of order.
pub fn eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::None, V::None) => true,
        (V::Str(x), V::Str(y)) => x == y,
        (V::List(x), V::List(y)) | (V::Tuple(x), V::Tuple(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| eq(p, q)),
        (V::Set(x), V::Set(y)) => x.len() == y.len() && x.iter().all(|p| y.iter().any(|q| eq(p, q))),
        (V::Dict(x), V::Dict(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| eq(v, w))),
        _ => match (a.num(), b.num()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

fn cmp(a: &V, b: &V, op: &str) -> Result<Option<Ordering>, String> {
    match (a, b) {
        (V::Str(x), V::Str(y)) => Ok(Some(x.cmp(y))),
        (V::List(x), V::List(y)) | (V::Tuple(x), V::Tuple(y)) => {
            for (p, q) in x.iter().zip(y) {
                if !eq(p, q) {
                    return cmp(p, q, op);
                }
            }
            Ok(Some(x.len().cmp(&y.len())))
        }
        _ => match (a.num(), b.num()) {
            (Some(x), Some(y)) => Ok(x.partial_cmp(&y)),
            _ => Err(format!(
                "TypeError: '{op}' not supported between instances of '{}' and '{}'",
                a.type_name(),
                b.type_name()
            )),
        },
    }
}

fn contains(hay: &V, needle: &V) -> Result<bool, String> {
    match hay {
        V::Str(s) => match needle {
            V::Str(n) => Ok(s.contains(n.as_str())),
            other => Err(format!("TypeError: 'in <string>' requires string as left operand, not {}", other.type_name())),
        },
        V::Dict(m) => Ok(matches!(needle, V::Str(k) if m.contains_key(k))),
        V::List(x) | V::Tuple(x) | V::Set(x) => Ok(x.iter().any(|v| eq(v, needle))),
        other => Err(format!("TypeError: argument of type '{}' is not iterable", other.type_name())),
    }
}

// ── the syntax tree ─────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum E {
    Const(V),
    Name(String, (usize, usize)),
    Attr(Box<E>, String, (usize, usize)),
    Index(Box<E>, Box<E>),
    Slice(Box<E>, Option<Box<E>>, Option<Box<E>>, Option<Box<E>>),
    Call(String, Vec<E>),
    Not(Box<E>),
    Neg(Box<E>),
    Pos(Box<E>),
    Bin(String, Box<E>, Box<E>),
    And(Vec<E>),
    Or(Vec<E>),
    Compare(Box<E>, Vec<(String, E)>),
    IfExp(Box<E>, Box<E>, Box<E>),
    List(Vec<E>),
    Tuple(Vec<E>),
    Set(Vec<E>),
    Dict(Vec<(E, E)>),
    /// kind: list | set | gen; the element, then each `for target in iter if ...`.
    Comp(&'static str, Box<E>, Vec<(Target, E, Vec<E>)>),
}

#[derive(Clone, Debug)]
pub enum Target {
    Name(String),
    Tuple(Vec<Target>),
}

pub const HELPERS: &[&str] = &[
    "len", "set", "sorted", "min", "max", "sum", "abs", "any", "all", "round", "count", "dupes", "isfinite", "get",
    "keys", "values", "diff",
];
const CONSTS: &[&str] = &["None", "True", "False"];

// ── tokens ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum T {
    Name(String),
    Int(i64),
    Float(f64),
    Str(String),
    Op(&'static str),
    End,
}

const OPS: &[&str] = &[
    "**", "//", "==", "!=", "<=", ">=", "<<", ">>", ":=", "->", "(", ")", "[", "]", "{", "}", ",", ":", ".", "+", "-", "*",
    "/", "%", "<", ">", "=", "|", "&", "^", "~", "@", ";",
];

fn lex(src: &str) -> Result<Vec<(T, usize, usize)>, String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c.is_ascii_alphabetic() || c == '_' {
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let w = &src[start..i];
            if i < b.len() && (b[i] == b'\'' || b[i] == b'"') && matches!(w.to_ascii_lowercase().as_str(), "f" | "b" | "rb" | "br" | "fr" | "rf") {
                return Err(format!("SyntaxError: {w}-strings are not allowed in a rule"));
            }
            if i < b.len() && (b[i] == b'\'' || b[i] == b'"') && (w == "r" || w == "R") {
                let (s, n) = string(src, i, true)?;
                i = n;
                out.push((T::Str(s), start, i));
                continue;
            }
            out.push((T::Name(w.to_string()), start, i));
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && i + 1 < b.len() && (b[i + 1] as char).is_ascii_digit()) {
            let mut float = false;
            while i < b.len() && ((b[i] as char).is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
            if i < b.len() && b[i] == b'.' {
                float = true;
                i += 1;
                while i < b.len() && (b[i] as char).is_ascii_digit() {
                    i += 1;
                }
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                float = true;
                i += 1;
                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
                while i < b.len() && (b[i] as char).is_ascii_digit() {
                    i += 1;
                }
            }
            let text = src[start..i].replace('_', "");
            let tok = if float {
                T::Float(text.parse().map_err(|_| format!("SyntaxError: bad number {text}"))?)
            } else {
                T::Int(text.parse().map_err(|_| format!("SyntaxError: bad number {text}"))?)
            };
            out.push((tok, start, i));
            continue;
        }
        if c == '\'' || c == '"' {
            let (s, n) = string(src, i, false)?;
            i = n;
            out.push((T::Str(s), start, i));
            continue;
        }
        match OPS.iter().find(|op| src[i..].starts_with(**op)) {
            Some(op) => {
                i += op.len();
                out.push((T::Op(op), start, i));
            }
            None => return Err(format!("SyntaxError: unexpected character {c:?}")),
        }
    }
    out.push((T::End, b.len(), b.len()));
    Ok(out)
}

fn string(src: &str, at: usize, raw: bool) -> Result<(String, usize), String> {
    let q = src.as_bytes()[at] as char;
    let chars: Vec<(usize, char)> = src[at + 1..].char_indices().map(|(k, c)| (k + at + 1, c)).collect();
    let mut s = String::new();
    let mut k = 0;
    while k < chars.len() {
        let (pos, c) = chars[k];
        if c == q {
            return Ok((s, pos + 1));
        }
        if c == '\\' && k + 1 < chars.len() {
            let n = chars[k + 1].1;
            k += 2;
            if raw {
                s.push('\\');
                s.push(n);
                continue;
            }
            match n {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                '0' => s.push('\0'),
                '\\' | '\'' | '"' => s.push(n),
                'u' if k + 4 <= chars.len() => {
                    let hex: String = chars[k..k + 4].iter().map(|x| x.1).collect();
                    let cp = u32::from_str_radix(&hex, 16).map_err(|_| "SyntaxError: bad \\u escape".to_string())?;
                    s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                    k += 4;
                }
                other => {
                    s.push('\\');
                    s.push(other);
                }
            }
            continue;
        }
        s.push(c);
        k += 1;
    }
    Err("SyntaxError: unterminated string literal".into())
}

// ── parser (Python's precedence) ────────────────────────────────────────────

struct P<'a> {
    toks: Vec<(T, usize, usize)>,
    i: usize,
    src: &'a str,
}

impl<'a> P<'a> {
    fn peek(&self) -> &T {
        &self.toks[self.i].0
    }
    fn next(&mut self) -> (T, usize, usize) {
        let t = self.toks[self.i].clone();
        if self.i + 1 < self.toks.len() {
            self.i += 1;
        }
        t
    }
    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), T::Op(o) if *o == op)
    }
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), T::Name(n) if n == kw)
    }
    fn expect_op(&mut self, op: &str) -> Result<(), String> {
        if self.is_op(op) {
            self.next();
            Ok(())
        } else {
            Err(format!("SyntaxError: expected '{op}' at {:?}", self.peek()))
        }
    }

    fn expr(&mut self) -> Result<E, String> {
        if self.is_kw("lambda") {
            return Err("not allowed in a rule: Lambda".into());
        }
        let body = self.or()?;
        if self.is_kw("if") {
            self.next();
            let cond = self.or()?;
            if !self.is_kw("else") {
                return Err("SyntaxError: expected 'else'".into());
            }
            self.next();
            let other = self.expr()?;
            return Ok(E::IfExp(Box::new(cond), Box::new(body), Box::new(other)));
        }
        Ok(body)
    }

    fn or(&mut self) -> Result<E, String> {
        let mut xs = vec![self.and()?];
        while self.is_kw("or") {
            self.next();
            xs.push(self.and()?);
        }
        Ok(if xs.len() == 1 { xs.pop().unwrap() } else { E::Or(xs) })
    }

    fn and(&mut self) -> Result<E, String> {
        let mut xs = vec![self.not()?];
        while self.is_kw("and") {
            self.next();
            xs.push(self.not()?);
        }
        Ok(if xs.len() == 1 { xs.pop().unwrap() } else { E::And(xs) })
    }

    fn not(&mut self) -> Result<E, String> {
        if self.is_kw("not") {
            self.next();
            return Ok(E::Not(Box::new(self.not()?)));
        }
        self.compare()
    }

    fn compare(&mut self) -> Result<E, String> {
        let first = self.arith()?;
        let mut ops = Vec::new();
        loop {
            let op = match self.peek() {
                T::Op(o) if matches!(*o, "==" | "!=" | "<" | "<=" | ">" | ">=") => o.to_string(),
                T::Name(n) if n == "in" => "in".to_string(),
                T::Name(n) if n == "not" => {
                    if matches!(&self.toks[self.i + 1].0, T::Name(m) if m == "in") {
                        self.next();
                        "not in".to_string()
                    } else {
                        break;
                    }
                }
                T::Name(n) if n == "is" => {
                    if matches!(&self.toks[self.i + 1].0, T::Name(m) if m == "not") {
                        self.next();
                        "is not".to_string()
                    } else {
                        "is".to_string()
                    }
                }
                _ => break,
            };
            self.next();
            ops.push((op, self.arith()?));
        }
        Ok(if ops.is_empty() { first } else { E::Compare(Box::new(first), ops) })
    }

    fn arith(&mut self) -> Result<E, String> {
        let mut l = self.term()?;
        while let T::Op(o @ ("+" | "-")) = self.peek().clone() {
            self.next();
            l = E::Bin(o.to_string(), Box::new(l), Box::new(self.term()?));
        }
        if let T::Op(o @ ("|" | "&" | "^" | "<<" | ">>" | "@")) = self.peek() {
            return Err(format!("not allowed in a rule: operator {o}"));
        }
        Ok(l)
    }

    fn term(&mut self) -> Result<E, String> {
        let mut l = self.unary()?;
        while let T::Op(o @ ("*" | "/" | "//" | "%")) = self.peek().clone() {
            self.next();
            l = E::Bin(o.to_string(), Box::new(l), Box::new(self.unary()?));
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<E, String> {
        match self.peek() {
            T::Op("-") => {
                self.next();
                Ok(E::Neg(Box::new(self.unary()?)))
            }
            T::Op("+") => {
                self.next();
                Ok(E::Pos(Box::new(self.unary()?)))
            }
            T::Op("~") => Err("not allowed in a rule: Invert".into()),
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<E, String> {
        let base = self.postfix()?;
        if self.is_op("**") {
            self.next();
            return Ok(E::Bin("**".into(), Box::new(base), Box::new(self.unary()?)));
        }
        Ok(base)
    }

    fn postfix(&mut self) -> Result<E, String> {
        let start = self.toks[self.i].1;
        let mut e = self.atom()?;
        loop {
            if self.is_op(".") {
                self.next();
                let (t, _, end) = self.next();
                let T::Name(n) = t else { return Err("SyntaxError: expected a name after '.'".into()) };
                if n.starts_with('_') {
                    return Err("attributes starting with _ are not allowed".into());
                }
                e = E::Attr(Box::new(e), n, (start, end));
            } else if self.is_op("[") {
                self.next();
                e = self.subscript(e)?;
                self.expect_op("]")?;
            } else if self.is_op("(") {
                let E::Name(f, _) = &e else {
                    return Err(format!("only helper calls are allowed ({})", HELPERS.join(", ")));
                };
                if !HELPERS.contains(&f.as_str()) {
                    return Err(format!("only helper calls are allowed ({})", HELPERS.join(", ")));
                }
                let f = f.clone();
                self.next();
                let mut args = Vec::new();
                while !self.is_op(")") {
                    let a = self.expr()?;
                    if self.is_kw("for") {
                        args.push(self.comp("gen", a)?);
                    } else {
                        args.push(a);
                    }
                    if matches!(self.peek(), T::Op("=")) {
                        return Err("not allowed in a rule: keyword".into());
                    }
                    if !self.is_op(",") {
                        break;
                    }
                    self.next();
                }
                self.expect_op(")")?;
                e = E::Call(f, args);
            } else {
                return Ok(e);
            }
        }
    }

    fn subscript(&mut self, base: E) -> Result<E, String> {
        let part = |p: &mut P| -> Result<Option<Box<E>>, String> {
            if p.is_op(":") || p.is_op("]") {
                Ok(None)
            } else {
                Ok(Some(Box::new(p.expr()?)))
            }
        };
        let lo = part(self)?;
        if !self.is_op(":") {
            return Ok(E::Index(Box::new(base), lo.ok_or("SyntaxError: empty subscript")?));
        }
        self.next();
        let hi = part(self)?;
        let step = if self.is_op(":") {
            self.next();
            part(self)?
        } else {
            None
        };
        Ok(E::Slice(Box::new(base), lo, hi, step))
    }

    fn target(&mut self) -> Result<Target, String> {
        let one = |p: &mut P| -> Result<Target, String> {
            if p.is_op("(") {
                p.next();
                let t = p.target()?;
                p.expect_op(")")?;
                return Ok(t);
            }
            match p.next().0 {
                T::Name(n) => Ok(Target::Name(n)),
                other => Err(format!("SyntaxError: bad loop target {other:?}")),
            }
        };
        let first = one(self)?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let mut xs = vec![first];
        while self.is_op(",") {
            self.next();
            if self.is_kw("in") || self.is_op(")") {
                break;
            }
            xs.push(one(self)?);
        }
        Ok(Target::Tuple(xs))
    }

    fn comp(&mut self, kind: &'static str, elt: E) -> Result<E, String> {
        let mut gens = Vec::new();
        while self.is_kw("for") {
            self.next();
            let t = self.target()?;
            if !self.is_kw("in") {
                return Err("SyntaxError: expected 'in'".into());
            }
            self.next();
            let it = self.or()?;
            let mut ifs = Vec::new();
            while self.is_kw("if") {
                self.next();
                ifs.push(self.or()?);
            }
            gens.push((t, it, ifs));
        }
        Ok(E::Comp(kind, Box::new(elt), gens))
    }

    fn atom(&mut self) -> Result<E, String> {
        let (t, s, e) = self.next();
        match t {
            T::Int(i) => Ok(E::Const(V::Int(i))),
            T::Float(f) => Ok(E::Const(V::Float(f))),
            T::Str(mut x) => {
                while let T::Str(more) = self.peek().clone() {
                    self.next();
                    x.push_str(&more);
                }
                Ok(E::Const(V::Str(x)))
            }
            T::Name(n) => match n.as_str() {
                "None" => Ok(E::Const(V::None)),
                "True" => Ok(E::Const(V::Bool(true))),
                "False" => Ok(E::Const(V::Bool(false))),
                "lambda" => Err("not allowed in a rule: Lambda".into()),
                "for" | "if" | "else" | "in" | "is" | "and" | "or" | "not" | "import" | "yield" | "await" => {
                    Err(format!("SyntaxError: unexpected '{n}'"))
                }
                _ => Ok(E::Name(n, (s, e))),
            },
            T::Op("(") => {
                if self.is_op(")") {
                    self.next();
                    return Ok(E::Tuple(vec![]));
                }
                let first = self.expr()?;
                if self.is_kw("for") {
                    let c = self.comp("gen", first)?;
                    self.expect_op(")")?;
                    return Ok(c);
                }
                if self.is_op(")") {
                    self.next();
                    return Ok(first);
                }
                let mut xs = vec![first];
                while self.is_op(",") {
                    self.next();
                    if self.is_op(")") {
                        break;
                    }
                    xs.push(self.expr()?);
                }
                self.expect_op(")")?;
                Ok(E::Tuple(xs))
            }
            T::Op("[") => {
                if self.is_op("]") {
                    self.next();
                    return Ok(E::List(vec![]));
                }
                let first = self.expr()?;
                if self.is_kw("for") {
                    let c = self.comp("list", first)?;
                    self.expect_op("]")?;
                    return Ok(c);
                }
                let mut xs = vec![first];
                while self.is_op(",") {
                    self.next();
                    if self.is_op("]") {
                        break;
                    }
                    xs.push(self.expr()?);
                }
                self.expect_op("]")?;
                Ok(E::List(xs))
            }
            T::Op("{") => {
                if self.is_op("}") {
                    self.next();
                    return Ok(E::Dict(vec![]));
                }
                let first = self.expr()?;
                if self.is_op(":") {
                    self.next();
                    let v = self.expr()?;
                    if self.is_kw("for") {
                        return Err("not allowed in a rule: DictComp".into());
                    }
                    let mut kv = vec![(first, v)];
                    while self.is_op(",") {
                        self.next();
                        if self.is_op("}") {
                            break;
                        }
                        let k = self.expr()?;
                        self.expect_op(":")?;
                        kv.push((k, self.expr()?));
                    }
                    self.expect_op("}")?;
                    return Ok(E::Dict(kv));
                }
                if self.is_kw("for") {
                    let c = self.comp("set", first)?;
                    self.expect_op("}")?;
                    return Ok(c);
                }
                let mut xs = vec![first];
                while self.is_op(",") {
                    self.next();
                    if self.is_op("}") {
                        break;
                    }
                    xs.push(self.expr()?);
                }
                self.expect_op("}")?;
                Ok(E::Set(xs))
            }
            other => Err(format!("SyntaxError: unexpected {other:?} in {:?}", self.src)),
        }
    }
}

/// Compile a rule expression; anything outside the allowed language is an error.
pub fn compile(src: &str) -> Result<E, String> {
    let toks = lex(src.trim())?;
    let mut p = P { toks, i: 0, src };
    let mut e = p.expr()?;
    if p.is_op(",") {
        // a bare tuple: `a, b`
        let mut xs = vec![e];
        while p.is_op(",") {
            p.next();
            if matches!(p.peek(), T::End) {
                break;
            }
            xs.push(p.expr()?);
        }
        e = E::Tuple(xs);
    }
    if !matches!(p.peek(), T::End) {
        return Err(format!("SyntaxError: unexpected {:?}", p.peek()));
    }
    Ok(e)
}

// ── evaluation ──────────────────────────────────────────────────────────────

/// Names in scope, earlier layers first (runtime.py's `layers`).
pub struct Scope<'a> {
    pub layers: Vec<&'a IndexMap<String, V>>,
    locals: Vec<(String, V)>,
}

impl<'a> Scope<'a> {
    pub fn new(layers: Vec<&'a IndexMap<String, V>>) -> Self {
        Scope { layers, locals: Vec::new() }
    }
    fn lookup(&self, n: &str) -> Result<V, String> {
        if let Some((_, v)) = self.locals.iter().rev().find(|(k, _)| k == n) {
            return Ok(v.clone());
        }
        for l in &self.layers {
            if let Some(v) = l.get(n) {
                return Ok(v.clone());
            }
        }
        if HELPERS.contains(&n) {
            return Err(format!("TypeError: helper {n} used as a value"));
        }
        Err(format!("NameError: name '{n}' is not defined"))
    }
}

fn field(obj: &V, name: &str) -> Result<V, String> {
    match obj {
        V::Dict(m) => m.get(name).cloned().ok_or_else(|| {
            let mut ks: Vec<&String> = m.keys().collect();
            ks.sort();
            let ks: Vec<&str> = ks.iter().take(12).map(|k| k.as_str()).collect();
            format!("NameError: no field '{name}' (has: {})", ks.join(", "))
        }),
        other => {
            let j = dumps(other);
            Err(format!("NameError: can't read .{name} of {} {}", other.type_name(), j.chars().take(60).collect::<String>()))
        }
    }
}

fn index(base: &V, k: &V) -> Result<V, String> {
    match base {
        V::Dict(m) => match k {
            V::Str(s) => m.get(s).cloned().ok_or_else(|| format!("KeyError: '{s}'")),
            other => Err(format!("KeyError: {}", dumps(other))),
        },
        V::List(x) | V::Tuple(x) => {
            let i = k.int().ok_or_else(|| format!("TypeError: indices must be integers, not {}", k.type_name()))?;
            let j = if i < 0 { x.len() as i64 + i } else { i };
            x.get(j as usize).filter(|_| j >= 0).cloned().ok_or_else(|| "IndexError: index out of range".to_string())
        }
        V::Str(s) => {
            let cs: Vec<char> = s.chars().collect();
            let i = k.int().ok_or_else(|| "TypeError: string indices must be integers".to_string())?;
            let j = if i < 0 { cs.len() as i64 + i } else { i };
            cs.get(j as usize).filter(|_| j >= 0).map(|c| V::Str(c.to_string())).ok_or_else(|| "IndexError: string index out of range".to_string())
        }
        other => Err(format!("TypeError: '{}' object is not subscriptable", other.type_name())),
    }
}

fn slice(base: &V, lo: Option<i64>, hi: Option<i64>, step: Option<i64>) -> Result<V, String> {
    let step = step.unwrap_or(1);
    if step == 0 {
        return Err("ValueError: slice step cannot be zero".into());
    }
    let pick = |n: usize| -> Vec<usize> {
        let n = n as i64;
        let norm = |x: i64, dflt: i64, lo_b: i64, hi_b: i64| -> i64 {
            let x = if x < 0 { x + n } else { x };
            let _ = dflt;
            x.clamp(lo_b, hi_b)
        };
        let mut out = Vec::new();
        if step > 0 {
            let a = lo.map(|x| norm(x, 0, 0, n)).unwrap_or(0);
            let b = hi.map(|x| norm(x, n, 0, n)).unwrap_or(n);
            let mut i = a;
            while i < b {
                out.push(i as usize);
                i += step;
            }
        } else {
            let a = lo.map(|x| norm(x, n - 1, -1, n - 1)).unwrap_or(n - 1);
            let b = hi.map(|x| norm(x, -1, -1, n - 1)).unwrap_or(-1);
            let mut i = a;
            while i > b {
                out.push(i as usize);
                i += step;
            }
        }
        out
    };
    match base {
        V::List(x) => Ok(V::List(pick(x.len()).into_iter().map(|i| x[i].clone()).collect())),
        V::Tuple(x) => Ok(V::Tuple(pick(x.len()).into_iter().map(|i| x[i].clone()).collect())),
        V::Str(s) => {
            let cs: Vec<char> = s.chars().collect();
            Ok(V::Str(pick(cs.len()).into_iter().map(|i| cs[i]).collect()))
        }
        other => Err(format!("TypeError: '{}' object is not subscriptable", other.type_name())),
    }
}

fn arith(op: &str, a: &V, b: &V) -> Result<V, String> {
    match (op, a, b) {
        ("+", V::Str(x), V::Str(y)) => return Ok(V::Str(format!("{x}{y}"))),
        ("+", V::List(x), V::List(y)) => return Ok(V::List(x.iter().chain(y).cloned().collect())),
        ("+", V::Tuple(x), V::Tuple(y)) => return Ok(V::Tuple(x.iter().chain(y).cloned().collect())),
        ("*", V::Str(x), n) | ("*", n, V::Str(x)) if n.int().is_some() => {
            return Ok(V::Str(x.repeat(n.int().unwrap().max(0) as usize)))
        }
        ("*", V::List(x), n) | ("*", n, V::List(x)) if n.int().is_some() => {
            let k = n.int().unwrap().max(0) as usize;
            return Ok(V::List((0..k).flat_map(|_| x.clone()).collect()));
        }
        ("-", V::Set(x), V::Set(y)) => return Ok(V::Set(x.iter().filter(|v| !y.iter().any(|w| eq(v, w))).cloned().collect())),
        _ => {}
    }
    let bad = || format!("TypeError: unsupported operand type(s) for {op}: '{}' and '{}'", a.type_name(), b.type_name());
    if let (Some(x), Some(y)) = (a.int(), b.int()) {
        return match op {
            "+" => Ok(x.checked_add(y).map(V::Int).unwrap_or(V::Float(x as f64 + y as f64))),
            "-" => Ok(x.checked_sub(y).map(V::Int).unwrap_or(V::Float(x as f64 - y as f64))),
            "*" => Ok(x.checked_mul(y).map(V::Int).unwrap_or(V::Float(x as f64 * y as f64))),
            "/" if y == 0 => Err("ZeroDivisionError: division by zero".into()),
            "/" => Ok(V::Float(x as f64 / y as f64)),
            "//" if y == 0 => Err("ZeroDivisionError: integer division or modulo by zero".into()),
            "//" => Ok(V::Int(x.div_euclid(y) - if y < 0 && x.rem_euclid(y) != 0 { 1 } else { 0 })),
            "%" if y == 0 => Err("ZeroDivisionError: integer division or modulo by zero".into()),
            "%" => Ok(V::Int(((x % y) + y) % y)),
            "**" if y >= 0 => Ok(x.checked_pow(y.min(u32::MAX as i64) as u32).map(V::Int).unwrap_or(V::Float((x as f64).powf(y as f64)))),
            "**" => Ok(V::Float((x as f64).powf(y as f64))),
            _ => Err(bad()),
        };
    }
    let (Some(x), Some(y)) = (a.num(), b.num()) else { return Err(bad()) };
    match op {
        "+" => Ok(V::Float(x + y)),
        "-" => Ok(V::Float(x - y)),
        "*" => Ok(V::Float(x * y)),
        "/" | "//" | "%" if y == 0.0 => Err("ZeroDivisionError: float division by zero".into()),
        "/" => Ok(V::Float(x / y)),
        "//" => Ok(V::Float((x / y).floor())),
        "%" => Ok(V::Float(x - y * (x / y).floor())),
        "**" => Ok(V::Float(x.powf(y))),
        _ => Err(bad()),
    }
}

fn bind(scope: &mut Scope, t: &Target, v: V) -> Result<usize, String> {
    match t {
        Target::Name(n) => {
            scope.locals.push((n.clone(), v));
            Ok(1)
        }
        Target::Tuple(ts) => {
            let xs = v.items()?;
            if xs.len() != ts.len() {
                return Err(format!("ValueError: expected {} values to unpack, got {}", ts.len(), xs.len()));
            }
            let mut n = 0;
            for (t, x) in ts.iter().zip(xs) {
                n += bind(scope, t, x)?;
            }
            Ok(n)
        }
    }
}

fn comp_rec(scope: &mut Scope, gens: &[(Target, E, Vec<E>)], elt: &E, out: &mut Vec<V>) -> Result<(), String> {
    let Some(((t, it, ifs), rest)) = gens.split_first() else {
        out.push(eval(elt, scope)?);
        return Ok(());
    };
    for x in eval(it, scope)?.items()? {
        let n = bind(scope, t, x)?;
        let mut keep = true;
        for c in ifs {
            if !eval(c, scope)?.truthy() {
                keep = false;
                break;
            }
        }
        let r = if keep { comp_rec(scope, rest, elt, out) } else { Ok(()) };
        scope.locals.truncate(scope.locals.len() - n);
        r?;
    }
    Ok(())
}

fn dedup(xs: Vec<V>) -> Vec<V> {
    let mut out: Vec<V> = Vec::new();
    for x in xs {
        if !out.iter().any(|y| eq(y, &x)) {
            out.push(x);
        }
    }
    out
}

pub fn eval(e: &E, scope: &mut Scope) -> Result<V, String> {
    match e {
        E::Const(v) => Ok(v.clone()),
        E::Name(n, _) => scope.lookup(n),
        E::Attr(b, n, _) => field(&eval(b, scope)?, n),
        E::Index(b, k) => {
            let b = eval(b, scope)?;
            let k = eval(k, scope)?;
            index(&b, &k)
        }
        E::Slice(b, lo, hi, st) => {
            let b = eval(b, scope)?;
            let mut part = |x: &Option<Box<E>>| -> Result<Option<i64>, String> {
                match x {
                    None => Ok(None),
                    Some(x) => match eval(x, scope)? {
                        V::None => Ok(None),
                        v => v.int().map(Some).ok_or_else(|| "TypeError: slice indices must be integers".to_string()),
                    },
                }
            };
            let (lo, hi, st) = (part(lo)?, part(hi)?, part(st)?);
            slice(&b, lo, hi, st)
        }
        E::Call(f, args) => {
            let mut vs = Vec::new();
            for a in args {
                vs.push(eval(a, scope)?);
            }
            helper(f, vs)
        }
        E::Not(x) => Ok(V::Bool(!eval(x, scope)?.truthy())),
        E::Neg(x) => match eval(x, scope)? {
            V::Int(i) => Ok(V::Int(-i)),
            V::Bool(b) => Ok(V::Int(-(b as i64))),
            V::Float(f) => Ok(V::Float(-f)),
            other => Err(format!("TypeError: bad operand type for unary -: '{}'", other.type_name())),
        },
        E::Pos(x) => match eval(x, scope)? {
            v @ (V::Int(_) | V::Float(_)) => Ok(v),
            V::Bool(b) => Ok(V::Int(b as i64)),
            other => Err(format!("TypeError: bad operand type for unary +: '{}'", other.type_name())),
        },
        E::Bin(op, a, b) => {
            let a = eval(a, scope)?;
            let b = eval(b, scope)?;
            arith(op, &a, &b)
        }
        E::And(xs) => {
            let mut last = V::Bool(true);
            for x in xs {
                last = eval(x, scope)?;
                if !last.truthy() {
                    return Ok(last);
                }
            }
            Ok(last)
        }
        E::Or(xs) => {
            let mut last = V::Bool(false);
            for x in xs {
                last = eval(x, scope)?;
                if last.truthy() {
                    return Ok(last);
                }
            }
            Ok(last)
        }
        E::Compare(first, ops) => {
            let mut l = eval(first, scope)?;
            for (op, r) in ops {
                let r = eval(r, scope)?;
                let ok = match op.as_str() {
                    "==" => eq(&l, &r),
                    "!=" => !eq(&l, &r),
                    "in" => contains(&r, &l)?,
                    "not in" => !contains(&r, &l)?,
                    "is" => is(&l, &r),
                    "is not" => !is(&l, &r),
                    o => match cmp(&l, &r, o)? {
                        None => false, // NaN: every ordering is false
                        Some(ord) => match o {
                            "<" => ord == Ordering::Less,
                            "<=" => ord != Ordering::Greater,
                            ">" => ord == Ordering::Greater,
                            _ => ord != Ordering::Less,
                        },
                    },
                };
                if !ok {
                    return Ok(V::Bool(false));
                }
                l = r;
            }
            Ok(V::Bool(true))
        }
        E::IfExp(c, a, b) => {
            if eval(c, scope)?.truthy() {
                eval(a, scope)
            } else {
                eval(b, scope)
            }
        }
        E::List(xs) => Ok(V::List(xs.iter().map(|x| eval(x, scope)).collect::<Result<_, _>>()?)),
        E::Tuple(xs) => Ok(V::Tuple(xs.iter().map(|x| eval(x, scope)).collect::<Result<_, _>>()?)),
        E::Set(xs) => Ok(V::Set(dedup(xs.iter().map(|x| eval(x, scope)).collect::<Result<_, _>>()?))),
        E::Dict(kv) => {
            let mut m = IndexMap::new();
            for (k, v) in kv {
                let k = match eval(k, scope)? {
                    V::Str(s) => s,
                    other => dumps(&other),
                };
                m.insert(k, eval(v, scope)?);
            }
            Ok(V::Dict(m))
        }
        E::Comp(kind, elt, gens) => {
            let mut out = Vec::new();
            comp_rec(scope, gens, elt, &mut out)?;
            Ok(if *kind == "set" { V::Set(dedup(out)) } else { V::List(out) })
        }
    }
}

fn is(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::None, V::None) => true,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Int(x), V::Int(y)) => x == y,
        (V::Str(x), V::Str(y)) => x == y,
        _ => false,
    }
}

fn helper(f: &str, args: Vec<V>) -> Result<V, String> {
    let arity = |n: usize| -> Result<(), String> {
        if args.len() == n {
            Ok(())
        } else {
            Err(format!("TypeError: {f}() takes {n} argument(s) ({} given)", args.len()))
        }
    };
    let many = |args: &[V]| -> Result<Vec<V>, String> {
        if args.len() == 1 {
            args[0].items()
        } else {
            Ok(args.to_vec())
        }
    };
    match f {
        "len" => {
            arity(1)?;
            match &args[0] {
                V::Str(s) => Ok(V::Int(s.chars().count() as i64)),
                V::Dict(m) => Ok(V::Int(m.len() as i64)),
                V::List(x) | V::Tuple(x) | V::Set(x) => Ok(V::Int(x.len() as i64)),
                other => Err(format!("TypeError: object of type '{}' has no len()", other.type_name())),
            }
        }
        "set" => match args.first() {
            None => Ok(V::Set(vec![])),
            Some(x) => Ok(V::Set(dedup(x.items()?))),
        },
        "sorted" => {
            arity(1)?;
            let mut xs = args[0].items()?;
            let mut err = None;
            xs.sort_by(|a, b| match cmp(a, b, "<") {
                Ok(o) => o.unwrap_or(Ordering::Equal),
                Err(e) => {
                    err.get_or_insert(e);
                    Ordering::Equal
                }
            });
            match err {
                Some(e) => Err(e),
                None => Ok(V::List(xs)),
            }
        }
        "min" | "max" => {
            let xs = many(&args)?;
            let mut it = xs.into_iter();
            let mut best = it.next().ok_or_else(|| format!("ValueError: {f}() arg is an empty sequence"))?;
            for x in it {
                let o = cmp(&x, &best, "<")?;
                if (f == "min" && o == Some(Ordering::Less)) || (f == "max" && o == Some(Ordering::Greater)) {
                    best = x;
                }
            }
            Ok(best)
        }
        "sum" => {
            let mut acc = V::Int(0);
            for x in args.first().ok_or("TypeError: sum() takes an iterable")?.items()? {
                acc = arith("+", &acc, &x)?;
            }
            Ok(acc)
        }
        "abs" => {
            arity(1)?;
            match &args[0] {
                V::Int(i) => Ok(V::Int(i.abs())),
                V::Bool(b) => Ok(V::Int(*b as i64)),
                V::Float(x) => Ok(V::Float(x.abs())),
                other => Err(format!("TypeError: bad operand type for abs(): '{}'", other.type_name())),
            }
        }
        "any" => {
            arity(1)?;
            Ok(V::Bool(args[0].items()?.iter().any(V::truthy)))
        }
        "all" => {
            arity(1)?;
            Ok(V::Bool(args[0].items()?.iter().all(V::truthy)))
        }
        "round" => {
            let x = args.first().and_then(V::num).ok_or("TypeError: round() needs a number")?;
            match args.get(1) {
                None => Ok(V::Int(round_half_even(x) as i64)),
                Some(n) => {
                    let n = n.int().ok_or("TypeError: round() digits must be an integer")?;
                    let p = 10f64.powi(n as i32);
                    Ok(V::Float(round_half_even(x * p) / p))
                }
            }
        }
        "count" => {
            arity(2)?;
            Ok(V::Int(args[0].items()?.iter().filter(|x| eq(x, &args[1])).count() as i64))
        }
        "dupes" => {
            arity(1)?;
            let mut seen: Vec<V> = Vec::new();
            let mut out: Vec<V> = Vec::new();
            for x in args[0].items()? {
                if seen.iter().any(|y| eq(y, &x)) && !out.iter().any(|y| eq(y, &x)) {
                    out.push(x.clone());
                }
                seen.push(x);
            }
            Ok(V::List(out))
        }
        "isfinite" => {
            arity(1)?;
            Ok(V::Bool(match &args[0] {
                V::Int(_) | V::Bool(_) => true,
                V::Float(x) => x.is_finite(),
                _ => false,
            }))
        }
        "get" => {
            if args.len() < 2 || args.len() > 3 {
                return Err("TypeError: get(d, k, default=None)".into());
            }
            let dflt = args.get(2).cloned().unwrap_or(V::None);
            match (&args[0], &args[1]) {
                (V::None, _) => Ok(dflt),
                (V::Dict(m), V::Str(k)) => Ok(m.get(k).cloned().unwrap_or(dflt)),
                (V::Dict(_), _) => Ok(dflt),
                (other, _) => Err(format!("TypeError: get() needs a dict, not '{}'", other.type_name())),
            }
        }
        "keys" | "values" => {
            arity(1)?;
            match &args[0] {
                V::None => Ok(V::List(vec![])),
                V::Dict(m) if f == "keys" => Ok(V::List(m.keys().map(|k| V::Str(k.clone())).collect())),
                V::Dict(m) => Ok(V::List(m.values().cloned().collect())),
                other => Err(format!("TypeError: {f}() needs a dict, not '{}'", other.type_name())),
            }
        }
        "diff" => {
            arity(2)?;
            let empty = IndexMap::new();
            let a = match &args[0] {
                V::Dict(m) => m,
                _ => &empty,
            };
            let b = match &args[1] {
                V::Dict(m) => m,
                _ => &empty,
            };
            let mut ks: Vec<&String> = a.keys().chain(b.keys()).collect();
            ks.sort();
            ks.dedup();
            let mut out = IndexMap::new();
            for k in ks {
                let x = a.get(k).cloned().unwrap_or(V::None);
                let y = b.get(k).cloned().unwrap_or(V::None);
                if !eq(&x, &y) {
                    out.insert(k.clone(), V::List(vec![x, y]));
                }
            }
            Ok(V::Dict(out))
        }
        other => Err(format!("only helper calls are allowed, not {other}")),
    }
}

fn round_half_even(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        r
    }
}

// ── names a rule reads (for the explanation of a finding) ───────────────────

/// The source text of each name or outermost attribute chain the expression
/// reads, skipping helpers, comprehension variables, and the root of a chain
/// (runtime.py's `_explain`): `p is None or p.x == 1` shows `p.x`, not `p`.
pub fn read_names(src: &str, e: &E) -> Vec<String> {
    let mut bound = Vec::new();
    collect_bound(e, &mut bound);
    let mut roots = Vec::new();
    collect_roots(e, &mut roots);
    let mut out: Vec<String> = Vec::new();
    walk_names(src.trim(), e, &bound, &roots, &mut out);
    out
}

fn collect_bound(e: &E, out: &mut Vec<String>) {
    fn names(t: &Target, out: &mut Vec<String>) {
        match t {
            Target::Name(n) => out.push(n.clone()),
            Target::Tuple(ts) => ts.iter().for_each(|t| names(t, out)),
        }
    }
    visit(e, &mut |x| {
        if let E::Comp(_, _, gens) = x {
            for (t, _, _) in gens {
                names(t, out);
            }
        }
    });
}

fn collect_roots(e: &E, out: &mut Vec<String>) {
    visit(e, &mut |x| {
        if let E::Attr(..) = x {
            let mut b = x;
            while let E::Attr(inner, _, _) = b {
                b = inner;
            }
            if let E::Name(n, _) = b {
                out.push(n.clone());
            }
        }
    });
}

fn walk_names(src: &str, e: &E, bound: &[String], roots: &[String], out: &mut Vec<String>) {
    let root_name = |x: &E| -> Option<String> {
        let mut b = x;
        while let E::Attr(inner, _, _) = b {
            b = inner;
        }
        match b {
            E::Name(n, _) => Some(n.clone()),
            _ => None,
        }
    };
    let push = |s: String, out: &mut Vec<String>| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    match e {
        E::Name(n, (a, b)) => {
            if !bound.contains(n) && !HELPERS.contains(&n.as_str()) && !CONSTS.contains(&n.as_str()) && !roots.contains(n) {
                push(src.get(*a..*b).unwrap_or(n).to_string(), out);
            }
        }
        E::Attr(inner, _, (a, b)) => {
            match root_name(e) {
                Some(r) if bound.contains(&r) || HELPERS.contains(&r.as_str()) => {}
                Some(_) => push(src.get(*a..*b).unwrap_or("").trim().to_string(), out),
                // `a.b[k].c`: the whole chain, then what it is built from
                None => {
                    push(src.get(*a..*b).unwrap_or("").trim().to_string(), out);
                    walk_names(src, inner, bound, roots, out)
                }
            }
        }
        _ => children(e).into_iter().for_each(|c| walk_names(src, c, bound, roots, out)),
    }
}

fn children(e: &E) -> Vec<&E> {
    match e {
        E::Const(_) | E::Name(..) => vec![],
        E::Attr(b, _, _) => vec![b],
        E::Index(a, b) => vec![a, b],
        E::Slice(b, x, y, z) => {
            let mut v: Vec<&E> = vec![b];
            v.extend([x, y, z].into_iter().flatten().map(|x| x.as_ref()));
            v
        }
        E::Call(_, xs) | E::And(xs) | E::Or(xs) | E::List(xs) | E::Tuple(xs) | E::Set(xs) => xs.iter().collect(),
        E::Not(x) | E::Neg(x) | E::Pos(x) => vec![x],
        E::Bin(_, a, b) => vec![a, b],
        E::Compare(a, ops) => std::iter::once(a.as_ref()).chain(ops.iter().map(|(_, x)| x)).collect(),
        E::IfExp(a, b, c) => vec![a, b, c],
        E::Dict(kv) => kv.iter().flat_map(|(k, v)| [k, v]).collect(),
        E::Comp(_, elt, gens) => {
            let mut v: Vec<&E> = vec![elt];
            for (_, it, ifs) in gens {
                v.push(it);
                v.extend(ifs.iter());
            }
            v
        }
    }
}

fn visit<'a>(e: &'a E, f: &mut impl FnMut(&'a E)) {
    f(e);
    for c in children(e) {
        visit(c, f);
    }
}

// ── Python's json.dumps(sort_keys=True), for messages and subjects ──────────

pub fn dumps(v: &V) -> String {
    let mut s = String::new();
    dump(v, &mut s);
    s
}

fn dump_str(x: &str, out: &mut String) {
    out.push('"');
    for c in x.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
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

pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if f == f.trunc() && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

fn dump(v: &V, out: &mut String) {
    match v {
        V::None => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Int(i) => out.push_str(&i.to_string()),
        V::Float(f) => out.push_str(&float_repr(*f)),
        V::Str(s) => dump_str(s, out),
        V::List(xs) | V::Tuple(xs) | V::Set(xs) => {
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
