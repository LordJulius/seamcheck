"""Dependency use (python3 -m unittest discover tests).

Cases from elixir-pot's first run: a dep only another package needs
optionally (postgrex for ecto_sql) is a peer, not unused; a dev tool a script
runs as a mix task is used; a crate whose [lib] name differs (rustdesk ->
librustdesk) is found; a sub-project's code under deploy/ is code, not config;
a library that wires itself in (META-INF/services) is not "unused".
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, deps  # noqa: E402

FILES = {
    "mix.exs": """defmodule App.MixProject do
  use Mix.Project

  defp deps do
    [
      {:req, "~> 0.5"},
      {:ecto_sql, "~> 3.0"},
      {:postgrex, ">= 0.0.0"},
      {:dialyxir, "~> 1.4", only: :dev, runtime: false},
      {:gone, "~> 1.0"},
      {:mox, "~> 1.0"},
      {:telemetry, "~> 1.0"}
    ]
  end
end
""",
    "deps/req/lib/req.ex": "defmodule Req do\nend\n",
    "deps/ecto_sql/lib/ecto/adapters/sql.ex": "defmodule Ecto.Adapters.SQL do\nend\n",
    "deps/ecto_sql/mix.exs": '[{:postgrex, "~> 0.19", optional: true}]\n',
    "deps/postgrex/lib/postgrex.ex": "defmodule Postgrex do\nend\n",
    "deps/dialyxir/lib/mix/tasks/dialyzer.ex": "defmodule Mix.Tasks.Dialyzer do\nend\n",
    "deps/gone/lib/gone.ex": "defmodule Gone do\nend\n",
    "deps/mox/lib/mox.ex": "defmodule Mox do\nend\n",
    "deps/telemetry/src/telemetry.erl": "-module(telemetry).\n",
    "lib/app.ex": """defmodule App do
  def get(url), do: Req.get(url)
  def emit, do: :telemetry.execute([:a], %{})
  def unused_mock, do: Ecto.Adapters.SQL.query()
end
""",
    "lib/app/web.ex": "defmodule App.Web do\n  def index, do: {App.get(\"u\"), App.emit()}\nend\n",
    "test/web_test.exs": "defmodule App.WebTest do\n  test \"i\" do\n    App.Web.index()\n  end\nend\n",
    "lib/app/old.ex": """defmodule App.Old do
  def never_called, do: Mox.defmock(X, for: Y)
end
""",
    "test/app_test.exs": "defmodule AppTest do\n  test \"x\" do\n    Mox.stub()\n  end\nend\n",
    "scripts/ci.sh": "mix dialyzer\n",
    "web/package.json": '{"dependencies": {"left-pad": "1", "gone-js": "1"}, "devDependencies": {"@types/left-pad": "1"}}',
    "web/node_modules/left-pad/package.json": '{"name": "left-pad"}',
    "web/node_modules/gone-js/package.json": '{"name": "gone-js"}',
    "web/node_modules/@types/left-pad/package.json": '{"name": "@types/left-pad"}',
    "web/src/a.ts": 'import pad from "left-pad"\nexport function a() {\n  return pad("x")\n}\n',
    "rd/Cargo.toml": '[package]\nname = "rd"\n\n[dependencies]\nrustdesk = { path = "../rustdesk" }\nserde = "1"\n',
    "rd/src/lib.rs": "use librustdesk::embed;\n",
    "rustdesk/Cargo.toml": '[package]\nname = "rustdesk"\n\n[lib]\nname = "librustdesk"\n',
    "deploy/plugin/requirements.txt": "requests\n",
    "deploy/plugin/main.py": "import requests\n",
}


class DepsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        root = cls.dir.name
        for name, body in FILES.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cfg = os.path.join(root, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "d"\ninclude = ["lib/**", "test/**", "web/**", "rd/**"]\n')
        p = core.load(root, cfg, use_cache=False)
        cls.st = {d.name: d.status for d in deps.analyze(p)}

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def test_mix(self):
        self.assertEqual(self.st["req"], "used")
        self.assertEqual(self.st["telemetry"], "used")  # an erlang module call
        self.assertEqual(self.st["postgrex"], "peer")
        self.assertEqual(self.st["dialyxir"], "config")
        self.assertEqual(self.st["gone"], "unused")

    def test_dead_and_test_only(self):
        self.assertEqual(self.st["mox"], "dead-only")  # code use sits in a function nothing calls
        self.assertEqual(self.st["ecto_sql"], "dead-only")

    def test_npm(self):
        # a top-level import counts as live (the file may be imported for effect)
        self.assertEqual(self.st["left-pad"], "used")
        self.assertEqual(self.st["@types/left-pad"], "used")
        self.assertEqual(self.st["gone-js"], "unused")

    def test_cargo_lib_name(self):
        self.assertEqual(self.st["rustdesk"], "used")
        self.assertEqual(self.st["serde"], "unused")

    def test_subproject_code_is_code(self):
        self.assertEqual(self.st["requests"], "used")


class UninstalledTest(unittest.TestCase):
    """2026-10-06: every lane commit in a git worktree (.claude/worktrees/*) was
    blocked by lean-deps: the worktree has no deps/ or node_modules/, so
    sagents' {:langchain} (module LangChain, guessed "Langchain") and
    typescript (bin tsc) read as unused. A worktree now reads the main
    checkout's installed packages; with none anywhere, the manifest gets one
    warning, not an error per dependency."""

    def _tree(self, root, files):
        for name, body in files.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)

    MIX = 'defmodule P.MixProject do\n  defp deps do\n    [\n      {:langchain, "~> 0.3"}\n    ]\n  end\nend\n'
    PKG = '{"devDependencies": {"typescript": "5"}, "scripts": {"build": "tsc -p ."}}'

    def test_nothing_installed_is_one_warning(self):
        with tempfile.TemporaryDirectory() as root:
            self._tree(root, {"pkg/mix.exs": self.MIX, "pkg/lib/p.ex": "defmodule P do\n  def x, do: LangChain.run()\nend\n",
                              "web/package.json": self.PKG})
            cfg = os.path.join(root, "seamcheck.toml")
            with open(cfg, "w") as fh:
                fh.write('[project]\nname = "u"\ninclude = ["pkg/**", "web/**"]\n'
                         '[[rule]]\nid = "lean"\nkind = "deps"\nseverity = "error"\n')
            p = core.load(root, cfg, use_cache=False)
            st = {d.name: d.status for d in deps.analyze(p)}
            self.assertEqual(st["langchain"], "uninstalled")
            self.assertEqual(st["typescript"], "uninstalled")
            found = core.run_rules(p)
            self.assertFalse([f for f in found if f.severity == "error"])
            self.assertEqual(len([f for f in found if f.rule == "lean"]), 2)  # one note per manifest

    def test_worktree_reads_main_checkout(self):
        import subprocess
        with tempfile.TemporaryDirectory() as base:
            main = os.path.join(base, "main")
            self._tree(main, {"pkg/mix.exs": self.MIX, "pkg/lib/p.ex": "defmodule P do\n  def x, do: LangChain.run()\nend\n",
                              "pkg/deps/langchain/lib/lang_chain.ex": "defmodule LangChain do\nend\n",
                              "web/package.json": self.PKG,
                              "web/node_modules/typescript/package.json": '{"name": "typescript", "bin": {"tsc": "bin/tsc"}}',
                              ".gitignore": "deps/\nnode_modules/\n",
                              "seamcheck.toml": '[project]\nname = "w"\ninclude = ["pkg/**", "web/**"]\n'})
            git = lambda *a, cwd=main: subprocess.run(["git", *a], cwd=cwd, check=True, capture_output=True)
            git("init", "-q")
            git("-c", "user.email=t@t", "-c", "user.name=t", "add", ".")
            git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x")
            wt = os.path.join(base, "wt")
            git("worktree", "add", "-q", wt)
            self.assertFalse(os.path.exists(os.path.join(wt, "pkg", "deps")))
            deps._MAIN_ROOT.clear()
            p = core.load(wt, os.path.join(wt, "seamcheck.toml"), use_cache=False)
            st = {d.name: d.status for d in deps.analyze(p)}
            self.assertNotIn(st["langchain"], ("unused", "uninstalled"))  # resolved: LangChain is its module
            self.assertEqual(st["typescript"], "config")


if __name__ == "__main__":
    unittest.main()
