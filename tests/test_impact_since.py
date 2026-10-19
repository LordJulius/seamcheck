"""impact --since REF starts from the functions the diff touched (python3 -m unittest discover tests)."""
import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck.__main__ import main  # noqa: E402

A = """extends Node

func base(x):
	return x

func middle(x):
	return base(x)

func untouched(x):
	return x
"""
B = """extends Node
const A := preload("res://a.gd")

func top():
	A.middle(1)
"""


class ImpactSince(unittest.TestCase):
    def run_cli(self, *args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = main(list(args) + ["-C", self.dir.name, "--no-cache"])
        return code, out.getvalue()

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        root = self.dir.name
        for name, body in (("a.gd", A), ("b.gd", B), ("seamcheck.toml", '[project]\nname = "t"\ninclude = ["**"]\n')):
            with open(os.path.join(root, name), "w") as fh:
                fh.write(body)
        git = ["git", "-c", "user.email=t@t", "-c", "user.name=t"]
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        subprocess.run(git + ["add", "."], cwd=root, check=True)
        subprocess.run(git + ["commit", "-qm", "x"], cwd=root, check=True)

    def tearDown(self):
        self.dir.cleanup()

    def test_nothing_changed(self):
        code, out = self.run_cli("impact", "--since", "HEAD")
        self.assertEqual(code, 0)
        self.assertIn("no functions changed", out)

    def test_edit_reaches_callers(self):
        with open(os.path.join(self.dir.name, "a.gd"), "w") as fh:
            fh.write(A.replace("return x\n\nfunc middle", "return x + 1\n\nfunc middle", 1))
        code, out = self.run_cli("impact", "--since", "HEAD")
        self.assertIn("1 functions changed", out)
        self.assertIn("a.middle", out)
        self.assertIn("b.top", out)
        self.assertNotIn("untouched", out)

    def test_deleted_line_counts(self):
        with open(os.path.join(self.dir.name, "a.gd"), "w") as fh:
            fh.write(A.replace("func middle(x):\n\treturn base(x)\n", "func middle(x):\n", 1))
        code, out = self.run_cli("impact", "--since", "HEAD")
        self.assertIn("1 functions changed", out)
        self.assertIn("b.top", out)


if __name__ == "__main__":
    unittest.main()
