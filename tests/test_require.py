"""`kind = "require"` (python3 -m unittest discover tests): source-grep tests
in elixir-pot ("session wires inject before enqueue") became rules; unlike a
`together` rule, a deleted file or function is a finding, not a silent pass."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

SESSION = """defmodule App.Session do
  def start(id, input) do
    maybe_inject(id, input)
    enqueue(id, input)
  end

  def other(id), do: enqueue(id, :x)
end
"""

RULES = """[project]
name = "r"
include = ["lib/**"]

[[rule]]
id = "inject-first"
kind = "require"
in = ["lib/session.ex"]
func = "start"
pattern = ['maybe_inject\\(', 'enqueue\\(']
ordered = true

[[rule]]
id = "reversed"
kind = "require"
in = ["lib/session.ex"]
func = "start"
pattern = ['enqueue\\(', 'maybe_inject\\(']
ordered = true

[[rule]]
id = "gone"
kind = "require"
in = ["lib/removed.ex", "lib/nothing/*.ex"]
pattern = 'x'

[[rule]]
id = "no-func"
kind = "require"
in = ["lib/session.ex"]
func = "stop"
pattern = 'x'
"""


class RequireTest(unittest.TestCase):
    def test_require(self):
        with tempfile.TemporaryDirectory() as root:
            os.makedirs(os.path.join(root, "lib"))
            with open(os.path.join(root, "lib", "session.ex"), "w") as fh:
                fh.write(SESSION)
            cfg = os.path.join(root, "seamcheck.toml")
            with open(cfg, "w") as fh:
                fh.write(RULES)
            p = core.load(root, cfg, use_cache=False)
            by = {}
            for f in core.run_rules(p):
                by.setdefault(f.rule, []).append(f.message)
        self.assertNotIn("inject-first", by)
        self.assertEqual(len(by["reversed"]), 1)
        self.assertIn("in order", by["reversed"][0])
        self.assertEqual(len(by["gone"]), 2)  # a missing file and an empty glob
        self.assertEqual(len(by["no-func"]), 1)


if __name__ == "__main__":
    unittest.main()
