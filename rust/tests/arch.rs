//! Architecture rules and the rules that follow calls, ported from the
//! Python suite: test_arch.py (depend, layers, cycles, graph, path, taint),
//! test_contracts_ui.py's together-with-depth case, test_infra.py's
//! auth-in-background and test_lint.py's ets-owner.

mod common;

use std::collections::BTreeMap;

use common::{dedent, fixture, lint, run};
use seamcheck::project::Finding;
use seamcheck::rules::run_rules;

fn by_rule(found: Vec<Finding>) -> BTreeMap<String, Vec<Finding>> {
    let mut by: BTreeMap<String, Vec<Finding>> = BTreeMap::new();
    for f in found {
        by.entry(f.rule.clone()).or_default().push(f);
    }
    by
}

fn arch() -> (tempfile::TempDir, seamcheck::project::Project, BTreeMap<String, Vec<Finding>>) {
    std::env::set_var("SEAMCHECK_RS_QUIET", "1");
    let (d, p) = fixture("arch");
    let found = by_rule(run_rules(&p, None, None));
    (d, p, found)
}

// test_arch.py: Arch.test_depend
#[test]
fn depend() {
    let (_d, _p, found) = arch();
    let f = &found["ui-not-db"];
    assert_eq!(f.len(), 1);
    assert_eq!((f[0].file.as_str(), f[0].line), ("src/ui/view.ts", 1));
}

// test_arch.py: Arch.test_layers_and_type_only_imports
#[test]
fn layers_and_type_only_imports() {
    // top first: ui may use db and util, db may use util; util's `import { type Shown }`
    // from ui is erased at runtime, so it is no upward dependency
    let (_d, _p, found) = arch();
    assert!(!found.contains_key("layers"), "{:?}", found.get("layers"));
}

// test_arch.py: Arch.test_cycles
#[test]
fn cycles() {
    let (_d, _p, found) = arch();
    let f = &found["no-cycles"];
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("src/a/one.ts") && f[0].message.contains("src/b/two.ts"), "{}", f[0].message);
}

// test_arch.py: Arch.test_find_takes_the_id_it_prints_and_file_line
#[test]
fn find_takes_the_id_it_prints_and_file_line() {
    let (_d, p, _) = arch();
    let g = p.graph();
    let ids = |spec: &str| g.find(spec).iter().map(|f| f.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids("src/db/store.save"), vec!["src/db/store.save".to_string()]);
    assert_eq!(ids("src/db/store.ts:4"), vec!["src/db/store.save".to_string()]);
}

// test_arch.py: Arch.test_graph_renders_cycles
#[test]
fn graph_renders_cycles() {
    let (_d, p, _) = arch();
    let g = seamcheck::arch::graph(&p, "file", "import", &["src/a/**".to_string(), "src/b/**".to_string()]);
    let want: BTreeMap<String, BTreeMap<String, usize>> = [
        ("src/a/one.ts".to_string(), [("src/b/two.ts".to_string(), 1)].into()),
        ("src/b/two.ts".to_string(), [("src/a/one.ts".to_string(), 1)].into()),
    ]
    .into();
    assert_eq!(g, want);
    assert!(seamcheck::arch::render(&g, "dot").contains("\"src/a/one.ts\" -> \"src/b/two.ts\" [color=red];"));
    assert!(seamcheck::arch::render(&g, "mermaid").contains("graph LR"));
}

// test_arch.py: Arch.test_path_reaches_sinks_through_calls
#[test]
fn path_reaches_sinks_through_calls() {
    let (_d, _p, found) = arch();
    let got: Vec<(String, String)> = found["no-shell"]
        .iter()
        .map(|f| (f.func.clone(), f.message.split_once(": ").map(|x| x.1.to_string()).unwrap_or_default()))
        .collect();
    assert!(got.contains(&("handler".into(), "handler -> run_it".into())), "{got:?}");
    assert!(got.contains(&("handler".into(), "handler".into()))); // the constant `subprocess.run("date")`
}

// test_arch.py: Arch.test_path_with_a_source_follows_the_tainted_value
#[test]
fn path_with_a_source_follows_the_tainted_value() {
    // handler's `cmd` -> `full` -> run_it's parameter -> subprocess.run(text): reported;
    // the constant `date` and the value through safe_name are not
    let (_d, _p, found) = arch();
    let f = &found["no-tainted-shell"];
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].func, "handler");
    assert!(f[0].message.contains("web/util.py:5"), "{}", f[0].message);
}

// test_arch.py: Arch.test_a_call_through_an_interface_is_no_package_dependency
#[test]
fn a_call_through_an_interface_is_no_package_dependency() {
    // Traversor calls Visitor.head; Printer implements it: select doesn't depend on examples
    let (_d, _p, found) = arch();
    assert!(!found.contains_key("no-package-cycles"), "{:?}", found.get("no-package-cycles"));
}

// test_arch.py: Taint.test_follows_arguments_branches_and_sanitizers
#[test]
fn taint_follows_arguments_branches_and_sanitizers() {
    std::env::set_var("SEAMCHECK_RS_QUIET", "1");
    let (_d, p) = fixture("taint");
    let mut got: Vec<String> = run_rules(&p, None, None)
        .into_iter()
        .filter(|f| f.rule == "taint")
        .map(|f| f.message.split(" at ").nth(1).unwrap().split(':').nth(1).unwrap().to_string())
        .collect();
    got.sort();
    // line 11: the branch that escapes doesn't clean the other; 25: run's `h` is the request value.
    // Not 17 (escaped first), not 21 (load is only ever passed a constant)
    assert_eq!(got, vec!["11".to_string(), "25".to_string()]);
}

// --- together: func / depth (test_contracts_ui.py) ---------------------------------------

const CHANNEL_BEFORE: &str = r#"
    defmodule Web.OsdChannel do
      def join("osd:" <> key, _payload, socket) do
        case authorize(key) do
          {:ok, socket} -> {:ok, socket}
          {:error, _} -> {:error, %{reason: "unauthorized"}}
        end
      end

      def handle_in("x", _, socket), do: {:reply, {:error, %{reason: "unknown_event"}}, socket}
    end
"#;
const CHANNEL_AFTER: &str = r#"
    defmodule Web.OsdChannel do
      require Logger

      def join("osd:" <> key, _payload, socket) do
        case authorize(key) do
          {:ok, socket} -> {:ok, socket}
          {:error, why} -> refuse(key, why)
        end
      end

      defp refuse(key, why) do
        Logger.warning("[OsdChannel] refused #{key}: #{inspect(why)}")
        {:error, %{reason: "unauthorized"}}
      end
    end
"#;
const JOIN_RULE: &str = r#"
    [[rule]]
    id = "joins-log"
    kind = "together"
    in = ["lib/**"]
    func = "join"
    if = '\{:error,(?![^\n]*(?:->|<-))'
    then = 'Logger\.'
    depth = 1
"#;

// test_contracts_ui.py: TogetherFuncDepth.test_a_refused_join_said_nothing
#[test]
fn together_a_refused_join_said_nothing() {
    let got = run(&[("lib/osd_channel.ex", CHANNEL_BEFORE)], JOIN_RULE);
    let at: Vec<(String, usize)> = got["joins-log"].iter().map(|(f, ln, _)| (f.clone(), *ln)).collect();
    assert_eq!(at, vec![("lib/osd_channel.ex".to_string(), 6)]);
    assert!(!run(&[("lib/osd_channel.ex", CHANNEL_AFTER)], JOIN_RULE).contains_key("joins-log"));
}

// --- auth-in-background (test_infra.py) ----------------------------------------------------

// test_infra.py: InfraLints.test_auth_in_background
#[test]
fn auth_in_background() {
    // the Robinhood connector runner retried reconnect on a timer: random approval prompts
    let got = lint(
        "auth-in-background",
        &[
            (
                "lib/runner.ex",
                "defmodule Runner do\n  use GenServer\n  def handle_info(:tick, s) do\n    Conn.reconnect(s)\n    {:noreply, s}\n  end\n  \
                 def handle_info(:refresh, s), do: {:noreply, Conn.refresh(s)}\nend\n",
            ),
            ("lib/conn.ex", "defmodule Conn do\n  def reconnect(s), do: s\n  def refresh(s), do: s\nend\n"),
        ],
        "",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].2.contains("Conn.reconnect"));
}

// --- ets-owner (test_lint.py) ----------------------------------------------------------------

const PMTILES_BEFORE: &[(&str, &str)] = &[
    (
        "lib/map/pmtiles.ex",
        r#"
        defmodule App.Map.Pmtiles do
          @table :pmtiles
          def get(path) do
            ensure_table()
            :ets.lookup(@table, path)
          end

          defp ensure_table do
            if :ets.whereis(@table) == :undefined do
              :ets.new(@table, [:named_table, :public, :set])
            end
          end
        end
    "#,
    ),
    (
        "lib/web/tile_controller.ex",
        r#"
        defmodule App.Web.TileController do
          def tile(conn, %{"path" => p}), do: json(conn, App.Map.Pmtiles.get(p))
        end
    "#,
    ),
];
const PMTILES_AFTER: &[(&str, &str)] = &[
    (
        "lib/map/pmtiles_tables.ex",
        r#"
        defmodule App.Map.PmtilesTables do
          use GenServer
          def start_link(o), do: GenServer.start_link(__MODULE__, o, name: __MODULE__)

          @impl true
          def init(_) do
            :ets.new(:pmtiles, [:named_table, :public, :set])
            {:ok, nil}
          end
        end
    "#,
    ),
    (
        "lib/asn.ex",
        r#"
        defmodule App.Asn do
          use GenServer
          def init(_) do
            create_tables()
            {:ok, %{}}
          end

          def handle_call(:reload, _from, s) do
            create_tables()
            {:reply, :ok, s}
          end

          defp create_tables, do: :ets.new(:asn, [:named_table])
        end
    "#,
    ),
];

// test_lint.py: EtsOwner.test_lazy_table_made_by_the_first_request
#[test]
fn ets_lazy_table_made_by_the_first_request() {
    let got = lint("ets-owner", PMTILES_BEFORE, "");
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("ensure_table (reached from App.Map.Pmtiles.get)"), "{}", got[0].2);
}

// test_lint.py: EtsOwner.test_tables_owned_by_a_process
#[test]
fn ets_tables_owned_by_a_process() {
    assert_eq!(lint("ets-owner", PMTILES_AFTER, ""), vec![]);
}

// test_lint.py: EtsOwner.test_a_callback_of_another_process_behaviour
#[test]
fn ets_a_callback_of_another_process_behaviour() {
    // plausible's Counters: a :gen_cycle process whose init_cycle makes the buffer's table
    let cycle = dedent(
        r#"
            defmodule App.Counters do
              @behaviour :gen_cycle
              def child_spec(opts), do: %{id: __MODULE__, start: {:gen_cycle, :start_link, [{:local, __MODULE__}, __MODULE__, opts]}}

              @impl true
              def init_cycle(opts) do
                App.Counters.Buffer.new(opts)
                {:ok, opts}
              end
            end
        "#,
    );
    let buffer = "defmodule App.Counters.Buffer do\n  def new(o) do\n    :ets.new(:buf, [:public])\n  end\nend\n";
    assert_eq!(lint("ets-owner", &[("lib/counters.ex", &cycle), ("lib/counters/buffer.ex", buffer)], ""), vec![]);
}

// test_lint.py: EtsOwner.test_a_table_only_test_setup_makes
#[test]
fn ets_a_table_only_test_setup_makes() {
    // hexpm: test_helper.exs calls Store.Memory.start(); the test run's process owns the table
    let store = "defmodule App.Store.Memory do\n  def start do\n    :ets.new(:store, [:named_table])\n  end\nend\n";
    let helper = "ExUnit.start()\nApp.Store.Memory.start()\n";
    assert_eq!(lint("ets-owner", &[("lib/store/memory.ex", store), ("test/test_helper.exs", helper)], ""), vec![]);
}

// test_lint.py: EtsOwner.test_plug_init_is_not_a_process
#[test]
fn ets_plug_init_is_not_a_process() {
    let plug = "defmodule App.Plug do\n  def init(o) do\n    :ets.new(:x, [:named_table])\n    o\n  end\nend\n";
    assert_eq!(lint("ets-owner", &[("lib/plug.ex", plug)], "").len(), 1);
}
