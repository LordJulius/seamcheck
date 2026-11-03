//! The rule expressions' syntax (runtime.py's `compile_expr`): Python's
//! tokens and precedence for the small expression language `expr` runs, and
//! the refusals Python's AST allowlist makes, in the order it makes them.

use super::expr::{Target, E, HELPERS, V};
use super::pysyntax;

// ── tokens ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum T {
    Name(String),
    Int(i64),
    Float(f64),
    Str(String),
    /// an f-string (or a t-string): Python's JoinedStr, refused
    FStr,
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
            let lw = w.to_ascii_lowercase();
            if i < b.len() && (b[i] == b'\'' || b[i] == b'"') && matches!(lw.as_str(), "r" | "u" | "b" | "rb" | "br" | "f" | "fr" | "rf" | "t" | "tr" | "rt") {
                // (a bytes literal reads as its text)
                let (s, n) = string(src, i, lw.contains('r'))?;
                i = n;
                out.push((if lw.contains('f') || lw.contains('t') { T::FStr } else { T::Str(s) }, start, i));
                continue;
            }
            out.push((T::Name(w.to_string()), start, i));
            continue;
        }
        if c == '0' && i + 1 < b.len() && matches!(b[i + 1], b'x' | b'X' | b'o' | b'O' | b'b' | b'B') {
            let radix = match b[i + 1] {
                b'x' | b'X' => 16,
                b'o' | b'O' => 8,
                _ => 2,
            };
            i += 2;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let text = src[start + 2..i].replace('_', "");
            let n = i64::from_str_radix(&text, radix).map_err(|_| format!("SyntaxError: bad number {text}"))?;
            out.push((T::Int(n), start, i));
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
    let triple = src[at..].starts_with(&q.to_string().repeat(3));
    let open = if triple { 3 } else { 1 };
    let chars: Vec<(usize, char)> = src[at + open..].char_indices().map(|(k, c)| (k + at + open, c)).collect();
    let mut s = String::new();
    let mut k = 0;
    while k < chars.len() {
        let (pos, c) = chars[k];
        if c == q && (!triple || src[pos..].starts_with(&q.to_string().repeat(3))) {
            return Ok((s, pos + open));
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
            return Ok(());
        }
        // two expressions side by side inside brackets: Python asks about a comma
        if matches!(op, ")" | "]" | "}") && self.i > 0 {
            let prev = &self.toks[self.i - 1].0;
            let starts = match self.peek() {
                T::Name(n) => !KEYWORDS.contains(&n.as_str()) || matches!(n.as_str(), "None" | "True" | "False" | "lambda" | "not" | "await"),
                T::Int(_) | T::Float(_) | T::Str(_) | T::FStr => true,
                T::Op(o) => matches!(*o, "(" | "[" | "{" | "~"),
                T::End => false,
            };
            if starts {
                if matches!(prev, T::Name(n) if n == "print" || n == "exec") {
                    let T::Name(n) = prev else { unreachable!() };
                    return Err(format!("SyntaxError: Missing parentheses in call to '{n}'. Did you mean {n}(...)?"));
                }
                let soft = matches!(self.peek(), T::Name(n) if matches!(n.as_str(), "match" | "case" | "type" | "_"));
                if !(matches!(prev, T::Name(_)) && matches!(self.peek(), T::Str(_) | T::FStr)) && !soft {
                    return Err("SyntaxError: invalid syntax. Perhaps you forgot a comma?".into());
                }
            }
        }
        Err(format!("SyntaxError: expected '{op}' at {:?}", self.peek()))
    }

    /// One element of a bracket: `*x` (Starred) and `y := v` (NamedExpr)
    /// parse there, and are refused.
    fn elt(&mut self) -> Result<E, String> {
        if self.is_op("*") {
            self.next();
            let v = self.bor()?;
            return Ok(E::Refused("not allowed in a rule: Starred".into(), vec![v]));
        }
        let e = self.expr()?;
        if self.is_op(":=") && matches!(e, E::Name(..)) {
            self.next();
            let v = self.expr()?;
            return Ok(E::Refused("not allowed in a rule: NamedExpr".into(), vec![e, v]));
        }
        Ok(e)
    }

    /// `lambda params: body` (the keyword just seen): refused, its body kept.
    fn lambda(&mut self) -> Result<E, String> {
        let mut depth = 0;
        loop {
            match self.next().0 {
                T::Op(":") if depth == 0 => break,
                T::Op("(" | "[" | "{") => depth += 1,
                T::Op(")" | "]" | "}") => depth -= 1,
                T::End => return Err("SyntaxError: invalid syntax".into()),
                _ => {}
            }
        }
        let body = self.expr()?;
        Ok(E::Refused("not allowed in a rule: Lambda".into(), vec![body]))
    }

    fn expr(&mut self) -> Result<E, String> {
        if self.is_kw("lambda") {
            self.next();
            return self.lambda();
        }
        let body = self.or()?;
        if self.is_kw("if") {
            self.next();
            let cond = self.or()?;
            if !self.is_kw("else") {
                return Err("SyntaxError: expected 'else' after 'if' expression".into());
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
        let first = self.bor()?;
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
            ops.push((op, self.bor()?));
        }
        Ok(if ops.is_empty() { first } else { E::Compare(Box::new(first), ops) })
    }

    /// A binary operator Python parses and the allowlist refuses: the BinOp
    /// is allowed, its operator child is not (after the left operand).
    fn refused_bin(l: E, name: &str, r: E) -> E {
        E::Refused(String::new(), vec![l, E::Refused(format!("not allowed in a rule: {name}"), vec![]), r])
    }

    fn bor(&mut self) -> Result<E, String> {
        let mut l = self.bxor()?;
        while self.is_op("|") {
            self.next();
            l = Self::refused_bin(l, "BitOr", self.bxor()?);
        }
        Ok(l)
    }

    fn bxor(&mut self) -> Result<E, String> {
        let mut l = self.band()?;
        while self.is_op("^") {
            self.next();
            l = Self::refused_bin(l, "BitXor", self.band()?);
        }
        Ok(l)
    }

    fn band(&mut self) -> Result<E, String> {
        let mut l = self.shift()?;
        while self.is_op("&") {
            self.next();
            l = Self::refused_bin(l, "BitAnd", self.shift()?);
        }
        Ok(l)
    }

    fn shift(&mut self) -> Result<E, String> {
        let mut l = self.arith()?;
        while let T::Op(o @ ("<<" | ">>")) = self.peek().clone() {
            self.next();
            l = Self::refused_bin(l, if o == "<<" { "LShift" } else { "RShift" }, self.arith()?);
        }
        Ok(l)
    }

    fn arith(&mut self) -> Result<E, String> {
        let mut l = self.term()?;
        while let T::Op(o @ ("+" | "-")) = self.peek().clone() {
            self.next();
            l = E::Bin(o.to_string(), Box::new(l), Box::new(self.term()?));
        }
        Ok(l)
    }

    fn term(&mut self) -> Result<E, String> {
        let mut l = self.unary()?;
        while let T::Op(o @ ("*" | "/" | "//" | "%" | "@")) = self.peek().clone() {
            self.next();
            let r = self.unary()?;
            l = if o == "@" { Self::refused_bin(l, "MatMult", r) } else { E::Bin(o.to_string(), Box::new(l), Box::new(r)) };
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
            T::Op("~") => {
                self.next();
                let v = self.unary()?;
                Ok(E::Refused(String::new(), vec![E::Refused("not allowed in a rule: Invert".into(), vec![]), v]))
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<E, String> {
        let base = if self.is_kw("await") {
            self.next();
            E::Refused("not allowed in a rule: Await".into(), vec![self.postfix()?])
        } else {
            self.postfix()?
        };
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
                e = if n.starts_with('_') {
                    E::Refused("attributes starting with _ are not allowed".into(), vec![e])
                } else {
                    E::Attr(Box::new(e), n, (start, end))
                };
            } else if self.is_op("[") {
                self.next();
                e = self.subscript(e)?;
                self.expect_op("]")?;
            } else if self.is_op("(") {
                self.next();
                let (mut args, mut kws, mut plain) = (Vec::new(), Vec::new(), true);
                while !self.is_op(")") {
                    if self.is_op("**") {
                        self.next();
                        kws.push(E::Refused("not allowed in a rule: keyword".into(), vec![self.expr()?]));
                    } else {
                        let a = self.elt()?;
                        if self.is_op("=") && matches!(a, E::Name(..)) {
                            self.next();
                            kws.push(E::Refused("not allowed in a rule: keyword".into(), vec![self.expr()?]));
                        } else if self.is_kw("for") {
                            args.push(self.comp("gen", a)?);
                        } else {
                            plain &= !matches!(a, E::Refused(..));
                            args.push(a);
                        }
                    }
                    if !self.is_op(",") {
                        break;
                    }
                    self.next();
                }
                self.expect_op(")")?;
                let helper = matches!(&e, E::Name(f, _) if HELPERS.contains(&f.as_str()));
                e = match &e {
                    E::Name(f, _) if helper && kws.is_empty() && plain => E::Call(f.clone(), args),
                    // the Call: refused unless a helper's; its func, args and keywords after it
                    _ => {
                        let msg = if helper { String::new() } else { format!("only helper calls are allowed ({})", HELPERS.join(", ")) };
                        let mut kids = vec![e];
                        kids.extend(args);
                        kids.extend(kws);
                        E::Refused(msg, kids)
                    }
                };
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
                let mut joined = false;
                loop {
                    match self.peek().clone() {
                        T::Str(more) => x.push_str(&more),
                        T::FStr => joined = true,
                        _ => break,
                    }
                    self.next();
                }
                Ok(if joined { Self::fstring() } else { E::Const(V::Str(x)) })
            }
            T::FStr => {
                while matches!(self.peek(), T::Str(_) | T::FStr) {
                    self.next();
                }
                Ok(Self::fstring())
            }
            T::Name(n) => match n.as_str() {
                "None" => Ok(E::Const(V::None)),
                "True" => Ok(E::Const(V::Bool(true))),
                "False" => Ok(E::Const(V::Bool(false))),
                "lambda" => self.lambda(),
                k if KEYWORDS.contains(&k) => Err(format!("SyntaxError: unexpected '{n}'")),
                _ => Ok(E::Name(n, (s, e))),
            },
            T::Op("(") => {
                if self.is_op(")") {
                    self.next();
                    return Ok(E::Tuple(vec![]));
                }
                let first = self.elt()?;
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
                    xs.push(self.elt()?);
                }
                self.expect_op(")")?;
                Ok(E::Tuple(xs))
            }
            T::Op("[") => {
                if self.is_op("]") {
                    self.next();
                    return Ok(E::List(vec![]));
                }
                let first = self.elt()?;
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
                    xs.push(self.elt()?);
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
                        let E::Comp(_, _, gens) = self.comp("set", E::Const(V::None))? else { unreachable!() };
                        let mut kids = vec![first, v];
                        for (_, it, ifs) in gens {
                            kids.push(it);
                            kids.extend(ifs);
                        }
                        self.expect_op("}")?;
                        return Ok(E::Refused("not allowed in a rule: DictComp".into(), kids));
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

/// Python's keywords: never a name.
const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or",
    "pass", "raise", "return", "try", "while", "with", "yield",
];

impl P<'_> {
    fn fstring() -> E {
        E::Refused("not allowed in a rule: JoinedStr".into(), vec![])
    }
}

fn parse(src: &str) -> Result<E, (String, usize)> {
    let toks = lex(src).map_err(|e| (e, src.len()))?;
    let mut p = P { toks, i: 0, src };
    let at = |p: &P| p.toks[p.i].1;
    let mut e = p.expr().map_err(|m| (m, at(&p)))?;
    if p.is_op(",") {
        // a bare tuple: `a, b`
        let mut xs = vec![e];
        while p.is_op(",") {
            p.next();
            if matches!(p.peek(), T::End) {
                break;
            }
            xs.push(p.elt().map_err(|m| (m, at(&p)))?);
        }
        e = E::Tuple(xs);
    }
    if !matches!(p.peek(), T::End) {
        return Err((format!("SyntaxError: unexpected {:?}", p.peek()), at(&p)));
    }
    Ok(e)
}

/// Compile a rule expression (runtime.py's `compile_expr`): a SyntaxError in
/// Python's words, or the first node Python's allowlist refuses, breadth
/// first as `ast.walk` meets them.
pub fn compile(src: &str) -> Result<E, String> {
    let src = crate::pyre::strip(src);
    if let Some(e) = pysyntax::tokenizer_error(src) {
        return Err(e);
    }
    let syntax = |m: String, pos: usize| -> String {
        let msg = m.strip_prefix("SyntaxError: ").unwrap_or(&m);
        let msg = if msg.starts_with("invalid syntax") || msg.starts_with("expected 'else' after") || msg.starts_with("Missing parentheses") {
            msg
        } else {
            "invalid syntax"
        };
        pysyntax::message(msg, pysyntax::line_at(src, pos))
    };
    // (Python's eval input ends at a newline outside brackets: what follows is an error)
    let cut = pysyntax::top_level_newline(src);
    let e = parse(&src[..cut.unwrap_or(src.len())]).map_err(|(m, pos)| syntax(m, pos))?;
    if let Some(nl) = cut {
        let next = nl + src[nl..].len() - src[nl..].trim_start().len();
        return Err(syntax("SyntaxError: invalid syntax".into(), next));
    }
    for n in super::names::walk(&e) {
        if let E::Refused(m, _) = n {
            if !m.is_empty() {
                return Err(m.clone());
            }
        }
    }
    Ok(e)
}
