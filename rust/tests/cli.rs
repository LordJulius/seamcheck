//! The command line around the checks (test_cli.py): SARIF and GitHub
//! formats, exit codes, and the commands phase 1 leaves to phase 2.

mod common;

use std::process::Command;


fn run(root: &std::path::Path, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_seamcheck"))
        .args(args)
        .current_dir(root)
        .env("SEAMCHECK_RS_QUIET", "1")
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned())
}

// test_cli.py: test_sarif_and_github_formats
#[test]
fn sarif_and_github_formats() {
    let (d, _p) = common::fixture("cli");
    let (code, out) = run(d.path(), &["check", "--format", "sarif"]);
    assert_eq!(code, 1);
    let log: serde_json::Value = serde_json::from_str(&out).unwrap();
    let run_ = &log["runs"][0];
    assert_eq!(run_["tool"]["driver"]["rules"][0]["id"], "writes-go-through-store");
    assert_eq!(run_["tool"]["driver"]["rules"][0]["shortDescription"]["text"], "the store clears the cache");
    let res = run_["results"].as_array().unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"], "src/app/view.py");
    assert_eq!(res[0]["locations"][0]["physicalLocation"]["region"]["startLine"], 5);
    let (_, out) = run(d.path(), &["check", "--format", "github"]);
    assert!(
        out.starts_with("::error file=src/app/view.py,line=5,title=seamcheck writes-go-through-store::"),
        "{out}"
    );
    assert!(out.contains("(fix: call save())"));
}

// test_cli.py: test_github_escapes
#[test]
fn github_escapes() {
    let f = serde_json::json!({"rule": "r", "severity": "warn", "file": "a,b.py", "line": 3, "message": "50% x\ny"});
    assert_eq!(seamcheck::output::github(&[f]), "::warning file=a%2Cb.py,line=3,title=seamcheck r::50%25 x%0Ay");
}

#[test]
fn json_is_pythons_indent_1_and_ascii() {
    let v = serde_json::json!([{"a": "é", "b": []}]);
    assert_eq!(seamcheck::output::dumps(&v), "[\n {\n  \"a\": \"\\u00e9\",\n  \"b\": []\n }\n]");
}

#[test]
fn graph_commands_say_they_are_phase_2() {
    let (d, _p) = common::fixture("cli");
    let o = Command::new(env!("CARGO_BIN_EXE_seamcheck")).args(["callers", "save"]).current_dir(d.path()).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("phase 2"));
}
