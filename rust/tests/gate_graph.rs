//! The land gate's graph half (test_gate.py, test_gate_moves.py,
//! test_gate_reach.py, test_gate_exactness.py): dead code, copies and
//! do-nothing tests in the changed lines, and the exactness guard.

mod common;

use std::path::Path;

use common::{commit_all, git, load, write};
use seamcheck::gate::{gate, gate_with, Report};
use seamcheck::graph::Graph;
use seamcheck::tidy::exactness::{Book, DROP};
use seamcheck::tidy::{audit, Tidy};

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

fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), files);
    git(d.path(), &["init", "-q"]);
    commit_all(d.path(), "base");
    d
}

fn base() -> tempfile::TempDir {
    let one = format!("def big(a, b):\n{}\n    return x0\n\n\ndef main():\n    big(1, 2)\n", body());
    repo(&[("lib/search.ex", SEARCH), ("lib/web.ex", WEB), ("a/one.py", &one), ("seamcheck.toml", CFG)])
}

fn run(root: &Path) -> Report {
    common::scratch_cache();
    let mut p = load(root);
    gate(&mut p, "HEAD", false)
}

fn ids(rows: &[seamcheck::tidy::dead::Row]) -> Vec<String> {
    rows.iter().map(|r| r.id.clone()).collect()
}

// test_gate.py: test_new_debt_blocks
#[test]
fn new_debt_blocks() {
    let d = base();
    let search = SEARCH
        .replacen("end\n", "end\n\n  def never_called(y), do: y\n\n  def run_fallback(q), do: search_all(q)\n", 1)
        .replace("def search_all(q), do: q", "def search_all(q), do: run_fallback(q)");
    write(
        d.path(),
        &[
            ("lib/search.ex", &search),
            ("b/two.py", &format!("def big(a, b):\n{}\n    return x0\n", body())),
            ("lib/web.ex", "defmodule Web do\n  # the legacy index, a comment: fine\n  def index(q), do: Search.search_all(q)\nend\n"),
        ],
    );
    let rep = run(d.path());
    assert!(rep.failed());
    assert_eq!(ids(&rep.unused), vec!["Search.never_called"]);
    assert_eq!(rep.forbidden.iter().map(|f| f.word.as_str()).collect::<Vec<_>>(), vec!["run_fallback", "run_fallback"]);
    assert_eq!(rep.dups.len(), 1);
}

// test_gate.py: test_editing_old_debt_is_not_new_debt
#[test]
fn editing_old_debt_is_not_new_debt() {
    let d = base();
    write(d.path(), &[("lib/search.ex", &SEARCH.replace("x + 1", "x + 2"))]);
    assert!(!run(d.path()).failed());
}

// test_gate.py: test_waiver_on_the_declaration
#[test]
fn waiver_on_the_declaration() {
    let d = base();
    let s = SEARCH.replacen("end\n", "end\n\n  # seamcheck: allow gate (called over RPC)\n  def remote_only(y), do: y\n", 1);
    write(d.path(), &[("lib/search.ex", &s)]);
    assert!(!run(d.path()).failed());
}

// test_gate.py: test_started_child_counts_as_reached
#[test]
fn started_child_counts_as_reached() {
    let d = base();
    write(
        d.path(),
        &[
            (
                "lib/worker.ex",
                "defmodule Worker do\n  use GenServer\n  def start_link(o), do: GenServer.start_link(__MODULE__, o)\n  def init(o), do: {:ok, o}\nend\n",
            ),
            (
                "test/worker_test.exs",
                "defmodule WorkerTest do\n  use ExUnit.Case\n\n  test \"starts\" do\n    pid = start_supervised!({Worker, name: :w})\n    assert is_pid(pid)\n  end\nend\n",
            ),
        ],
    );
    assert!(run(d.path()).tests.is_empty());
}

// test_gate.py: test_staged_ignores_unstaged_work (the graph half)
#[test]
fn staged_ignores_unstaged_dead_code() {
    let d = base();
    write(
        d.path(),
        &[
            ("lib/search.ex", &SEARCH.replacen("end\n", "end\n\n  def never_called(y), do: y\n", 1)),
            ("b/two.py", &format!("def big(a, b):\n{}\n    return x0\n", body())),
        ],
    );
    common::scratch_cache();
    let mut p = load(d.path());
    assert!(!gate(&mut p, "HEAD", true).failed());
    git(d.path(), &["add", "lib/search.ex"]);
    let mut p = load(d.path());
    let rep = gate(&mut p, "HEAD", true);
    assert_eq!(ids(&rep.unused), vec!["Search.never_called"]);
    assert!(rep.dups.is_empty());
}

const MCFG: &str =
    "[project]\nname = \"t\"\ninclude = [\"lib/**\", \"test/**\", \"src/**\", \"app/**\"]\n[gate]\nforbid_added = [\"fallback\", \"legacy\"]\n";

fn moves(files: &[(&str, &str)]) -> tempfile::TempDir {
    let mut all = vec![("seamcheck.toml", MCFG)];
    all.extend_from_slice(files);
    repo(&all)
}

// test_gate_moves.py: test_pure_split_passes
#[test]
fn a_pure_split_has_no_copies_or_dead_code() {
    let b = body();
    let big = format!(
        "def big(a, b):\n{b}\n    return x0\n\n\ndef big2(a, b):\n{b}\n    return x1\n\n\ndef use_fallback(q):\n    return big(q, q) + big2(q, q)\n"
    );
    let d = moves(&[("src/one.py", &big), ("src/main.py", "from one import use_fallback\nuse_fallback(1)\n")]);
    let parts: Vec<&str> = big.split("\n\n\n").collect();
    write(
        d.path(),
        &[("src/one.py", &format!("{}\n", parts[0])), ("src/two.py", &format!("{}\n", parts[1])), ("src/three.py", parts[2])],
    );
    let rep = run(d.path());
    assert!(rep.forbidden.is_empty());
    assert!(rep.dups.is_empty());
    assert!(rep.unused.is_empty());
}

// test_gate_moves.py: test_generated_parsers_are_not_copies
#[test]
fn generated_parsers_are_not_copies() {
    let gen = "/* Generated from api/schema/x.json by api/generate.mjs. Do not edit. */\n";
    let parser = |n: usize| format!("def parse_{n}(a, b):\n{}\n    return x{}\n\n\n", body(), n % 3);
    let d = moves(&[("src/one.generated.py", &format!("{gen}{}", parser(1))), ("src/main.py", "x = 1\n")]);
    write(d.path(), &[("src/two.generated.py", &format!("{gen}{}{}", parser(2), parser(4)))]);
    assert!(run(d.path()).dups.is_empty());
    write(d.path(), &[("src/hand.py", &format!("{}{}", parser(5), parser(7)))]);
    assert!(!run(d.path()).dups.is_empty());
}

// test_gate_moves.py: test_exunit_setup_is_not_a_test
#[test]
fn exunit_setup_is_not_a_test() {
    let d = moves(&[
        ("lib/a.ex", "defmodule A do\n  def go, do: 1\nend\n"),
        ("test/a_test.exs", "defmodule ATest do\n  use ExUnit.Case\n  test \"go\" do\n    A.go()\n  end\nend\n"),
    ]);
    write(
        d.path(),
        &[(
            "test/a_test.exs",
            "defmodule ATest do\n  use ExUnit.Case\n  setup do\n    System.put_env(\"X\", \"1\")\n    :ok\n  end\n\n  test \"go\" do\n    A.go()\n  end\nend\n",
        )],
    );
    assert!(run(d.path()).tests.is_empty());
}

// test_gate_moves.py: test_kotlin_enum_only_test_reaches_the_enum
#[test]
fn kotlin_enum_only_test_reaches_the_enum() {
    let d = moves(&[
        ("app/Area.kt", "package app\n\nenum class Area(val wire: String) { HUB(\"hub\"), OSA(\"osa\") }\n"),
        ("app/AreaTest.kt", "package app\n\nclass AreaTest {\n}\n"),
    ]);
    write(
        d.path(),
        &[(
            "app/AreaTest.kt",
            "package app\n\nimport org.junit.Test\n\nclass AreaTest {\n    @Test\n    fun wireNames() {\n        check(Area.HUB.wire == \"hub\")\n    }\n}\n",
        )],
    );
    assert!(run(d.path()).tests.is_empty());
}

const RCFG: &str =
    "[project]\nname = \"t\"\ninclude = [\"lib/**\", \"test/**\", \"scripts/**\"]\n[gate]\nforbid_added = [\"fallback\", \"shim\"]\n";
const ROUTER: &str = "defmodule AppWeb.Router do\n  use AppWeb, :router\n\n  pipeline :api do\n    plug :accepts, [\"json\"]\n  end\n\n  scope \"/api\", AppWeb do\n    pipe_through :api\n    get \"/car\", CarController, :show\n    post \"/car/:id/fill\", CarController, :fill\n  end\nend\n";
const CONTROLLER: &str =
    "defmodule AppWeb.CarController do\n  def show(conn, _), do: Car.read(conn)\n  def fill(conn, _), do: Car.fill(conn)\nend\n";
const CAR: &str = "defmodule Car do\n  def read(c), do: c\n  def fill(c), do: c\nend\n";

fn reach_repo() -> tempfile::TempDir {
    repo(&[
        ("seamcheck.toml", RCFG),
        ("lib/router.ex", ROUTER),
        ("lib/car_controller.ex", CONTROLLER),
        ("lib/car.ex", CAR),
        ("scripts/lib/tool.py", "print('ok')\n"),
        ("deploy/hub/compose.yml", "services: {}\n"),
    ])
}

fn audit_rows(root: &Path) -> Vec<audit::TestRow> {
    let p = load(root);
    let g = Graph::load(&p);
    let t = Tidy::new(&p, &g);
    audit::test_audit(&t, None)
}

// test_gate_reach.py: test_a_script_named_from_the_tests_folder_is_reached
#[test]
fn a_script_named_from_the_tests_folder_is_reached() {
    let d = reach_repo();
    write(
        d.path(),
        &[(
            "test/deploy/tool_test.exs",
            "defmodule ToolTest do\n  use ExUnit.Case\n  @tool Path.expand(\"../../scripts/lib/tool.py\", __DIR__)\n\n  test \"the tool prints ok\" do\n    {out, 0} = System.cmd(\"python3\", [@tool])\n    assert out == \"ok\\n\"\n  end\nend\n",
        )],
    );
    assert!(run(d.path()).tests.is_empty());
}

// test_gate_reach.py: test_an_http_test_reaches_the_controller_through_the_router
#[test]
fn an_http_test_reaches_the_controller_through_the_router() {
    let d = reach_repo();
    write(
        d.path(),
        &[(
            "test/car_controller_test.exs",
            "defmodule AppWeb.CarControllerTest do\n  use AppWeb.ConnCase\n\n  test \"read the car back\", %{conn: conn} do\n    assert conn |> get(~p\"/api/car\") |> json_response(200)\n    assert post(build_conn(), \"/api/car/#{7}/fill\", %{}) |> json_response(200)\n  end\nend\n",
        )],
    );
    let rows = audit_rows(d.path());
    assert_eq!(rows.len(), 1);
    assert!(rows[0].reach >= 3, "{rows:?}"); // show, fill, and the Car functions they call
    assert!(run(d.path()).tests.is_empty());
}

// test_gate_reach.py: test_a_test_whose_head_spans_two_lines_is_a_test
#[test]
fn a_test_whose_head_spans_two_lines_is_a_test() {
    let d = reach_repo();
    write(
        d.path(),
        &[(
            "test/car_controller_test.exs",
            "defmodule AppWeb.CarControllerTest do\n  use AppWeb.ConnCase\n\n  test \"read the car back\",\n       %{conn: conn} do\n    assert conn |> get(~p\"/api/car\") |> json_response(200)\n  end\n\n  test \"fill the car\", %{\n    conn: conn\n  } do\n    assert post(conn, \"/api/car/7/fill\", %{}) |> json_response(200)\n  end\nend\n",
        )],
    );
    let mut rows: Vec<(String, usize)> =
        audit_rows(d.path()).into_iter().map(|r| (r.id.rsplit('.').next().unwrap().to_string(), r.reach)).collect();
    rows.sort();
    assert_eq!(rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["test fill the car", "test read the car back"]);
    assert!(rows.iter().all(|r| r.1 >= 2), "{rows:?}");
}

// test_gate_reach.py: test_an_unknown_route_still_reaches_nothing
#[test]
fn an_unknown_route_still_reaches_nothing() {
    let d = reach_repo();
    write(
        d.path(),
        &[(
            "test/car_controller_test.exs",
            "defmodule AppWeb.CarControllerTest do\n  use AppWeb.ConnCase\n\n  test \"a route nobody serves\", %{conn: conn} do\n    assert conn |> get(~p\"/api/boat\") |> json_response(404)\n  end\nend\n",
        )],
    );
    assert_eq!(run(d.path()).tests.len(), 1);
}

// test_gate_reach.py: test_a_test_of_a_deploy_file_tests_that_file
#[test]
fn a_test_of_a_deploy_file_tests_that_file() {
    let d = reach_repo();
    write(
        d.path(),
        &[(
            "test/deploy/lan_test.exs",
            "defmodule LanTest do\n  use ExUnit.Case\n  @root Path.expand(\"../..\", __DIR__)\n\n  test \"the hub has services\" do\n    assert File.read!(Path.join(@root, \"deploy/hub/compose.yml\")) =~ \"services\"\n  end\nend\n",
        )],
    );
    assert!(run(d.path()).tests.is_empty());
}

fn skills(name: &str, ids: &[&str]) -> String {
    let entries: String = ids
        .iter()
        .map(|i| {
            format!(
                "      %Skill{{\n        id: \"{i}\",\n        subject: \"math\",\n        title: \"{i}\",\n        group: \"Linear\",\n        generate: &gen/2\n      }},\n"
            )
        })
        .collect();
    format!("defmodule {name} do\n  def skills do\n    [\n{entries}    ]\n  end\n\n  def gen(a, b), do: a + b\nend\n")
}

// test_gate_reach.py: test_two_literal_tables_are_not_copies
#[test]
fn two_literal_tables_are_not_copies() {
    let d = reach_repo();
    write(
        d.path(),
        &[
            ("lib/eq.ex", &skills("Eq", &["eq.a", "eq.b", "eq.c"])),
            ("lib/frac.ex", &skills("Frac", &["fr.a", "fr.b", "fr.c"])),
            ("lib/use.ex", "defmodule Use do\n  def all, do: Eq.skills() ++ Frac.skills()\nend\n"),
        ],
    );
    assert!(run(d.path()).dups.is_empty());
}

// test_gate_reach.py: test_a_forbidden_word_is_a_whole_part_of_a_name
#[test]
fn a_forbidden_word_is_a_whole_part_of_a_name() {
    let d = reach_repo();
    let car = CAR.replace(
        "  def fill(c), do: c\n",
        "  def fill(c), do: c\n  def load_stash_images(c), do: c\n  def loadStashImages(c), do: c\n  def use_shim(c), do: c\n",
    );
    write(d.path(), &[("lib/car.ex", &car)]);
    assert_eq!(run(d.path()).forbidden.iter().map(|f| f.word.as_str()).collect::<Vec<_>>(), vec!["use_shim"]);
}

// --- the exactness guard (test_gate_exactness.py) -------------------------------

const VIEW: &str =
    "import { makeStore } from \"./store\";\n\nexport function show() {\n  const s = makeStore();\n  s.save(1);\n  return s.load(2);\n}\n";

fn exact_repo() -> (tempfile::TempDir, Book) {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("repo");
    write(
        &root,
        &[
            ("seamcheck.toml", "[project]\nname = \"t\"\n"),
            (
                "src/store.ts",
                "export function makeStore() {\n  return new Store();\n}\nexport class Store {\n  save(x: number) { return x; }\n  load(x: number) { return x; }\n}\n",
            ),
            ("src/cache.ts", "export class Cache {\n  save(x: number) { return x; }\n}\n"),
            ("src/view.ts", VIEW),
            ("src/main.ts", "import { show } from \"./view\";\nimport { Cache } from \"./cache\";\n\nshow();\nnew Cache().save(3);\n"),
            ("lib/app.py", "def go():\n    return 1\n\n\ndef main():\n    return go()\n"),
        ],
    );
    git(&root, &["init", "-q"]);
    commit_all(&root, "base");
    let book = Book { dir: d.path().join("cache"), cheap_files: seamcheck::tidy::exactness::CHEAP_FILES };
    (d, book)
}

/// the edit: the import goes, `s.save()` / `s.load()` become guesses
fn lose_import(root: &Path) {
    write(root, &[("src/view.ts", VIEW.splitn(3, '\n').nth(2).unwrap())]);
}

fn egate(root: &Path, book: &Book) -> Report {
    let mut p = load(root);
    gate_with(&mut p, "HEAD", false, &[], book)
}

// test_gate_exactness.py: test_a_lost_import_warns_without_blocking
#[test]
fn a_lost_import_warns_without_blocking() {
    let (d, book) = exact_repo();
    let root = d.path().join("repo");
    assert!(egate(&root, &book).exactness.is_empty()); // a clean tree: recorded, nothing to compare
    lose_import(&root);
    let rep = egate(&root, &book);
    assert!(!rep.failed());
    assert_eq!(rep.exactness.len(), 1);
    let x = &rep.exactness[0];
    assert_eq!((x.ext.as_str(), x.file.as_str()), (".ts", "src/view.ts"));
    assert!(x.before - x.after > DROP);
    assert!(x.message("HEAD").contains("TypeScript (.ts) calls into project code exact"));
}

// test_gate_exactness.py: test_from_the_record_when_measuring_head_is_not_cheap
#[test]
fn from_the_record_when_measuring_head_is_not_cheap() {
    let (d, book) = exact_repo();
    let root = d.path().join("repo");
    let p = load(&root);
    let g = Graph::load(&p);
    book.record(&p, &|| &g, None);
    lose_import(&root);
    let dear = Book { dir: book.dir.clone(), cheap_files: 0 };
    assert_eq!(egate(&root, &dear).exactness.iter().map(|x| x.ext.as_str()).collect::<Vec<_>>(), vec![".ts"]);
}

// test_gate_exactness.py: test_measured_from_head_when_nothing_was_recorded
#[test]
fn measured_from_head_when_nothing_was_recorded() {
    let (d, book) = exact_repo();
    let root = d.path().join("repo");
    lose_import(&root);
    assert_eq!(egate(&root, &book).exactness.iter().map(|x| x.ext.as_str()).collect::<Vec<_>>(), vec![".ts"]);
    // unknown and too big to measure: no warning, no guess
    let empty = Book { dir: d.path().join("empty"), cheap_files: 0 };
    assert!(egate(&root, &empty).exactness.is_empty());
}

// test_gate_exactness.py: test_only_touched_languages_and_real_drops
#[test]
fn only_touched_languages_and_real_drops() {
    let (d, book) = exact_repo();
    let root = d.path().join("repo");
    egate(&root, &book);
    write(&root, &[("lib/app.py", "def go():\n    return 1\n\n\ndef main():\n    return go()\n\n\ndef more():\n    return go()\n")]);
    assert!(egate(&root, &book).exactness.is_empty());
    write(&root, &[("src/view.ts", &format!("{VIEW}// a note\n"))]);
    assert!(egate(&root, &book).exactness.is_empty());
}

// test_gate_exactness.py: test_cli_text_and_json (a clean-tree gate records HEAD; Rust has no `stats` yet)
#[test]
fn exactness_cli_text_and_json() {
    let (d, book) = exact_repo();
    let root = d.path().join("repo");
    let run = |args: &[&str]| {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_seamcheck"))
            .args(args)
            .current_dir(&root)
            .env("SEAMCHECK_CACHE_DIR", &book.dir)
            .output()
            .unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string())
    };
    assert_eq!(run(&["gate"]).0, 0); // records HEAD
    lose_import(&root);
    let (code, out) = run(&["gate"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("warning (not blocking): TypeScript (.ts) calls into project code exact: 100.0% at HEAD"), "{out}");
    let (_, out) = run(&["gate", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["blocked"], false);
    let f: Vec<&serde_json::Value> = v["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == "exactness").collect();
    assert_eq!(f.len(), 1);
    assert_eq!((f[0]["severity"].as_str(), f[0]["file"].as_str()), (Some("warn"), Some("src/view.ts")));
    let (_, out) = run(&["gate", "--format", "github"]);
    assert!(out.starts_with("::warning file=src/view.ts,title=seamcheck exactness::"), "{out}");
}
