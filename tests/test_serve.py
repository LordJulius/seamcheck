"""`seamcheck serve`, the MCP server (python3 -m unittest discover tests).

An agent's loop of small questions (who calls this, is it clean now) paid a
process start and a load per question. The server keeps the loaded project;
the bug to never have is a stale answer: an edit between two calls must be
seen by the second, and an unchanged tree must not reload.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
from seamcheck import core, serve  # noqa: E402

FILES = {
    "seamcheck.toml": ('[project]\nname = "t"\n\n[[rule]]\nid = "writes-go-through-store"\nkind = "forbid"\n'
                       'pattern = \'db\\.execute\'\nallow = ["src/app/store.py"]\nwhy = "the store clears the cache"\n'),
    "src/app/__init__.py": "",
    "src/app/store.py": "def save(x):\n    db.execute(x)\n",
    "src/app/view.py": "from .store import save\n\n\ndef show(x):\n    return save(x)\n",
    "tests/test_view.py": "from app.view import show\n\n\ndef test_show():\n    show(1)\n",
}
TOOLS = {"check", "gate", "callers", "callees", "path", "find", "refs", "impact", "stats", "unused", "tests"}


def git(root, *args):
    subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t", *args], cwd=root, check=True, capture_output=True)


class Session:
    """A server subprocess, spoken to one JSON line at a time."""

    def __init__(self, root, home):
        env = dict(os.environ, PYTHONPATH=ROOT, HOME=home)
        self.proc = subprocess.Popen([sys.executable, "-m", "seamcheck", "serve", "-C", root], env=env, text=True,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.n = 0

    def send(self, msg):
        self.proc.stdin.write((msg if isinstance(msg, str) else json.dumps(msg)) + "\n")
        self.proc.stdin.flush()

    def read(self):
        return json.loads(self.proc.stdout.readline())  # (a stray print would fail here)

    def request(self, method, params=None):
        self.n += 1
        self.send({"jsonrpc": "2.0", "id": self.n, "method": method, **({"params": params} if params else {})})
        reply = self.read()
        assert reply["id"] == self.n, reply
        return reply

    def tool(self, tool, **args):
        res = self.request("tools/call", {"name": tool, "arguments": args})["result"]
        return json.loads(res["content"][0]["text"]), res["isError"]

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=30)
        self.proc.stdout.close()
        self.proc.stderr.close()


class ServeTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = os.path.join(self.dir.name, "repo")
        self.write(FILES)
        git(self.root, "init", "-q")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-qm", "base")

    def tearDown(self):
        self.dir.cleanup()

    def write(self, files):
        for name, body in files.items():
            path = os.path.join(self.root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)

    def session(self):
        s = Session(self.root, self.dir.name)
        self.addCleanup(s.close)
        init = s.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                        "clientInfo": {"name": "test", "version": "0"}})["result"]
        self.assertEqual((init["protocolVersion"], init["serverInfo"]["name"]), ("2025-06-18", "seamcheck"))
        s.send({"jsonrpc": "2.0", "method": "notifications/initialized"})  # no reply: the next line is ping's
        self.assertEqual(s.request("ping")["result"], {})
        return s

    def test_an_edit_between_two_calls_is_seen(self):
        s = self.session()
        [hit], _ = s.tool("callers", name="save")
        self.assertEqual([r["id"] for r in hit["reach"]], ["src/app/view.show", "tests/test_view.test_show"])
        findings, _ = s.tool("check")
        self.assertEqual([(f["rule"], f["file"]) for f in findings], [])
        self.write({"src/app/view.py": FILES["src/app/view.py"] + "\n\ndef again(x):\n    db.execute(x)\n    return save(x)\n"})
        [hit], _ = s.tool("callers", name="save", depth=1)
        self.assertIn("src/app/view.again", [r["id"] for r in hit["reach"]])
        findings, _ = s.tool("check")
        self.assertEqual([(f["rule"], f["file"], f["line"], f["why"]) for f in findings],
                         [("writes-go-through-store", "src/app/view.py", 9, "the store clears the cache")])
        gate, _ = s.tool("gate")
        self.assertTrue(gate["blocked"])
        self.assertEqual(gate["changed_files"], ["src/app/view.py"])
        # a gate looks only at the change; the check after it still sees everything
        self.write({"src/app/other.py": "def other(x):\n    db.execute(x)\n"})
        git(self.root, "add", "-A")
        git(self.root, "commit", "-qm", "more")
        self.assertEqual(s.tool("gate")[0]["blocked"], False)
        self.assertEqual(len(s.tool("check")[0]), 2)

    def test_every_tool_answers(self):
        s = self.session()
        tools = s.request("tools/list")["result"]["tools"]
        self.assertEqual({t["name"] for t in tools}, TOOLS)
        for t in tools:
            self.assertEqual(t["inputSchema"]["type"], "object")
            self.assertTrue(t["description"])
        self.assertEqual(s.tool("find", name="show")[0][0]["file"], "src/app/view.py")
        self.assertEqual([c["id"] for c in s.tool("callees", name="show")[0][0]["reach"]], ["src/app/store.save"])
        self.assertEqual([w["id"] for w in s.tool("path", **{"from": "test_show", "to": "save"})[0]["path"]],
                         ["tests/test_view.test_show", "src/app/view.show", "src/app/store.save"])
        self.assertIn("tests", s.tool("impact", targets=["src/app/store.py"])[0]["areas"])
        self.assertEqual(s.tool("stats")[0]["resolution"][".py"]["project_exact"], 100)
        self.assertEqual(s.tool("unused")[0], [])
        self.assertEqual([r["reach"] > 0 for r in s.tool("tests")[0]], [True])
        self.assertTrue(s.tool("refs", name="save")[0]["callers"])
        self.assertEqual(s.tool("unused", since="HEAD")[0], [])

    def test_errors(self):
        s = self.session()
        out, bad = s.tool("callers", name="nosuch")
        self.assertTrue(bad)
        self.assertIn("no function matches 'nosuch'", out["error"])
        out, bad = s.tool("path", to="save")
        self.assertTrue(bad)
        self.assertIn("missing from", out["error"])
        self.assertEqual(s.request("tools/call", {"name": "nosuch"})["error"]["code"], -32602)
        self.assertEqual(s.request("resources/list")["error"]["code"], -32601)
        s.send("{not json")
        self.assertEqual(s.read()["error"]["code"], -32700)
        self.assertEqual(s.request("ping")["result"], {})  # still serving


class Reload(unittest.TestCase):
    def test_an_unchanged_tree_is_not_loaded_again(self):
        with tempfile.TemporaryDirectory() as root:
            for name, body in FILES.items():
                os.makedirs(os.path.dirname(os.path.join(root, name)), exist_ok=True)
                with open(os.path.join(root, name), "w") as fh:
                    fh.write(body)
            srv = serve.Server(root, [], use_cache=False)
            with mock.patch.object(core, "load", wraps=core.load) as load:
                srv.call("find", {"name": "save"})
                srv.call("check", {})
                self.assertEqual(load.call_count, 1)
                with open(os.path.join(root, "src/app/new.py"), "w") as fh:  # a new file is a change too
                    fh.write("def fresh():\n    return 1\n")
                self.assertEqual(srv.call("find", {"name": "fresh"})[0]["file"], "src/app/new.py")
                self.assertEqual(load.call_count, 2)


if __name__ == "__main__":
    unittest.main()
