//! A rule that doesn't compile says why in Python's words (widens
//! test_runtime.py's Sandbox.test_refuses_calls_and_dunders): each source and the text
//! Python's compile_expr raises for it, SyntaxError or refusal.

use seamcheck::runtime::parse::compile;

// (from Python 3.14 from Python's compile_expr on these sources)
const CASES: &[(&str, &str)] = &[
    ("x ==", "invalid syntax (<unknown>, line 1)"),
    ("len(xs", "'(' was never closed (<unknown>, line 1)"),
    ("[1, (2", "'(' was never closed (<unknown>, line 1)"),
    ("x)", "unmatched ')' (<unknown>, line 1)"),
    ("(]", "closing parenthesis ']' does not match opening parenthesis '(' (<unknown>, line 1)"),
    ("'abc", "unterminated string literal (detected at line 1) (<unknown>, line 1)"),
    ("a b", "invalid syntax (<unknown>, line 1)"),
    ("f(a b)", "invalid syntax. Perhaps you forgot a comma? (<unknown>, line 1)"),
    ("x = 1", "invalid syntax (<unknown>, line 1)"),
    ("x := 1", "invalid syntax (<unknown>, line 1)"),
    ("(y := 1)", "not allowed in a rule: NamedExpr"),
    ("1 +", "invalid syntax (<unknown>, line 1)"),
    ("f(x=1)", "only helper calls are allowed (len, set, sorted, min, max, sum, abs, any, all, round, count, dupes, isfinite, get, keys, values, diff)"),
    ("{k: v for k in d}", "not allowed in a rule: DictComp"),
    ("a | b", "not allowed in a rule: BitOr"),
    ("~a", "not allowed in a rule: Invert"),
    ("a @ b", "not allowed in a rule: MatMult"),
    ("a << 1", "not allowed in a rule: LShift"),
    ("f'x'", "not allowed in a rule: JoinedStr"),
    ("*a", "invalid syntax (<unknown>, line 1)"),
    ("x if y", "expected 'else' after 'if' expression (<unknown>, line 1)"),
    ("x.__y", "attributes starting with _ are not allowed"),
    ("lambda: 1", "not allowed in a rule: Lambda"),
    ("(lambda: 1)()", "only helper calls are allowed (len, set, sorted, min, max, sum, abs, any, all, round, count, dupes, isfinite, get, keys, values, diff)"),
    ("len(lambda: 1)", "not allowed in a rule: Lambda"),
    ("[].append(1)", "only helper calls are allowed (len, set, sorted, min, max, sum, abs, any, all, round, count, dupes, isfinite, get, keys, values, diff)"),
    ("a.b()", "only helper calls are allowed (len, set, sorted, min, max, sum, abs, any, all, round, count, dupes, isfinite, get, keys, values, diff)"),
    ("await x", "not allowed in a rule: Await"),
    ("yield x", "invalid syntax (<unknown>, line 1)"),
    ("x[1:2:3]", "OK"),
    ("not", "invalid syntax (<unknown>, line 1)"),
    ("a not b", "invalid syntax (<unknown>, line 1)"),
    ("'''abc", "unterminated triple-quoted string literal (detected at line 1) (<unknown>, line 1)"),
    ("x\n", "OK"),
    ("\"a\" 'b'", "OK"),
    ("1e", "invalid decimal literal (<unknown>, line 1)"),
    ("0x10 > 1", "OK"),
    ("1_000 > 2", "OK"),
    ("a.b.c(1)", "only helper calls are allowed (len, set, sorted, min, max, sum, abs, any, all, round, count, dupes, isfinite, get, keys, values, diff)"),
    ("x is not", "invalid syntax (<unknown>, line 1)"),
    ("!x", "invalid syntax (<unknown>, line 1)"),
    ("len(x) +", "invalid syntax (<unknown>, line 1)"),
    ("{1: 2", "'{' was never closed (<unknown>, line 1)"),
    ("f(", "'(' was never closed (<unknown>, line 1)"),
    ("a.b.__c", "attributes starting with _ are not allowed"),
    ("open('x')", "only helper calls are allowed (len, set, sorted, min, max, sum, abs, any, all, round, count, dupes, isfinite, get, keys, values, diff)"),
    ("x.__class__", "attributes starting with _ are not allowed"),
    ("__import__('os')", "only helper calls are allowed (len, set, sorted, min, max, sum, abs, any, all, round, count, dupes, isfinite, get, keys, values, diff)"),
];

#[test]
fn compile_errors_read_as_pythons() {
    let mut bad = Vec::new();
    for (src, want) in CASES {
        let got = match compile(src) {
            Ok(_) => "OK".to_string(),
            Err(e) => e,
        };
        if got != *want {
            bad.push(format!("{src:?}\n  python: {want}\n  rust:   {got}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
