"""The land gate (python3 -m unittest discover tests): debt already in the
repo doesn't block; debt in the changed lines does."""
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, tidy  # noqa: E402

BODY = "\n".join(f"    x{i} = compute(a, b, {i}) + other(x{i - 1 if i else 0}, 'k{i}')" for i in range(14))
BASE = {
    "lib/search.ex": "defmodule Search do\n  def search_all(q), do: q\n\n  def forgotten(x) do\n    x + 1\n  end\nend\n",
    "lib/web.ex": "defmodule Web do\n  def index(q), do: Search.search_all(q)\nend\n",
    "a/one.py": "def big(a, b):\n" + BODY + "\n    return x0\n\n\ndef main():\n    big(1, 2)\n",
}
CFG = ('[project]\nname = "t"\ninclude = ["lib/**", "test/**", "a/**", "b/**"]\n'
       '[gate]\nforbid_added = ["fallback", "legacy"]\n')


def git(root, *args):
    subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)


class GateTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name
        self.write({**BASE, "seamcheck.toml": CFG})
        git(self.root, "init", "-q")
        git(self.root, "-c", "user.email=t@t", "-c", "user.name=t", "add", "-A")
        git(self.root, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base")

    def tearDown(self):
        self.dir.cleanup()

    def write(self, files):
        for name, body in files.items():
            path = os.path.join(self.root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)

    def gate(self):
        p = core.load(self.root, os.path.join(self.root, "seamcheck.toml"), use_cache=False)
        return tidy.gate(p, "HEAD")

    def test_clean_tree_and_old_debt_pass(self):
        rep = self.gate()
        self.assertFalse(tidy.gate_failed(rep))
        # `forgotten` is unused, but nobody touched it
        self.write({"lib/web.ex": "defmodule Web do\n  def index(q), do: Search.search_all(q) <> \"\"\nend\n"})
        self.assertFalse(tidy.gate_failed(self.gate()))

    def test_new_debt_blocks(self):
        self.write({
            "lib/search.ex": BASE["lib/search.ex"].replace(
                "end\n", "end\n\n  def never_called(y), do: y\n\n  def run_fallback(q), do: search_all(q)\n", 1)
            .replace("def search_all(q), do: q", "def search_all(q), do: run_fallback(q)"),
            "b/two.py": "def big(a, b):\n" + BODY + "\n    return x0\n",
            "lib/web.ex": "defmodule Web do\n  # the legacy index, a comment: fine\n  def index(q), do: Search.search_all(q)\nend\n",
        })
        rep = self.gate()
        self.assertTrue(tidy.gate_failed(rep))
        self.assertEqual([r["id"] for r in rep["unused"]], ["Search.never_called"])
        self.assertEqual([f["word"] for f in rep["forbidden"]], ["run_fallback", "run_fallback"])
        self.assertEqual(len(rep["dups"]), 1)

    def test_version_bump_doesnt_judge_dependencies(self):
        # 2026-10-06: every land bumps mix.exs' version; that alone ran the deps
        # rule over every manifest and blocked on another project's deps
        self.write({"mix.exs": 'defmodule T.MixProject do\n  def project, do: [\n      version: "1.0.0"\n  ]\n'
                               '  defp deps do\n    [\n      {:gone, "~> 1.0"}\n    ]\n  end\nend\n',
                    "deps/gone/lib/gone.ex": "defmodule Gone do\nend\n",
                    "deps/other/lib/other.ex": "defmodule Other do\nend\n",
                    "seamcheck.toml": CFG + '[[rule]]\nid = "lean"\nkind = "deps"\nseverity = "error"\n'})
        git(self.root, "-c", "user.email=t@t", "-c", "user.name=t", "add", "-A")
        git(self.root, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "deps")
        with open(os.path.join(self.root, "mix.exs")) as fh:
            body = fh.read()
        self.write({"mix.exs": body.replace('"1.0.0"', '"1.0.1"')})
        rep = self.gate()
        self.assertFalse([f for f in rep["rules"] if f.rule == "lean"])
        # adding a dependency is a dependency change: judged
        self.write({"mix.exs": body.replace('{:gone, "~> 1.0"}', '{:gone, "~> 1.0"},\n      {:other, "~> 2.0"}')})
        rep = self.gate()
        self.assertTrue([f for f in rep["rules"] if f.rule == "lean"])

    def test_command_rule_blocks_only_the_files_it_touches(self):
        # 2026-10-06: the kit / contrast checks existed but no gate ran them, so
        # allowlist growth landed unseen in files nobody's change was about
        self.write({"kit/check.sh": "#!/bin/sh\necho 'kit allowlist must match the scan:'\necho 'grew FontFloor ui/a.qml 3'\n"
                                    "[ -f ui/b.bad ] && echo 'grew FontFloor ui/b.qml 1'\nexit 1\n",
                    "ui/a.qml": "Item {}\n", "ui/b.qml": "Item {}\n", "lib/other.ex": "defmodule O do\nend\n",
                    "seamcheck.toml": CFG + '[[rule]]\nid = "kit"\nkind = "command"\nrun = "sh kit/check.sh"\n'
                                            'when = ["ui/**"]\nfiles = "(ui/[\\\\w.]+)"\nseverity = "warn"\ngate = "touched"\n'})
        git(self.root, "-c", "user.email=t@t", "-c", "user.name=t", "add", "-A")
        git(self.root, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "kit")
        self.write({"lib/other.ex": "defmodule O do\n  # x\nend\n"})
        self.assertFalse([f for f in self.gate()["rules"] if f.rule == "kit"])  # didn't touch ui/: not run
        self.write({"ui/b.qml": "Item { }\n"})
        self.assertFalse([f for f in self.gate()["rules"] if f.rule == "kit"])  # old debt in ui/a.qml doesn't block
        self.write({"ui/a.qml": "Item { }\n"})
        hits = [f for f in self.gate()["rules"] if f.rule == "kit"]
        self.assertEqual([f.file for f in hits], ["ui/a.qml"])

    def test_a_script_outside_include_is_gated(self):
        # 2026-10-06: deploy/ scripts were never gated (outside [project] include,
        # the gate returned early), so the HA agents' bugs all landed clean
        self.write({"seamcheck.toml": CFG + '[[rule]]\nid = "names"\nkind = "lint"\nlint = "shell-name-domain"\nin = ["deploy/**"]\n'})
        git(self.root, "-c", "user.email=t@t", "-c", "user.name=t", "add", "-A")
        git(self.root, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "rule")
        self.write({"deploy/agent": "#!/bin/bash\nto=$(tr -dc 'a-z' < \"$REQUEST_DIR/handoff\")\n"})
        rep = self.gate()
        self.assertEqual([(f.rule, f.file, f.line) for f in rep["rules"]], [("names", "deploy/agent", 2)])

    def test_editing_old_debt_is_not_new_debt(self):
        # `forgotten` was already unused: changing its body doesn't make it the committer's
        self.write({"lib/search.ex": BASE["lib/search.ex"].replace("x + 1", "x + 2")})
        self.assertFalse(tidy.gate_failed(self.gate()))

    def test_waiver_on_the_declaration(self):
        self.write({"lib/search.ex": BASE["lib/search.ex"].replace(
            "end\n", "end\n\n  # seamcheck: allow gate (called over RPC)\n  def remote_only(y), do: y\n", 1)})
        self.assertFalse(tidy.gate_failed(self.gate()))

    def test_started_child_counts_as_reached(self):
        self.write({
            "lib/worker.ex": "defmodule Worker do\n  use GenServer\n  def start_link(o), do: GenServer.start_link(__MODULE__, o)\n"
                             "  def init(o), do: {:ok, o}\nend\n",
            "test/worker_test.exs": "defmodule WorkerTest do\n  use ExUnit.Case\n\n  test \"starts\" do\n"
                                    "    pid = start_supervised!({Worker, name: :w})\n    assert is_pid(pid)\n  end\nend\n",
        })
        rep = self.gate()
        self.assertEqual(rep["tests"], [])

    def test_staged_ignores_unstaged_work(self):
        self.write({"lib/search.ex": BASE["lib/search.ex"].replace("end\n", "end\n\n  def never_called(y), do: y\n", 1),
                    "b/two.py": "def big(a, b):\n" + BODY + "\n    return x0\n"})
        p = core.load(self.root, os.path.join(self.root, "seamcheck.toml"), use_cache=False)
        self.assertFalse(tidy.gate_failed(tidy.gate(p, "HEAD", staged=True)))
        git(self.root, "add", "lib/search.ex")
        rep = tidy.gate(p, "HEAD", staged=True)
        self.assertEqual([r["id"] for r in rep["unused"]], ["Search.never_called"])
        self.assertEqual(rep["dups"], [])


if __name__ == "__main__":
    unittest.main()
