"""`kind = "lint"` (python3 -m unittest discover tests). Each check gets the
real bug it was built from (before) and its fix (after), cut down from the
elixir-pot history of 2026-09-28..10-01."""
import os
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402


def run(files: dict, rules: str) -> dict:
    """{rule id: [(file, line, message)]} for a tiny project."""
    with tempfile.TemporaryDirectory() as root:
        for name, body in files.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(textwrap.dedent(body))
        cfg = os.path.join(root, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "t"\ninclude = ["**"]\n' + textwrap.dedent(rules))
        p = core.load(root, cfg, use_cache=False)
        out: dict = {}
        for f in core.run_rules(p):
            out.setdefault(f.rule, []).append((f.file, f.line, f.message))
        return out


def lint(name: str, files: dict, options: str = "") -> list:
    rules = f'[[rule]]\nid = "x"\nkind = "lint"\nlint = "{name}"\n' + (f"options = {options}\n" if options else "")
    got = run(files, rules)
    assert "config" not in got, got["config"]
    return got.get("x", [])


# --- disabled-by-data -----------------------------------------------------------------

NOTICE_ROW_BEFORE = """
    @Composable
    private fun NoticeRow(notice: HubNotice, busy: Boolean, onOpen: () -> Unit, onApprove: () -> Unit) {
        val state = if (!notice.unread) VState.Disabled else VState.Enabled
        VRow(
            title = notice.title,
            state = state,
            onClick = onOpen,
            trailing = {
                VButton("Approve", onClick = onApprove, state = if (busy) VState.Loading else VState.Enabled)
            },
        )
    }
"""
NOTICE_ROW_AFTER = """
    @Composable
    private fun NoticeRow(notice: HubNotice, busy: Boolean, onOpen: () -> Unit, onApprove: () -> Unit) {
        VRow(
            title = notice.title,
            onClick = onOpen,
            leading = if (notice.unread) { { VBadge(dot = true) } } else null,
            trailing = {
                VButton("Approve", onClick = onApprove, state = if (busy) VState.Loading else VState.Enabled)
            },
        )
    }
"""
SIGN_IN_LIST_BEFORE = """
    fun SignInList(requests: List<Req>) {
        requests.forEach { row ->
            VRow(
                title = row.title,
                state = if (row.pending) VState.Enabled else VState.Disabled,
                onClick = { open(row.id) },
            )
        }
        rules.forEach { rule ->
            VRow(
                title = rule.kind,
                state = if (rule.expired) VState.Disabled else VState.Enabled,
                trailing = { VButton("Delete", onClick = { delete(rule.id) }) },
            )
        }
    }
"""


class DisabledByData(unittest.TestCase):
    def test_read_notices_were_disabled(self):
        got = lint("disabled-by-data", {"NotifyCenter.kt": NOTICE_ROW_BEFORE})
        self.assertEqual(len(got), 1)
        self.assertIn("notice.unread", got[0][2])
        self.assertEqual(lint("disabled-by-data", {"NotifyCenter.kt": NOTICE_ROW_AFTER}), [])

    def test_settled_sign_ins_were_disabled_but_a_row_without_a_tap_only_dims(self):
        got = lint("disabled-by-data", {"SignInReview.kt": SIGN_IN_LIST_BEFORE})
        self.assertEqual([g[2].split(" ")[0] for g in got], ["VRow"])
        self.assertIn("row.pending", got[0][2])

    def test_jsx_and_qml_rows(self):
        tsx = """
            export function Rows({ items, busy }) {
              return (
                <ul>
                  {items.map((n) => (
                    <button className="notice-row" disabled={n.read} onClick={() => open(n)}>{n.title}</button>
                  ))}
                  <button className="notice-row" disabled={busy || saving} onClick={save}>Save</button>
                  <button className="approve" disabled={!req.pending} onClick={approve}>Approve</button>
                </ul>
              );
            }
        """
        got = lint("disabled-by-data", {"Rows.tsx": tsx})
        self.assertEqual(len(got), 1)
        self.assertIn("n.read", got[0][2])
        qml = """
            ListView {
              delegate: Rectangle {
                MouseArea {
                  anchors.fill: parent
                  enabled: modelData.unread
                  onClicked: open(modelData)
                }
              }
            }
        """
        self.assertEqual(len(lint("disabled-by-data", {"Drawer.qml": qml})), 1)


# --- inert-control ------------------------------------------------------------------------

PLACES_BEFORE = """
    export function PlaceCard({ place }) {
      return (
        <div>
          <form className="places-map-settings" onSubmit={(e) => e.preventDefault()}>
            <label><input type="checkbox" defaultChecked data-testid="places_setting_map_place_names" /> Place names</label>
            <select data-testid="places_setting_map_base" defaultValue="wireframe">
              <option value="wireframe">Theme wireframe</option>
            </select>
          </form>
          <div style={{ display: "flex", gap: 8 }}>
            <button type="button" className="surf-chip" data-testid="places_navigate">Navigate</button>
            <button type="button" className="surf-chip">Save</button>
            <button type="button" className="surf-chip">Call</button>
            <button type="button" className="surf-chip">Share</button>
          </div>
        </div>
      );
    }
"""
PLACES_AFTER = """
    export function PlaceCard({ place, setBase, base }) {
      return (
        <div>
          <form onSubmit={(e) => e.preventDefault()}>
            <select value={base} onChange={(e) => setBase(e.target.value)}>
              <option value="wireframe">Theme wireframe</option>
            </select>
            <button type="submit">Apply</button>
          </form>
          <button type="button" className="surf-chip" onClick={() => navigate(place)}>Navigate</button>
          <button type="button" className="surf-chip" disabled>Soon</button>
          <button {...chipProps}>Spread</button>
        </div>
      );
    }
"""


class InertControl(unittest.TestCase):
    def test_places_chips_and_settings_did_nothing(self):
        got = lint("inert-control", {"PlacesDiveWidget.tsx": PLACES_BEFORE})
        msgs = [g[2] for g in got]
        self.assertEqual(sum("has no handler" in m for m in msgs), 4)
        self.assertEqual(sum("saves nothing" in m for m in msgs), 2)
        self.assertIn("'Navigate'", msgs[2] + msgs[3] + msgs[4] + msgs[5])
        self.assertEqual(lint("inert-control", {"PlacesDiveWidget.tsx": PLACES_AFTER}), [])

    def test_empty_handlers(self):
        kt = """
            fun Bar() {
                VButton("Share", onClick = {})
                Box(Modifier.clickable(interactionSource = i, indication = null, onClick = {}))
                DropdownMenuItem(text = { Text("Needs hub") }, onClick = {}, enabled = false)
            }
        """
        got = lint("inert-control", {"Bar.kt": kt})
        self.assertEqual([g[1] for g in got], [3])  # the tap-eater and the disabled label are deliberate
        self.assertEqual(len(lint("inert-control", {"A.qml": "Item {\n  MouseArea { onClicked: {} }\n}\n"})), 1)
        self.assertEqual(len(lint("inert-control", {"A.tsx": "const a = <button onClick={() => {}}>x</button>;\n"})), 1)


# --- qml-anchors-in-positioner --------------------------------------------------------

SPOTIFY_BEFORE = """
    Rectangle {
      Repeater {
        model: spot.queue
        delegate: Column {
          width: parent.width
          Text { text: modelData.name }
          Text {
            text: modelData.artist
            anchors.left: parent.left
          }
          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: spot.playHit({ uri: modelData.uri })
          }
        }
      }
    }
"""
SPOTIFY_AFTER = SPOTIFY_BEFORE.replace("""          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: spot.playHit({ uri: modelData.uri })
          }""", """          HoverHandler { cursorShape: Qt.PointingHandCursor }
          TapHandler { onTapped: spot.playHit({ uri: modelData.uri }) }""")


class QmlAnchors(unittest.TestCase):
    def test_mouse_area_filling_a_column_row(self):
        got = lint("qml-anchors-in-positioner", {"SpotifyHost.qml": SPOTIFY_BEFORE})
        self.assertEqual(len(got), 1)
        self.assertIn("MouseArea inside a Column sets anchors.fill", got[0][2])
        self.assertEqual(lint("qml-anchors-in-positioner", {"SpotifyHost.qml": SPOTIFY_AFTER}), [])

    def test_layouts_rows_and_assigned_items(self):
        qml = """
            Item {
              Row {
                Text { anchors.verticalCenter: parent.verticalCenter }
                Rectangle { anchors.left: parent.left }
              }
              ColumnLayout {
                Label { anchors.margins: 4 }
                Button {
                  anchors.right: parent.right
                  background: Rectangle { anchors.fill: parent }
                }
              }
              Repeater {
                model: 3
                Rectangle { anchors.fill: parent }
              }
            }
        """
        got = lint("qml-anchors-in-positioner", {"A.qml": qml})
        self.assertEqual(sorted(g[2].split(":")[0] for g in got),
                         ["Button inside a ColumnLayout sets anchors.right", "Rectangle inside a Row sets anchors.left"])


# --- kotlin-init-order ------------------------------------------------------------------

VM_BEFORE = """
    class LauncherViewModel(app: Application) : AndroidViewModel(app) {
        private val apps = MutableStateFlow(emptyList<App>())

        init {
            viewModelScope.launch {
                apps.collect { searchResults(it) }
            }
        }

        fun searchResults(list: List<App>): List<App> = synchronized(indexLock) { list }

        fun categoryCounts(list: List<App>) {
            synchronized(indexLock) { cachedCounts = list.size }
        }

        private val indexLock = Any()
        private var cachedCounts: Int = 0
        lateinit var late: String
    }
"""
VM_AFTER = """
    class LauncherViewModel(app: Application) : AndroidViewModel(app) {
        private val apps = MutableStateFlow(emptyList<App>())
        // declared before init: the collector in init runs at once
        private val indexLock = Any()

        init {
            viewModelScope.launch {
                apps.collect { searchResults(it) }
            }
        }

        fun searchResults(list: List<App>): List<App> {
            synchronized(indexLock) { return list }
        }

        fun other() {
            val cachedCounts = 1
            println(cachedCounts)
        }

        private var cachedCounts: Int = 0
        data class Row(val label: String = "")
    }
"""


class KotlinInitOrder(unittest.TestCase):
    def test_lock_declared_below_init(self):
        got = lint("kotlin-init-order", {"LauncherViewModel.kt": VM_BEFORE})
        self.assertEqual(len(got), 1)
        self.assertIn("init uses indexLock (through searchResults)", got[0][2])

    def test_declared_above_and_shadowed(self):
        self.assertEqual(lint("kotlin-init-order", {"LauncherViewModel.kt": VM_AFTER}), [])


# --- id-kind-mix --------------------------------------------------------------------------

ROUTER_BEFORE = """
    object NoticeRouter {
        fun launchIntent(context: Context, notice: HubNotice): Intent {
            val dest = destOf(notice)
            return Intent(context, SignInReviewActivity::class.java).apply {
                putExtra(NotifyIntents.EXTRA_REQUEST_ID, notice.requestId ?: dest.id)
            }
        }
    }
"""
ROUTER_AFTER = """
    object NoticeRouter {
        fun launchIntent(context: Context, notice: HubNotice): Intent {
            val local = notice.local
            return Intent(context, SignInReviewActivity::class.java).apply {
                notice.requestId?.let { putExtra(NotifyIntents.EXTRA_REQUEST_ID, it) }
                putExtra(EXTRA_CLIENT, local.clientMessageId ?: local.id)
                putExtra(EXTRA_SHADE, challengeId?.let { "signin:$it" } ?: noticeId?.let { "notice:$it" })
            }
        }
    }
"""


class IdKindMix(unittest.TestCase):
    def test_request_id_falling_back_to_the_dest_id(self):
        got = lint("id-kind-mix", {"NoticeRouter.kt": ROUTER_BEFORE})
        self.assertEqual(len(got), 1)
        self.assertIn("'request' id with dest.id", got[0][2])
        self.assertEqual(lint("id-kind-mix", {"NoticeRouter.kt": ROUTER_AFTER}), [])

    def test_ts_guards_and_named_pairs(self):
        ts = """
            export function pick(n, dest, a, b) {
              const id = n.challengeId ?? dest.id;
              if (!a.sceneId || !b.id) return;
              return text(meta.challenge_id) || text(meta.request_id);
            }
        """
        self.assertEqual([g[1] for g in lint("id-kind-mix", {"x.ts": ts})], [3])
        self.assertEqual([g[1] for g in lint("id-kind-mix", {"x.ts": ts}, "{ named = true }")], [3, 5])
        ex = 'defmodule D do\n  def id(m), do: blank(m["challenge_id"]) || blank(m["request_id"]) || blank(m["identity_request_id"])\nend\n'
        self.assertEqual(lint("id-kind-mix", {"dest.ex": ex}), [])
        self.assertEqual(len(lint("id-kind-mix", {"dest.ex": ex}, "{ named = true }")), 1)


# --- ets-owner ------------------------------------------------------------------------------

PMTILES_BEFORE = {
    "lib/map/pmtiles.ex": """
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
    """,
    "lib/web/tile_controller.ex": """
        defmodule App.Web.TileController do
          def tile(conn, %{"path" => p}), do: json(conn, App.Map.Pmtiles.get(p))
        end
    """,
}
PMTILES_AFTER = {
    "lib/map/pmtiles_tables.ex": """
        defmodule App.Map.PmtilesTables do
          use GenServer
          def start_link(o), do: GenServer.start_link(__MODULE__, o, name: __MODULE__)

          @impl true
          def init(_) do
            :ets.new(:pmtiles, [:named_table, :public, :set])
            {:ok, nil}
          end
        end
    """,
    "lib/asn.ex": """
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
    """,
}


class EtsOwner(unittest.TestCase):
    def test_lazy_table_made_by_the_first_request(self):
        got = lint("ets-owner", PMTILES_BEFORE)
        self.assertEqual(len(got), 1)
        self.assertIn("ensure_table (reached from App.Map.Pmtiles.get)", got[0][2])

    def test_tables_owned_by_a_process(self):
        self.assertEqual(lint("ets-owner", PMTILES_AFTER), [])

    def test_a_callback_of_another_process_behaviour(self):
        # plausible's Counters: a :gen_cycle process whose init_cycle makes the buffer's table
        cycle = """
            defmodule App.Counters do
              @behaviour :gen_cycle
              def child_spec(opts), do: %{id: __MODULE__, start: {:gen_cycle, :start_link, [{:local, __MODULE__}, __MODULE__, opts]}}

              @impl true
              def init_cycle(opts) do
                App.Counters.Buffer.new(opts)
                {:ok, opts}
              end
            end
        """
        buffer = "defmodule App.Counters.Buffer do\n  def new(o) do\n    :ets.new(:buf, [:public])\n  end\nend\n"
        self.assertEqual(lint("ets-owner", {"lib/counters.ex": cycle, "lib/counters/buffer.ex": buffer}), [])

    def test_a_table_only_test_setup_makes(self):
        # hexpm: test_helper.exs calls Store.Memory.start(); the test run's process owns the table
        store = "defmodule App.Store.Memory do\n  def start do\n    :ets.new(:store, [:named_table])\n  end\nend\n"
        helper = "ExUnit.start()\nApp.Store.Memory.start()\n"
        self.assertEqual(lint("ets-owner", {"lib/store/memory.ex": store, "test/test_helper.exs": helper}), [])

    def test_plug_init_is_not_a_process(self):
        plug = "defmodule App.Plug do\n  def init(o) do\n    :ets.new(:x, [:named_table])\n    o\n  end\nend\n"
        self.assertEqual(len(lint("ets-owner", {"lib/plug.ex": plug})), 1)


# --- handle-info-catch-all ----------------------------------------------------------------

STOCKS_BEFORE = """
    defmodule App.Stocks do
      use GenServer
      def subscribe, do: Phoenix.PubSub.subscribe(App.PubSub, "stocks")

      def init(_) do
        Phoenix.PubSub.subscribe(App.PubSub, "settings:updated")
        {:ok, %{}}
      end

      def handle_info(%{key: key}, state) when key in ["finnhub_api_key"], do: {:noreply, state}
      def handle_info(%{key: _key}, state), do: {:noreply, state}
      def handle_info(:fetch_quotes, state), do: {:noreply, state}
    end
"""
STOCKS_AFTER = STOCKS_BEFORE.replace("""      def handle_info(:fetch_quotes, state), do: {:noreply, state}
""", """      def handle_info(:fetch_quotes, state), do: {:noreply, state}

      def handle_info(message, state) do
        Logger.warning("[Stocks] ignored message #{inspect(message)}")
        {:noreply, state}
      end
""")


class HandleInfoCatchAll(unittest.TestCase):
    def test_a_shared_topic_crashed_stocks(self):
        self.assertEqual(len(lint("handle-info-catch-all", {"lib/stocks.ex": STOCKS_BEFORE})), 1)
        self.assertEqual(lint("handle-info-catch-all", {"lib/stocks.ex": STOCKS_AFTER}), [])

    def test_a_client_subscribe_api_is_not_the_server_subscribing(self):
        api = STOCKS_BEFORE.replace('        Phoenix.PubSub.subscribe(App.PubSub, "settings:updated")\n', "")
        self.assertEqual(lint("handle-info-catch-all", {"lib/stocks.ex": api}), [])


# --- CSS: clipped popups and overlays under the shell ---------------------------------------

BAR_TSX = """
    export function CommandInput({ busy }) {
      return (
        <div className="command-dock">
          <div className="command-dock-shell">
            <div className="command-row">
              <div className="command-menu-anchor">
                <button type="button" onClick={toggle}>Model</button>
                {chipMenu ? (
                  <div className="v-menu command-pop" aria-label="Veridian">
                    <button type="button" onClick={pick}>Switch</button>
                  </div>
                ) : null}
              </div>
            </div>
          </div>
          <div className="command-menu-anchor">
            <div className="command-scroll">
              <div className="command-pop">not clipped: the scroller is below its containing block</div>
            </div>
          </div>
        </div>
      );
    }
"""
CSS_BEFORE = """
    :root { --space-2: 0.5rem; --z-dock: 50; }
    .command-dock { position: absolute; left: 50%; bottom: 1rem; z-index: var(--z-dock); }
    .command-dock-shell {
      display: flex;
      overflow: hidden; /* rounded corners */
    }
    .command-scroll { overflow-y: auto; }
    .command-menu-anchor { position: relative; }
    .command-pop { position: absolute; bottom: calc(100% + var(--space-2)); left: 0; z-index: 8; }
    .command-pop:hover { overflow: hidden; }
    .hitl-overlay { position: absolute; inset: 0; z-index: 40; display: grid; }
    .particle-backdrop { position: fixed; inset: 0; z-index: 1; pointer-events: none; }
"""
CSS_AFTER = CSS_BEFORE.replace("      overflow: hidden; /* rounded corners */\n", "") + \
    ".hitl-overlay {\n  position: fixed;\n  z-index: 90;\n}\n"


class CssLayers(unittest.TestCase):
    def test_the_model_menu_was_cropped_by_the_bar_shell(self):
        got = lint("css-popup-clipped", {"CommandInput.tsx": BAR_TSX, "styles/surface.css": CSS_BEFORE})
        self.assertEqual(len(got), 1)
        self.assertIn(".command-pop opens outside its box (bottom: calc(100% + 0.5rem))", got[0][2])
        self.assertIn(".command-dock-shell (overflow: hidden)", got[0][2])
        self.assertEqual(lint("css-popup-clipped", {"CommandInput.tsx": BAR_TSX, "styles/surface.css": CSS_AFTER}), [])

    def test_the_approval_drew_under_the_dock(self):
        got = lint("css-overlay-order", {"styles/surface.css": CSS_BEFORE})
        self.assertEqual(len(got), 1)
        self.assertIn(".hitl-overlay covers the screen at z-index 40, under .command-dock (50)", got[0][2])
        self.assertEqual(lint("css-overlay-order", {"styles/surface.css": CSS_AFTER}), [])

    def test_an_explicit_ladder(self):
        got = lint("css-overlay-order", {"styles/surface.css": CSS_AFTER + ".top-strip { position: fixed; z-index: 95; }\n"},
                   '{ above = ["command-dock", "top-strip"] }')
        self.assertEqual(len(got), 1)
        self.assertIn("under .top-strip (95)", got[0][2])


class LintConfig(unittest.TestCase):
    def test_unknown_lint_and_option(self):
        got = run({"a.kt": "fun a() {}\n"}, '[[rule]]\nid = "x"\nkind = "lint"\nlint = "disabled-by-dta"\n'
                  '[[rule]]\nid = "y"\nkind = "lint"\nlint = "id-kind-mix"\noptions = { nmed = true }\n')
        self.assertIn("did you mean 'disabled-by-data'", got["x"][0][2])
        self.assertIn("unknown option(s) nmed", got["y"][0][2])


if __name__ == "__main__":
    unittest.main()
