//! The Elixir adapter and the call graph, ported from the Python suite
//! (test_resolution.py, test_module_miss.py, test_imports.py's Elixir cases).

mod common;

use std::collections::BTreeMap;

use common::{dedent, write};
use seamcheck::graph::Graph;
use seamcheck::project::Project;

fn load(files: &[(&str, &str)]) -> (tempfile::TempDir, Project, Graph) {
    let d = tempfile::tempdir().unwrap();
    let files: Vec<(String, String)> = files.iter().map(|(n, b)| (n.to_string(), dedent(b))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, b)| (n.as_str(), b.as_str())).collect();
    write(d.path(), &refs);
    write(d.path(), &[("seamcheck.toml", "[project]\nname = \"t\"\ninclude = [\"**\"]\n")]);
    let p = Project::load(d.path(), &[]);
    let g = Graph::load(&p);
    (d, p, g)
}

/// {callee id: exact} for the one function whose id ends with `suffix` (test_resolution.edges)
fn edges(g: &Graph, suffix: &str) -> BTreeMap<String, bool> {
    let ids: Vec<&String> = g.funcs.keys().filter(|f| f.ends_with(suffix)).collect();
    assert_eq!(ids.len(), 1, "{suffix}: {ids:?}");
    g.edges.get(ids[0]).map(|es| es.iter().map(|(t, _, ex)| (t.clone(), *ex)).collect()).unwrap_or_default()
}

fn to(g: &Graph, fid: &str, name: &str) -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = g
        .edges
        .get(fid)
        .map(|es| es.iter().filter(|(t, _, _)| t.ends_with(&format!(".{name}"))).map(|(t, _, e)| (t.clone(), *e)).collect())
        .unwrap_or_default();
    v.sort();
    v.dedup();
    v
}

// test_resolution.py: test_an_elixir_sigil_is_not_a_call
#[test]
fn an_elixir_sigil_is_not_a_call() {
    let (_d, _p, g) = load(&[
        ("lib/a.ex", "defmodule A do\n  def kinds, do: ~w(a b c)\nend\n"),
        ("lib/b.ex", "defmodule B do\n  def w(x), do: x\nend\n"),
    ]);
    assert!(edges(&g, "A.kinds").is_empty());
}

// test_resolution.py: test_use_brings_in_what_its_using_block_imports
#[test]
fn use_brings_in_what_its_using_block_imports() {
    let (_d, _p, g) = load(&[
        (
            "test/support/data_case.ex",
            r#"
            defmodule App.DataCase do
              use ExUnit.CaseTemplate

              using do
                quote do
                  import Ecto.Query
                  import App.DataCase
                end
              end

              def await_tasks!, do: :ok
            end
            "#,
        ),
        (
            "test/session_test.exs",
            r#"
            defmodule App.SessionTest do
              use App.DataCase

              test "the session ends" do
                await_tasks!()
                from(x in "t")
              end
            end
            "#,
        ),
    ]);
    let want: BTreeMap<String, bool> = [("App.DataCase.await_tasks!".to_string(), true)].into();
    assert_eq!(edges(&g, "test the session ends"), want);
}

const MISS: &[(&str, &str)] = &[
    ("lib/app/policy.ex", "defmodule App.Policy do\n  def reject(x), do: x\nend\n"),
    ("lib/app/release.ex", "defmodule App.Release do\n  def rollback(x), do: x\nend\n"),
    ("lib/app/settings.ex", "defmodule App.Settings do\n  def get(k), do: k\nend\n"),
    (
        "lib/app/user.ex",
        "defmodule App.User do\n  alias App.Settings\n  alias App.Policy\n\n  def library(xs) do\n    \
         Enum.reject(xs, & &1)\n    App.Repo.rollback(xs)\n    Map.get(xs, :a)\n  end\n\n  def project(xs) do\n    \
         Policy.reject(xs)\n    Settings.get(:a)\n  end\nend\n",
    ),
];

// test_module_miss.py: test_library_module_is_nothing
#[test]
fn a_library_module_is_nothing() {
    let (_d, _p, g) = load(MISS);
    for n in ["reject", "rollback", "get"] {
        assert!(to(&g, "App.User.library", n).is_empty(), "{n}");
    }
}

// test_module_miss.py: test_project_module_still_exact
#[test]
fn a_project_module_is_still_exact() {
    let (_d, _p, g) = load(MISS);
    assert_eq!(to(&g, "App.User.project", "reject"), vec![("App.Policy.reject".to_string(), true)]);
    assert_eq!(to(&g, "App.User.project", "get"), vec![("App.Settings.get".to_string(), true)]);
}

const IMPORTS: &[(&str, &str)] = &[
    ("lib/settings.ex", "\ndefmodule App.Settings do\n  def get(key, default \\\\ nil), do: key || default\nend\n"),
    ("lib/helpers.ex", "\ndefmodule App.Helpers do\n  def fixture, do: :ok\nend\n"),
    (
        "test/settings_test.exs",
        "\ndefmodule App.SettingsTest do\n  use ExUnit.Case\n  alias App.{Settings, Helpers}\n  import App.Helpers\n  \
         alias App.{\n    Other\n  }\n\n  setup do\n    fixture()\n    :ok\n  end\n\n  test \"captures\" do\n    \
         Enum.map([1], &Settings.get/1)\n  end\n\n  test \"reads a key\" do\n    assert Settings.get(\"k\") == \"k\"\n  \
         end\n\n  test \"no block\", :skip\nend\n",
    ),
];

fn exact_callees(g: &Graph, fid: &str) -> Vec<String> {
    g.edges.get(fid).map(|es| es.iter().filter(|e| e.2).map(|e| e.0.clone()).collect()).unwrap_or_default()
}

// test_imports.py: test_exunit_blocks_are_test_functions
#[test]
fn exunit_blocks_are_test_functions() {
    let (_d, _p, g) = load(IMPORTS);
    let t = g.func("App.SettingsTest.test reads a key").expect("the test is a function");
    assert_eq!(t.kind, "test");
    assert!(g.func("App.SettingsTest.test no block").is_none());
}

// test_imports.py: test_brace_alias_resolves, test_capture_is_a_call, test_elixir_import
#[test]
fn brace_aliases_captures_and_imports_resolve() {
    let (_d, _p, g) = load(IMPORTS);
    assert!(exact_callees(&g, "App.SettingsTest.test reads a key").contains(&"App.Settings.get".to_string()));
    assert!(exact_callees(&g, "App.SettingsTest.test captures").contains(&"App.Settings.get".to_string()));
    assert!(exact_callees(&g, "App.SettingsTest.setup").contains(&"App.Helpers.fixture".to_string()));
}

// test_imports.py: test_multiline_brace_alias
#[test]
fn a_multiline_brace_alias() {
    let (_d, _p, g) = load(IMPORTS);
    assert_eq!(g.modules["App.SettingsTest"].aliases["Other"], "App.Other");
}

// test_module_miss.py: test_pattern (core._MODULE_Q), through the resolver: a
// miss on an all-caps qualifier is a guess, on a module name nothing
#[test]
fn an_all_caps_qualifier_that_misses_is_a_guess() {
    let (_d, _p, g) = load(&[
        ("lib/a.ex", "defmodule A do\n  def go(x) do\n    IO.shout(x)\n    Enum.shout(x)\n  end\nend\n"),
        ("lib/b.ex", "defmodule B do\n  def shout(x), do: x\nend\n"),
    ]);
    let got = g.edges.get("A.go").cloned().unwrap_or_default();
    assert_eq!(got, vec![("B.shout".to_string(), 3, false)]);
}

// core.load: a second module of one name is `name@file`; the clauses of one
// function are one function
#[test]
fn a_second_module_of_one_name_and_merged_clauses() {
    let (_d, _p, g) = load(&[
        ("lib/a.ex", "defmodule A do\n  def f(0), do: 0\n  def f(n), do: g(n)\n  defp g(n), do: n\nend\n"),
        ("lib/b.ex", "defmodule A do\n  def h, do: :ok\nend\n"),
    ]);
    assert!(g.modules.contains_key("A@lib/b.ex"));
    assert!(g.func("A@lib/b.ex.h").is_some());
    let f = g.func("A.f").unwrap();
    assert_eq!((f.start, f.end), (2, 3));
    assert_eq!(edges(&g, "A.f"), [("A.g".to_string(), true)].into());
    assert_eq!(g.func("A.g").unwrap().kind, "private");
}
