"""QML, shell and Elixir lints from the bugs of 2026-10-12..10-19
(python3 -m unittest discover tests). Each check gets the real bug, cut
down (before), and its fix (after)."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_lint import run  # noqa: E402

CMAKE = """qt_add_qml_module(app
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
"""
FILES = {
    "CMakeLists.txt": CMAKE,
    "qml/Tokens.qml": "pragma Singleton\nimport QtQuick\nQtObject { property int gap: 8 }\n",
    "qml/Theme.qml": "pragma Singleton\nimport QtQuick\nimport VeridianOsd\nQtObject { property int gap: Tokens.gap }\n",
    "qml/HubMove.qml": "import QtQuick\nItem { }\n",
    "src/MapHost.h": "class MapHost : public QQuickItem {\n  Q_OBJECT\n  QML_ELEMENT\n};\n",
}


def rule(lint: str) -> str:
    return f'[[rule]]\nid = "{lint}"\nkind = "lint"\nlint = "{lint}"\n'


class QmlTest(unittest.TestCase):
    def test_unknown_type(self):
        # FoodBookDive.qml (4f053c251): `Sheet { visible: page.sheetOpen }`, no Sheet anywhere
        page = ("import QtQuick\nimport QtQuick.Controls\nimport VeridianOsd\nItem {\n  id: page\n"
                "  component Row2: Row { }\n  Rectangle { }\n  delegate: Button { }\n  Behavior on x { NumberAnimation { } }\n"
                "  VButton { }\n  MapHost { }\n  Row2 { }\n  Sheet { visible: true }\n}\n")
        files = {**FILES, "qml/VButton.qml": "import QtQuick\nRectangle { }\n", "qml/FoodBookDive.qml": page}
        out = run(files, rule("qml-unknown-type"))
        self.assertEqual([(f, ln) for f, ln, _ in out["qml-unknown-type"]], [("qml/FoodBookDive.qml", 13)])
        fixed = {**files, "qml/FoodBookDive.qml": page.replace("  Sheet { visible: true }\n", "")}
        self.assertNotIn("qml-unknown-type", run(fixed, rule("qml-unknown-type")))

    def test_unknown_type_skips_modules_it_cannot_see(self):
        page = "import QtQuick\nimport MapLibre\nItem { MapView { } }\n"
        self.assertNotIn("qml-unknown-type", run({**FILES, "qml/Main.qml": page}, rule("qml-unknown-type")))

    def test_singleton_without_its_module_import(self):
        # eab0b522c: VButton used Tokens with only `import QtQuick`; Qt 6.11.2 read
        # the plain Tokens.qml (an empty object) and the window went black
        button = "import QtQuick\nRectangle {\n  radius: Tokens.gap\n}\n"
        out = run({**FILES, "qml/VButton.qml": button}, rule("qml-singleton-import"))
        self.assertEqual([(f, ln) for f, ln, _ in out["qml-singleton-import"]], [("qml/VButton.qml", 3)])
        fixed = {**FILES, "qml/VButton.qml": button.replace("import QtQuick\n", "import QtQuick\nimport VeridianOsd\n")}
        self.assertNotIn("qml-singleton-import", run(fixed, rule("qml-singleton-import")))

    def test_js_library_named_like_a_type(self):
        # eab0b522c: HubMove.js beside the HubMove singleton hid it (now HubMoveRules.js)
        files = {**FILES, "qml/HubMove.js": ".pragma library\nfunction ok() { return 1 }\n"}
        out = run(files, rule("qml-js-shadows-type"))
        self.assertEqual([f for f, _, _ in out["qml-js-shadows-type"]], ["qml/HubMove.js"])
        renamed = {**FILES, "qml/HubMoveRules.js": files["qml/HubMove.js"]}
        self.assertNotIn("qml-js-shadows-type", run(renamed, rule("qml-js-shadows-type")))


class ShellPkillSelfTest(unittest.TestCase):
    def test_pkill_inside_the_command_string_it_runs_in(self):
        # 2026-10-19: restarting the TV laptop's game server killed the restart's own shell
        bad = "#!/bin/bash\nssh tvlaptop \"bash -lc 'cd ~/bbb-live; pkill -f \\\"mix phx.server\\\"; mix phx.server'\"\n"
        out = run({"scripts/restart.sh": bad}, rule("shell-pkill-self"))
        self.assertEqual([(f, ln) for f, ln, _ in out["shell-pkill-self"]], [("scripts/restart.sh", 2)])
        good = bad.replace('\\"mix phx.server\\"', "'[m]ix phx.server'")
        self.assertNotIn("shell-pkill-self", run({"scripts/restart.sh": good}, rule("shell-pkill-self")))

    def test_pkill_naming_the_script_itself(self):
        out = run({"scripts/restart-server.sh": "#!/bin/bash\npkill -f server\n"}, rule("shell-pkill-self"))
        self.assertIn("shell-pkill-self", out)
        self.assertNotIn("shell-pkill-self", run({"scripts/restart.sh": "#!/bin/bash\npkill -f 'mix phx.server'\n"},
                                                 rule("shell-pkill-self")))


class ElixirRescueDebugTest(unittest.TestCase):
    def test_a_failure_logged_only_at_debug(self):
        # f3ddc68cd: Spotify.Player's poll rescue logged at debug; the save it hid raised every poll
        bad = ("defmodule Player do\n  require Logger\n\n  def poll do\n    save()\n  rescue\n"
               "    e ->\n      Logger.debug(\"poll failed: #{Exception.message(e)}\")\n      :not_playing\n  end\nend\n")
        out = run({"lib/player.ex": bad}, rule("elixir-rescue-debug"))
        self.assertEqual([(f, ln) for f, ln, _ in out["elixir-rescue-debug"]], [("lib/player.ex", 8)])
        good = bad.replace("Logger.debug", "Logger.warning")
        self.assertNotIn("elixir-rescue-debug", run({"lib/player.ex": good}, rule("elixir-rescue-debug")))
        reraised = bad.replace("      :not_playing\n", "      reraise e, __STACKTRACE__\n")
        self.assertNotIn("elixir-rescue-debug", run({"lib/player.ex": reraised}, rule("elixir-rescue-debug")))


if __name__ == "__main__":
    unittest.main()
