"""[[bridge]] edges and `impact` (python3 -m unittest discover tests): a
client's HTTP call reaches the controller action a route table names; a hub
push reaches each client's listener; impact follows callers plus bridges,
certain edges only unless asked."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "lib/web/inbox_controller.ex": "defmodule Web.InboxController do\n  def index(conn), do: Inbox.list(conn)\nend\n",
    "lib/inbox.ex": "defmodule Inbox do\n  def list(x), do: x\nend\n",
    "lib/web/channel.ex": 'defmodule Web.Channel do\n  def handle_info(:tick, socket) do\n    push(socket, "tick", %{})\n  end\nend\n',
    "routes.txt": "  GET  /api/inbox/:id  Web.InboxController :index\n",
    "app/Api.kt": 'class Api {\n    fun inbox(id: String) {\n        get("$root/api/inbox/$id")\n    }\n}\n',
    "web/clock.ts": 'export function bind(ch: any) {\n  ch.on("tick", () => redraw())\n}\nfunction redraw() {}\n',
}

RULES = r"""[project]
name = "b"
include = ["lib/**", "app/**", "web/**"]
areas = { hub = ["lib/**"], phone = ["app/**"], web = ["web/**"] }

[[bridge]]
id = "http"
match = "segments"
from = { in = ["app/**"], pattern = '"[^"]*?(/api/[\w/$]*)', rewrite = [['\$\w+', ':p']] }
to = { run = "cat routes.txt", pattern = '(?m)^\s*\w+\s+(/api/\S*)\s+(\S+)\s+:(\w+)', token = 1, rewrite = [[':\w+', ':p']], target = "{2}.{3}" }

[[bridge]]
id = "push"
from = { in = ["lib/**"], pattern = 'push\(socket,\s*"(\w+)"' }
to = { in = ["web/**"], pattern = '\.on\(\s*"(\w+)"' }
"""


class BridgeTest(unittest.TestCase):
    def test_bridges_and_impact(self):
        with tempfile.TemporaryDirectory() as root:
            for name, body in FILES.items():
                os.makedirs(os.path.join(root, os.path.dirname(name)), exist_ok=True)
                with open(os.path.join(root, name), "w") as fh:
                    fh.write(body)
            cfg = os.path.join(root, "seamcheck.toml")
            with open(cfg, "w") as fh:
                fh.write(RULES)
            p = core.load(root, cfg, use_cache=False)
        self.assertEqual(p.config_problems, [])
        self.assertEqual(p.bridged.get(("app/Api.inbox", "Web.InboxController.index")), "http")
        self.assertEqual(p.bridged.get(("Web.Channel.handle_info", "web/clock.bind")), "push")
        # changing Inbox.list reaches the phone code that calls its route
        hit = core.impact(p, {"Inbox.list"})
        self.assertIn("app/Api.inbox", hit)
        # changing the channel reaches the web listener (forward through the bridge)
        self.assertIn("web/clock.bind", core.impact(p, {"Web.Channel.handle_info"}))


if __name__ == "__main__":
    unittest.main()
