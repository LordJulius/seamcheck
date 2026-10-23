"""Architecture rules (arch.py): `depend`, `layers` and `cycles` over imports
and exact calls, in any language. Checked against dependency-cruiser's
no-circular on excalidraw (the same 172 files, plus a cycle through
`import ... from "."` it misses) and ArchUnit's slices().beFreeOfCycles() on
jsoup (the same five packages)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "package.json": '{"name": "t", "type": "module"}',
    "src/ui/view.ts": 'import { save } from "../db/store";\n\nexport function show() {\n  save();\n}\n',
    "src/db/store.ts": 'import { log } from "../util/log";\n\nexport function save() {\n  log();\n}\n',
    "src/util/log.ts": 'import { type Shown } from "../ui/types";\n\nexport function log() {\n  return 1;\n}\n',
    "src/ui/types.ts": "export type Shown = { a: number };\n",
    "src/a/one.ts": 'import { two } from "../b/two";\n\nexport function one() {\n  two();\n}\n',
    "src/b/two.ts": 'import { one } from "../a/one";\n\nexport function two() {\n  one();\n}\n',
    "web/app.py": """import subprocess
from web.util import run_it, safe_name


def handler(request):
    cmd = request.args.get("c")
    full = "ls " + cmd
    run_it(full)
    subprocess.run("date")


def other(request):
    name = safe_name(request.args.get("n"))
    subprocess.run(name)
""",
    "web/util.py": """import subprocess


def run_it(text):
    subprocess.run(text, shell=True)


def safe_name(n):
    return n
""",
    "src/main/java/org/x/select/Visitor.java": "package org.x.select;\n\npublic interface Visitor {\n    void head(int n);\n}\n",
    "src/main/java/org/x/select/Traversor.java": """package org.x.select;

public class Traversor {
    public static void walk(Visitor v) {
        v.head(1);
    }
}
""",
    "src/main/java/org/x/examples/Printer.java": """package org.x.examples;

import org.x.select.Traversor;
import org.x.select.Visitor;

public class Printer implements Visitor {
    public void head(int n) {
    }

    public void run() {
        Traversor.walk(this);
    }
}
""",
}

RULES = """[project]
name = "t"

[[rule]]
id = "ui-not-db"
kind = "depend"
from = ["src/ui/**"]
to = ["src/db/**"]

[[rule]]
id = "layers"
kind = "layers"
layers = [{ name = "ui", in = ["src/ui/**"] }, { name = "db", in = ["src/db/**"] }, { name = "util", in = ["src/util/**"] }]

[[rule]]
id = "no-cycles"
kind = "cycles"
in = ["src/**"]

[[rule]]
id = "no-shell"
kind = "path"
from = ["web/app.py:*"]
to = ['^subprocess\\.run$']

[[rule]]
id = "no-tainted-shell"
kind = "path"
from = ["web/app.py:*"]
to = ['^subprocess\\.run$']
source = 'request\\.args'
unless = ["safe"]

[[rule]]
id = "no-package-cycles"
kind = "cycles"
in = ["src/main/java/**"]
level = "dir"
via = "any"
"""


class Arch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        for name, body in FILES.items():
            path = os.path.join(cls.dir.name, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cfg = os.path.join(cls.dir.name, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write(RULES)
        p = core.load(cls.dir.name, cfg, use_cache=False)
        cls.found = {}
        for f in core.run_rules(p):
            cls.found.setdefault(f.rule, []).append(f)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def test_depend(self):
        [f] = self.found["ui-not-db"]
        self.assertEqual((f.file, f.line), ("src/ui/view.ts", 1))

    def test_layers_and_type_only_imports(self):
        # top first: ui may use db and util, db may use util; util's `import { type Shown }`
        # from ui is erased at runtime, so it is no upward dependency
        self.assertNotIn("layers", self.found)

    def test_cycles(self):
        [f] = self.found["no-cycles"]
        self.assertIn("src/a/one.ts", f.message)
        self.assertIn("src/b/two.ts", f.message)

    def test_find_takes_the_id_it_prints_and_file_line(self):
        p = core.load(self.dir.name, os.path.join(self.dir.name, "seamcheck.toml"))
        self.assertEqual([f.id for f in p.find("src/db/store.save")], ["src/db/store.save"])
        self.assertEqual([f.id for f in p.find("src/db/store.ts:4")], ["src/db/store.save"])

    def test_graph_renders_cycles(self):
        from seamcheck import arch
        p = core.load(self.dir.name, os.path.join(self.dir.name, "seamcheck.toml"))
        g = arch.graph(p, "file", "import", ["src/a/**", "src/b/**"])
        self.assertEqual(g, {"src/a/one.ts": {"src/b/two.ts": 1}, "src/b/two.ts": {"src/a/one.ts": 1}})
        self.assertIn('"src/a/one.ts" -> "src/b/two.ts" [color=red];', arch.render(g, "dot"))
        self.assertIn("graph LR", arch.render(g, "mermaid"))

    def test_path_reaches_sinks_through_calls(self):
        got = sorted((f.func, f.message.split(": ", 1)[1]) for f in self.found["no-shell"])
        self.assertIn(("handler", "handler -> run_it"), got)
        self.assertIn(("handler", "handler"), got)  # the constant `subprocess.run("date")`

    def test_path_with_a_source_follows_the_tainted_value(self):
        # handler's `cmd` -> `full` -> run_it's parameter -> subprocess.run(text): reported;
        # the constant `date` and the value through safe_name are not
        [f] = self.found["no-tainted-shell"]
        self.assertEqual(f.func, "handler")
        self.assertIn("web/util.py:5", f.message)

    def test_a_call_through_an_interface_is_no_package_dependency(self):
        # Traversor calls Visitor.head; Printer implements it: select doesn't depend on examples
        self.assertNotIn("no-package-cycles", self.found)


if __name__ == "__main__":
    unittest.main()


TAINT = {
    "web/views.py": """import subprocess
from flask import request


def handler():
    host = request.args.get("host")
    page = load("index")             # a constant: load's parameter is clean here
    run(host)
    if request.args.get("level") == "high":
        host = escape(host)          # sanitized in one branch only
    subprocess.run("ping " + host, shell=True)


def careful():
    name = request.args.get("name")
    name = escape(name)              # sanitized for the rest of the function
    subprocess.run("echo " + name, shell=True)


def load(name):
    return open("pages/" + name).read()


def run(h):
    subprocess.call("dig " + h, shell=True)


def escape(s):
    return s
""",
}

TAINT_RULES = """[project]
name = "t"

[[rule]]
id = "taint"
kind = "path"
from = ["web/**:*"]
to = ['^subprocess\\.(run|call)$', '^open$']
source = 'request\\.args'
unless = ["escape"]
"""


class Taint(unittest.TestCase):
    def test_follows_arguments_branches_and_sanitizers(self):
        with tempfile.TemporaryDirectory() as d:
            for name, body in TAINT.items():
                os.makedirs(os.path.dirname(os.path.join(d, name)), exist_ok=True)
                with open(os.path.join(d, name), "w") as fh:
                    fh.write(body)
            with open(os.path.join(d, "seamcheck.toml"), "w") as fh:
                fh.write(TAINT_RULES)
            p = core.load(d, os.path.join(d, "seamcheck.toml"), use_cache=False)
            got = sorted(f.message.split(" at ")[1].split(":")[1] for f in core.run_rules(p) if f.rule == "taint")
        # line 11: the branch that escapes doesn't clean the other; 25: run's `h` is the request value.
        # Not 17 (escaped first), not 21 (load is only ever passed a constant)
        self.assertEqual(got, ["11", "25"])
