//! tests/test_infra.py and test_infra_pass2.py: lints across deploy scripts,
//! containers and services. Each case is a bug that every gate passed.

mod common;

use serde_json::{json, Map, Value};

use seamcheck::config::Config;
use seamcheck::lint::infra as L;
use seamcheck::project::{Mode, Project};

const AGENT_HEAD: &str = "#!/usr/bin/env bash\nHERE=\"$(dirname \"$0\")\"\nCONF=${VERIDIAN_HA_CONF:-/etc/veridian-ha.env}\n. \"$CONF\"\n";

fn bare(root: &std::path::Path) -> Project {
    Project::with_files(root, Config::default(), vec![], vec![])
}

fn opts(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn none() -> Map<String, Value> {
    Map::new()
}

#[test]
fn reexec_watch_misses_a_sourced_file() {
    let d = tempfile::tempdir().unwrap();
    let p = bare(d.path());
    let shipped = format!(
        "{AGENT_HEAD}. \"$HERE/node-${{ADAPTER:-$NODE}}.sh\"\n[ -f \"$HERE/nodes.env\" ] && . \"$HERE/nodes.env\"\nstamp() {{ stat -c %Y \"$self\" \"$HERE/node-${{ADAPTER:-$NODE}}.sh\"; }}\nexec \"$self\" run\n"
    );
    let hits = L::shell_reexec_watch("deploy/ha/veridian-ha", &shipped, &shipped, &none(), &p).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].1.contains("nodes.env"));
    let fixed = shipped.replace("-$NODE}.sh\"; }", "-$NODE}.sh\" \"$HERE/nodes.env\"; }");
    assert!(L::shell_reexec_watch("deploy/ha/veridian-ha", &fixed, &fixed, &none(), &p).unwrap().is_empty());
}

#[test]
fn reexec_watch_watches_another_file() {
    let d = tempfile::tempdir().unwrap();
    let p = bare(d.path());
    let shipped = format!(
        "{AGENT_HEAD}. \"$HERE/node-${{ADAPTER:-$NODE}}.sh\"\nstamp() {{ stat -c %Y \"$self\" \"$HERE/node-$NODE.sh\"; }}\nexec \"$self\" run\n"
    );
    let msgs: Vec<String> = L::shell_reexec_watch("x", &shipped, &shipped, &none(), &p).unwrap().into_iter().map(|h| h.1).collect();
    assert!(msgs.iter().any(|m| m.contains("the stamp watches $HERE/node-$NODE.sh")), "{msgs:?}");
}

#[test]
fn name_domain() {
    let d = tempfile::tempdir().unwrap();
    let p = bare(d.path());
    let shipped = "#!/bin/bash\nIP=$(eval echo \"\\${${OTHER}_IP:-}\")\nto=$(tr -dc 'a-z' < \"$REQUEST_DIR/handoff\")\npsql -c \"select pg_create_physical_replication_slot('veridian_$NODE')\"\nm=$(tr -dc 'a-z' < \"$MODE_FILE\")\n";
    let mut lines: Vec<usize> = L::shell_name_domain("deploy/ha/x", shipped, shipped, &none(), &p).unwrap().iter().map(|h| h.0).collect();
    lines.sort();
    assert_eq!(lines, vec![2, 3, 4]);
    let fixed = "#!/bin/bash\nk=${OTHER//-/_}\nIP=$(eval echo \"\\${${k}_IP:-}\")\nto=$(tr -dc 'a-z0-9-' < \"$REQUEST_DIR/handoff\")\npsql -c \"select pg_create_physical_replication_slot('veridian_${NODE//-/_}')\"\n";
    assert!(L::shell_name_domain("deploy/ha/x", fixed, fixed, &none(), &p).unwrap().is_empty());
}

#[test]
fn css_dropped_brace() {
    let d = tempfile::tempdir().unwrap();
    let p = bare(d.path());
    let ok = ".a { color: red; }\n/* } */\n.b::after { content: \"}\"; }\n";
    assert!(L::css_braces("s.css", ok, ok, &none(), &p).unwrap().is_empty());
    let bad = ".a { color: red;\n.b { x: 1; }\n";
    assert_eq!(L::css_braces("s.css", bad, bad, &none(), &p).unwrap().iter().map(|h| h.0).collect::<Vec<_>>(), vec![1]);
}

fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, Project) {
    let d = tempfile::tempdir().unwrap();
    common::write(d.path(), files);
    std::fs::write(d.path().join("seamcheck.toml"), "[project]\nname = \"i\"\ninclude = [\"lib/**\"]\n").unwrap();
    let p = Project::load(d.path(), &[]);
    (d, p)
}

const COMPOSE: &str = "services:\n  hub:\n    image: h\n    volumes:\n      - /srv/files/elixir-pot:/home/malice/.local/share/elixir-pot\n      - \"/srv/share:/home/malice/Documents/Veridian Share Files\"\n  other:\n    image: o\n";

#[test]
fn home_paths_mounted() {
    let compose = COMPOSE.replace("      - \"/srv/share:/home/malice/Documents/Veridian Share Files\"\n", "");
    let forms = "defmodule Forms do\n  def dir, do: Path.join([System.user_home!(), \"Documents\", \"Veridian Share Files\"])\n  def pack, do: Path.expand(\"~/.local/share/veridian/voice-pack\")\n  def ok, do: Path.expand(\"~/.local/share/elixir-pot/osa\")\nend\n";
    let (d, p) = project(&[("deploy/compose.yml", &compose), ("lib/forms.ex", forms)]);
    let raw = p.text("lib/forms.ex");
    let mut lines: Vec<usize> = L::home_paths_mounted("lib/forms.ex", &raw, &raw, &opts(json!({"compose": "deploy/compose.yml"})), &p)
        .unwrap()
        .iter()
        .map(|h| h.0)
        .collect();
    lines.sort();
    assert_eq!(lines, vec![2, 3]);
    // the compose index is per load (Python clears its cache): a fresh project sees the new file
    std::fs::write(d.path().join("deploy/compose.yml"), COMPOSE).unwrap();
    let p = Project::load(d.path(), &[]);
    let hits = L::home_paths_mounted(
        "lib/forms.ex",
        &raw,
        &raw,
        &opts(json!({"compose": "deploy/compose.yml", "allow": ["voice-pack"]})),
        &p,
    )
    .unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}

// auth-in-background: skipped, it walks the call graph (exact reach from handle_info) — phase 2.

#[test]
fn stale_module_ref() {
    let mut files: Vec<(String, String)> =
        (0..5).map(|i| (format!("lib/app/m{i}.ex"), format!("defmodule App.M{i} do\nend\n"))).collect();
    files.push((
        "lib/app/doc.ex".into(),
        "defmodule App.Doc do\n  @moduledoc \"Talks to App.Portfolio.Connector over HTTP.\"\n  # App.Old.Thing was removed in 1.2\n  def x, do: App.M1\nend\n".into(),
    ));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let (_d, p) = project(&refs);
    let raw = p.text("lib/app/doc.ex");
    let code = p.joined("lib/app/doc.ex", Mode::Code);
    let hits = L::stale_module_ref("lib/app/doc.ex", &raw, &code, &none(), &p).unwrap();
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![2]);
}

#[test]
fn lockfile_sync() {
    let (d, p) = project(&[
        ("t/package.json", r#"{"devDependencies": {"typescript": "^5.9.3"}}"#),
        ("t/package-lock.json", r#"{"packages": {"": {"devDependencies": {"typescript": "^7.0.2"}}}}"#),
    ]);
    let raw = p.text("t/package-lock.json");
    let hits = L::lockfile_sync("t/package-lock.json", &raw, &raw, &none(), &p).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].1.contains("is '^5.9.3' but package-lock.json records '^7.0.2'"), "{}", hits[0].1);
    let good = r#"{"packages": {"": {"devDependencies": {"typescript": "^5.9.3"}}}}"#;
    std::fs::write(d.path().join("t/package-lock.json"), good).unwrap();
    assert!(L::lockfile_sync("t/package-lock.json", good, good, &none(), &p).unwrap().is_empty());
}

#[test]
fn path_unit_oneshot() {
    let (d, p) = project(&[
        ("u/theme.path", "[Path]\nPathChanged=%h/.config/veridian/theme.json\n"),
        ("u/theme.service", "[Service]\nType=oneshot\nExecStart=%h/.local/bin/apply\n"),
    ]);
    let raw = p.text("u/theme.path");
    assert_eq!(L::path_unit_oneshot("u/theme.path", &raw, &raw, &none(), &p).unwrap().len(), 1);
    std::fs::write(
        d.path().join("u/theme.service"),
        "[Service]\nType=oneshot\n# settles: re-reads theme.json until it is stable\nExecStart=apply\n",
    )
    .unwrap();
    assert!(L::path_unit_oneshot("u/theme.path", &raw, &raw, &none(), &p).unwrap().is_empty());
}

// --- test_infra_pass2.py --------------------------------------------------------------

#[test]
fn toolchain_pin() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("native/core")).unwrap();
    std::fs::write(d.path().join("native/core/rust-toolchain.toml"), "[toolchain]\nchannel = \"1.75.0\"\n").unwrap();
    let p = bare(d.path());
    let script = "cd native/core\nrustup run 1.75 cargo ndk build\n";
    let hits = L::toolchain_pin("scripts/b.sh", script, script, &none(), &p).unwrap();
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![2]);
    let ok = script.replace("1.75 ", "1.75.0 ");
    assert!(L::toolchain_pin("scripts/b.sh", &ok, &ok, &none(), &p).unwrap().is_empty());
    let var = "rustup run \"$toolchain\" cargo build\n";
    assert!(L::toolchain_pin("scripts/b.sh", var, var, &none(), &p).unwrap().is_empty());
}

#[test]
fn test_reads_home() {
    let d = tempfile::tempdir().unwrap();
    let p = bare(d.path());
    let raw = "test \"x\" do\n  # Path.expand(\"~/note\") in a comment is fine\n  File.read!(Path.expand(\"~/.local/share/a\"))\nend\n";
    assert_eq!(L::test_reads_home("test/a_test.exs", raw, raw, &none(), &p).unwrap().iter().map(|h| h.0).collect::<Vec<_>>(), vec![3]);
    let js = "const f = os.homedir();\n";
    assert_eq!(L::test_reads_home("x.test.mjs", js, js, &none(), &p).unwrap().len(), 1);
    let clean = "File.read!(Path.join(System.tmp_dir!(), \"a\"))\n";
    assert!(L::test_reads_home("test/a_test.exs", clean, clean, &none(), &p).unwrap().is_empty());
}
