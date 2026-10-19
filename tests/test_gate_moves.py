"""Pass 2 false positives (python3 -m unittest discover tests): a pure file
split (lane M, ContinuityClient.cpp -> 13 files) is old code in a new place;
a generator's own export name (HUB_THEME_FALLBACKS, lane K) isn't hand-written
fallback code; an ExUnit `setup` isn't a test; a Kotlin test that only names
an enum exercises it (lane N)."""
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, tidy  # noqa: E402

CFG = ('[project]\nname = "t"\ninclude = ["lib/**", "test/**", "src/**", "app/**"]\n'
       '[gate]\nforbid_added = ["fallback", "legacy"]\n')
BODY = "\n".join(f"    x{i} = compute(a, b, {i}) + other(x{i - 1 if i else 0}, 'k{i}')" for i in range(14))


def git(root, *args):
    subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t", *args], cwd=root, check=True,
                   capture_output=True)


class MovesTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name

    def tearDown(self):
        self.dir.cleanup()

    def write(self, files):
        for name, body in files.items():
            path = os.path.join(self.root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)

    def commit(self, files):
        self.write({"seamcheck.toml": CFG, **files})
        git(self.root, "init", "-q")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-qm", "base")

    def gate(self):
        p = core.load(self.root, os.path.join(self.root, "seamcheck.toml"), use_cache=False)
        return tidy.gate(p, "HEAD")

    def test_pure_split_passes(self):
        # one file with an old near-duplicate pair and an old `fallback` name
        big = ("def big(a, b):\n" + BODY + "\n    return x0\n\n\n"
               "def big2(a, b):\n" + BODY + "\n    return x1\n\n\n"
               "def use_fallback(q):\n    return big(q, q) + big2(q, q)\n")
        self.commit({"src/one.py": big, "src/main.py": "from one import use_fallback\nuse_fallback(1)\n"})
        parts = big.split("\n\n\n")
        self.write({"src/one.py": parts[0] + "\n",
                    "src/two.py": parts[1] + "\n",
                    "src/three.py": parts[2]})
        rep = self.gate()
        self.assertEqual(rep["forbidden"], [])
        self.assertEqual(rep["dups"], [])
        self.assertEqual(rep["unused"], [])

    def test_new_code_in_a_split_still_blocks(self):
        self.commit({"src/one.py": "def a():\n    return 1\n", "src/main.py": "from one import a\na()\n"})
        self.write({"src/two.py": "def b_fallback():\n    return 2\n"})
        rep = self.gate()
        self.assertEqual([f["word"] for f in rep["forbidden"]], ["b_fallback"])

    def test_generated_names_are_allowed(self):
        self.commit({"src/tokens.generated.ts": "/* Generated from x by gen.mjs. Do not edit. */\n"
                                                "export const HUB_THEME_FALLBACKS = {a: 1};\n",
                     "src/use.ts": "export const z = 1;\n"})
        self.write({"src/use.ts": "import { HUB_THEME_FALLBACKS } from './tokens.generated';\n"
                                  "export const z = HUB_THEME_FALLBACKS.a;\n"})
        self.assertEqual(self.gate()["forbidden"], [])
        # a hand-written name with the word still blocks
        self.write({"src/use.ts": "export const myFallback = 1;\n"})
        self.assertEqual([f["word"] for f in self.gate()["forbidden"]], ["myFallback"])

    def test_generated_parsers_are_not_copies(self):
        # lane S: api/generate.mjs emits one near-identical parser per schema def
        gen = "/* Generated from api/schema/x.json by api/generate.mjs. Do not edit. */\n"
        parser = lambda n: f"def parse_{n}(a, b):\n" + BODY + f"\n    return x{n % 3}\n\n\n"
        self.commit({"src/one.generated.py": gen + parser(1), "src/main.py": "x = 1\n"})
        self.write({"src/two.generated.py": gen + parser(2) + parser(4)})
        self.assertEqual(self.gate()["dups"], [])
        # hand-written copies still block
        self.write({"src/hand.py": parser(5) + parser(7)})
        self.assertTrue(self.gate()["dups"])

    def test_exunit_setup_is_not_a_test(self):
        self.commit({"lib/a.ex": "defmodule A do\n  def go, do: 1\nend\n",
                     "test/a_test.exs": "defmodule ATest do\n  use ExUnit.Case\n  test \"go\" do\n    A.go()\n  end\nend\n"})
        self.write({"test/a_test.exs": "defmodule ATest do\n  use ExUnit.Case\n  setup do\n"
                                       "    System.put_env(\"X\", \"1\")\n    :ok\n  end\n\n"
                                       "  test \"go\" do\n    A.go()\n  end\nend\n"})
        self.assertEqual(self.gate()["tests"], [])

    def test_kotlin_enum_only_test_reaches_the_enum(self):
        self.commit({"app/Area.kt": "package app\n\nenum class Area(val wire: String) { HUB(\"hub\"), OSA(\"osa\") }\n",
                     "app/AreaTest.kt": "package app\n\nclass AreaTest {\n}\n"})
        self.write({"app/AreaTest.kt": "package app\n\nimport org.junit.Test\n\nclass AreaTest {\n    @Test\n"
                                       "    fun wireNames() {\n        check(Area.HUB.wire == \"hub\")\n    }\n}\n"})
        self.assertEqual(self.gate()["tests"], [])

    def test_a_check_pinned_to_a_split_file_blocks(self):
        # lane M: two tests read src/Client.cpp; the split moved their symbols out
        body = "\n".join(f"void Client::op{i}() {{ send(QStringLiteral(\"op{i}\")); }}" for i in range(8))
        self.commit({"src/Client.cpp": "#include \"Client.h\"\n" + body + "\n",
                     "test/client.test.mjs": "const src = readFileSync('src/Client.cpp', 'utf8');\n"
                                             "assert(src.includes('op3'));\n"})
        lines = body.split("\n")
        self.write({"src/Client.cpp": "#include \"Client.h\"\n" + "\n".join(lines[:2]) + "\n",
                    "src/ClientMore.cpp": "#include \"Client.h\"\n" + "\n".join(lines[2:]) + "\n"})
        rep = self.gate()
        self.assertEqual([(r["file"], r["moved_from"]) for r in rep["moved_refs"]],
                         [("test/client.test.mjs", "src/Client.cpp")])
        self.assertTrue(tidy.gate_failed(rep))
        # pointing the test at a glob (it changes in the same change) clears it
        self.write({"test/client.test.mjs": "const src = glob('src/Client*.cpp');\nassert(src.includes('op3'));\n"})
        self.assertEqual(self.gate()["moved_refs"], [])

    def test_a_command_that_cant_run_here_is_one_warning(self):
        # lane M's worktree had no node_modules: check_surface_kit's stack trace
        # came back as 12 blocking findings
        self.commit({"src/a.ts": "export const a = 1;\n"})
        self.write({"seamcheck.toml": CFG + '[[rule]]\nid = "kit"\nkind = "command"\nwhen = ["src/**"]\ngate = "touched"\n'
                                            'run = "echo \\"Error [ERR_MODULE_NOT_FOUND]: Cannot find package \'x\'\\" >&2; '
                                            'echo at foo; exit 1"\n',
                    "src/a.ts": "export const a = 2;\n"})
        p = core.load(self.root, os.path.join(self.root, "seamcheck.toml"), use_cache=False)
        found = [f for f in core.run_rules(p) if f.rule == "kit"]
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0].severity, "warn")
        self.assertIn("couldn't run", found[0].message)
        self.assertFalse(tidy.gate_failed(tidy.gate(p, "HEAD")))


if __name__ == "__main__":
    unittest.main()
