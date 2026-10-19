"""Pass 2 infra lints (python3 -m unittest discover tests): toolchain names
that disagree with rust-toolchain.toml (build_rd_core.sh `1.75` vs pinned
`1.75.0`), and tests that read the home directory (video.stageable)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import lint_infra  # noqa: E402


class P:
    def __init__(self, root):
        self.root = root
        self.files = []


class Pass2Test(unittest.TestCase):
    def test_toolchain_pin(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "native/core"))
            with open(os.path.join(d, "native/core/rust-toolchain.toml"), "w") as fh:
                fh.write('[toolchain]\nchannel = "1.75.0"\n')
            script = "cd native/core\nrustup run 1.75 cargo ndk build\n"
            hits = lint_infra.toolchain_pin("scripts/b.sh", script, script, {}, project=P(d))
            self.assertEqual([h[0] for h in hits], [2])
            ok = script.replace("1.75 ", "1.75.0 ")
            self.assertEqual(lint_infra.toolchain_pin("scripts/b.sh", ok, ok, {}, project=P(d)), [])
            var = "rustup run \"$toolchain\" cargo build\n"
            self.assertEqual(lint_infra.toolchain_pin("scripts/b.sh", var, var, {}, project=P(d)), [])

    def test_test_reads_home(self):
        raw = 'test "x" do\n  # Path.expand("~/note") in a comment is fine\n  File.read!(Path.expand("~/.local/share/a"))\nend\n'
        self.assertEqual([h[0] for h in lint_infra.test_reads_home("test/a_test.exs", raw, raw, {})], [3])
        js = "const f = os.homedir();\n"
        self.assertEqual(len(lint_infra.test_reads_home("x.test.mjs", js, js, {})), 1)
        clean = 'File.read!(Path.join(System.tmp_dir!(), "a"))\n'
        self.assertEqual(lint_infra.test_reads_home("test/a_test.exs", clean, clean, {}), [])


if __name__ == "__main__":
    unittest.main()
