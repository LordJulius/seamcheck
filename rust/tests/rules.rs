//! Rule kinds without the call graph, ported from the Python suite
//! (test_require.py, test_fanout.py, test_packs.py's config cases, test_cli.py).

mod common;

use common::{by_rule, fixture};
use seamcheck::rules::run_rules;

// test_require.py: RequireTest.test_require
#[test]
fn require_in_order_gone_files_and_missing_functions() {
    let (_d, p) = fixture("require");
    let by = by_rule(&run_rules(&p, None, None));
    assert!(!by.contains_key("inject-first"));
    assert_eq!(by["reversed"].len(), 1);
    assert!(by["reversed"][0].contains("in order"));
    assert_eq!(by["gone"].len(), 2); // a missing file and an empty glob
    assert_eq!(by["no-func"].len(), 1);
}

// The Python test reads the surface view with `func = "r"` (a TypeScript
// function: its adapter is phase 2); the fixture reads it `within` that function.
fn fanout_fixture() -> (tempfile::TempDir, seamcheck::project::Project) {
    fixture("fanout")
}

// test_fanout.py: test_missing_extra_except_fallback
#[test]
fn fanout_missing_extra_except_fallback() {
    let (_d, p) = fanout_fixture();
    let found = by_rule(&run_rules(&p, None, None));
    let msgs = &found["sections"];
    assert!(msgs.contains(&"surface does not handle 'video'".to_string()), "{msgs:?}");
    assert!(msgs.contains(&"surface handles 'chart', which the source never produces".to_string()));
    assert!(msgs.contains(&"osd does not handle 'list' (falls back to generic host)".to_string()));
    assert!(!msgs.iter().any(|m| m.contains("osa")), "video is an intentional gap");
    assert!(msgs.contains(&"views.osd: unknown key 'mtach' (did you mean 'match'?)".to_string()));
    let rule = p.config.rules().find(|r| seamcheck::config::id(r) == "sections").unwrap();
    let (_toks, _views, cell) = seamcheck::rules::fanout::matrix(&p, rule).unwrap();
    assert_eq!(cell[&("video".to_string(), "osa".to_string())], "except");
    assert_eq!(cell[&("header".to_string(), "osd".to_string())], "ok"); // "head" is in "header"
    assert_eq!(cell[&("list".to_string(), "osd".to_string())], "fallback");
}

// test_fanout.py: test_normalize_before_lookup
#[test]
fn fanout_normalize_before_lookup() {
    let (_d, p) = fanout_fixture();
    assert!(!by_rule(&run_rules(&p, None, None)).contains_key("types"));
}

// test_fanout.py: test_routes_with_params_and_suffixes
#[test]
fn fanout_routes_with_params_and_suffixes() {
    let (_d, p) = fanout_fixture();
    let found = by_rule(&run_rules(&p, None, None));
    assert_eq!(found["routes"], vec!["hub does not handle '/api/nope'".to_string()]);
}

// test_packs.py: test_a_project_overrides_a_pack_rule_by_its_id (config half)
#[test]
fn a_project_overrides_a_pack_rule_by_its_id() {
    let dir = tempfile::tempdir().unwrap();
    common::write(
        dir.path(),
        &[(
            "seamcheck.toml",
            "[project]\npacks = [\"phoenix\"]\n\n[[rule]]\nid = \"phoenix/handle-info-catch-all\"\nseverity = \"warn\"\n",
        )],
    );
    let (cfg, problems) = seamcheck::config::load(dir.path(), &[]);
    assert!(problems.is_empty(), "{problems:?}");
    let rule = cfg.rules().find(|r| seamcheck::config::id(r) == "phoenix/handle-info-catch-all").unwrap();
    assert_eq!(seamcheck::config::s(rule, "severity"), Some("warn"));
    assert_eq!(seamcheck::config::s(rule, "lint"), Some("handle-info-catch-all"));
    assert_eq!(seamcheck::config::s(rule, "_file"), Some("seamcheck.toml"));
}

// test_packs.py: test_unknown_packs_and_overrides_are_config_problems
#[test]
fn unknown_packs_and_overrides_are_config_problems() {
    let dir = tempfile::tempdir().unwrap();
    common::write(
        dir.path(),
        &[(
            "seamcheck.toml",
            "[project]\npacks = [\"phoenx\", \"react\"]\n\n[[rule]]\nid = \"react/no-such\"\nseverity = \"warn\"\n\n[[rule]]\nid = \"qml/unknown-type\"\nseverity = \"warn\"\n",
        )],
    );
    let (_, problems) = seamcheck::config::load(dir.path(), &[]);
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(problems[0].contains("unknown pack 'phoenx' (did you mean 'phoenix'?)"));
    let all = problems.join(" ");
    assert!(all.contains("pack react has no rule 'no-such'"));
    assert!(all.contains("overrides pack qml, which [project] packs doesn't list"));
}

// test_packs.py: test_only_names_a_pack
#[test]
fn only_names_a_pack() {
    use seamcheck::rules::picked;
    assert!(picked("security/java-injection", Some(&["security".to_string()])));
    assert!(!picked("securityx/a", Some(&["security".to_string()])));
    assert!(!picked("a", None));
}

#[test]
fn forbid_lines_files_allow_and_waivers() {
    let (_d, p) = fixture("forbid");
    let found = run_rules(&p, None, None);
    let got: Vec<(String, String, usize, String)> =
        found.iter().map(|f| (f.rule.clone(), f.file.clone(), f.line, f.snippet.clone())).collect();
    assert_eq!(
        got,
        vec![
            ("no-print".into(), "a.py".into(), 2, "print(x)  # a".into()),
            ("spans".into(), "c.js".into(), 1, "open(a)".into()),
        ]
    );
    assert_eq!(found[0].message, "forbidden: print\\(");
    assert_eq!(found[1].message, "opened then closed");
}

#[test]
fn together_by_function_near_and_file() {
    let (_d, p) = fixture("together");
    let found = run_rules(&p, None, None);
    let got: Vec<(String, usize, String, String)> =
        found.iter().map(|f| (f.rule.clone(), f.line, f.func.clone(), f.message.clone())).collect();
    assert_eq!(
        got,
        vec![
            ("logged".into(), 10, "bad".into(), "missing: Logger\\.".into()),
            ("near".into(), 18, "p".into(), "missing nearby: ack".into()),
        ]
    );
}

#[test]
fn coverage_defined_and_used() {
    let (_d, p) = fixture("coverage");
    let found = run_rules(&p, None, None);
    let got: Vec<(String, String, usize, String)> =
        found.iter().map(|f| (f.severity.clone(), f.file.clone(), f.line, f.message.clone())).collect();
    assert_eq!(
        got,
        vec![
            ("error".into(), "use.ex".into(), 2, "'c' is used but never defined".into()),
            ("warn".into(), "defs.ex".into(), 1, "'b' is defined but never used".into()),
        ]
    );
}

#[test]
fn unknown_keys_severities_and_duplicate_ids() {
    let (d, _p) = fixture("config-problems");
    let (cfg, problems) = seamcheck::config::load(d.path(), &[]);
    assert_eq!(
        problems,
        vec![
            "seamcheck.toml: rule 'a': unknown key 'wehre' (did you mean 'when'?)".to_string(),
            "seamcheck.toml: rule 'a': severity 'loud' (use error, info, warn)".to_string(),
            "seamcheck.toml: rule 'a': gate 'maybe' (use all, changed, touched or off)".to_string(),
            "seamcheck.d/more.toml: rule id 'a' already defined in seamcheck.toml".to_string(),
        ]
    );
    assert_eq!(cfg.rules().count(), 3);
}

#[test]
fn bad_patterns_and_missing_keys_name_the_rule() {
    let (_d, p) = fixture("config-problems");
    let found: Vec<_> = run_rules(&p, None, None).into_iter().filter(|f| f.rule == "bad" || f.rule == "nokey").collect();
    let rules: Vec<&str> = found.iter().map(|f| f.rule.as_str()).collect();
    assert_eq!(rules, vec!["bad", "nokey"]);
    assert!(found[0].message.starts_with("bad pattern in rule bad: "));
    assert_eq!(found[1].message, "rule nokey needs 'if'");
}

// test_gate_moves.py: test_a_command_that_cant_run_here_is_one_warning (the rule half)
#[test]
fn a_command_that_cant_run_here_is_one_warning() {
    let (_d, p) = fixture("command");
    let found: Vec<_> = run_rules(&p, None, None).into_iter().filter(|f| f.rule == "env").collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].severity, "warn");
    assert!(found[0].message.contains("couldn't run"), "{}", found[0].message);
}

#[test]
fn command_lines_become_findings_with_files() {
    let (_d, p) = fixture("command");
    let found: Vec<(String, String)> =
        run_rules(&p, None, None).into_iter().filter(|f| f.rule == "kit").map(|f| (f.file, f.message)).collect();
    assert_eq!(
        found,
        vec![
            ("".to_string(), "whole  (fix: shrink it)".to_string()),
            ("app/ui/a.qml".to_string(), "grew ui/a.qml 3  (fix: shrink it)".to_string()),
        ]
    );
}
