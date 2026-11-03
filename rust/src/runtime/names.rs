//! The names a rule's assertion reads, for the explanation of a finding
//! (runtime.py's `_explain`), in the order Python's `ast.walk` meets them.

use std::collections::VecDeque;

use super::expr::{Target, E, CONSTS, HELPERS};

/// A node of Python's tree for this expression: one of ours, or one Python
/// has between ours (a subscript's Slice, a comprehension's `for` clause).
enum Q<'a> {
    N(&'a E),
    G(Vec<Q<'a>>),
}

/// The children of a node in Python's field order, its in-between nodes kept.
fn kids(e: &E) -> Vec<Q<'_>> {
    use Q::{G, N};
    match e {
        E::Const(_) | E::Name(..) => vec![],
        E::Attr(b, _, _) => vec![N(b)],
        E::Index(a, b) => vec![N(a), N(b)],
        E::Slice(b, x, y, z) => vec![N(b), G([x, y, z].into_iter().flatten().map(|x| N(x.as_ref())).collect())],
        E::Call(_, xs) | E::And(xs) | E::Or(xs) | E::List(xs) | E::Tuple(xs) | E::Set(xs) | E::Refused(_, xs) => {
            xs.iter().map(N).collect()
        }
        E::Not(x) | E::Neg(x) | E::Pos(x) => vec![N(x)],
        E::Bin(_, a, b) => vec![N(a), N(b)],
        E::Compare(a, ops) => std::iter::once(N(a.as_ref())).chain(ops.iter().map(|(_, x)| N(x))).collect(),
        E::IfExp(a, b, c) => vec![N(a), N(b), N(c)],
        E::Dict(kv) => kv.iter().map(|(k, _)| N(k)).chain(kv.iter().map(|(_, v)| N(v))).collect(),
        E::Comp(_, elt, gens) => {
            let mut v = vec![N(elt.as_ref())];
            for (_, it, ifs) in gens {
                v.push(G(std::iter::once(N(it)).chain(ifs.iter().map(N)).collect()));
            }
            v
        }
    }
}

/// Every node, breadth first, as `ast.walk` yields them.
pub fn walk(e: &E) -> Vec<&E> {
    let mut out = Vec::new();
    let mut q: VecDeque<Q> = VecDeque::from([Q::N(e)]);
    while let Some(x) = q.pop_front() {
        match x {
            Q::N(n) => {
                out.push(n);
                q.extend(kids(n));
            }
            Q::G(v) => q.extend(v),
        }
    }
    out
}

fn root(e: &E) -> &E {
    let mut b = e;
    while let E::Attr(inner, _, _) = b {
        b = inner;
    }
    b
}

fn target_names(t: &Target, out: &mut Vec<String>) {
    match t {
        Target::Name(n) => out.push(n.clone()),
        Target::Tuple(ts) => ts.iter().for_each(|t| target_names(t, out)),
    }
}

/// `ast.unparse` of a name or an attribute chain on one (`a.b.c`); other
/// chains as written.
fn unparse(src: &str, e: &E) -> String {
    match e {
        E::Name(n, _) => n.clone(),
        E::Attr(inner, a, (s, t)) => match root(e) {
            E::Name(..) => format!("{}.{a}", unparse(src, inner)),
            _ => src.get(*s..*t).unwrap_or("").trim().to_string(),
        },
        _ => String::new(),
    }
}

/// The source text of each name or outermost attribute chain the expression
/// reads, skipping helpers, comprehension variables, and the root of a chain:
/// `p is None or p.x == 1` shows `p.x`, not `p`.
pub fn read_names(src: &str, e: &E) -> Vec<String> {
    let src = src.trim();
    let nodes = walk(e);
    let mut bound = Vec::new();
    let mut roots = Vec::new();
    let mut inner: Vec<*const E> = Vec::new();
    for n in &nodes {
        match n {
            E::Comp(_, _, gens) => gens.iter().for_each(|(t, _, _)| target_names(t, &mut bound)),
            E::Attr(v, _, _) => {
                inner.push(v.as_ref() as *const E);
                if let E::Name(r, _) = root(n) {
                    roots.push(r.clone());
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<String> = Vec::new();
    for n in nodes {
        if inner.contains(&(n as *const E)) || !matches!(n, E::Name(..) | E::Attr(..)) {
            continue;
        }
        if let E::Name(r, _) = root(n) {
            if bound.contains(r) || HELPERS.contains(&r.as_str()) || CONSTS.contains(&r.as_str()) {
                continue;
            }
        }
        if let E::Name(r, _) = n {
            if roots.contains(r) {
                continue;
            }
        }
        let s = unparse(src, n);
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}
