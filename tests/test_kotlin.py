"""Kotlin spans and call resolution (python3 -m unittest discover tests).

Each case was a wrong answer on elixir-pot's OS-A app:
- `fun f() = expr` took the NEXT function's `{` body as its own;
- a 60-parameter Compose function lost its whole body (4 KB signature cap);
- `MapPrefs.set(...)` resolved to nothing (path-named module, no short name);
- `icon()` on a lambda parameter was an "exact" edge to the only `icon` function.
"""
import os
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

PREFS = """
package x

object MapPrefs {
    fun on(values: Map<String, String>, key: String): Boolean = values[key] == "true"

    fun load(origin: String) {
        fetch(origin)
    }

    fun chain(a: Int): Int =
        a.plus(1)
            .times(2)

    fun set(k: String) { store(k) }
}

interface Repo {
    fun get(id: String): String
    fun put(id: String)
}
"""

NAV = """
package x

object Nav {
    fun icon(type: Int): String = if (type == 1) "left" else "right"
}
"""


def screen() -> str:
    params = ",\n".join(f"    p{i}: () -> Unit = {{}}" for i in range(120))
    return textwrap.dedent(f"""
    package x

    fun Screen(
    {params},
    ) {{
        val t = {{ k: String -> MapPrefs.set(k) }}
        helper()
    }}

    fun helper() = MapPrefs.load("x")

    fun Tile(icon: () -> Unit) {{
        icon()
    }}
    """)


class KotlinTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        root = cls.dir.name
        for name, body in {"MapPrefs.kt": PREFS, "Nav.kt": NAV, "Screen.kt": screen()}.items():
            with open(os.path.join(root, name), "w") as fh:
                fh.write(body)
        cfg = os.path.join(root, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "kt"\ninclude = ["*.kt"]\n')
        cls.p = core.load(root, cfg, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def span(self, fid):
        f = self.p.funcs[fid]
        return f.start, f.end

    def callers(self, fid, exact=False):
        return {src for src, _, ex in self.p.redges.get(fid, []) if ex or not exact}

    def test_expression_bodies_stop_at_their_own_end(self):
        self.assertEqual(self.span("MapPrefs.on"), (5, 5))
        self.assertEqual(self.span("MapPrefs.chain"), (11, 13))
        self.assertEqual(self.callers("MapPrefs.load"), {"Screen.helper"})  # not MapPrefs.on

    def test_declarations_without_bodies(self):
        self.assertEqual(self.span("MapPrefs.Repo.get"), (19, 19))  # (an interface in the file is its own module)

    def test_long_signature_keeps_its_body(self):
        start, end = self.span("Screen.Screen")
        self.assertGreater(end - start, 100)
        self.assertIn("Screen.Screen", self.callers("MapPrefs.set", exact=True))

    def test_qualifier_by_file_or_declared_type(self):
        self.assertIn("Screen.helper", self.callers("MapPrefs.load", exact=True))

    def test_a_name_guess_is_never_exact(self):
        self.assertNotIn("Screen.Tile", self.callers("Nav.icon", exact=True))


if __name__ == "__main__":
    unittest.main()
