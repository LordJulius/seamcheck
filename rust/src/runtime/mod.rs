//! Runtime checks (runtime.py): judge what a running program reports about
//! itself. The program prints `SEAMCHECK {json}` lines with a kind `k`:
//! `state` (a snapshot), `act` (one action with `before` / `after`), `end`
//! (finished cleanly) and `waive` (rules a test breaks on purpose). The
//! project's `[[invariant]]` rules are checked on every state line and its
//! `[[contract]]` rules on every act line whose name matches; see the README's
//! "Runtime checks". A run never passes by saying nothing: no state line, no
//! end line, or a rule that can't be evaluated are findings.
//!
//! `seamcheck verify` writes its evidence in this protocol and judges it here.

pub mod cmd;
pub mod expr;
pub mod names;
pub mod parse;
pub mod pyfmt;
pub mod pyjson;
pub mod pysyntax;

use indexmap::IndexMap;
use serde_json::Value as Json;

use crate::glob::fnmatchcase;
use crate::pyre::Rx;
use expr::{eval, Scope, E, V};
use names::read_names;
use parse::compile;
use pyfmt::{dumps, fmt_g, py_repr, py_str};

#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    pub rule: String,
    pub kind: String, // invariant | contract | run
    pub severity: String,
    pub t: f64,
    pub subject: String,
    pub message: String,
    pub line: usize,
    pub count: usize,
}

struct Rule {
    spec: IndexMap<String, Json>,
    kind: &'static str,
    code: Option<E>,
    when: Option<E>,
    where_: Option<E>,
    each: Option<E>,
    evaluated: usize,
    error: String,
    waived: String,
}

impl Rule {
    fn id(&self) -> String {
        self.spec.get("id").and_then(Json::as_str).unwrap_or("?").to_string()
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.spec.get(k).and_then(Json::as_str)
    }
    fn bad(&self) -> bool {
        self.error.starts_with("bad rule")
    }
}

/// One run's judge. Feed it lines (`feed_line`) or objects (`feed`), then `finish`.
pub struct Checker {
    rules: Vec<Rule>,
    violations: IndexMap<(String, String), Violation>,
    prev: Option<IndexMap<String, V>>,
    pub states: usize,
    pub acts: usize,
    pub ended: bool,
    lineno: usize,
    since: IndexMap<(String, String), f64>,
    pub prefix: String,
    fail_on: Vec<Rx>,
    strict: bool,
}

fn table(j: &Json) -> IndexMap<String, Json> {
    j.as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
}

fn layer(pairs: Vec<(&str, V)>) -> IndexMap<String, V> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn as_dict(v: &V) -> IndexMap<String, V> {
    match v {
        V::Dict(m) => m.clone(),
        _ => IndexMap::new(),
    }
}

fn opt(v: &Option<IndexMap<String, V>>) -> V {
    v.as_ref().map(|m| V::Dict(m.clone())).unwrap_or(V::None)
}

impl Checker {
    /// `config` is the merged project config as JSON: `runtime`, `invariant`, `contract`.
    pub fn new(config: &Json, strict: bool) -> Self {
        let rt = config.get("runtime").cloned().unwrap_or(Json::Null);
        let prefix = rt.get("prefix").and_then(Json::as_str).unwrap_or("SEAMCHECK").to_string();
        let mut c = Checker {
            rules: Vec::new(),
            violations: IndexMap::new(),
            prev: None,
            states: 0,
            acts: 0,
            ended: false,
            lineno: 0,
            since: IndexMap::new(),
            prefix,
            fail_on: Vec::new(),
            strict,
        };
        for pat in rt.get("fail_on").and_then(Json::as_array).into_iter().flatten() {
            let pat = pat.as_str().unwrap_or("");
            match Rx::new(pat) {
                Ok(r) => c.fail_on.push(r),
                Err(e) => c.rules.push(Rule {
                    spec: table(&serde_json::json!({"id": "fail_on"})),
                    kind: "run",
                    code: None,
                    when: None,
                    where_: None,
                    each: None,
                    evaluated: 0,
                    error: format!("bad fail_on pattern {}: {e}", crate::pyre::repr(pat)),
                    waived: String::new(),
                }),
            }
        }
        for kind in ["invariant", "contract"] {
            for spec in config.get(kind).and_then(Json::as_array).into_iter().flatten() {
                let spec = table(spec);
                let mut r = Rule {
                    spec,
                    kind,
                    code: None,
                    when: None,
                    where_: None,
                    each: None,
                    evaluated: 0,
                    error: String::new(),
                    waived: String::new(),
                };
                let compiled = (|| -> Result<(), String> {
                    let a = r.get("assert").ok_or("'assert'")?.to_string();
                    r.code = Some(compile(&a)?);
                    if let Some(w) = r.get("when").filter(|w| !w.is_empty()) {
                        r.when = Some(compile(w)?);
                    }
                    if let Some(w) = r.get("where").filter(|w| !w.is_empty()) {
                        r.where_ = Some(compile(w)?);
                    }
                    if let Some(w) = r.get("each").filter(|w| !w.is_empty()) {
                        r.each = Some(compile(w)?);
                    }
                    Ok(())
                })();
                if let Err(e) = compiled {
                    r.error = format!("bad rule: {e}");
                }
                c.rules.push(r);
            }
        }
        c
    }

    pub fn feed_line(&mut self, raw: &str) {
        self.lineno += 1;
        let marker = format!("{} ", self.prefix);
        let Some(i) = raw.find(&marker) else {
            let hits: Vec<String> = self.fail_on.iter().filter(|rx| rx.is_match(raw)).map(|rx| rx.pattern().to_string()).collect();
            for pat in hits {
                let msg = crate::pyre::take_chars(crate::pyre::strip(raw), 300).to_string();
                self.flag("fail_on", "run", "error", 0.0, &pat, msg);
            }
            return;
        };
        match pyjson::loads(&raw[i + marker.len()..]) {
            Ok(V::Dict(obj)) => self.feed(obj),
            Ok(_) => {}
            Err(e) => self.flag("protocol", "run", "error", 0.0, "line", format!("unreadable {} line: {e}", self.prefix)),
        }
    }

    pub fn feed(&mut self, obj: IndexMap<String, V>) {
        match obj.get("k") {
            Some(V::Str(k)) if k == "state" => {
                self.states += 1;
                self.state(&obj);
                self.prev = Some(obj);
            }
            Some(V::Str(k)) if k == "act" => {
                self.acts += 1;
                self.act(&obj);
            }
            Some(V::Str(k)) if k == "end" => self.ended = true,
            Some(V::Str(k)) if k == "waive" => {
                let why = obj.get("why").map(py_str).filter(|s| !s.is_empty()).unwrap_or_else(|| "(no reason given)".into());
                let ids = match obj.get("rules") {
                    Some(V::List(xs)) => xs.clone(),
                    Some(x) => vec![x.clone()],
                    None => vec![],
                };
                for rid in ids {
                    let found = match &rid {
                        V::Str(s) => self.rules.iter_mut().find(|r| r.id() == *s),
                        _ => None,
                    };
                    match found {
                        Some(r) => r.waived = why.clone(),
                        None => {
                            let msg = format!("waived a rule that doesn't exist: {}", py_repr(&rid));
                            self.flag("waive", "run", "error", 0.0, &py_str(&rid), msg)
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn flag(&mut self, rule: &str, kind: &str, severity: &str, t: f64, subject: &str, message: String) {
        let key = (rule.to_string(), subject.to_string());
        if let Some(v) = self.violations.get_mut(&key) {
            v.count += 1;
            return;
        }
        let v = Violation {
            rule: rule.into(),
            kind: kind.into(),
            severity: severity.into(),
            t,
            subject: subject.into(),
            message,
            line: self.lineno,
            count: 1,
        };
        self.violations.insert(key, v);
    }

    fn check(&mut self, ri: usize, layers: &[IndexMap<String, V>], t: f64, subject: &str, hold: f64) {
        let id = self.rules[ri].id();
        let mut scope = Scope::new(layers.iter().collect());
        let r = &self.rules[ri];
        let passed = match &r.where_ {
            Some(w) => eval(w, &mut scope).map(|v| v.truthy()),
            None => Ok(true),
        };
        let result = match passed {
            Ok(false) => {
                self.since.shift_remove(&(id, subject.to_string()));
                return;
            }
            // (counted as evaluated before the assertion runs, so a rule that raises was exercised)
            Ok(true) => {
                let got = eval(r.code.as_ref().unwrap(), &mut scope).map(|v| v.truthy());
                self.rules[ri].evaluated += 1;
                got
            }
            Err(e) => Err(e),
        };
        let ok = match result {
            Err(e) => {
                let r = &mut self.rules[ri];
                if r.error.is_empty() {
                    r.error = e;
                }
                return;
            }
            Ok(ok) => ok,
        };
        if ok {
            self.since.shift_remove(&(id, subject.to_string()));
            return;
        }
        if hold > 0.0 {
            let first = *self.since.entry((id.clone(), subject.to_string())).or_insert(t);
            if t - first < hold {
                return;
            }
        }
        let msg = self.explain(ri, layers);
        let r = &self.rules[ri];
        let (kind, sev) = (r.kind, r.get("severity").unwrap_or("error").to_string());
        self.flag(&id, kind, &sev, t, subject, msg);
    }

    fn explain(&self, ri: usize, layers: &[IndexMap<String, V>]) -> String {
        let r = &self.rules[ri];
        let src = r.get("assert").unwrap_or("").trim().to_string();
        let names = read_names(&src, r.code.as_ref().unwrap());
        let mut vals = Vec::new();
        for n in names.iter().take(6) {
            let Ok(e) = compile(n) else { continue };
            let mut scope = Scope::new(layers.iter().collect());
            if let Ok(v) = eval(&e, &mut scope) {
                let s = dumps(&v);
                vals.push(format!("{n}={}", s.chars().take(160).collect::<String>()));
            }
        }
        format!("{src}  [{}]", vals.join(", "))
    }

    fn eval_with(&mut self, ri: usize, which: fn(&Rule) -> Option<&E>, layers: &[IndexMap<String, V>], ctx: &str) -> Option<V> {
        let e = which(&self.rules[ri])?.clone();
        let mut scope = Scope::new(layers.iter().collect());
        match eval(&e, &mut scope) {
            Ok(v) => Some(v),
            Err(err) => {
                let r = &mut self.rules[ri];
                if r.error.is_empty() {
                    r.error = format!("{ctx}{err}");
                }
                None
            }
        }
    }

    fn state(&mut self, st: &IndexMap<String, V>) {
        let t = st.get("t").and_then(py_float).unwrap_or(self.states as f64);
        let stv = V::Dict(st.clone());
        let prev = opt(&self.prev);
        for ri in 0..self.rules.len() {
            let r = &self.rules[ri];
            if r.kind != "invariant" || r.bad() || !r.waived.is_empty() {
                continue;
            }
            let base = vec![layer(vec![("state", stv.clone()), ("line", stv.clone()), ("prev", prev.clone())]), st.clone()];
            if r.when.is_some() {
                match self.eval_with(ri, |r| r.when.as_ref(), &base, "") {
                    Some(v) if v.truthy() => {}
                    _ => continue,
                }
            }
            let hold = self.rules[ri].spec.get("for").and_then(Json::as_f64).unwrap_or(0.0);
            if self.rules[ri].each.is_none() {
                self.check(ri, &base, t, "", hold);
                continue;
            }
            let Some(items) = self.eval_with(ri, |r| r.each.as_ref(), &base, "each: ") else { continue };
            let items = match items {
                v if !v.truthy() => vec![],
                v => match v.items() {
                    Ok(xs) => xs,
                    Err(e) => {
                        let r = &mut self.rules[ri];
                        if r.error.is_empty() {
                            r.error = format!("each: {e}");
                        }
                        continue;
                    }
                },
            };
            let key = self.rules[ri].get("key").unwrap_or("id").to_string();
            let id = self.rules[ri].id();
            let subject_of = |it: &V, i: usize| -> String {
                match it {
                    V::Dict(m) => dumps(m.get(&key).unwrap_or(&V::Int(i as i64))),
                    _ => i.to_string(),
                }
            };
            if hold > 0.0 {
                let here: Vec<String> = items.iter().enumerate().map(|(i, it)| subject_of(it, i).trim_matches('"').to_string()).collect();
                self.since.retain(|k, _| k.0 != id || here.contains(&k.1));
            }
            let mut prev_items: IndexMap<String, V> = IndexMap::new();
            if let Some(p) = &self.prev {
                let pv = V::Dict(p.clone());
                let players = vec![layer(vec![("state", pv.clone()), ("line", pv)]), p.clone()];
                let e = self.rules[ri].each.clone().unwrap();
                let mut scope = Scope::new(players.iter().collect());
                if let Ok(v) = eval(&e, &mut scope) {
                    for x in if v.truthy() { v.items().unwrap_or_default() } else { vec![] } {
                        if let V::Dict(m) = &x {
                            if let Some(k) = m.get(&key) {
                                prev_items.insert(dumps(k), x.clone());
                            }
                        }
                    }
                }
            }
            for (idx, it) in items.iter().enumerate() {
                let subj = subject_of(it, idx);
                let layers = vec![
                    layer(vec![
                        ("state", stv.clone()),
                        ("line", stv.clone()),
                        ("prev", prev.clone()),
                        ("item", it.clone()),
                        ("prev_item", prev_items.get(&subj).cloned().unwrap_or(V::None)),
                    ]),
                    as_dict(it),
                    st.clone(),
                ];
                self.check(ri, &layers, t, subj.trim_matches('"'), hold);
            }
        }
    }

    fn act(&mut self, a: &IndexMap<String, V>) {
        let t = a.get("t").and_then(py_float).unwrap_or(0.0);
        let name = a.get("act").map(py_str).unwrap_or_default();
        let av = V::Dict(a.clone());
        let prev = opt(&self.prev);
        for ri in 0..self.rules.len() {
            let r = &self.rules[ri];
            if r.kind != "contract" || r.bad() || !r.waived.is_empty() {
                continue;
            }
            let globs: Vec<String> = match r.spec.get("act") {
                None => vec!["*".into()],
                Some(Json::String(s)) => vec![s.clone()],
                Some(Json::Array(xs)) => xs.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                Some(_) => vec![],
            };
            if !globs.iter().any(|g| fnmatchcase(&name, g)) {
                continue;
            }
            let layers = vec![layer(vec![("line", av.clone()), ("state", prev.clone()), ("prev", prev.clone())]), a.clone()];
            if r.when.is_some() {
                match self.eval_with(ri, |r| r.when.as_ref(), &layers, "") {
                    Some(v) if v.truthy() => {}
                    _ => continue,
                }
            }
            let subject = format!("{name}#{}", self.acts);
            self.check(ri, &layers, t, &subject, 0.0);
        }
    }

    pub fn finish(&self) -> Vec<Violation> {
        let mut out: Vec<Violation> = self.violations.values().cloned().collect();
        let run = |rule: &str, msg: String, line: usize| Violation {
            rule: rule.into(),
            kind: "run".into(),
            severity: "error".into(),
            t: 0.0,
            subject: String::new(),
            message: msg,
            line,
            count: 1,
        };
        if self.states == 0 && self.acts == 0 {
            out.insert(
                0,
                run(
                    "no-signal",
                    format!("no {} lines at all: the emitter isn't on (or printed nothing), so nothing was checked", self.prefix),
                    self.lineno,
                ),
            );
        } else if !self.ended {
            out.push(run(
                "no-end",
                format!("the stream stopped without a {{\"k\": \"end\"}} line: the program crashed, was killed, or never finished"),
                self.lineno,
            ));
        }
        for r in &self.rules {
            if !r.error.is_empty() {
                let mut v = run(&r.id(), r.error.clone(), 0);
                v.kind = r.kind.into();
                out.push(v);
            } else if r.evaluated == 0 && self.strict {
                let mut v = run(&r.id(), "never had anything to check in this run".into(), 0);
                v.kind = r.kind.into();
                out.push(v);
            }
        }
        out
    }

    /// The `why` of the first rule with this id (printed under its findings).
    pub fn why(&self, id: &str) -> Option<String> {
        let r = self.rules.iter().find(|r| r.id() == id)?;
        r.spec.get("why").filter(|v| crate::config::truthy(Some(v))).map(crate::config::py_str)
    }

    pub fn summary(&self) -> String {
        let ran = self.rules.iter().filter(|r| r.evaluated > 0).count();
        let idle: Vec<String> =
            self.rules.iter().filter(|r| r.evaluated == 0 && r.error.is_empty() && r.waived.is_empty()).map(Rule::id).collect();
        let waived: Vec<String> =
            self.rules.iter().filter(|r| !r.waived.is_empty()).map(|r| format!("{} ({})", r.id(), r.waived)).collect();
        format!(
            "{} runtime rules ({ran} exercised{}{}), {} states, {} actions",
            self.rules.len(),
            if idle.is_empty() { String::new() } else { format!("; not exercised: {}", idle.join(", ")) },
            if waived.is_empty() { String::new() } else { format!("; WAIVED by the program: {}", waived.join(", ")) },
            self.states,
            self.acts
        )
    }
}

pub fn format_violation(v: &Violation) -> String {
    let t = fmt_g(v.t);
    let subj = if v.subject.is_empty() { String::new() } else { format!(" {}", v.subject) };
    let more = if v.count > 1 { format!(" (x{})", v.count) } else { String::new() };
    format!("[{}] {}: t={t}{subj}{more}\n    {}", v.severity, v.rule, v.message)
}

/// Python's `float(x)` of a line's `t` (a number, a bool, a number's text);
/// None for what Python's float() refuses.
fn py_float(v: &V) -> Option<f64> {
    match v {
        V::Int(i) => Some(*i as f64),
        V::Float(f) => Some(*f),
        V::Bool(b) => Some(*b as i64 as f64),
        V::Str(s) => crate::pyre::strip(s).replace('_', "").parse().ok(),
        _ => None,
    }
}
