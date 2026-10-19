"""Tidy reports (python3 -m unittest discover tests): test-only, unused, dups,
tests that reach nothing / read product source, split, remove. Built from
the passes that cleaned elixir-pot: a shim only its test called, a helper a
script calls by name (so NOT unused), a copied file, a grep test."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, tidy  # noqa: E402

BODY = "\n".join(f"    x{i} = compute(a, b, {i}) + other(x{i - 1 if i else 0}, 'k{i}')" for i in range(14))
FILES = {
    "lib/search.ex": """defmodule Search do
  def search_all(q), do: run(q)

  @doc \"\"\"
  Old shim.
  \"\"\"
  def search_with_ai(q, _opts \\\\ []) do
    ai_helper(q)
  end

  defp ai_helper(q), do: search_all(q)

  def seed!(n), do: n

  def forgotten(x) do
    x + 1
  end

  defp run(q), do: q
end
""",
    "lib/web.ex": "defmodule Web do\n  def index(q), do: Search.search_all(q)\nend\n",
    "scripts/seed.exs": "Search.seed!(3)\n",
    "test/search_test.exs": """defmodule SearchTest do
  use ExUnit.Case

  test "shim" do
    assert Search.search_with_ai("x") == "x"
  end

  test "reads source" do
    assert File.read!("lib/search.ex") =~ "shim"
  end

  test "nothing" do
    assert 1 + 1 == 2
  end
end
""",
    "a/copy_one.py": "def big(a, b):\n" + BODY + "\n    return x0\n",
    "b/copy_two.py": "def big(a, b):\n" + BODY + "\n    return x0\n",
}


def read(path):
    with open(path) as fh:
        return fh.read()


class TidyTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        root = self.dir.name
        for name, body in FILES.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cfg = os.path.join(root, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "t"\ninclude = ["lib/**", "test/**", "a/**", "b/**"]\n')
        self.p = core.load(root, cfg, use_cache=False)
        self.c = tidy.Corpus(self.p)

    def tearDown(self):
        self.dir.cleanup()

    def test_test_only_and_unused(self):
        self.assertEqual([r["id"] for r in tidy.test_only(self.p, self.c)], ["Search.search_with_ai"])
        unused = {r["id"] for r in tidy.unused(self.p, self.c)}
        self.assertIn("Search.forgotten", unused)
        self.assertNotIn("Search.seed!", unused)  # a script calls it by name

    def test_tests_audit(self):
        rows = {r["id"].split(".test ")[-1]: r for r in tidy.test_audit(self.p)}
        self.assertGreater(rows["shim"]["reach"], 0)
        self.assertEqual(rows["nothing"]["reach"], 0)
        self.assertEqual(rows["reads source"]["reads_source"], ["lib/search.ex"])

    def test_subprocess_script_counts_as_reach(self):
        # 2026-10-06 (lane F): tests driving scripts/lib/ota_changelog.py through
        # System.cmd were "do-nothing tests" (the graph stops at the process boundary)
        import shutil
        root = self.dir.name
        files = {"scripts/lib/tool.py": "def main():\n    print(1)\n",
                 "test/tool_test.exs": 'defmodule ToolTest do\n  use ExUnit.Case\n  @script "scripts/lib/tool.py"\n\n'
                                       '  defp run(args), do: System.cmd("python3", [@script | args])\n\n'
                                       '  test "records" do\n    assert {_, 0} = run(["x"])\n  end\n\n'
                                       '  test "nothing" do\n    assert 1 == 1\n  end\nend\n'}
        for name, body in files.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        p = core.load(root, os.path.join(root, "seamcheck.toml"), use_cache=False)
        rows = {r["id"].split(".test ")[-1]: r for r in tidy.test_audit(p) if r["file"] == "test/tool_test.exs"}
        self.assertEqual(rows["records"]["runs"], ["scripts/lib/tool.py"])
        self.assertGreater(rows["records"]["reach"], 0)
        self.assertEqual(rows["nothing"]["reach"], 0)  # still a do-nothing test
        shutil.rmtree(os.path.join(root, "scripts"))

    def test_dups(self):
        groups = tidy.dups(self.p)
        self.assertEqual(groups[0]["files"], ["a/copy_one.py", "b/copy_two.py"])

    def test_remove_with_doc(self):
        dry = tidy.remove(self.p, ["Search.search_with_ai", "SearchTest.test shim"], dry=True)
        self.assertEqual(len(dry), 3)  # + ai_helper, orphaned
        self.assertTrue(any("(orphaned)" in d for d in dry))
        before = read(os.path.join(self.dir.name, "lib/search.ex"))
        self.assertIn("search_with_ai", before)  # dry run wrote nothing
        tidy.remove(self.p, ["Search.search_with_ai", "SearchTest.test shim"], dry=False)
        after = read(os.path.join(self.dir.name, "lib/search.ex"))
        self.assertNotIn("search_with_ai", after)
        self.assertNotIn("Old shim", after)  # its @doc heredoc went with it
        self.assertNotIn("ai_helper", after)  # orphaned by the removal: cascades
        self.assertIn("def seed!", after)
        test_after = read(os.path.join(self.dir.name, "test/search_test.exs"))
        self.assertNotIn('test "shim"', test_after)
        self.assertIn('test "reads source"', test_after)

    def test_remove_takes_a_whole_heredoc_attribute(self):
        path = os.path.join(self.dir.name, "lib/prompted.ex")
        with open(path, "w") as fh:
            fh.write('defmodule Prompted do\n  @prompt """\n  Classify (one of: a, b).\n  Message:\n  """\n'
                     '  @keep 1\n\n  def run(m), do: ask(@prompt <> m)\n\n  def other, do: @keep\n\n'
                     '  defp ask(p), do: p\nend\n')
        p = core.load(self.dir.name, os.path.join(self.dir.name, "seamcheck.toml"), use_cache=False)
        tidy.remove(p, ["Prompted.run"], dry=False)
        after = read(path)
        self.assertNotIn("Classify", after)
        self.assertNotIn('"""', after)
        self.assertIn("@keep 1", after)
        self.assertIn("def other", after)

    def test_split_and_hotspots(self):
        r = tidy.split(self.p, "lib/search.ex")
        self.assertEqual(r["functions"], 6)
        self.assertTrue(tidy.hotspots(self.p))


if __name__ == "__main__":
    unittest.main()
