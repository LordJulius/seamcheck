//! The land gate, phase 1 (test_gate.py, test_gate_moves.py): rule errors,
//! `[gate] forbid_added` names in changed lines, moved code, generated names
//! and checks pinned to a file the change split. The dead-code, copy and
//! test-audit halves of these tests need the call graph (phase 2).

mod common;

use std::path::Path;

use common::{commit_all, git, load, write};
use seamcheck::gate::{gate, Report};

fn body() -> String {
    (0..14)
        .map(|i| format!("    x{i} = compute(a, b, {i}) + other(x{}, 'k{i}')", if i > 0 { i - 1 } else { 0 }))
        .collect::<Vec<_>>()
        .join("\n")
}

const SEARCH: &str = "defmodule Search do\n  def search_all(q), do: q\n\n  def forgotten(x) do\n    x + 1\n  end\nend\n";
const WEB: &str = "defmodule Web do\n  def index(q), do: Search.search_all(q)\nend\n";
const CFG: &str =
    "[project]\nname = \"t\"\ninclude = [\"lib/**\", \"test/**\", \"a/**\", \"b/**\"]\n[gate]\nforbid_added = [\"fallback\", \"legacy\"]\n";

fn base() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let one = format!("def big(a, b):\n{}\n    return x0\n\n\ndef main():\n    big(1, 2)\n", body());
    write(d.path(), &[("lib/search.ex", SEARCH), ("lib/web.ex", WEB), ("a/one.py", &one), ("seamcheck.toml", CFG)]);
    git(d.path(), &["init", "-q"]);
    commit_all(d.path(), "base");
    d
}

fn run(root: &Path) -> Report {
    let mut p = load(root);
    gate(&mut p, "HEAD", false)
}

fn words(r: &Report) -> Vec<String> {
    r.forbidden.iter().map(|f| f.word.clone()).collect()
}

// test_gate.py: test_clean_tree_and_old_debt_pass
#[test]
fn clean_tree_and_old_debt_pass() {
    let d = base();
    assert!(!run(d.path()).failed());
    write(d.path(), &[("lib/web.ex", "defmodule Web do\n  def index(q), do: Search.search_all(q) <> \"\"\nend\n")]);
    assert!(!run(d.path()).failed());
}

// test_gate.py: test_new_debt_blocks (the forbid_added half)
#[test]
fn new_names_block_and_comments_dont() {
    let d = base();
    let search = SEARCH
        .replacen("end\n", "end\n\n  def never_called(y), do: y\n\n  def run_fallback(q), do: search_all(q)\n", 1)
        .replace("def search_all(q), do: q", "def search_all(q), do: run_fallback(q)");
    write(
        d.path(),
        &[
            ("lib/search.ex", &search),
            ("lib/web.ex", "defmodule Web do\n  # the legacy index, a comment: fine\n  def index(q), do: Search.search_all(q)\nend\n"),
        ],
    );
    let rep = run(d.path());
    assert!(rep.failed());
    assert_eq!(words(&rep), vec!["run_fallback", "run_fallback"]);
}

// test_gate.py: test_command_rule_blocks_only_the_files_it_touches
#[test]
fn command_rule_blocks_only_the_files_it_touches() {
    let d = base();
    let cfg = format!(
        "{CFG}[[rule]]\nid = \"kit\"\nkind = \"command\"\nrun = \"sh kit/check.sh\"\nwhen = [\"ui/**\"]\nfiles = \"(ui/[\\\\w.]+)\"\nseverity = \"warn\"\ngate = \"touched\"\n"
    );
    write(
        d.path(),
        &[
            (
                "kit/check.sh",
                "#!/bin/sh\necho 'kit allowlist must match the scan:'\necho 'grew FontFloor ui/a.qml 3'\n[ -f ui/b.bad ] && echo 'grew FontFloor ui/b.qml 1'\nexit 1\n",
            ),
            ("ui/a.qml", "Item {}\n"),
            ("ui/b.qml", "Item {}\n"),
            ("lib/other.ex", "defmodule O do\nend\n"),
            ("seamcheck.toml", &cfg),
        ],
    );
    commit_all(d.path(), "kit");
    let kit = |r: &Report| r.rules.iter().filter(|f| f.rule == "kit").map(|f| f.file.clone()).collect::<Vec<_>>();
    write(d.path(), &[("lib/other.ex", "defmodule O do\n  # x\nend\n")]);
    assert!(kit(&run(d.path())).is_empty()); // didn't touch ui/: not run
    write(d.path(), &[("ui/b.qml", "Item { }\n")]);
    assert!(kit(&run(d.path())).is_empty()); // old debt in ui/a.qml doesn't block
    write(d.path(), &[("ui/a.qml", "Item { }\n")]);
    assert_eq!(kit(&run(d.path())), vec!["ui/a.qml"]);
}

// test_gate.py: test_a_script_outside_include_is_gated
#[test]
fn a_script_outside_include_is_gated() {
    let d = base();
    let cfg = format!("{CFG}[[rule]]\nid = \"names\"\nkind = \"lint\"\nlint = \"shell-name-domain\"\nin = [\"deploy/**\"]\n");
    write(d.path(), &[("seamcheck.toml", &cfg)]);
    commit_all(d.path(), "rule");
    write(d.path(), &[("deploy/agent", "#!/bin/bash\nto=$(tr -dc 'a-z' < \"$REQUEST_DIR/handoff\")\n")]);
    let rep = run(d.path());
    let got: Vec<(String, String, usize)> = rep.rules.iter().map(|f| (f.rule.clone(), f.file.clone(), f.line)).collect();
    assert_eq!(got, vec![("names".to_string(), "deploy/agent".to_string(), 2)]);
}

// test_gate.py: test_waiver_on_the_declaration (a waived name)
#[test]
fn a_waiver_lets_a_name_through() {
    let d = base();
    write(
        d.path(),
        &[(
            "lib/search.ex",
            &SEARCH.replacen("end\n", "end\n\n  # seamcheck: allow gate (the old API, called over RPC)\n  def legacy_api(y), do: y\n", 1),
        )],
    );
    assert!(!run(d.path()).failed());
}

// test_gate.py: test_staged_ignores_unstaged_work
#[test]
fn staged_ignores_unstaged_work() {
    let d = base();
    write(d.path(), &[("lib/search.ex", &SEARCH.replacen("end\n", "end\n\n  def use_fallback(y), do: y\n", 1))]);
    let mut p = load(d.path());
    assert!(!gate(&mut p, "HEAD", true).failed());
    git(d.path(), &["add", "lib/search.ex"]);
    let mut p = load(d.path());
    assert_eq!(words(&gate(&mut p, "HEAD", true)), vec!["use_fallback"]);
}

const MCFG: &str =
    "[project]\nname = \"t\"\ninclude = [\"lib/**\", \"test/**\", \"src/**\", \"app/**\"]\n[gate]\nforbid_added = [\"fallback\", \"legacy\"]\n";

fn commit(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), &[("seamcheck.toml", MCFG)]);
    write(d.path(), files);
    git(d.path(), &["init", "-q"]);
    commit_all(d.path(), "base");
    d
}

// test_gate_moves.py: test_pure_split_passes (the forbid_added half)
#[test]
fn a_pure_split_passes() {
    let b = body();
    let big = format!(
        "def big(a, b):\n{b}\n    return x0\n\n\ndef big2(a, b):\n{b}\n    return x1\n\n\ndef use_fallback(q):\n    return big(q, q) + big2(q, q)\n"
    );
    let d = commit(&[("src/one.py", &big), ("src/main.py", "from one import use_fallback\nuse_fallback(1)\n")]);
    let parts: Vec<&str> = big.split("\n\n\n").collect();
    write(
        d.path(),
        &[("src/one.py", &format!("{}\n", parts[0])), ("src/two.py", &format!("{}\n", parts[1])), ("src/three.py", parts[2])],
    );
    assert!(run(d.path()).forbidden.is_empty());
}

// test_gate_moves.py: test_new_code_in_a_split_still_blocks
#[test]
fn new_code_in_a_split_still_blocks() {
    let d = commit(&[("src/one.py", "def a():\n    return 1\n"), ("src/main.py", "from one import a\na()\n")]);
    write(d.path(), &[("src/two.py", "def b_fallback():\n    return 2\n")]);
    assert_eq!(words(&run(d.path())), vec!["b_fallback"]);
}

// test_gate_moves.py: test_generated_names_are_allowed
#[test]
fn generated_names_are_allowed() {
    let d = commit(&[
        (
            "src/tokens.generated.ts",
            "/* Generated from x by gen.mjs. Do not edit. */\nexport const HUB_THEME_FALLBACKS = {a: 1};\n",
        ),
        ("src/use.ts", "export const z = 1;\n"),
    ]);
    write(
        d.path(),
        &[(
            "src/use.ts",
            "import { HUB_THEME_FALLBACKS } from './tokens.generated';\nexport const z = HUB_THEME_FALLBACKS.a;\n",
        )],
    );
    assert!(run(d.path()).forbidden.is_empty());
    write(d.path(), &[("src/use.ts", "export const myFallback = 1;\n")]);
    assert_eq!(words(&run(d.path())), vec!["myFallback"]);
}

// test_gate_moves.py: test_a_check_pinned_to_a_split_file_blocks
#[test]
fn a_check_pinned_to_a_split_file_blocks() {
    let body: Vec<String> =
        (0..8).map(|i| format!("void Client::op{i}() {{ send(QStringLiteral(\"op{i}\")); }}")).collect();
    let d = commit(&[
        ("src/Client.cpp", &format!("#include \"Client.h\"\n{}\n", body.join("\n"))),
        ("test/client.test.mjs", "const src = readFileSync('src/Client.cpp', 'utf8');\nassert(src.includes('op3'));\n"),
    ]);
    write(
        d.path(),
        &[
            ("src/Client.cpp", &format!("#include \"Client.h\"\n{}\n", body[..2].join("\n"))),
            ("src/ClientMore.cpp", &format!("#include \"Client.h\"\n{}\n", body[2..].join("\n"))),
        ],
    );
    let rep = run(d.path());
    let got: Vec<(String, String)> = rep.moved_refs.iter().map(|r| (r.file.clone(), r.moved_from.clone())).collect();
    assert_eq!(got, vec![("test/client.test.mjs".to_string(), "src/Client.cpp".to_string())]);
    assert!(rep.failed());
    write(d.path(), &[("test/client.test.mjs", "const src = glob('src/Client*.cpp');\nassert(src.includes('op3'));\n")]);
    assert!(run(d.path()).moved_refs.is_empty());
}

// test_gate_moves.py: test_a_command_that_cant_run_here_is_one_warning (the gate half)
#[test]
fn a_command_that_cant_run_here_does_not_block() {
    let d = commit(&[("src/a.ts", "export const a = 1;\n")]);
    let cfg = format!(
        "{MCFG}[[rule]]\nid = \"kit\"\nkind = \"command\"\nwhen = [\"src/**\"]\ngate = \"touched\"\nrun = \"echo \\\"Error [ERR_MODULE_NOT_FOUND]: Cannot find package 'x'\\\" >&2; echo at foo; exit 1\"\n"
    );
    write(d.path(), &[("seamcheck.toml", &cfg), ("src/a.ts", "export const a = 2;\n")]);
    assert!(!run(d.path()).failed());
}

#[test]
fn changed_mode_blocks_only_new_lines() {
    let d = commit(&[
        (
            "seamcheck.toml",
            &format!("{MCFG}[[rule]]\nid = \"no-todo\"\npattern = 'TODO'\nseverity = \"warn\"\ngate = \"changed\"\n"),
        ),
        ("src/a.py", "x = 1  # old\nTODO = 1\n"),
    ]);
    assert!(!run(d.path()).failed());
    write(d.path(), &[("src/a.py", "x = 1  # old\nTODO = 1\ny = TODO\n")]);
    let rep = run(d.path());
    let got: Vec<(String, usize)> = rep.rules.iter().map(|f| (f.file.clone(), f.line)).collect();
    assert_eq!(got, vec![("src/a.py".to_string(), 3)]);
}
