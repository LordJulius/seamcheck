"""Bugs a field run found (a Flask repo with its own seamcheck.toml, 2026-11-03):
a reach rule crashed `check`, one broken rule took every other rule down, a bare
`import time` resolved to an unrelated app/utils/time.py, and `.pyw` files were
not read at all. The Rust port has the same tests (rust/tests/field_run.rs)."""
import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402


def project(files: dict, rules: str = ""):
    d = tempfile.TemporaryDirectory()
    for name, body in files.items():
        path = os.path.join(d.name, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as fh:
            fh.write(body)
    with open(os.path.join(d.name, "seamcheck.toml"), "w") as fh:
        fh.write('[project]\nname = "t"\n' + rules)
    return d, core.load(d.name, os.path.join(d.name, "seamcheck.toml"), use_cache=False)


ROUTES = {
    "app/routes/x.py": """from app.auth import _caller, _admin_caller
from app.db import db


def api_list():
    _caller()
    return []


def api_save():
    db.session.commit()
    return 1


def tool_archive():
    _admin_caller()
    db.session.commit()
""",
    "app/auth.py": "def _caller():\n    return 1\n\n\ndef _admin_caller():\n    return 2\n",
    "app/db.py": "class _S:\n    def commit(self):\n        pass\n\n\nclass _Db:\n    session = _S()\n\n\ndb = _Db()\n",
}

REACH = """
[[rule]]
id = "routes-check-caller"
kind = "reach"
from = ["app/routes/x.py:api_*", "app/routes/x.py:tool_archive"]
to = ["_caller", "_admin_caller"]
depth = 2

[[rule]]
id = "writes-check-caller"
kind = "reach"
from = ["app/routes/x.py:*"]
when = 'db\\.session\\.commit\\(\\)'
to = ["_caller", "_admin_caller"]
depth = 2
"""


class ReachRules(unittest.TestCase):
    def test_a_reach_rule_runs_with_and_without_when(self):
        d, p = project(ROUTES, REACH)
        with d:
            got = sorted((f.rule, f.func) for f in core.run_rules(p))
        # api_save reaches neither; the rest reach one (any mode)
        self.assertEqual(got, [("routes-check-caller", "api_save"), ("writes-check-caller", "api_save")])


class BrokenRule(unittest.TestCase):
    def test_a_rule_that_crashes_is_one_finding_and_the_others_run(self):
        d, p = project({"a.py": "x = 1  # TODO\n"},
                       '\n[[rule]]\nid = "todo"\npattern = "TODO"\ntext = "raw"\n'
                       '\n[[rule]]\nid = "keys"\nkind = "coverage"\ndefined = { pattern = "x" }\nused = { pattern = "y" }\n')
        with d, mock.patch.object(core, "_coverage", side_effect=RuntimeError("boom")):
            found = core.run_rules(p)
        by = {f.rule: f for f in found}
        self.assertIn("todo", by)  # the other rule still ran
        self.assertEqual(by["keys"].severity, "error")
        self.assertIn("rule keys crashed", by["keys"].message)


IMPORTS = {
    "app/utils/time.py": "def sleep(n):\n    return n\n",
    "app/utils/helpers.py": "def fmt(x):\n    return x\n",
    "CKSuper/tools/x/engines/foo.py": "import time\nimport helpers\n\n\ndef go():\n    time.sleep(1)\n    helpers.fmt(1)\n",
    "CKSuper/tools/x/engines/near.py": "import local\n\n\ndef go():\n    local.run()\n",
    "CKSuper/tools/x/engines/local.py": "def run():\n    return 1\n",
    "src/pkg/__init__.py": "def entry():\n    return 1\n",
    "tests/test_pkg.py": "import pkg\n\n\ndef test_entry():\n    pkg.entry()\n",
}


class BareImports(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.d, cls.p = project(IMPORTS)

    @classmethod
    def tearDownClass(cls):
        cls.d.cleanup()

    def callees(self, fid):
        return {t for t, _, ex in self.p.edges.get(fid, []) if ex}

    def test_a_stdlib_name_is_the_stdlibs(self):
        foo = self.p.modules["CKSuper/tools/x/engines/foo"]
        self.assertIn("time", foo.external)
        self.assertNotIn("app/utils/time.sleep", {t for t, _, _ in self.p.edges.get("CKSuper/tools/x/engines/foo.go", [])})

    def test_a_lone_file_in_an_unrelated_folder_is_not_imported(self):
        self.assertNotIn("app/utils/helpers.fmt", {t for t, _, _ in self.p.edges.get("CKSuper/tools/x/engines/foo.go", [])})

    def test_a_module_beside_the_importer_and_a_package_elsewhere_still_are(self):
        self.assertIn("CKSuper/tools/x/engines/local.run", self.callees("CKSuper/tools/x/engines/near.go"))
        self.assertIn("src/pkg/__init__.entry", self.callees("tests/test_pkg.test_entry"))


class PywFiles(unittest.TestCase):
    def test_a_pyw_file_is_python(self):
        d, p = project({"launcher.pyw": "import sys\n\n\ndef download(url):\n    return fetch(url)\n\n\ndef fetch(u):\n    return u\n"},
                       '\n[[rule]]\nid = "dl"\nkind = "require"\nin = ["launcher.pyw"]\nfunc = "download"\npattern = "fetch"\n')
        with d:
            self.assertEqual([f for f in core.run_rules(p) if f.rule == "dl"], [])
            self.assertEqual([f.id for f in p.find("download")], ["launcher.download"])
            self.assertIn("launcher.fetch", {t for t, _, _ in p.edges.get("launcher.download", [])})
            self.assertEqual(core.scan_family("launcher.pyw"), "python")


if __name__ == "__main__":
    unittest.main()
