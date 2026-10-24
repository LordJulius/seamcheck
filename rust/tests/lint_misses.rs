//! tests/test_lint_misses.py: QML, shell and Elixir lints from the bugs of
//! 2026-10-12..10-19, each with the real bug (before) and its fix (after).

mod common;
use common::{rule, run, Got};

const CMAKE: &str = "qt_add_qml_module(app
  URI VeridianOsd
  VERSION 1.0
  QML_FILES
    qml/Main.qml
    qml/Tokens.qml
    qml/Theme.qml
    qml/VButton.qml
    qml/FoodBookDive.qml
    qml/HubMove.qml
    qml/HubMove.js
)
set_source_files_properties(qml/Tokens.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)
";

fn files() -> Vec<(&'static str, String)> {
    vec![
        ("CMakeLists.txt", CMAKE.to_string()),
        ("qml/Tokens.qml", "pragma Singleton\nimport QtQuick\nQtObject { property int gap: 8 }\n".into()),
        ("qml/Theme.qml", "pragma Singleton\nimport QtQuick\nimport VeridianOsd\nQtObject { property int gap: Tokens.gap }\n".into()),
        ("qml/HubMove.qml", "import QtQuick\nItem { }\n".into()),
        ("src/MapHost.h", "class MapHost : public QQuickItem {\n  Q_OBJECT\n  QML_ELEMENT\n};\n".into()),
    ]
}

fn with(mut base: Vec<(&'static str, String)>, extra: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
    for (k, v) in extra {
        base.retain(|(n, _)| n != k);
        base.push((k, v.to_string()));
    }
    base
}

fn go(files: &[(&'static str, String)], r: &str) -> Got {
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (*a, b.as_str())).collect();
    run(&refs, r)
}

fn at(got: &Got, id: &str) -> Vec<(String, usize)> {
    got.get(id).map(|v| v.iter().map(|(f, l, _)| (f.clone(), *l)).collect()).unwrap_or_default()
}

#[test]
fn unknown_type() {
    let page = "import QtQuick\nimport QtQuick.Controls\nimport VeridianOsd\nItem {\n  id: page\n  component Row2: Row { }\n  Rectangle { }\n  delegate: Button { }\n  Behavior on x { NumberAnimation { } }\n  VButton { }\n  MapHost { }\n  Row2 { }\n  Sheet { visible: true }\n}\n";
    let fs = with(files(), &[("qml/VButton.qml", "import QtQuick\nRectangle { }\n"), ("qml/FoodBookDive.qml", page)]);
    let got = go(&fs, &rule("qml-unknown-type"));
    assert_eq!(at(&got, "qml-unknown-type"), vec![("qml/FoodBookDive.qml".to_string(), 13)]);
    let fixed_page = page.replace("  Sheet { visible: true }\n", "");
    let fixed = with(fs, &[("qml/FoodBookDive.qml", &fixed_page)]);
    assert!(!go(&fixed, &rule("qml-unknown-type")).contains_key("qml-unknown-type"));
}

#[test]
fn unknown_type_skips_modules_it_cannot_see() {
    let fs = with(files(), &[("qml/Main.qml", "import QtQuick\nimport MapLibre\nItem { MapView { } }\n")]);
    assert!(!go(&fs, &rule("qml-unknown-type")).contains_key("qml-unknown-type"));
}

#[test]
fn singleton_without_its_module_import() {
    let button = "import QtQuick\nRectangle {\n  radius: Tokens.gap\n}\n";
    let got = go(&with(files(), &[("qml/VButton.qml", button)]), &rule("qml-singleton-import"));
    assert_eq!(at(&got, "qml-singleton-import"), vec![("qml/VButton.qml".to_string(), 3)]);
    let fixed = button.replace("import QtQuick\n", "import QtQuick\nimport VeridianOsd\n");
    assert!(!go(&with(files(), &[("qml/VButton.qml", &fixed)]), &rule("qml-singleton-import")).contains_key("qml-singleton-import"));
}

#[test]
fn js_library_named_like_a_type() {
    let js = ".pragma library\nfunction ok() { return 1 }\n";
    let got = go(&with(files(), &[("qml/HubMove.js", js)]), &rule("qml-js-shadows-type"));
    assert_eq!(at(&got, "qml-js-shadows-type").iter().map(|x| x.0.clone()).collect::<Vec<_>>(), vec!["qml/HubMove.js"]);
    assert!(!go(&with(files(), &[("qml/HubMoveRules.js", js)]), &rule("qml-js-shadows-type")).contains_key("qml-js-shadows-type"));
}

#[test]
fn pkill_inside_the_command_string_it_runs_in() {
    let bad = "#!/bin/bash\nssh tvlaptop \"bash -lc 'cd ~/bbb-live; pkill -f \\\"mix phx.server\\\"; mix phx.server'\"\n";
    let got = run(&[("scripts/restart.sh", bad)], &rule("shell-pkill-self"));
    assert_eq!(at(&got, "shell-pkill-self"), vec![("scripts/restart.sh".to_string(), 2)]);
    let good = bad.replace("\\\"mix phx.server\\\"", "'[m]ix phx.server'");
    assert!(!run(&[("scripts/restart.sh", &good)], &rule("shell-pkill-self")).contains_key("shell-pkill-self"));
}

#[test]
fn pkill_naming_the_script_itself() {
    let got = run(&[("scripts/restart-server.sh", "#!/bin/bash\npkill -f server\n")], &rule("shell-pkill-self"));
    assert!(got.contains_key("shell-pkill-self"));
    let ok = run(&[("scripts/restart.sh", "#!/bin/bash\npkill -f 'mix phx.server'\n")], &rule("shell-pkill-self"));
    assert!(!ok.contains_key("shell-pkill-self"));
}

#[test]
fn a_failure_logged_only_at_debug() {
    let bad = "defmodule Player do\n  require Logger\n\n  def poll do\n    save()\n  rescue\n    e ->\n      Logger.debug(\"poll failed: #{Exception.message(e)}\")\n      :not_playing\n  end\nend\n";
    let got = run(&[("lib/player.ex", bad)], &rule("elixir-rescue-debug"));
    assert_eq!(at(&got, "elixir-rescue-debug"), vec![("lib/player.ex".to_string(), 8)]);
    let good = bad.replace("Logger.debug", "Logger.warning");
    assert!(!run(&[("lib/player.ex", &good)], &rule("elixir-rescue-debug")).contains_key("elixir-rescue-debug"));
    let reraised = bad.replace("      :not_playing\n", "      reraise e, __STACKTRACE__\n");
    assert!(!run(&[("lib/player.ex", &reraised)], &rule("elixir-rescue-debug")).contains_key("elixir-rescue-debug"));
}
