"""The gate's exactness guard (python3 -m unittest discover tests).

A change that drops an import seamcheck followed turns exact calls into name
guesses: `s.save()` on a store the file no longer imports lands on every
`save` in the project. Rules and queries over that code then see less, and
the gate said nothing (only `seamcheck stats` showed it, if anyone looked).
Now the gate compares each touched language's "project calls exact" share
against HEAD's and warns, without blocking, when it falls more than 2 points.
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
from seamcheck import core, exactness, fingerprint, tidy  # noqa: E402

FILES = {
    "seamcheck.toml": '[project]\nname = "t"\n',
    "src/store.ts": ("export function makeStore() {\n  return new Store();\n}\n"
                     "export class Store {\n  save(x: number) { return x; }\n  load(x: number) { return x; }\n}\n"),
    "src/cache.ts": "export class Cache {\n  save(x: number) { return x; }\n}\n",
    "src/view.ts": ('import { makeStore } from "./store";\n\nexport function show() {\n  const s = makeStore();\n'
                    "  s.save(1);\n  return s.load(2);\n}\n"),
    "src/main.ts": 'import { show } from "./view";\nimport { Cache } from "./cache";\n\nshow();\nnew Cache().save(3);\n',
    "lib/app.py": "def go():\n    return 1\n\n\ndef main():\n    return go()\n",
}
# the edit: the import goes, `s.save()` / `s.load()` become guesses
EDIT = {"src/view.ts": FILES["src/view.ts"].split("\n", 2)[2]}


def git(root, *args):
    subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t", *args], cwd=root, check=True, capture_output=True)


class ExactnessGuard(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = os.path.join(self.dir.name, "repo")
        self.cache = os.path.join(self.dir.name, "cache")
        self.write(FILES)
        git(self.root, "init", "-q")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-qm", "base")
        patch = mock.patch.object(fingerprint, "CACHE_DIR", self.cache)
        patch.start()
        self.addCleanup(patch.stop)

    def tearDown(self):
        self.dir.cleanup()

    def write(self, files):
        for name, body in files.items():
            path = os.path.join(self.root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)

    def gate(self):
        return tidy.gate(core.load(self.root, use_cache=False), "HEAD")

    def test_a_lost_import_warns_without_blocking(self):
        self.assertEqual(self.gate()["exactness"], [])  # a clean tree: recorded, nothing to compare
        self.write(EDIT)
        rep = self.gate()
        self.assertFalse(tidy.gate_failed(rep))
        [d] = rep["exactness"]
        self.assertEqual((d["ext"], d["file"]), (".ts", "src/view.ts"))
        self.assertGreater(d["before"] - d["after"], exactness.DROP)
        self.assertIn("TypeScript (.ts) calls into project code exact", exactness.message(d, "HEAD"))

    def test_from_the_record_when_measuring_head_is_not_cheap(self):
        exactness.record(core.load(self.root, use_cache=False))
        self.write(EDIT)
        with mock.patch.object(exactness, "CHEAP_FILES", 0), mock.patch.object(exactness, "_measure") as measure:
            self.assertEqual([d["ext"] for d in self.gate()["exactness"]], [".ts"])
            measure.assert_not_called()

    def test_measured_from_head_when_nothing_was_recorded(self):
        self.write(EDIT)
        self.assertEqual([d["ext"] for d in self.gate()["exactness"]], [".ts"])
        # unknown and too big to measure: no warning, no guess
        with mock.patch.object(fingerprint, "CACHE_DIR", os.path.join(self.dir.name, "empty")), \
                mock.patch.object(exactness, "CHEAP_FILES", 0):
            self.assertEqual(self.gate()["exactness"], [])

    def test_only_touched_languages_and_real_drops(self):
        self.gate()
        # a Python edit: TypeScript's share isn't this change's business
        self.write({"lib/app.py": FILES["lib/app.py"] + "\n\ndef more():\n    return go()\n"})
        self.assertEqual(self.gate()["exactness"], [])
        # a TypeScript edit that keeps every import
        self.write({"src/view.ts": FILES["src/view.ts"] + "// a note\n"})
        self.assertEqual(self.gate()["exactness"], [])

    def test_cli_text_and_json(self):
        env = dict(os.environ, PYTHONPATH=ROOT, HOME=self.dir.name)
        def run(*args):
            return subprocess.run([sys.executable, "-m", "seamcheck", *args], cwd=self.root, env=env,
                                  capture_output=True, text=True)
        self.assertEqual(run("stats").returncode, 0)  # records HEAD
        self.write(EDIT)
        r = run("gate")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("warning (not blocking): TypeScript (.ts) calls into project code exact: 100.0% at HEAD", r.stdout)
        r = run("gate", "--json")
        out = json.loads(r.stdout)
        self.assertFalse(out["blocked"])
        [f] = [f for f in out["findings"] if f["kind"] == "exactness"]
        self.assertEqual((f["severity"], f["file"]), ("warn", "src/view.ts"))
        r = run("gate", "--format", "github")
        self.assertTrue(r.stdout.startswith("::warning file=src/view.ts,title=seamcheck exactness::"), r.stdout)


if __name__ == "__main__":
    unittest.main()
