//! What `parity --chains` compares: the shared text helpers (chains,
//! lambdas) run on every call site, assignment, declared type and bracket
//! of a file, so each helper is checked against Python's on real code before
//! an adapter leans on it.
//!
//! The Rust side writes one JSON object per file: the blanked code it read,
//! the sites (character offsets, as Python indexes) and its answers. The
//! Python side (parity.rs, PY_CHAINS) reads that object, answers the same
//! sites on the same code, and the two are compared.

use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::{json, Value};

use crate::lang::assign::assignments;
use crate::lang::chains::{self, Opts};
use crate::lang::lambdas;
use crate::pyre::Rx;

const TKS: &[&str] = &["jvm", "rust", "kotlin", "js", "swift"];
const FAMS: &[&str] = &["js", "kotlin", "swift", "rust"];
const ASSIGN_TKS: &[&str] = &["js", "kotlin", "swift", "rust", "go", "jvm", "cpp", "php", "py"];

fn char_pos(code: &str, byte: usize) -> usize {
    code[..byte].chars().count()
}

fn guarded<T: serde::Serialize>(f: impl FnOnce() -> T) -> Value {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => serde_json::to_value(v).unwrap_or(Value::Null),
        Err(_) => json!("error"),
    }
}

/// The sites of one file and Rust's answers. `code`: the file's blanked text.
pub fn probe(rel: &str, code: &str) -> Value {
    let o = Opts::for_file(rel);
    let call = Rx::lit(r"[A-Za-z_$][\w$]*\s*\(");
    let assign = Rx::lit(r"[^=<>!]=(?![=>])");
    let decl = Rx::lit(r":[ \t]*([^=;,(){}\n]{1,120})");
    let mut recv = Vec::new();
    for m in call.find_iter(code) {
        let p = m.start();
        recv.push(json!([char_pos(code, p), chains::receiver(code, p, 40, o)]));
    }
    let mut expr = Vec::new();
    for m in assign.find_iter(code) {
        let p = m.end();
        expr.push(json!([char_pos(code, p), chains::expression(code, p, 40, o)]));
    }
    let mut types = Vec::new();
    for m in decl.find_iter(code) {
        let t = m.s(1);
        types.push(json!([
            t,
            chains::type_name(t),
            chains::elem_type(t),
            chains::generic_name(t),
            lambdas::lambda_params(t),
            lambdas::generic_param(t)
        ]));
    }
    let mut parens = Vec::new();
    let mut braces = Vec::new();
    for (k, ch) in code.char_indices() {
        if ch == '(' {
            let close = lambdas::matching(code, k);
            if close > 0 && (close as usize) - k < 400 {
                let inner = &code[k + 1..close as usize];
                let args = lambdas::split_top(inner, ',');
                parens.push(json!([
                    inner,
                    TKS.iter().map(|tk| lambdas::param_types(tk, inner)).collect::<Vec<_>>(),
                    FAMS.iter().map(|f| guarded(|| lambdas::lambda_args(f, &args))).collect::<Vec<_>>(),
                ]));
            }
        } else if ch == '{' {
            let end = code[k..].char_indices().nth(200).map(|(e, _)| k + e).unwrap_or(code.len());
            let text = &code[k..end];
            braces.push(json!([
                text,
                guarded(|| lambdas::brace_lambda("kotlin", text)),
                guarded(|| lambdas::brace_lambda("swift", text))
            ]));
        }
    }
    let assign: Vec<Value> = ASSIGN_TKS.iter().map(|tk| guarded(|| assignments(tk, code, o))).collect();
    json!({
        "file": rel, "opts": [o.call_targs, o.trailing_closures], "code": code,
        "recv": recv, "expr": expr, "types": types, "parens": parens, "braces": braces, "assign": assign,
    })
}
