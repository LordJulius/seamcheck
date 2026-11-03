//! Python's `json.loads` (CPython 3.13+'s scanner): the same values (bare
//! `NaN` / `Infinity`, a repeated key's last value in its first place) and
//! the same error text, `Expecting value: line 1 column 1 (char 0)`, since a
//! runtime finding quotes it. Positions count characters, as Python does.

use indexmap::IndexMap;

use super::expr::V;

struct P {
    s: Vec<char>,
}

type R<T> = Result<T, (String, usize)>;

/// What `scan_once` raises when no value starts at a position.
const STOP: &str = "\0stop";

fn ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl P {
    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn skip(&self, mut i: usize) -> usize {
        while self.at(i).is_some_and(ws) {
            i += 1;
        }
        i
    }

    fn word(&self, i: usize, w: &str) -> bool {
        w.chars().enumerate().all(|(k, c)| self.at(i + k) == Some(c))
    }

    fn scan(&self, i: usize) -> R<(V, usize)> {
        let Some(c) = self.at(i) else { return Err((STOP.into(), i)) };
        match c {
            '"' => self.string(i + 1).map(|(s, e)| (V::Str(s), e)),
            '{' => self.object(i + 1),
            '[' => self.array(i + 1),
            'n' if self.word(i, "null") => Ok((V::None, i + 4)),
            't' if self.word(i, "true") => Ok((V::Bool(true), i + 4)),
            'f' if self.word(i, "false") => Ok((V::Bool(false), i + 5)),
            'N' if self.word(i, "NaN") => Ok((V::Float(f64::NAN), i + 3)),
            'I' if self.word(i, "Infinity") => Ok((V::Float(f64::INFINITY), i + 8)),
            '-' if self.word(i, "-Infinity") => Ok((V::Float(f64::NEG_INFINITY), i + 9)),
            _ => self.number(i),
        }
    }

    fn number(&self, start: usize) -> R<(V, usize)> {
        let digit = |i: usize| self.at(i).is_some_and(|c| c.is_ascii_digit());
        let mut i = start;
        if self.at(i) == Some('-') {
            i += 1;
        }
        match self.at(i) {
            Some('0') => i += 1,
            Some('1'..='9') => {
                while digit(i) {
                    i += 1;
                }
            }
            _ => return Err((STOP.into(), start)),
        }
        let mut float = false;
        if self.at(i) == Some('.') && digit(i + 1) {
            i += 1;
            while digit(i) {
                i += 1;
            }
            float = true;
        }
        if matches!(self.at(i), Some('e' | 'E')) {
            let mut j = i + 1;
            if matches!(self.at(j), Some('+' | '-')) {
                j += 1;
            }
            if digit(j) {
                while digit(j) {
                    j += 1;
                }
                i = j;
                float = true;
            }
        }
        let text: String = self.s[start..i].iter().collect();
        let v = if float {
            V::Float(text.parse().unwrap_or(f64::NAN))
        } else {
            // (past i64 Python keeps an exact int; here it becomes the nearest float)
            text.parse::<i64>().map(V::Int).unwrap_or_else(|_| V::Float(text.parse().unwrap_or(f64::NAN)))
        };
        Ok((v, i))
    }

    /// The string whose opening quote is just before `end`.
    fn string(&self, mut end: usize) -> R<(String, usize)> {
        let begin = end - 1;
        let len = self.s.len();
        let mut out = String::new();
        loop {
            let mut next = end;
            while next < len {
                let d = self.s[next];
                if d == '"' || d == '\\' {
                    break;
                }
                if (d as u32) <= 0x1f {
                    return Err(("Invalid control character at".into(), next));
                }
                next += 1;
            }
            if next >= len {
                return Err(("Unterminated string starting at".into(), begin));
            }
            out.extend(&self.s[end..next]);
            if self.s[next] == '"' {
                return Ok((out, next + 1));
            }
            next += 1; // the backslash
            if next == len {
                return Err(("Unterminated string starting at".into(), begin));
            }
            let c = self.s[next];
            if c != 'u' {
                end = next + 1;
                out.push(match c {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\x08',
                    'f' => '\x0c',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(("Invalid \\escape".into(), end - 2)),
                });
                continue;
            }
            next += 1;
            end = next + 4;
            if end >= len {
                return Err(("Invalid \\uXXXX escape".into(), next - 1));
            }
            let hex = |from: usize, to: usize| -> Option<u32> {
                self.s[from..to].iter().try_fold(0u32, |acc, d| d.to_digit(16).map(|x| acc * 16 + x))
            };
            let Some(mut u) = hex(next, end) else { return Err(("Invalid \\uXXXX escape".into(), end - 5)) };
            if (0xd800..0xdc00).contains(&u) && end + 6 < len && self.s[end] == '\\' && self.s[end + 1] == 'u' {
                match hex(end + 2, end + 6) {
                    None => return Err(("Invalid \\uXXXX escape".into(), end + 1)),
                    Some(lo) if (0xdc00..0xe000).contains(&lo) => {
                        u = 0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00);
                        end += 6;
                    }
                    Some(_) => {}
                }
            }
            // (a lone surrogate Python keeps as it is; a Rust string can't hold one)
            out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
        }
    }

    fn object(&self, i: usize) -> R<(V, usize)> {
        let mut m: IndexMap<String, V> = IndexMap::new();
        let mut i = self.skip(i);
        if self.at(i) == Some('}') {
            return Ok((V::Dict(m), i + 1));
        }
        loop {
            if self.at(i) != Some('"') {
                return Err(("Expecting property name enclosed in double quotes".into(), i));
            }
            let (k, e) = self.string(i + 1)?;
            i = self.skip(e);
            if self.at(i) != Some(':') {
                return Err(("Expecting ':' delimiter".into(), i));
            }
            i = self.skip(i + 1);
            let (v, e) = self.scan(i)?;
            m.insert(k, v);
            i = self.skip(e);
            if self.at(i) == Some('}') {
                return Ok((V::Dict(m), i + 1));
            }
            if self.at(i) != Some(',') {
                return Err(("Expecting ',' delimiter".into(), i));
            }
            let comma = i;
            i = self.skip(i + 1);
            if self.at(i) == Some('}') {
                return Err(("Illegal trailing comma before end of object".into(), comma));
            }
        }
    }

    fn array(&self, i: usize) -> R<(V, usize)> {
        let mut xs = Vec::new();
        let mut i = self.skip(i);
        if self.at(i) == Some(']') {
            return Ok((V::List(xs), i + 1));
        }
        loop {
            let (v, e) = self.scan(i)?;
            xs.push(v);
            i = self.skip(e);
            if self.at(i) == Some(']') {
                return Ok((V::List(xs), i + 1));
            }
            if self.at(i) != Some(',') {
                return Err(("Expecting ',' delimiter".into(), i));
            }
            let comma = i;
            i = self.skip(i + 1);
            if self.at(i) == Some(']') {
                return Err(("Illegal trailing comma before end of array".into(), comma));
            }
        }
    }

    fn err(&self, msg: &str, pos: usize) -> String {
        let line = self.s[..pos].iter().filter(|c| **c == '\n').count() + 1;
        let col = match self.s[..pos].iter().rposition(|c| *c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        format!("{msg}: line {line} column {col} (char {pos})")
    }
}

/// `json.loads(text)`: the value, or the text of Python's JSONDecodeError.
pub fn loads(text: &str) -> Result<V, String> {
    let p = P { s: text.chars().collect() };
    if p.at(0) == Some('\u{feff}') {
        return Err(p.err("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    let start = p.skip(0);
    let (v, end) = match p.scan(start) {
        Ok(x) => x,
        Err((m, pos)) if m == STOP => return Err(p.err("Expecting value", pos)),
        Err((m, pos)) => return Err(p.err(&m, pos)),
    };
    let end = p.skip(end);
    if end != p.s.len() {
        return Err(p.err("Extra data", end));
    }
    Ok(v)
}
