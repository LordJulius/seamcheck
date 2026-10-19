"""A qualifier naming a module that misses records nothing (python3 -m unittest discover tests).

elixir-pot: `Enum.reject` found no project module Enum, then attached itself to
every function named reject (ActionPolicy.reject: 1861 callers), and when one
imported module had the name it even counted as exact (`Map.get` ->
Settings.get, 3182 such edges). A module's name that misses is outside the
project, or a wrong alias: an unresolved call, not a guess.
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "lib/app/policy.ex": """defmodule App.Policy do
  def reject(x), do: x
end
""",
    "lib/app/release.ex": """defmodule App.Release do
  def rollback(x), do: x
end
""",
    "lib/app/settings.ex": """defmodule App.Settings do
  def get(k), do: k
end
""",
    "lib/app/user.ex": """defmodule App.User do
  alias App.Settings
  alias App.Policy

  def library(xs) do
    Enum.reject(xs, & &1)
    App.Repo.rollback(xs)
    Map.get(xs, :a)
  end

  def project(xs) do
    Policy.reject(xs)
    Settings.get(:a)
  end
end
""",
}


def _to(p, fid, name):
    return {(dst, exact) for dst, _l, exact in p.edges.get(fid, []) if dst.endswith("." + name)}


class ModuleMiss(unittest.TestCase):
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
            fh.write('[project]\nname = "t"\ninclude = ["**"]\n')
        cls.p = core.load(cls.dir.name, cfg, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def test_library_module_is_nothing(self):
        self.assertEqual(_to(self.p, "App.User.library", "reject"), set())
        self.assertEqual(_to(self.p, "App.User.library", "rollback"), set())
        self.assertEqual(_to(self.p, "App.User.library", "get"), set())

    def test_project_module_still_exact(self):
        self.assertEqual(_to(self.p, "App.User.project", "reject"), {("App.Policy.reject", True)})
        self.assertEqual(_to(self.p, "App.User.project", "get"), {("App.Settings.get", True)})

    def test_pattern(self):
        for q in ("Enum", "Foo.Bar", "Vector3"):
            self.assertTrue(core._MODULE_Q.match(q), q)
        for q in ("main", "LOG", "INSTANCE", "Foo.get()", "self.x", "conn"):
            self.assertFalse(core._MODULE_Q.match(q), q)


if __name__ == "__main__":
    unittest.main()
