"""Manifest-backed contracts (python3 -m unittest discover tests):
`kind = "essential"` (controls a bar always shows), fanout `unhandled` (a
branch that names a token and still sends it to the default), `together`
with `func` / `depth` (an error path that logs, maybe in a helper), and a
rule's `gate = "changed"`. Fixtures are the elixir-pot bugs of 2026-10-01."""
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, tidy  # noqa: E402
from test_lint import run  # noqa: E402

# --- essential controls -------------------------------------------------------------------

BAR_BEFORE = """
    export function CommandInput({ onSend, onCancel }) {
      return (
        <div className="command-dock">
          <div className="command-row">
            <div className="command-menu-anchor">
              <button type="button" data-testid="parity-model-chip" onClick={() => setChipMenu((o) => !o)}>Model</button>
              {chipMenu ? (
                <div className="v-menu command-pop" aria-label="Veridian">
                  <button type="button" aria-label="Minimize chat dock" onClick={() => setDockMinimized(true)}>
                    Minimize chat
                  </button>
                  {!busy || autoApproveArmed ? (
                    <button type="button" data-testid="parity-auto-approve" disabled={busy} onClick={toggle}>
                      Auto-approve
                    </button>
                  ) : null}
                </div>
              ) : null}
            </div>
            <button type="button" className="command-send" data-testid="parity-send" onClick={submit}
              disabled={!text.trim() || uploading}>Send</button>
            {busy && (
              <button type="button" className="command-cancel" data-testid="parity-abort" onClick={onCancel}>Abort</button>
            )}
          </div>
        </div>
      );
    }
"""
BAR_AFTER = """
    export function CommandInput({ onSend, onCancel }) {
      return (
        <div className="command-dock">
          <div className="command-row">
            <div className="command-menu-anchor">
              <button type="button" data-testid="parity-model-chip" onClick={() => setChipMenu((o) => !o)}>Model</button>
              {chipMenu ? <div className="v-menu command-pop">...</div> : null}
            </div>
            <button type="button" data-testid="parity-auto-approve" aria-pressed={on} onClick={toggle}>Auto-approve</button>
            <button type="button" aria-label="Minimize chat dock" onClick={() => setDockMinimized(true)}>Minimize</button>
            <button type="button" className="command-send" data-testid="parity-send" onClick={submit}
              disabled={!text.trim() || uploading}>Send</button>
            {busy && (
              <button type="button" className="command-cancel" data-testid="parity-abort" onClick={onCancel}>Abort</button>
            )}
          </div>
        </div>
      );
    }
"""
BAR_RULE = """
    [[rule]]
    id = "chat-bar"
    kind = "essential"
    in = ["CommandInput.tsx"]
    bar = 'className="command-row"'
    [rule.controls]
    send = 'data-testid="parity-send"'
    auto_approve = { marker = 'data-testid="parity-auto-approve"', disabled = false }
    minimize = 'aria-label="Minimize chat dock"'
    abort = { marker = 'data-testid="parity-abort"', when = '^busy\\b' }
"""

COMPOSER_KT = """
    @Composable
    fun VoiceChatComposer(vm: Vm, chat: Chat) {
        var menuOpen by remember { mutableStateOf(false) }
        Column(M.fillMaxWidth()) {
            Row(M.fillMaxWidth()) {
                Box(M.background(if (sendEnabled) A else B).clickable(enabled = sendEnabled) { send() }.testTag("chat_send")) {}
                if (chat.primaryBusy) {
                    Box(M.clickable { vm.abort() }.testTag("chat_abort")) {}
                }
                Box {
                    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                        DropdownMenuItem(text = { Text("AUTO") }, onClick = { vm.toggle() }, modifier = M.testTag("chat_auto_approve"))
                    }
                }
            }
        }
    }
"""
COMPOSER_RULE = """
    [[rule]]
    id = "osa-bar"
    kind = "essential"
    in = ["VoiceChatBar.kt"]
    bar = 'fun VoiceChatComposer\\('
    controls = { send = 'testTag\\("chat_send"\\)', abort = 'testTag\\("chat_abort"\\)', auto_approve = 'testTag\\("chat_auto_approve"\\)', gone = 'testTag\\("chat_gone"\\)' }
"""

CHAT_QML = """
    Rectangle {
      Row {
        id: composerRow
        DeskButton { text: "Send"; onClicked: chat.sendDraft() }
        DeskButton {
          text: "auto"
          visible: !chat.busy
          onClicked: continuity.chatAutoApprove = !continuity.chatAutoApprove
        }
      }
    }
"""
QML_RULE = """
    [[rule]]
    id = "osd-bar"
    kind = "essential"
    in = ["ChatHost.qml"]
    bar = 'id:\\s*composerRow'
    controls = { send = 'text:\\s*"Send"', auto_approve = 'chatAutoApprove = !continuity' }
"""


class Essential(unittest.TestCase):
    def test_minimize_and_auto_approve_were_hidden_in_the_chip_menu(self):
        got = run({"CommandInput.tsx": BAR_BEFORE}, BAR_RULE)["chat-bar"]
        msgs = sorted(m for _, _, m in got)
        self.assertEqual(len(msgs), 2)
        self.assertIn("'auto_approve' can be disabled (disabled={busy})", msgs[0])
        self.assertIn("'minimize' is inside a menu", msgs[1])
        self.assertNotIn("chat-bar", run({"CommandInput.tsx": BAR_AFTER}, BAR_RULE))

    def test_abort_without_its_allowed_condition(self):
        rule = BAR_RULE.replace("abort = { marker = 'data-testid=\"parity-abort\"', when = '^busy\\b' }",
                                "abort = 'data-testid=\"parity-abort\"'")
        got = run({"CommandInput.tsx": BAR_AFTER}, rule)["chat-bar"]
        self.assertEqual(len(got), 1)
        self.assertIn("'abort' renders only when {busy &&", got[0][2])

    def test_compose_bar(self):
        got = {m.split("'")[1]: m for _, _, m in run({"VoiceChatBar.kt": COMPOSER_KT}, COMPOSER_RULE)["osa-bar"]}
        self.assertEqual(sorted(got), ["abort", "auto_approve", "gone"])
        self.assertIn("renders only when if (chat.primaryBusy)", got["abort"])
        self.assertIn("inside a menu (DropdownMenu", got["auto_approve"])
        self.assertIn("is gone", got["gone"])

    def test_qml_bar(self):
        got = run({"ChatHost.qml": CHAT_QML}, QML_RULE)["osd-bar"]
        self.assertEqual(len(got), 1)
        self.assertIn("'auto_approve' renders only when visible: !chat.busy", got[0][2])


# --- fanout: a branch that still goes to the default ------------------------------------------

DEST_EX = 'defmodule Dest do\n  @kinds ~w(chat sign_in share_link inbox)\nend\n'
ROUTE_TS_BEFORE = """
    export function routeDest(dest) {
      const kind = text(dest?.kind) || "inbox";
      switch (kind) {
        case "sign_in":
          return { kind: "sign_in", id };
        case "chat":
          return { kind: "chat", conversationId: id };
        // Share-link notices open the notification center; Surface has no share-link screen.
        case "share_link":
          return { kind: "center" };
        default:
          return { kind: "center" };
      }
    }
"""
ROUTE_TS_AFTER = ROUTE_TS_BEFORE.replace('return { kind: "center" };\n        default', 'return { kind: "share_link", id };\n        default')
ROUTE_KT = """
    object NoticeRouter {
        fun route(dest: Dest): Target =
            when (dest.kind) {
                "sign_in", "chat" -> Target.Chat(dest.id)
                "share_link" -> Target.Center
                else -> Target.Center
            }
    }
"""
FANOUT = """
    [[rule]]
    id = "dest-kinds"
    kind = "fanout"
    source = { in = ["dest.ex"], within = '@kinds ~w\\(([^)]*)\\)', pattern = '(\\w+)', text = "raw" }
    [rule.views.surface]
    in = ["noticeDest.ts"]
    func = "routeDest"
    pattern = 'case "(\\w+)":'
    unhandled = 'kind:\\s*"center"'
    except = { inbox = "the default is the list itself" }
    [rule.views.osa]
    in = ["NoticeRouter.kt"]
    func = "route"
    pattern = '"(\\w+)"'
    unhandled = 'Target\\.Center\\b'
    except = { inbox = "the default is the list itself" }
"""


class FanoutUnhandled(unittest.TestCase):
    def test_share_link_named_but_sent_to_the_default(self):
        got = run({"dest.ex": DEST_EX, "noticeDest.ts": ROUTE_TS_BEFORE, "NoticeRouter.kt": ROUTE_KT}, FANOUT)
        msgs = sorted(m for _, _, m in got["dest-kinds"])
        self.assertEqual(msgs, ["osa does not handle 'share_link'", "surface does not handle 'share_link'"])
        got = run({"dest.ex": DEST_EX, "noticeDest.ts": ROUTE_TS_AFTER,
                   "NoticeRouter.kt": ROUTE_KT.replace("Target.Center\n                else", "Target.ShareLink(dest.id)\n                else")},
                  FANOUT)
        self.assertNotIn("dest-kinds", got)


# --- together: func / depth ---------------------------------------------------------------

CHANNEL_BEFORE = """
    defmodule Web.OsdChannel do
      def join("osd:" <> key, _payload, socket) do
        case authorize(key) do
          {:ok, socket} -> {:ok, socket}
          {:error, _} -> {:error, %{reason: "unauthorized"}}
        end
      end

      def handle_in("x", _, socket), do: {:reply, {:error, %{reason: "unknown_event"}}, socket}
    end
"""
CHANNEL_AFTER = """
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
"""
JOIN_RULE = """
    [[rule]]
    id = "joins-log"
    kind = "together"
    in = ["lib/**"]
    func = "join"
    if = '\\{:error,(?![^\\n]*(?:->|<-))'
    then = 'Logger\\.'
    depth = 1
"""


class TogetherFuncDepth(unittest.TestCase):
    def test_a_refused_join_said_nothing(self):
        got = run({"lib/osd_channel.ex": CHANNEL_BEFORE}, JOIN_RULE)
        self.assertEqual([(f, ln) for f, ln, _ in got["joins-log"]], [("lib/osd_channel.ex", 6)])
        self.assertNotIn("joins-log", run({"lib/osd_channel.ex": CHANNEL_AFTER}, JOIN_RULE))


# --- gate = "changed" ----------------------------------------------------------------------

def git(root, *args):
    subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t", *args], cwd=root, check=True, capture_output=True)


class GateChanged(unittest.TestCase):
    def test_old_debt_shows_new_debt_blocks(self):
        cfg = ('[project]\nname = "t"\ninclude = ["**"]\n'
               '[[rule]]\nid = "ids"\nkind = "lint"\nlint = "id-kind-mix"\nseverity = "warn"\ngate = "changed"\n')
        old = "fun a(n: N, dest: D) = put(n.requestId ?: dest.id)\n"
        with tempfile.TemporaryDirectory() as root:
            for name, body in {"seamcheck.toml": cfg, "A.kt": old, "B.kt": "fun b() = 1\n"}.items():
                with open(os.path.join(root, name), "w") as fh:
                    fh.write(body)
            git(root, "init", "-q")
            git(root, "add", "-A")
            git(root, "commit", "-qm", "base")
            with open(os.path.join(root, "B.kt"), "w") as fh:
                fh.write("fun b() = 2\n")
            p = core.load(root, os.path.join(root, "seamcheck.toml"), use_cache=False)
            self.assertEqual(len([f for f in core.run_rules(p) if f.rule == "ids"]), 1)  # check still shows it
            self.assertEqual(tidy.gate(p, "HEAD")["rules"], [])  # old debt in an untouched line
            with open(os.path.join(root, "B.kt"), "w") as fh:
                fh.write("fun b(n: N, dest: D) = put(n.challengeId ?: dest.id)\n")
            p = core.load(root, os.path.join(root, "seamcheck.toml"), use_cache=False)
            rep = tidy.gate(p, "HEAD")
            self.assertEqual([(f.file, f.line) for f in rep["rules"]], [("B.kt", 1)])

    def test_bad_gate_value(self):
        got = run({"a.kt": "fun a() = 1\n"}, '[[rule]]\nid = "x"\nkind = "lint"\nlint = "id-kind-mix"\ngate = "sometimes"\n')
        self.assertIn("gate 'sometimes'", got["config"][0][2])


if __name__ == "__main__":
    unittest.main()
