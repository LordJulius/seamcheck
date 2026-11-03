//! Tidy reports (test_tidy.py, test_slint.py's copies): test-only, unused,
//! dups, tests that reach nothing / read product source, split, remove.

mod common;

use std::path::Path;

use common::write;
use seamcheck::graph::Graph;
use seamcheck::project::Project;
use seamcheck::tidy::{audit, dead, dups, remove, split, Corpus, Tidy};

fn body() -> String {
    (0..14)
        .map(|i| format!("    x{i} = compute(a, b, {i}) + other(x{}, 'k{i}')", if i > 0 { i - 1 } else { 0 }))
        .collect::<Vec<_>>()
        .join("\n")
}

const SEARCH: &str = r#"defmodule Search do
  def search_all(q), do: run(q)

  @doc """
  Old shim.
  """
  def search_with_ai(q, _opts \\ []) do
    ai_helper(q)
  end

  defp ai_helper(q), do: search_all(q)

  def seed!(n), do: n

  def forgotten(x) do
    x + 1
  end

  defp run(q), do: q
end
"#;
const TEST: &str = r#"defmodule SearchTest do
  use ExUnit.Case

  test "shim" do
    assert Search.search_with_ai("x") == "x"
  end

  test "reads source" do
    assert File.read!("lib/search.ex") =~ "shim"
  end

  test "nothing" do
    assert 1 + 1 == 2
  end
end
"#;

fn setup() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let copy = format!("def big(a, b):\n{}\n    return x0\n", body());
    write(
        d.path(),
        &[
            ("lib/search.ex", SEARCH),
            ("lib/web.ex", "defmodule Web do\n  def index(q), do: Search.search_all(q)\nend\n"),
            ("scripts/seed.exs", "Search.seed!(3)\n"),
            ("test/search_test.exs", TEST),
            ("a/copy_one.py", &copy),
            ("b/copy_two.py", &copy),
            ("seamcheck.toml", "[project]\nname = \"t\"\ninclude = [\"lib/**\", \"test/**\", \"a/**\", \"b/**\"]\n"),
        ],
    );
    d
}

fn loaded(root: &Path) -> (Project, Graph) {
    let p = Project::load(root, &[]);
    let g = Graph::load(&p);
    (p, g)
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

// test_tidy.py: test_test_only_and_unused
#[test]
fn test_only_and_unused() {
    let d = setup();
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    let c = Corpus::new(&p);
    let ids: Vec<String> = dead::test_only(&t, &c).into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec!["Search.search_with_ai"]);
    let unused: Vec<String> = dead::unused(&t, &c).into_iter().map(|r| r.id).collect();
    assert!(unused.contains(&"Search.forgotten".to_string()));
    assert!(!unused.contains(&"Search.seed!".to_string())); // a script calls it by name
}

// test_tidy.py: test_tests_audit
#[test]
fn tests_audit() {
    let d = setup();
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    let rows = audit::test_audit(&t, None);
    let row = |n: &str| rows.iter().find(|r| r.id.rsplit(".test ").next() == Some(n)).unwrap().clone();
    assert!(row("shim").reach > 0);
    assert_eq!(row("nothing").reach, 0);
    assert_eq!(row("reads source").reads_source, vec!["lib/search.ex"]);
}

// test_tidy.py: test_subprocess_script_counts_as_reach
#[test]
fn subprocess_script_counts_as_reach() {
    let d = setup();
    write(
        d.path(),
        &[
            ("scripts/lib/tool.py", "def main():\n    print(1)\n"),
            (
                "test/tool_test.exs",
                "defmodule ToolTest do\n  use ExUnit.Case\n  @script \"scripts/lib/tool.py\"\n\n  defp run(args), do: System.cmd(\"python3\", [@script | args])\n\n  test \"records\" do\n    assert {_, 0} = run([\"x\"])\n  end\n\n  test \"nothing\" do\n    assert 1 == 1\n  end\nend\n",
            ),
        ],
    );
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    let rows: Vec<audit::TestRow> = audit::test_audit(&t, None).into_iter().filter(|r| r.file == "test/tool_test.exs").collect();
    let row = |n: &str| rows.iter().find(|r| r.id.rsplit(".test ").next() == Some(n)).unwrap().clone();
    assert_eq!(row("records").runs, vec!["scripts/lib/tool.py"]);
    assert!(row("records").reach > 0);
    assert_eq!(row("nothing").reach, 0); // still a do-nothing test
}

// test_tidy.py: test_dups
#[test]
fn copies_across_files() {
    let d = setup();
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    let groups = dups::dups(&t, 12, 0.45, None);
    assert_eq!(groups[0].files, ["a/copy_one.py".to_string(), "b/copy_two.py".to_string()]);
}

// test_tidy.py: test_remove_with_doc
#[test]
fn remove_with_doc() {
    let d = setup();
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    let ids = vec!["Search.search_with_ai".to_string(), "SearchTest.test shim".to_string()];
    let dry = remove::remove(&t, &ids, true, true).unwrap();
    assert_eq!(dry.len(), 3, "{dry:?}"); // + ai_helper, orphaned
    assert!(dry.iter().any(|x| x.contains("(orphaned)")));
    assert!(read(&d.path().join("lib/search.ex")).contains("search_with_ai")); // dry run wrote nothing
    remove::remove(&t, &ids, false, true).unwrap();
    let after = read(&d.path().join("lib/search.ex"));
    assert!(!after.contains("search_with_ai"));
    assert!(!after.contains("Old shim")); // its @doc heredoc went with it
    assert!(!after.contains("ai_helper")); // orphaned by the removal: cascades
    assert!(after.contains("def seed!"));
    let test_after = read(&d.path().join("test/search_test.exs"));
    assert!(!test_after.contains("test \"shim\""));
    assert!(test_after.contains("test \"reads source\""));
}

// test_tidy.py: test_remove_takes_a_whole_heredoc_attribute
#[test]
fn remove_takes_a_whole_heredoc_attribute() {
    let d = setup();
    let path = d.path().join("lib/prompted.ex");
    std::fs::write(
        &path,
        "defmodule Prompted do\n  @prompt \"\"\"\n  Classify (one of: a, b).\n  Message:\n  \"\"\"\n  @keep 1\n\n  def run(m), do: ask(@prompt <> m)\n\n  def other, do: @keep\n\n  defp ask(p), do: p\nend\n",
    )
    .unwrap();
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    remove::remove(&t, &["Prompted.run".to_string()], false, true).unwrap();
    let after = read(&path);
    assert!(!after.contains("Classify"));
    assert!(!after.contains("\"\"\""));
    assert!(after.contains("@keep 1"));
    assert!(after.contains("def other"));
}

// test_tidy.py: test_split_and_hotspots
#[test]
fn split_and_hotspots() {
    let d = setup();
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    let r = split::split(&t, "lib/search.ex");
    assert_eq!(r["functions"], 6);
    assert!(!split::hotspots(&t, 15).is_empty());
}

// tidy.remove: an id nothing defines is refused, nothing written
#[test]
fn remove_refuses_unknown_ids() {
    let d = setup();
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    let e = remove::remove(&t, &["Nope.nada".to_string()], true, true).unwrap_err();
    assert!(e.starts_with("no such function: Nope.nada"));
}

const SLINT_COPY: &str = r#"export component %s inherits Rectangle {
    in property <string> text;
    background: Palette.face;
    border-color: Palette.rail;
    border-width: 1px;
    height: 56px;
    HorizontalLayout {
        spacing: 8px;
        padding: 12px;
        Text { text: root.text; font-size: 18px; color: Palette.type; }
        Rectangle { width: 4px; background: Palette.accent; }
        Text { text: "more"; font-size: 14px; color: Palette.muted; }
    }
}
"#;

// test_slint.py: SlintCopies.test_copied_element_bodies_are_found
#[test]
fn copied_slint_element_bodies_are_found() {
    let d = tempfile::tempdir().unwrap();
    for (n, comp) in [("a", "ChipA"), ("b", "ChipB")] {
        std::fs::write(d.path().join(format!("{n}.slint")), SLINT_COPY.replace("%s", comp)).unwrap();
    }
    let (p, g) = loaded(d.path());
    let t = Tidy::new(&p, &g);
    let pairs: Vec<(String, String)> =
        dups::dups(&t, 8, 0.45, None).iter().flat_map(|gr| gr.pairs.iter().map(|x| (x.a.clone(), x.b.clone()))).collect();
    assert_eq!(pairs, vec![("a.ChipA".to_string(), "b.ChipB".to_string())]);
}

// Python's round(x, 2) on the similarities: exact ties go to the even digit
#[test]
fn similarity_rounds_like_python() {
    assert_eq!(dups::round2(0.125), 0.12);
    assert_eq!(dups::round2(0.375), 0.38);
    assert_eq!(dups::round2(0.625), 0.62);
    assert_eq!(dups::round2(0.875), 0.88);
    assert_eq!(dups::round2(0.505), 0.51);
    assert_eq!(dups::round2(2.0 / 3.0), 0.67);
}
