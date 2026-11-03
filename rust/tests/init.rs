//! `seamcheck init`, ported from the Python suite (test_cli.py's init and
//! hook tests, test_packs.py's InitDetectsPacks).

mod common;

use std::path::Path;
use std::process::Command;

use common::write;
use seamcheck::init;

fn run(root: &Path, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_seamcheck")).args(args).current_dir(root).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

const CLI: &[(&str, &str)] = &[
    ("src/app/store.py", "def save(x):\n    db.execute(x)\n"),
    ("src/app/view.py", "from .store import save\n\n\ndef show(x):\n    db.execute(x)\n    return save(x)\n"),
    ("tests/test_view.py", "from app.view import show\n\n\ndef test_show():\n    show(1)\n"),
    ("web/src/main.ts", "export function main() {\n  return 1;\n}\n"),
];

// test_cli.py: Cli.test_init_writes_a_config_that_loads
#[test]
fn init_writes_a_config_that_loads() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), CLI);
    let (code, out, err) = run(d.path(), &["init"]);
    assert_eq!(code, 0, "{err}");
    let text = std::fs::read_to_string(d.path().join("seamcheck.toml")).unwrap();
    assert!(text.contains(r#"include = ["src/**", "tests/**", "web/**"]"#), "{text}");
    assert!(text.contains(r#"areas = { src = ["src/**"], web = ["web/**"] }"#), "{text}");
    assert!(out.contains("Python"), "{out}");
    let (code, out, err) = run(d.path(), &["check"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(!out.contains("config"), "{out}");
    // a second init leaves the file alone
    let (_, out, _) = run(d.path(), &["init"]);
    assert!(out.contains("exists"), "{out}");
}

// test_cli.py: Cli.test_hook_never_overwrites
#[test]
fn hook_never_overwrites() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".git/hooks")).unwrap();
    let root = d.path().to_string_lossy().into_owned();
    assert!(init::install_hook(&root).contains("installed"));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(d.path().join(".git/hooks/pre-commit")).unwrap().permissions().mode();
    assert_ne!(mode & 0o100, 0);
    assert!(init::install_hook(&root).contains("exists"));
}

// test_packs.py: InitDetectsPacks.test_init_writes_the_frameworks_it_finds
#[test]
fn init_writes_the_frameworks_it_finds() {
    let files: &[(&str, &str)] = &[
        ("hub/mix.exs", "defp deps do\n  [{:phoenix, \"~> 1.7\"}, {:jason, \"~> 1.4\"}]\nend\n"),
        ("hub/lib/app.ex", "defmodule App do\nend\n"),
        ("web/package.json", r#"{"dependencies": {"react": "^18.3.1", "react-dom": "^18.3.1"}}"#),
        ("web/src/main.tsx", "export const x = 1;\n"),
        ("desk/CMakeLists.txt", "find_package(Qt6 REQUIRED COMPONENTS Quick)\n"),
        ("android/app/build.gradle.kts", "android {\n  buildFeatures { compose = true }\n}\n"),
        ("tools/package.json", r#"{"dependencies": {"preact": "^10"}}"#),
    ];
    let d = tempfile::tempdir().unwrap();
    write(d.path(), files);
    let mut names: Vec<String> = files.iter().map(|f| f.0.to_string()).collect();
    names.sort();
    // a CMake project that links Qt Quick but has no QML is not a QML app
    assert_eq!(init::detect_packs(d.path(), &names), ["compose", "phoenix", "react"]);
    write(d.path(), &[("desk/Main.qml", "import QtQuick\nItem { }\n")]);
    let (text, summary) = init::render(d.path());
    assert!(text.contains(r#"packs = ["compose", "phoenix", "qml", "react", "security"]"#), "{text}");
    assert!(summary.iter().any(|s| s == "rule packs: compose, phoenix, qml, react, security"), "{summary:?}");
    write(d.path(), &[("seamcheck.toml", &text)]);
    let (cfg, problems) = seamcheck::config::load(d.path(), &[]);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(cfg.rules().any(|r| seamcheck::config::id(r) == "phoenix/ets-owner"));
}

#[test]
fn unusual_test_folders_join_the_tidy_tests() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        &[("e2e/a.ts", "export const a = 1;\n"), ("src/b.ts", "export const b = 2;\n"), ("main.py", "x = 1\n")],
    );
    let (text, summary) = init::render(d.path());
    assert!(text.contains(r#"include = ["e2e/**", "src/**", "*.py"]"#), "{text}");
    assert!(text.contains("\n[tidy]\ntests = [\"test/**\", "), "{text}");
    assert!(text.contains(r#""testutils/**", "**/testutils/**", "e2e/**"]"#), "{text}");
    assert!(!text.contains("areas ="), "{text}");
    assert_eq!(summary[0], "     2 files  TypeScript");
}
