//! Runtime checks (test_runtime.py): invariants, contracts, holds, the
//! run-level guards and the expression sandbox, against the Rust checker.

use seamcheck::runtime::parse::compile;
use seamcheck::runtime::{Checker, Violation};
use serde_json::{json, Value};

fn cfg() -> Value {
    json!({
        "runtime": {},
        "invariant": [
            {"id": "hp", "each": "units", "where": "not dead", "assert": "0 <= hp <= max_hp"},
            {"id": "wet", "each": "units", "assert": "not wet", "for": 2.0},
            {"id": "stones", "assert": "not dupes(stones)"},
            {"id": "frozen", "when": "paused and prev and prev.paused", "each": "units",
             "assert": "prev_item is None or (x, hp) == (prev_item.x, prev_item.hp)"}
        ],
        "contract": [
            {"id": "refused-noop", "act": "*", "when": "ok == False", "assert": "after == before"},
            {"id": "price", "act": ["build", "recruit"], "when": "ok and price >= 0",
             "assert": "after.gold == before.gold - price"}
        ]
    })
}

fn lines(objs: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = objs.iter().map(|o| format!("noise before SEAMCHECK {o}")).collect();
    v.push(format!("SEAMCHECK {}", json!({"k": "end"})));
    v
}

fn check_with(stream: &[String], cfg: &Value, strict: bool) -> (Checker, Vec<Violation>) {
    let mut c = Checker::new(cfg, strict);
    for l in stream {
        c.feed_line(l);
    }
    let vs = c.finish();
    (c, vs)
}

fn check(stream: &[String]) -> Vec<Violation> {
    check_with(stream, &cfg(), false).1
}

fn unit(id: &str, kw: Value) -> Value {
    let mut u = json!({"id": id, "hp": 10, "max_hp": 10, "dead": false, "wet": false, "x": 0});
    for (k, v) in kw.as_object().unwrap() {
        u[k] = v.clone();
    }
    u
}

fn state(t: i64, units: Vec<Value>, stones: &[&str], paused: bool) -> Value {
    json!({"k": "state", "t": t, "units": units, "stones": stones, "paused": paused})
}

fn rs(vs: &[Violation]) -> Vec<(String, String)> {
    vs.iter().map(|v| (v.rule.clone(), v.subject.clone())).collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[test]
fn clean_run_passes() {
    assert!(check(&lines(&[state(0, vec![unit("a", json!({}))], &[], false), state(1, vec![unit("a", json!({}))], &[], false)])).is_empty());
}

#[test]
fn item_rule_names_the_item_and_values() {
    let vs = check(&lines(&[state(0, vec![unit("a", json!({})), unit("b", json!({"hp": 12}))], &[], false)]));
    assert_eq!(rs(&vs), vec![pair("hp", "b")]);
    assert!(vs[0].message.contains("hp=12"), "{}", vs[0].message);
}

#[test]
fn where_skips() {
    assert!(check(&lines(&[state(0, vec![unit("a", json!({"hp": -5, "dead": true}))], &[], false)])).is_empty());
}

#[test]
fn hold_needs_to_last() {
    let wet = || unit("a", json!({"wet": true}));
    let short = lines(&[state(0, vec![wet()], &[], false), state(1, vec![wet()], &[], false), state(2, vec![unit("a", json!({}))], &[], false)]);
    assert!(check(&short).is_empty());
    let long: Vec<Value> = (0..4).map(|t| state(t, vec![wet()], &[], false)).collect();
    assert_eq!(check(&lines(&long)).iter().map(|v| v.rule.as_str()).collect::<Vec<_>>(), vec!["wet"]);
}

#[test]
fn repeats_are_counted_not_repeated() {
    let st: Vec<Value> = (0..5).map(|t| state(t, vec![unit("a", json!({"hp": 99}))], &[], false)).collect();
    let vs = check(&lines(&st));
    assert_eq!(vs.len(), 1);
    assert_eq!(vs[0].count, 5);
}

#[test]
fn prev_item_matches_by_key() {
    let s1 = state(0, vec![unit("a", json!({"x": 1})), unit("b", json!({"x": 5}))], &[], true);
    let s2 = state(1, vec![unit("b", json!({"x": 5})), unit("a", json!({"x": 2}))], &[], true);
    assert_eq!(rs(&check(&lines(&[s1, s2]))), vec![pair("frozen", "a")]);
}

#[test]
fn dupes() {
    let vs = check(&lines(&[state(0, vec![], &["gale", "ember", "gale"], false)]));
    assert_eq!(vs.iter().map(|v| v.rule.as_str()).collect::<Vec<_>>(), vec!["stones"]);
    assert!(vs[0].message.contains("gale"));
}

fn act(name: &str, ok: bool, before: Value, after: Value, price: i64) -> Value {
    json!({"k": "act", "act": name, "ok": ok, "before": before, "after": after, "price": price})
}

#[test]
fn refusal_that_changes_state() {
    let vs = check(&lines(&[act("buy", false, json!({"gold": 5}), json!({"gold": 3}), -1)]));
    assert_eq!(vs.iter().map(|v| v.rule.as_str()).collect::<Vec<_>>(), vec!["refused-noop"]);
}

#[test]
fn fields_named_like_methods_and_specials() {
    let c = json!({"invariant": [], "contract": [
        {"id": "items", "act": "*", "assert": "sorted(before.items) == sorted(after.items)"},
        {"id": "ok", "act": "*", "when": "get(line, 'ok') == False", "assert": "act == 'buy'"}]});
    let a = act("sell", false, json!({"items": ["a", "b"]}), json!({"items": ["a"]}), -1);
    let (_, vs) = check_with(&lines(&[a]), &c, false);
    let mut r: Vec<&str> = vs.iter().map(|v| v.rule.as_str()).collect();
    r.sort();
    assert_eq!(r, vec!["items", "ok"]);
}

#[test]
fn price() {
    let good = act("build", true, json!({"gold": 50}), json!({"gold": 20}), 30);
    let bad = act("recruit", true, json!({"gold": 50}), json!({"gold": 45}), 30);
    let other = act("sell", true, json!({"gold": 50}), json!({"gold": 45}), 30);
    assert_eq!(rs(&check(&lines(&[good, bad, other]))), vec![pair("price", "recruit#2")]);
}

#[test]
fn silence_fails() {
    let vs = check(&["hello".into(), "world".into()]);
    assert_eq!(vs.iter().map(|v| v.rule.as_str()).collect::<Vec<_>>(), vec!["no-signal"]);
}

#[test]
fn missing_end_fails() {
    let vs = check(&[format!("SEAMCHECK {}", state(0, vec![unit("a", json!({}))], &[], false))]);
    assert_eq!(vs.iter().map(|v| v.rule.as_str()).collect::<Vec<_>>(), vec!["no-end"]);
}

#[test]
fn broken_rule_is_an_error_not_a_pass() {
    let c = json!({"invariant": [{"id": "typo", "each": "units", "assert": "hitpoints > 0"}], "contract": []});
    let (_, vs) = check_with(&lines(&[state(0, vec![unit("a", json!({}))], &[], false)]), &c, false);
    assert_eq!(vs.iter().map(|v| (v.rule.as_str(), v.severity.as_str())).collect::<Vec<_>>(), vec![("typo", "error")]);
    assert!(vs[0].message.contains("hitpoints"));
}

#[test]
fn strict_flags_unexercised_rules() {
    let (_, vs) = check_with(&lines(&[state(0, vec![unit("a", json!({}))], &[], false)]), &cfg(), true);
    let mut r: Vec<&str> = vs.iter().map(|v| v.rule.as_str()).collect();
    r.sort();
    assert_eq!(r, vec!["frozen", "price", "refused-noop"]);
}

#[test]
fn bad_json_line() {
    let mut s = vec!["SEAMCHECK {nope".to_string()];
    s.extend(lines(&[state(0, vec![], &[], false)]));
    assert_eq!(check(&s).iter().map(|v| v.rule.as_str()).collect::<Vec<_>>(), vec!["protocol"]);
}

#[test]
fn fail_on_patterns() {
    let mut c = cfg();
    c["runtime"] = json!({"fail_on": ["SCRIPT ERROR"]});
    let mut s = vec!["SCRIPT ERROR: boom".to_string(), "SCRIPT ERROR: again".to_string()];
    s.extend(lines(&[state(0, vec![], &[], false)]));
    let (_, vs) = check_with(&s, &c, false);
    assert_eq!(vs.iter().map(|v| (v.rule.as_str(), v.count)).collect::<Vec<_>>(), vec![("fail_on", 2)]);
    assert!(vs[0].message.contains("boom"));
}

#[test]
fn hold_restarts_when_the_subject_comes_back() {
    let wet = || unit("a", json!({"wet": true}));
    let st = vec![
        state(0, vec![wet()], &[], false),
        state(1, vec![wet()], &[], false),
        state(2, vec![], &[], false),
        state(10, vec![wet()], &[], false),
        state(11, vec![wet()], &[], false),
    ];
    assert!(check(&lines(&st)).is_empty());
}

#[test]
fn summary_names_rules_that_never_ran() {
    let (c, _) = check_with(&lines(&[state(0, vec![unit("a", json!({}))], &[], false)]), &cfg(), false);
    assert!(c.summary().contains("not exercised: frozen, refused-noop, price"), "{}", c.summary());
}

#[test]
fn waived_rules_are_skipped_and_listed() {
    let w = json!({"k": "waive", "rules": ["hp"], "why": "the test overheals on purpose"});
    let (c, vs) = check_with(&lines(&[w, state(0, vec![unit("a", json!({"hp": 99}))], &[], false)]), &cfg(), false);
    assert!(vs.is_empty());
    assert!(c.summary().contains("WAIVED by the program: hp (the test overheals on purpose)"));
}

#[test]
fn waiving_an_unknown_rule_is_an_error() {
    let vs = check(&lines(&[json!({"k": "waive", "rules": ["hpp"]}), state(0, vec![unit("a", json!({}))], &[], false)]));
    assert_eq!(rs(&vs), vec![pair("waive", "hpp")]);
}

#[test]
fn nan_values_fail_comparisons_instead_of_crashing() {
    let c = json!({"invariant": [{"id": "fin", "each": "units", "assert": "isfinite(hp)"}], "contract": []});
    let s = vec![
        r#"SEAMCHECK {"k": "state", "t": 0, "units": [{"id": "a", "hp": NaN}]}"#.to_string(),
        r#"SEAMCHECK {"k": "end"}"#.to_string(),
    ];
    let (_, vs) = check_with(&s, &c, false);
    assert_eq!(rs(&vs), vec![pair("fin", "a")]);
}

#[test]
fn refuses_calls_and_dunders() {
    for src in ["__import__('os')", "open('x')", "x.__class__", "(lambda: 1)()", "[].append(1)"] {
        assert!(compile(src).is_err(), "{src} compiled");
    }
}

#[test]
fn generator_sees_names() {
    let c = json!({"invariant": [{"id": "g", "assert": "all(u.hp <= cap for u in units)"}], "contract": []});
    let mut s = state(0, vec![unit("a", json!({"hp": 3}))], &[], false);
    s["cap"] = json!(2);
    let (_, vs) = check_with(&lines(&[s]), &c, false);
    assert_eq!(vs.iter().map(|v| v.rule.as_str()).collect::<Vec<_>>(), vec!["g"]);
}

#[test]
fn expressions_mean_what_python_says() {
    use seamcheck::runtime::expr::{eval, Scope, V};
    let e = |src: &str| -> String {
        let m = indexmap::IndexMap::from([("xs".to_string(), V::List(vec![V::Int(3), V::Int(1), V::Int(2)])), ("d".to_string(), V::Dict(indexmap::IndexMap::from([("a".to_string(), V::Int(1))])))]);
        let mut sc = Scope::new(vec![&m]);
        seamcheck::runtime::pyfmt::dumps(&eval(&compile(src).unwrap(), &mut sc).unwrap())
    };
    assert_eq!(e("sorted(xs)[-1]"), "3");
    assert_eq!(e("xs[1:]"), "[1, 2]");
    assert_eq!(e("[x * 2 for x in xs if x > 1]"), "[6, 4]");
    assert_eq!(e("'a' in d and not 'b' in d"), "true");
    assert_eq!(e("1 < 2 < 3 > 5"), "false");
    assert_eq!(e("7 // 2, -7 // 2, 7 % -3"), "[3, -4, -2]");
    assert_eq!(e("None or 'x'"), "\"x\"");
    assert_eq!(e("'yes' if len(xs) == 3 else 'no'"), "\"yes\"");
    assert_eq!(e("{x % 2 for x in xs} == {0, 1}"), "true");
    assert_eq!(e("(1, 2) == [1, 2]"), "false");
    assert_eq!(e("diff({'a': 1}, d)"), "{}");
    assert_eq!(e("max(xs) + min(4, 5) + sum(xs)"), "13");
}
