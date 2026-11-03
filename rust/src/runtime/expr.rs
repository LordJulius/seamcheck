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
use super::pyfmt::dumps;

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
    /// What Python's AST allowlist refuses (the message), or with no message
    /// a node it allows over one it refuses; its children in Python's order.
    /// Only in a tree `compile` rejects: never evaluated.
    Refused(String, Vec<E>),
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
pub(super) const CONSTS: &[&str] = &["None", "True", "False"];

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
            let kind = if matches!(base, V::Tuple(_)) { "tuple" } else { "list" };
            x.get(j as usize).filter(|_| j >= 0).cloned().ok_or_else(|| format!("IndexError: {kind} index out of range"))
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
        E::Refused(m, _) => Err(m.clone()),
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
        // (runtime.py's set() holds a dict or a list as its JSON text)
        "set" => match args.first() {
            None => Ok(V::Set(vec![])),
            Some(x) => Ok(V::Set(dedup(
                x.items()?.into_iter().map(|v| if matches!(v, V::Dict(_) | V::List(_)) { V::Str(dumps(&v)) } else { v }).collect(),
            ))),
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
            let mut best = it.next().ok_or_else(|| format!("ValueError: {f}() iterable argument is empty"))?;
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
                    if let (V::Int(_) | V::Bool(_), true) = (&args[0], n >= 0) {
                        return Ok(V::Int(x as i64)); // (an int rounded to digits stays that int)
                    }
                    if n >= 0 && x.is_finite() {
                        // the correctly rounded decimal, as Python's round does (2.675 -> 2.67)
                        return Ok(V::Float(format!("{:.*}", n as usize, x).parse().unwrap_or(x)));
                    }
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
            // seen by JSON text (1 and 1.0 differ there), reported once by ==
            let mut seen: std::collections::HashSet<String> = Default::default();
            let mut out: Vec<V> = Vec::new();
            for x in args[0].items()? {
                let k = dumps(&x);
                if seen.contains(&k) && !out.iter().any(|y| eq(y, &x)) {
                    out.push(x.clone());
                }
                seen.insert(k);
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
