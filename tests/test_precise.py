"""Precise mode (python3 -m unittest discover tests): a language server settles
the calls the static resolver guesses. The client is tested against a fake
LSP server written here (no real server needed); each real server has one
test, skipped where it isn't installed."""
import contextlib
import io
import os
import sys
import tempfile
import textwrap
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, fingerprint, precise  # noqa: E402

# A tiny LSP server: a definition is the first `def NAME(` in the root's .py
# files, except `dumps`, which lives in a library outside the root. Modes:
# "crash" exits at once, "hang" never answers a definition. Each definition
# request is logged (one line) to the file named by argv[2].
FAKE = r'''
import json, os, re, sys
mode, log = sys.argv[1], sys.argv[2]
root, docs = None, {}
if mode == "crash":
    sys.stderr.write("fake: boom\n")
    sys.exit(3)

def read():
    n = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        k, _, v = line.partition(b":")
        if k.lower() == b"content-length":
            n = int(v)
    return json.loads(sys.stdin.buffer.read(n))

def send(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()

def where(name):
    if name == "dumps":
        return "file:///usr/lib/python3/json/__init__.py", 230, 4
    for dp, _, fs in sorted(os.walk(root)):
        for f in sorted(fs):
            if f.endswith(".py"):
                lines = open(os.path.join(dp, f)).read().split("\n")
                for i, l in enumerate(lines):
                    if re.match(r"\s*def " + name + r"\(", l):
                        return "file://" + os.path.join(dp, f), i, l.index(name)
    return None

while (msg := read()) is not None:
    m, p = msg.get("method"), msg.get("params") or {}
    if m == "initialize":
        root = p["rootPath"]
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {"definitionProvider": True}}})
        send({"jsonrpc": "2.0", "id": 99, "method": "workspace/configuration", "params": {"items": [{}]}})
    elif m == "textDocument/didOpen":
        docs[p["textDocument"]["uri"]] = p["textDocument"]["text"].split("\n")
    elif m == "textDocument/definition":
        with open(log, "a") as fh:
            fh.write("q\n")
        if mode == "hang":
            continue
        line = docs[p["textDocument"]["uri"]][p["position"]["line"]]
        name = re.match(r"\w+", line[p["position"]["character"]:]).group(0)
        hit = where(name)
        pos = {"line": hit[1], "character": hit[2]} if hit else None
        send({"jsonrpc": "2.0", "id": msg["id"], "result": hit and [{"uri": hit[0], "range": {"start": pos, "end": pos}}]})
    elif m == "shutdown":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    elif m == "exit":
        break
'''

FILES = {
    "shapes.py": """\
        class Circle:
            def area(self):
                return 3


        class Square:
            def area(self):
                return 4
        """,
    "util.py": """\
        def helper():
            return 1


        def helper2():
            return 2
        """,
    "main.py": """\
        from util import helper2


        def use(x, s):
            helper2()
            x.area()
            s.dumps()
            helper()
        """,
}


class Fixture:
    """A temp project whose .py server is FAKE in the given mode."""

    def __init__(self, mode="ok", extra=""):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name
        self.log = os.path.join(self.root, ".fake.log")
        fake = os.path.join(self.root, ".fake_server.py")
        with open(fake, "w") as fh:
            fh.write(FAKE)
        for name, body in FILES.items():
            with open(os.path.join(self.root, name), "w") as fh:
                fh.write(textwrap.dedent(body))
        with open(os.path.join(self.root, "seamcheck.toml"), "w") as fh:
            fh.write(f'[project]\nexclude = [".fake*"]\n[precise]\nservers = {{ ".py" = [{sys.executable!r}, '
                     f'{fake!r}, "{mode}", {self.log!r}] }}\n{extra}')

    def load(self, **kw):
        with contextlib.redirect_stderr(io.StringIO()):  # (the warnings the tests expect)
            return core.load(self.root, use_cache=kw.pop("use_cache", False), **kw)

    def asked(self) -> int:
        try:
            with open(self.log) as fh:
                return len(fh.readlines())
        except OSError:
            return 0


def edges(p, suffix):
    [fid] = [f for f in p.funcs if f.endswith(suffix)]
    return sorted((t, ex) for t, _, ex in p.edges.get(fid, []))


class FramingTest(unittest.TestCase):
    def test_messages_round_trip_and_tsserver_newlines_are_skipped(self):
        stream = io.BytesIO(precise.frame({"id": 1}) + b"\n" + precise.frame({"id": 2, "result": [1]})
                            + b"Content-Length: 40\r\n\r\n{\"cut")
        self.assertEqual(precise.read_message(stream), {"id": 1})
        self.assertEqual(precise.read_message(stream), {"id": 2, "result": [1]})
        self.assertIsNone(precise.read_message(stream))  # a body cut short: the server died

    def test_columns_count_utf16_units(self):
        line = 'x = "\U0001F600"; y.area()'
        col = line.index("area")
        self.assertEqual(precise.to_u16(line, col), col + 1)  # the emoji is two units
        self.assertEqual(precise.from_u16(line, col + 1), col)


class FakeServerTest(unittest.TestCase):
    def test_guesses_become_exact_or_library_and_counts_follow(self):
        fx = Fixture()
        with fx.dir:
            static, p = fx.load(), fx.load(precise=True)
            self.assertIn(("shapes.Square.area", False), edges(static, "main.use"))
            got = edges(p, "main.use")
            self.assertIn(("shapes.Circle.area", True), got)  # x.area(): the server's one answer
            self.assertNotIn(("shapes.Square.area", False), got)
            self.assertIn(("util.helper", True), got)  # helper(): nothing statically (no import), found
            # every call is still counted once: the totals don't move
            self.assertEqual(sum(static.resolution[".py"][:3]), sum(p.resolution[".py"][:3]))
            self.assertEqual(p.precise["exact_from_guess"], 1)
            self.assertEqual(p.precise["exact_from_nothing"], 1)
            self.assertEqual(p.precise["warnings"], [])
            self.assertIn("1 guesses and 1 nothings made exact", precise.summary(p.precise))

    def test_a_library_answer_drops_the_guesses(self):
        fx = Fixture()
        with fx.dir:
            with open(os.path.join(fx.root, "shapes.py"), "a") as fh:
                fh.write("\n\nclass Codec:\n    def dumps(self):\n        pass\n\n\nclass Other:\n    def dumps(self):\n        pass\n")
            static, p = fx.load(), fx.load(precise=True)
            self.assertIn(("shapes.Codec.dumps", False), edges(static, "main.use"))
            self.assertFalse([e for e in edges(p, "main.use") if "dumps" in e[0]])
            self.assertEqual(p.precise["library"], 1)
            self.assertEqual(p.resolution[".py"][1], static.resolution[".py"][1] - 2)  # area and dumps

    def test_a_crashing_server_is_a_warning_and_the_graph_stays_static(self):
        fx = Fixture("crash")
        with fx.dir:
            static, p = fx.load(), fx.load(precise=True)
            self.assertEqual(dict(p.edges), dict(static.edges))
            self.assertEqual(len(p.precise["warnings"]), 1)
            self.assertIn("code 3", p.precise["warnings"][0])
            self.assertEqual(core.run_rules(p), [])  # no config error: the run doesn't fail

    def test_a_server_that_never_answers_is_given_up_on(self):
        fx = Fixture("hang", "wait = 1\ntimeout = 1\n")
        with fx.dir:
            static, p = fx.load(), fx.load(precise=True)
            self.assertEqual(dict(p.edges), dict(static.edges))
            self.assertIn("no answer", p.precise["warnings"][0])

    def test_a_configured_server_that_is_not_installed_is_a_warning(self):
        fx = Fixture()
        with fx.dir:
            with open(os.path.join(fx.root, "seamcheck.toml"), "w") as fh:
                fh.write('[precise]\nenabled = true\nservers = { ".py" = ["no-such-server-x9"] }\n')
            p = fx.load()  # (enabled by config)
            self.assertEqual(p.precise["warnings"], ["precise: no-such-server-x9 (.py) is not installed"])

    def test_a_warm_run_asks_nothing(self):
        fx = Fixture()
        with fx.dir, tempfile.TemporaryDirectory() as cache, mock.patch.object(fingerprint, "CACHE_DIR", cache):
            p = fx.load(precise=True, use_cache=True)
            n = fx.asked()
            self.assertGreater(n, 0)
            # another file changes: the graph is rebuilt, main.py's answers come from the cache
            with open(os.path.join(fx.root, "util.py"), "a") as fh:
                fh.write("\n\ndef helper3():\n    return 3\n")
            again = fx.load(precise=True, use_cache=True)
            self.assertEqual(fx.asked(), n)
            self.assertEqual(edges(again, "main.use"), edges(p, "main.use"))
            self.assertEqual(again.precise["cached"], again.precise["asked"])

    def test_static_mode_never_starts_a_server(self):
        fx = Fixture()
        with fx.dir:
            p = fx.load()
            self.assertEqual(fx.asked(), 0)
            self.assertEqual(p.precise, {})


# --- the real servers: one case each, where the server is installed -----------------

SHAPES = {
    ".py": ("pyright-langserver", {"shapes.py": """\
        class Circle:
            def area(self):
                return 3


        class Square:
            def area(self):
                return 4


        def total():
            shapes = {"c": Circle()}
            return shapes["c"].area()
        """}, ("shapes.Circle.area", "shapes.Square.area")),
    ".rs": ("rust-analyzer", {"Cargo.toml": '[package]\nname = "shapes"\nversion = "0.1.0"\nedition = "2021"\n',
                              "src/main.rs": """\
        struct Circle;
        struct Square;

        impl Circle {
            fn area(&self) -> u32 { 3 }
        }

        impl Square {
            fn area(&self) -> u32 { 4 }
        }

        fn total() -> u32 {
            let shapes = vec![Circle];
            shapes[0].area()
        }

        fn main() {
            println!("{}", total());
        }
        """}, ("src/main.Circle.area", "src/main.Square.area")),
    ".go": ("gopls", {"go.mod": "module example.com/shapes\n\ngo 1.21\n", "main.go": """\
        package main

        type Circle struct{}
        type Square struct{}

        func (c *Circle) Area() int { return 3 }
        func (s *Square) Area() int { return 4 }

        func total() int {
        	shapes := map[string]*Circle{"c": {}}
        	return shapes["c"].Area()
        }

        func main() { println(total()) }
        """}, ("main.Circle.Area", "main.Square.Area")),
    ".ts": ("tsserver", {"shapes.ts": """\
        class Circle {
          area(): number { return 3 }
        }

        class Square {
          area(): number { return 4 }
        }

        export function total(): number {
          const shapes = [new Circle()]
          return shapes[0].area()
        }
        """}, ("shapes.Circle.area", "shapes.Square.area")),
}


class RealServerTest(unittest.TestCase):
    def check(self, ext):
        exe, files, (right, wrong) = SHAPES[ext]
        if not precise._which(exe):
            self.skipTest(f"{exe} is not installed")
        with tempfile.TemporaryDirectory() as d:
            for name, body in files.items():
                os.makedirs(os.path.dirname(os.path.join(d, name)), exist_ok=True)
                with open(os.path.join(d, name), "w") as fh:
                    fh.write(textwrap.dedent(body))
            static = core.load(d, use_cache=False)
            self.assertEqual(edges(static, ".total"), [(right, False), (wrong, False)])
            p = core.load(d, use_cache=False, precise=True)
            self.assertEqual(p.precise["warnings"], [])
            self.assertEqual(edges(p, ".total"), [(right, True)])

    def test_pyright(self):
        self.check(".py")

    def test_rust_analyzer(self):
        self.check(".rs")

    def test_gopls(self):
        self.check(".go")

    def test_tsserver(self):
        self.check(".ts")


if __name__ == "__main__":
    unittest.main()
