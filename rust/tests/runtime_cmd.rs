//! `seamcheck trace | run | fuzz` (test_runtime.py's Breaks, Config and Cli
//! classes) and the Python shapes the runtime output keeps: json.loads'
//! errors, float repr and `:g`, shlex.join, universal newlines.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use seamcheck::runtime::cmd::{seeds, shlex_join, Lines};
use seamcheck::runtime::pyfmt::{float_repr, fmt_g};
use seamcheck::runtime::pyjson::loads;

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

fn script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn seamcheck(dir: &Path, args: &[&str]) -> (i32, String) {
    let mut a: Vec<&str> = vec![args[0], "-C", dir.to_str().unwrap()];
    a.extend(&args[1..]);
    let o = Command::new(env!("CARGO_BIN_EXE_seamcheck")).args(&a).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Breaks.test_typo_keys_and_bad_severity_are_config_errors
#[test]
fn typo_keys_and_bad_severity_are_config_errors() {
    let d = tempfile::tempdir().unwrap();
    write(
        &d.path().join("seamcheck.toml"),
        "[[invariant]]\nid = \"a\"\nwehre = \"x\"\nassert = \"True\"\nseverity = \"warning\"\n[[contract]]\nid = \"b\"\nacts = [\"x\"]\nassert = \"True\"\n[[rule]]\nid = \"c\"\npattern = \"x\"\nalow = [\"y\"]\n",
    );
    let (_, problems) = seamcheck::config::load(d.path(), &[]);
    let text = problems.join("\n");
    assert!(text.contains("unknown key 'wehre' (did you mean 'where'?)"), "{text}");
    assert!(text.contains("severity 'warning'"));
    assert!(text.contains("unknown key 'acts' (did you mean 'act'?)"));
    assert!(text.contains("unknown key 'alow' (did you mean 'allow'?)"));
}

/// Breaks.test_run_timeout_kills_grandchildren_holding_the_pipe
#[test]
fn run_timeout_kills_grandchildren_holding_the_pipe() {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("seamcheck.toml"), "[[invariant]]\nid = \"x\"\nassert = \"True\"\n");
    let sh = d.path().join("hold.sh");
    script(&sh, "#!/bin/bash\n(sleep 30; echo late) &\necho 'SEAMCHECK {\"k\": \"state\"}'\nsleep 60\n");
    let t0 = Instant::now();
    let (_, out) = seamcheck(d.path(), &["run", "--timeout", "2", "--", sh.to_str().unwrap()]);
    assert!(t0.elapsed().as_secs() < 10, "the run must end at its timeout");
    assert!(out.contains("no-end"), "{out}");
}

/// Breaks.test_child_exit_ends_the_run_even_if_a_grandchild_keeps_the_pipe
#[test]
fn child_exit_ends_the_run_even_if_a_grandchild_keeps_the_pipe() {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("seamcheck.toml"), "[[invariant]]\nid = \"x\"\nassert = \"True\"\n");
    let sh = d.path().join("leave.sh");
    script(&sh, "#!/bin/bash\n(sleep 30) &\necho 'SEAMCHECK {\"k\": \"state\"}'\necho 'SEAMCHECK {\"k\": \"end\"}'\n");
    let t0 = Instant::now();
    let (code, out) = seamcheck(d.path(), &["run", "--", sh.to_str().unwrap()]);
    assert!(t0.elapsed().as_secs() < 10);
    assert_eq!(code, 0, "{out}");
}

/// Config.test_merges_root_dir_and_extra_and_catches_dup_ids
#[test]
fn merges_root_dir_and_extra_and_catches_dup_ids() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("seamcheck.d")).unwrap();
    write(&d.path().join("seamcheck.toml"), "[project]\ninclude = [\"a/**\"]\n[[rule]]\nid = \"r1\"\npattern = \"x\"\n");
    write(&d.path().join("seamcheck.d/rt.toml"), "[[invariant]]\nid = \"i1\"\nassert = \"True\"\n");
    let extra = d.path().join("extra.toml");
    write(&extra, "[project]\ninclude = [\"b/**\"]\n[[rule]]\nid = \"r1\"\npattern = \"y\"\n");
    let (cfg, problems) = seamcheck::config::load(d.path(), &[extra.to_string_lossy().into_owned()]);
    assert_eq!(cfg.table("project")["include"], serde_json::json!(["a/**", "b/**"]));
    let ids: Vec<&str> = cfg.list("rule").iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["r1"]);
    assert_eq!(cfg.list("invariant")[0]["_file"], "seamcheck.d/rt.toml");
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("already defined"));
}

/// Cli.test_run_and_fuzz_end_to_end (the program a shell script here)
#[test]
fn run_and_fuzz_end_to_end() {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("seamcheck.toml"), "[[invariant]]\nid = \"small\"\nassert = \"n < 3\"\n");
    let prog = d.path().join("prog.sh");
    script(&prog, "#!/bin/bash\necho \"SEAMCHECK {\\\"k\\\": \\\"state\\\", \\\"t\\\": 0, \\\"n\\\": $1}\"\necho 'SEAMCHECK {\"k\": \"end\"}'\n");
    let p = prog.to_str().unwrap();
    let (code, out) = seamcheck(d.path(), &["run", "--", p, "1"]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = seamcheck(d.path(), &["run", "--", p, "5"]);
    assert_eq!(code, 1);
    assert!(out.contains("n=5"), "{out}");
    let (code, out) = seamcheck(d.path(), &["fuzz", "--seeds", "1-4", "--", p, "{seed}"]);
    assert_eq!(code, 1);
    assert!(out.contains("2/4 seeds clean"), "{out}");
    assert!(out.contains("replay seed 3"));
    let empty = tempfile::tempdir().unwrap();
    let (code, _) = seamcheck(empty.path(), &["run", "--", "true"]);
    assert_eq!(code, 2, "no rules found must not pass");
}

#[test]
fn json_errors_read_as_pythons() {
    for (src, want) in [
        ("", "Expecting value: line 1 column 1 (char 0)"),
        ("{\"a\":1,}", "Illegal trailing comma before end of object: line 1 column 7 (char 6)"),
        ("[1, ]", "Illegal trailing comma before end of array: line 1 column 3 (char 2)"),
        ("{\"a\" 1}", "Expecting ':' delimiter: line 1 column 6 (char 5)"),
        ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
        ("\"ab", "Unterminated string starting at: line 1 column 1 (char 0)"),
        ("{\"a\":1} x", "Extra data: line 1 column 9 (char 8)"),
        ("\"a\tb\"", "Invalid control character at: line 1 column 3 (char 2)"),
        ("\"\\uZZZZ\"", "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
        ("\"\\ud83d\\uZZZZ\"", "Invalid \\uXXXX escape: line 1 column 9 (char 8)"),
        ("\"\\x\"", "Invalid \\escape: line 1 column 2 (char 1)"),
        (" \n {\"a\":\n x}", "Expecting value: line 3 column 2 (char 10)"),
        ("{1: 2}", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
        ("1.e5", "Extra data: line 1 column 2 (char 1)"),
        ("\u{feff}{}", "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)"),
    ] {
        assert_eq!(loads(src).err().as_deref(), Some(want), "{src:?}");
    }
    let v = loads("{\"a\":1,\"a\":2,\"b\":[NaN, -Infinity, 1e400, \"\\ud83d\\ude00\"]}").unwrap();
    assert_eq!(seamcheck::runtime::pyfmt::dumps(&v), "{\"a\": 2, \"b\": [NaN, -Infinity, Infinity, \"\\ud83d\\ude00\"]}");
}

#[test]
fn floats_print_as_pythons() {
    for (f, repr, g) in [
        (0.0, "0.0", "0"),
        (12.5, "12.5", "12.5"),
        (1e-5, "1e-05", "1e-05"),
        (1e16, "1e+16", "1e+16"),
        (1234567.891, "1234567.891", "1.23457e+06"),
        (0.0001, "0.0001", "0.0001"),
        (-0.0, "-0.0", "-0"),
        (12345678901234567890.0, "1.2345678901234567e+19", "1.23457e+19"),
        (100000.0, "100000.0", "100000"),
    ] {
        assert_eq!(float_repr(f), repr, "{f}");
        assert_eq!(fmt_g(f), g, "{f}");
    }
}

#[test]
fn seeds_and_replays_read_as_pythons() {
    assert_eq!(seeds("1-4").unwrap(), vec![1, 2, 3, 4]);
    assert_eq!(seeds("5,1, 3,-1").unwrap(), vec![5, 1, 3, -1]);
    assert_eq!(seeds("-3--1").err().is_some(), true); // (Python's int('') raises too)
    assert_eq!(shlex_join(&["a b".into(), "it's".into(), "".into(), "x/y=z".into()]), "'a b' 'it'\"'\"'s' '' x/y=z");
}

#[test]
fn lines_end_at_any_newline() {
    let got: Vec<(String, bool)> = Lines::new(&b"a\r\nb\rc\n\nd"[..]).collect();
    let want = [("a", true), ("b", true), ("c", true), ("", true), ("d", false)];
    assert_eq!(got, want.iter().map(|(s, e)| (s.to_string(), *e)).collect::<Vec<_>>());
}
