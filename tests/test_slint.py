"""Slint (`.slint`): components and globals are Funcs, `import { X } from "f.slint"`
is the edge, a `function` is `Owner.name`, and the generated Rust API
(an exported window or global, a `public function`) is a root."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, scan  # noqa: E402

FILES = {
    "ui/state.slint": """export global Shell {
    in-out property <string> page;
    callback navigate(string);
    public function open-welcome() { root.page = "w"; }
    function helper() { }
    function unused-fn() { }
    pure function pick(a: int) -> int { return helper2(a); }
    function helper2(a: int) -> int { return a + 1; }
}
export struct Row { name: string }
""",
    "ui/kit.slint": """// Hidden { } and Shell.unused-fn() in a comment
export component Press inherits Rectangle {
    callback clicked();
    function bump() { root.clicked(); }
    TouchArea { clicked => { root.bump(); } }
}
export component Orphan inherits Rectangle { Text { text: "Press { }"; } }
component Hidden inherits Rectangle { }
component Leaf inherits Rectangle { Text { text: "x"; } }
""",
    "ui/app.slint": """import { Press, Leaf } from "kit.slint";
import { Shell, Row } from "state.slint";
import { ListView } from "std-widgets.slint";
/* Shell.unused-fn(); */
component Wrapper inherits Rectangle {
    Leaf { }
}
export component AppWindow inherits Window {
    p := Press { clicked => { Shell.open-welcome(); Shell.pick(1); } }
    Wrapper { }
    ListView { }
    Button { clicked => { p.bump(); } }
}
""",
}


class Slint(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        for name, body in FILES.items():
            path = os.path.join(cls.dir.name, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cls.p = core.load(cls.dir.name, use_cache=False)
        cls.dead = {f.id for f in core.dead(cls.p)}

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def _reaches(self, src, dst):
        return any(d == dst and e for d, _l, e in self.p.edges.get(src, []))

    def test_cache_version_moved(self):
        self.assertGreaterEqual(core.CACHE_VERSION, 35)

    def test_comments_and_strings_are_blank(self):
        code, strings = scan.blank('a // Foo {\nText { text: "Bar { }"; } /* Baz { */ x', "slint")
        self.assertNotIn("Foo", code)
        self.assertNotIn("Bar", code)
        self.assertNotIn("Baz", code)
        self.assertIn("Text", code)
        self.assertEqual([s for _l, s in strings], ["Bar { }"])

    def test_declarations_are_funcs(self):
        for fid in ("ui/kit.Press", "ui/kit.Hidden", "ui/state.Shell", "ui/app.AppWindow", "ui/state.Shell.open-welcome",
                    "ui/kit.Press.bump"):
            self.assertIn(fid, self.p.funcs)

    def test_import_is_the_edge_across_files(self):
        self.assertTrue(self._reaches("ui/app.AppWindow", "ui/kit.Press"))
        self.assertTrue(self._reaches("ui/app.Wrapper", "ui/kit.Leaf"))
        self.assertTrue(self._reaches("ui/app.AppWindow", "ui/state.Shell.open-welcome"))
        self.assertTrue(self._reaches("ui/app.AppWindow", "ui/state.Shell.pick"))

    def test_element_id_reaches_the_components_function(self):
        self.assertTrue(self._reaches("ui/app.AppWindow", "ui/kit.Press.bump"))
        self.assertTrue(self._reaches("ui/kit.Press", "ui/kit.Press.bump"))

    def test_toolkit_elements_are_external(self):
        self.assertFalse(any(d.endswith("ListView") or d.endswith("Button") for d, _l, _e in self.p.edges["ui/app.AppWindow"]))

    def test_dead_code(self):
        self.assertEqual(self.dead, {"ui/kit.Orphan", "ui/kit.Hidden", "ui/state.Shell.helper", "ui/state.Shell.unused-fn"})

    def test_roots_are_not_dead(self):
        for fid in ("ui/app.AppWindow", "ui/state.Shell.open-welcome", "ui/state.Shell.pick", "ui/state.Shell.helper2"):
            self.assertNotIn(fid, self.dead)


COPY = """export component %s inherits Rectangle {
    in property <string> text;
    background: Palette.face;
    border-color: Palette.rail;
    border-width: 1px;
    height: 56px;
    HorizontalLayout {
        spacing: 8px;
        padding: 12px;
        Text { text: root.text; font-size: 18px; color: Palette.type; }
        Rectangle { width: 4px; background: Palette.accent; }
        Text { text: "more"; font-size: 14px; color: Palette.muted; }
    }
}
"""


class SlintCopies(unittest.TestCase):
    def test_copied_element_bodies_are_found(self):
        from seamcheck import tidy
        with tempfile.TemporaryDirectory() as d:
            for n, comp in (("a", "ChipA"), ("b", "ChipB")):
                with open(os.path.join(d, n + ".slint"), "w") as fh:
                    fh.write(COPY % comp)
            p = core.load(d, use_cache=False)
            pairs = tidy.dups(p, min_lines=8)
            self.assertEqual([(x["a"], x["b"]) for r in pairs for x in r["pairs"]], [("a.ChipA", "b.ChipB")])


if __name__ == "__main__":
    unittest.main()
