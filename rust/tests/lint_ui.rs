//! tests/test_lint.py: each check gets the real bug it was built from
//! (before) and its fix (after).

mod common;
use common::{lint, run};

// --- disabled-by-data -----------------------------------------------------------------

const NOTICE_ROW_BEFORE: &str = r#"
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
"#;
const NOTICE_ROW_AFTER: &str = r#"
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
"#;
const SIGN_IN_LIST_BEFORE: &str = r#"
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
"#;

#[test]
fn read_notices_were_disabled() {
    let got = lint("disabled-by-data", &[("NotifyCenter.kt", NOTICE_ROW_BEFORE)], "");
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("notice.unread"));
    assert!(lint("disabled-by-data", &[("NotifyCenter.kt", NOTICE_ROW_AFTER)], "").is_empty());
}

#[test]
fn settled_sign_ins_were_disabled_but_a_row_without_a_tap_only_dims() {
    let got = lint("disabled-by-data", &[("SignInReview.kt", SIGN_IN_LIST_BEFORE)], "");
    let heads: Vec<&str> = got.iter().map(|g| g.2.split(' ').next().unwrap()).collect();
    assert_eq!(heads, vec!["VRow"]);
    assert!(got[0].2.contains("row.pending"));
}

#[test]
fn jsx_and_qml_rows() {
    let tsx = r#"
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
    "#;
    let got = lint("disabled-by-data", &[("Rows.tsx", tsx)], "");
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("n.read"));
    let qml = r#"
        ListView {
          delegate: Rectangle {
            MouseArea {
              anchors.fill: parent
              enabled: modelData.unread
              onClicked: open(modelData)
            }
          }
        }
    "#;
    assert_eq!(lint("disabled-by-data", &[("Drawer.qml", qml)], "").len(), 1);
}

// --- inert-control ------------------------------------------------------------------------

const PLACES_BEFORE: &str = r#"
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
"#;
const PLACES_AFTER: &str = r#"
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
"#;

#[test]
fn places_chips_and_settings_did_nothing() {
    let got = lint("inert-control", &[("PlacesDiveWidget.tsx", PLACES_BEFORE)], "");
    let msgs: Vec<&str> = got.iter().map(|g| g.2.as_str()).collect();
    assert_eq!(msgs.iter().filter(|m| m.contains("has no handler")).count(), 4);
    assert_eq!(msgs.iter().filter(|m| m.contains("saves nothing")).count(), 2);
    assert!(format!("{}{}{}{}", msgs[2], msgs[3], msgs[4], msgs[5]).contains("'Navigate'"));
    assert!(lint("inert-control", &[("PlacesDiveWidget.tsx", PLACES_AFTER)], "").is_empty());
}

#[test]
fn empty_handlers() {
    let kt = r#"
        fun Bar() {
            VButton("Share", onClick = {})
            Box(Modifier.clickable(interactionSource = i, indication = null, onClick = {}))
            DropdownMenuItem(text = { Text("Needs hub") }, onClick = {}, enabled = false)
        }
    "#;
    let got = lint("inert-control", &[("Bar.kt", kt)], "");
    assert_eq!(got.iter().map(|g| g.1).collect::<Vec<_>>(), vec![3]);
    assert_eq!(lint("inert-control", &[("A.qml", "Item {\n  MouseArea { onClicked: {} }\n}\n")], "").len(), 1);
    assert_eq!(lint("inert-control", &[("A.tsx", "const a = <button onClick={() => {}}>x</button>;\n")], "").len(), 1);
}

// --- qml-anchors-in-positioner --------------------------------------------------------

const SPOTIFY_BEFORE: &str = r#"
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
"#;

#[test]
fn mouse_area_filling_a_column_row() {
    let got = lint("qml-anchors-in-positioner", &[("SpotifyHost.qml", SPOTIFY_BEFORE)], "");
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("MouseArea inside a Column sets anchors.fill"));
    let after = SPOTIFY_BEFORE.replace(
        "          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: spot.playHit({ uri: modelData.uri })
          }",
        "          HoverHandler { cursorShape: Qt.PointingHandCursor }
          TapHandler { onTapped: spot.playHit({ uri: modelData.uri }) }",
    );
    assert!(lint("qml-anchors-in-positioner", &[("SpotifyHost.qml", &after)], "").is_empty());
}

#[test]
fn layouts_rows_and_assigned_items() {
    let qml = r#"
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
    "#;
    let got = lint("qml-anchors-in-positioner", &[("A.qml", qml)], "");
    let mut heads: Vec<&str> = got.iter().map(|g| g.2.split(':').next().unwrap()).collect();
    heads.sort();
    assert_eq!(
        heads,
        vec!["Button inside a ColumnLayout sets anchors.right", "Rectangle inside a Row sets anchors.left"]
    );
}

// --- kotlin-init-order ------------------------------------------------------------------

const VM_BEFORE: &str = r#"
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
"#;
const VM_AFTER: &str = r#"
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
"#;

#[test]
fn lock_declared_below_init() {
    let got = lint("kotlin-init-order", &[("LauncherViewModel.kt", VM_BEFORE)], "");
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("init uses indexLock (through searchResults)"), "{}", got[0].2);
}

#[test]
fn declared_above_and_shadowed() {
    assert!(lint("kotlin-init-order", &[("LauncherViewModel.kt", VM_AFTER)], "").is_empty());
}

// --- id-kind-mix --------------------------------------------------------------------------

const ROUTER_BEFORE: &str = r#"
    object NoticeRouter {
        fun launchIntent(context: Context, notice: HubNotice): Intent {
            val dest = destOf(notice)
            return Intent(context, SignInReviewActivity::class.java).apply {
                putExtra(NotifyIntents.EXTRA_REQUEST_ID, notice.requestId ?: dest.id)
            }
        }
    }
"#;
const ROUTER_AFTER: &str = r#"
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
"#;

#[test]
fn request_id_falling_back_to_the_dest_id() {
    let got = lint("id-kind-mix", &[("NoticeRouter.kt", ROUTER_BEFORE)], "");
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("'request' id with dest.id"));
    assert!(lint("id-kind-mix", &[("NoticeRouter.kt", ROUTER_AFTER)], "").is_empty());
}

#[test]
fn ts_guards_and_named_pairs() {
    let ts = r#"
        export function pick(n, dest, a, b) {
          const id = n.challengeId ?? dest.id;
          if (!a.sceneId || !b.id) return;
          return text(meta.challenge_id) || text(meta.request_id);
        }
    "#;
    let lines = |o: &str| lint("id-kind-mix", &[("x.ts", ts)], o).iter().map(|g| g.1).collect::<Vec<_>>();
    assert_eq!(lines(""), vec![3]);
    assert_eq!(lines("{ named = true }"), vec![3, 5]);
    let ex = "defmodule D do\n  def id(m), do: blank(m[\"challenge_id\"]) || blank(m[\"request_id\"]) || blank(m[\"identity_request_id\"])\nend\n";
    assert!(lint("id-kind-mix", &[("dest.ex", ex)], "").is_empty());
    assert_eq!(lint("id-kind-mix", &[("dest.ex", ex)], "{ named = true }").len(), 1);
}

// --- ets-owner ------------------------------------------------------------------------------
// Skipped: ets-owner walks the call graph (who reaches the :ets.new) — phase 2.

// --- handle-info-catch-all ----------------------------------------------------------------

const STOCKS_BEFORE: &str = r#"
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
"#;

#[test]
fn a_shared_topic_crashed_stocks() {
    assert_eq!(lint("handle-info-catch-all", &[("lib/stocks.ex", STOCKS_BEFORE)], "").len(), 1);
    let after = STOCKS_BEFORE.replace(
        "      def handle_info(:fetch_quotes, state), do: {:noreply, state}\n",
        "      def handle_info(:fetch_quotes, state), do: {:noreply, state}\n\n      def handle_info(message, state) do\n        Logger.warning(\"[Stocks] ignored message #{inspect(message)}\")\n        {:noreply, state}\n      end\n",
    );
    assert!(lint("handle-info-catch-all", &[("lib/stocks.ex", &after)], "").is_empty());
}

#[test]
fn a_client_subscribe_api_is_not_the_server_subscribing() {
    let api = STOCKS_BEFORE.replace("        Phoenix.PubSub.subscribe(App.PubSub, \"settings:updated\")\n", "");
    assert!(lint("handle-info-catch-all", &[("lib/stocks.ex", &api)], "").is_empty());
}

// --- CSS: clipped popups and overlays under the shell ---------------------------------------

const BAR_TSX: &str = r#"
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
"#;
const CSS_BEFORE: &str = r#"
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
"#;

fn css_after() -> String {
    CSS_BEFORE.replace("      overflow: hidden; /* rounded corners */\n", "")
        + ".hitl-overlay {\n  position: fixed;\n  z-index: 90;\n}\n"
}

#[test]
fn the_model_menu_was_cropped_by_the_bar_shell() {
    let got = lint("css-popup-clipped", &[("CommandInput.tsx", BAR_TSX), ("styles/surface.css", CSS_BEFORE)], "");
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains(".command-pop opens outside its box (bottom: calc(100% + 0.5rem))"), "{}", got[0].2);
    assert!(got[0].2.contains(".command-dock-shell (overflow: hidden)"));
    let after = css_after();
    assert!(lint("css-popup-clipped", &[("CommandInput.tsx", BAR_TSX), ("styles/surface.css", &after)], "").is_empty());
}

#[test]
fn the_approval_drew_under_the_dock() {
    let got = lint("css-overlay-order", &[("styles/surface.css", CSS_BEFORE)], "");
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains(".hitl-overlay covers the screen at z-index 40, under .command-dock (50)"), "{}", got[0].2);
    assert!(lint("css-overlay-order", &[("styles/surface.css", &css_after())], "").is_empty());
}

#[test]
fn an_explicit_ladder() {
    let css = css_after() + ".top-strip { position: fixed; z-index: 95; }\n";
    let got = lint("css-overlay-order", &[("styles/surface.css", &css)], r#"{ above = ["command-dock", "top-strip"] }"#);
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("under .top-strip (95)"));
}

#[test]
fn unknown_lint_and_option() {
    let got = run(
        &[("a.kt", "fun a() {}\n")],
        "[[rule]]\nid = \"x\"\nkind = \"lint\"\nlint = \"disabled-by-dta\"\n[[rule]]\nid = \"y\"\nkind = \"lint\"\nlint = \"id-kind-mix\"\noptions = { nmed = true }\n",
    );
    assert!(got["x"][0].2.contains("did you mean 'disabled-by-data'"));
    assert!(got["y"][0].2.contains("unknown option(s) nmed"));
}
