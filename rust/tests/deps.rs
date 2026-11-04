//! Dependency use, ported from the Python suite (test_deps.py).
//!
//! Cases from elixir-pot's first run: a dep only another package needs
//! optionally (postgrex for ecto_sql) is a peer, not unused; a dev tool a
//! script runs as a mix task is used; a crate whose [lib] name differs
//! (rustdesk -> librustdesk) is found; a sub-project's code under deploy/ is
//! code, not config.

mod common;

use std::collections::HashMap;
use std::path::Path;

use common::write;
use seamcheck::deps;
use seamcheck::graph::Graph;
use seamcheck::project::Project;

const FILES: &[(&str, &str)] = &[
    (
        "mix.exs",
        r#"defmodule App.MixProject do
  use Mix.Project

  defp deps do
    [
      {:req, "~> 0.5"},
      {:ecto_sql, "~> 3.0"},
      {:postgrex, ">= 0.0.0"},
      {:dialyxir, "~> 1.4", only: :dev, runtime: false},
      {:gone, "~> 1.0"},
      {:mox, "~> 1.0"},
      {:telemetry, "~> 1.0"}
    ]
  end
end
"#,
    ),
    ("deps/req/lib/req.ex", "defmodule Req do\nend\n"),
    ("deps/ecto_sql/lib/ecto/adapters/sql.ex", "defmodule Ecto.Adapters.SQL do\nend\n"),
    ("deps/ecto_sql/mix.exs", "[{:postgrex, \"~> 0.19\", optional: true}]\n"),
    ("deps/postgrex/lib/postgrex.ex", "defmodule Postgrex do\nend\n"),
    ("deps/dialyxir/lib/mix/tasks/dialyzer.ex", "defmodule Mix.Tasks.Dialyzer do\nend\n"),
    ("deps/gone/lib/gone.ex", "defmodule Gone do\nend\n"),
    ("deps/mox/lib/mox.ex", "defmodule Mox do\nend\n"),
    ("deps/telemetry/src/telemetry.erl", "-module(telemetry).\n"),
    (
        "lib/app.ex",
        r#"defmodule App do
  def get(url), do: Req.get(url)
  def emit, do: :telemetry.execute([:a], %{})
  def unused_mock, do: Ecto.Adapters.SQL.query()
end
"#,
    ),
    ("lib/app/web.ex", "defmodule App.Web do\n  def index, do: {App.get(\"u\"), App.emit()}\nend\n"),
    ("test/web_test.exs", "defmodule App.WebTest do\n  test \"i\" do\n    App.Web.index()\n  end\nend\n"),
    ("lib/app/old.ex", "defmodule App.Old do\n  def never_called, do: Mox.defmock(X, for: Y)\nend\n"),
    ("test/app_test.exs", "defmodule AppTest do\n  test \"x\" do\n    Mox.stub()\n  end\nend\n"),
    ("scripts/ci.sh", "mix dialyzer\n"),
    (
        "web/package.json",
        r#"{"dependencies": {"left-pad": "1", "gone-js": "1"}, "devDependencies": {"@types/left-pad": "1"}}"#,
    ),
    ("web/node_modules/left-pad/package.json", r#"{"name": "left-pad"}"#),
    ("web/node_modules/gone-js/package.json", r#"{"name": "gone-js"}"#),
    ("web/node_modules/@types/left-pad/package.json", r#"{"name": "@types/left-pad"}"#),
    ("web/src/a.ts", "import pad from \"left-pad\"\nexport function a() {\n  return pad(\"x\")\n}\n"),
    (
        "rd/Cargo.toml",
        "[package]\nname = \"rd\"\n\n[dependencies]\nrustdesk = { path = \"../rustdesk\" }\nserde = \"1\"\n",
    ),
    ("rd/src/lib.rs", "use librustdesk::embed;\n"),
    ("rustdesk/Cargo.toml", "[package]\nname = \"rustdesk\"\n\n[lib]\nname = \"librustdesk\"\n"),
    ("deploy/plugin/requirements.txt", "requests\n"),
    ("deploy/plugin/main.py", "import requests\n"),
];

fn statuses(root: &Path) -> HashMap<String, String> {
    let p = Project::load(root, &[]);
    let g = Graph::load(&p);
    deps::analyze(&p, &g).into_iter().map(|d| (d.name, d.status)).collect()
}

fn project() -> (tempfile::TempDir, HashMap<String, String>) {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), FILES);
    write(
        d.path(),
        &[("seamcheck.toml", "[project]\nname = \"d\"\ninclude = [\"lib/**\", \"test/**\", \"web/**\", \"rd/**\"]\n")],
    );
    let st = statuses(d.path());
    (d, st)
}

// test_deps.py: DepsTest.test_mix
#[test]
fn mix() {
    let (_d, st) = project();
    assert_eq!(st["req"], "used");
    assert_eq!(st["telemetry"], "used"); // an erlang module call
    assert_eq!(st["postgrex"], "peer");
    assert_eq!(st["dialyxir"], "config");
    assert_eq!(st["gone"], "unused");
}

// test_deps.py: DepsTest.test_dead_and_test_only
#[test]
fn dead_and_test_only() {
    let (_d, st) = project();
    assert_eq!(st["mox"], "dead-only"); // code use sits in a function nothing calls
    assert_eq!(st["ecto_sql"], "dead-only");
}

// test_deps.py: DepsTest.test_npm
#[test]
fn npm() {
    let (_d, st) = project();
    // a top-level import counts as live (the file may be imported for effect)
    assert_eq!(st["left-pad"], "used");
    assert_eq!(st["@types/left-pad"], "used");
    assert_eq!(st["gone-js"], "unused");
}

// test_deps.py: DepsTest.test_cargo_lib_name
#[test]
fn cargo_lib_name() {
    let (_d, st) = project();
    assert_eq!(st["rustdesk"], "used");
    assert_eq!(st["serde"], "unused");
}

// test_deps.py: DepsTest.test_subproject_code_is_code
#[test]
fn subproject_code_is_code() {
    let (_d, st) = project();
    assert_eq!(st["requests"], "used");
}

const MIX: &str =
    "defmodule P.MixProject do\n  defp deps do\n    [\n      {:langchain, \"~> 0.3\"}\n    ]\n  end\nend\n";
const PKG: &str = r#"{"devDependencies": {"typescript": "5"}, "scripts": {"build": "tsc -p ."}}"#;
const P_EX: &str = "defmodule P do\n  def x, do: LangChain.run()\nend\n";

// test_deps.py: UninstalledTest.test_nothing_installed_is_one_warning
#[test]
fn nothing_installed_is_one_warning() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        &[
            ("pkg/mix.exs", MIX),
            ("pkg/lib/p.ex", P_EX),
            ("web/package.json", PKG),
            (
                "seamcheck.toml",
                "[project]\nname = \"u\"\ninclude = [\"pkg/**\", \"web/**\"]\n[[rule]]\nid = \"lean\"\nkind = \"deps\"\nseverity = \"error\"\n",
            ),
        ],
    );
    let p = Project::load(d.path(), &[]);
    let g = Graph::load(&p);
    let st: HashMap<String, String> = deps::analyze(&p, &g).into_iter().map(|d| (d.name, d.status)).collect();
    assert_eq!(st["langchain"], "uninstalled");
    assert_eq!(st["typescript"], "uninstalled");
    let r = p.config.rules().find(|r| seamcheck::config::id(r) == "lean").unwrap();
    assert_eq!(deps::rule(&p, &g, r).len(), 2);
    // as `check` runs it
    assert!(seamcheck::rules::deferred_rules(&p).is_empty());
    let found = seamcheck::rules::run_rules(&p, None, None);
    assert!(found.iter().all(|f| f.severity != "error"), "{found:?}");
    assert_eq!(found.iter().filter(|f| f.rule == "lean").count(), 2); // one note per manifest
}

// test_deps.py: UninstalledTest.test_worktree_reads_main_checkout
#[test]
fn worktree_reads_main_checkout() {
    let base = tempfile::tempdir().unwrap();
    let main = base.path().join("main");
    write(
        &main,
        &[
            ("pkg/mix.exs", MIX),
            ("pkg/lib/p.ex", P_EX),
            ("pkg/deps/langchain/lib/lang_chain.ex", "defmodule LangChain do\nend\n"),
            ("web/package.json", PKG),
            ("web/node_modules/typescript/package.json", r#"{"name": "typescript", "bin": {"tsc": "bin/tsc"}}"#),
            (".gitignore", "deps/\nnode_modules/\n"),
            ("seamcheck.toml", "[project]\nname = \"w\"\ninclude = [\"pkg/**\", \"web/**\"]\n"),
        ],
    );
    common::git(&main, &["init", "-q"]);
    common::commit_all(&main, "x");
    let wt = base.path().join("wt");
    common::git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert!(!wt.join("pkg/deps").exists());
    let st = statuses(&wt);
    assert!(!["unused", "uninstalled"].contains(&st["langchain"].as_str()), "{}", st["langchain"]); // LangChain is its module
    assert_eq!(st["typescript"], "config");
}

#[test]
fn report_lists_the_actionable_ones_per_manifest() {
    let (_d, _) = project();
    let p = Project::load(_d.path(), &[]);
    let g = Graph::load(&p);
    let found = deps::analyze(&p, &g);
    let text = deps::report(&found, false, None);
    assert!(text.starts_with("mix.exs  (7 deps: 2 used, 1 unused, 2 dead-only, 1 peer, 1 config)"), "{text}");
    assert!(text.contains("  dead-only mox"), "{text}");
    assert!(deps::report(&found, false, Some("nope")).contains("no dependency named 'nope'"));
}
