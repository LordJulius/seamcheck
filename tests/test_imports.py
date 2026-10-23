"""Import and test-block resolution (python3 -m unittest discover tests).

Each case was a wrong answer auditing elixir-pot's test suites:
- `alias A.{B, C}` aliased "" to "A." so `B.get()` reached nothing (hub tests
  looked like they tested no product code);
- `import {f} from "@/lib/x"` / `"./x"` went unresolved (990 Surface tests
  reached nothing);
- a Kotlin test calling a top-level function from another file of its package
  was only a name guess, never exact;
- `alias A.{` over several lines aliased nothing;
- `useStore.getState().applyOp()` (a zustand member `applyOp: (op) => {`) and
  `gestures.onTouch()` (an instance of a same-package class) reached nothing;
- `render(<Card />)` was no call to Card;
- `function Stage({\n a }: {...}) {` took the destructuring braces as its body;
- ExUnit `test "..." do` / vitest `it("...")` / JUnit `@Test` were not functions.
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "lib/settings.ex": """
defmodule App.Settings do
  def get(key, default \\\\ nil), do: key || default
end
""",
    "lib/helpers.ex": """
defmodule App.Helpers do
  def fixture, do: :ok
end
""",
    "test/settings_test.exs": """
defmodule App.SettingsTest do
  use ExUnit.Case
  alias App.{Settings, Helpers}
  import App.Helpers
  alias App.{
    Other
  }

  setup do
    fixture()
    :ok
  end

  test "captures" do
    Enum.map([1], &Settings.get/1)
  end

  test "reads a key" do
    assert Settings.get("k") == "k"
  end

  test "no block", :skip
end
""",
    "web/tsconfig.json": '{\n  // comment\n  "compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]},},\n}\n',
    "web/src/lib/fmt.ts": "export function fmt(x: number) {\n  return String(x)\n}\n",
    "web/src/lib/util/index.ts": "export function clamp(x: number) {\n  return x\n}\n",
    "web/src/lib/fmt.test.ts": """
import { fmt as format } from "@/lib/fmt"
import * as util from "./util"
// import { ghost } from "./ghost"

it("formats", () => {
  expect(format(1)).toBe("1")
  util.clamp(2)
})
""",
    "web/src/store/panel.ts": """
export interface Store {
  applyOp: (op: string) => void;
}
export const useStore = create<Store>((set) => ({
  applyOp: (op) => {
    set({ op })
  },
}))
""",
    "web/src/store/panel.test.ts": """
import { useStore } from "./panel"

it("applies", () => {
  useStore.getState().applyOp("x")
})
""",
    "app/ui/Gestures.kt": "package org.x.ui\n\nclass Gestures {\n    fun onTouch(x: Int) {\n        println(x)\n    }\n}\n",
    "app/test/ui/GesturesTest.kt": """package org.x.ui

import org.junit.Test

class GesturesTest {
    @Test
    fun taps() {
        val g = Gestures()
        g.onTouch(1)
    }
}
""",
    "web/src/Card.tsx": "export function Card(props: {title: string}) {\n  return <div>{props.title}</div>\n}\n",
    "web/src/Stage.tsx": 'function Stage({\n  a,\n}: {\n  a: string;\n}) {\n  const x = 1\n  return <Card title={a} />\n}\n',
    "web/src/Card.test.tsx": 'import { Card } from "./Card"\n\nit("renders", () => {\n  render(<Card title="x" />)\n})\n\nit("renders multi-line", () => {\n  render(\n    <Card\n      title="y"\n    />,\n  )\n})\n',
    "app/ui/Precip.kt": "package org.x.ui\n\nfun hourlyRows(xs: List<Int>): List<Int> = xs\n",
    "app/util/Text.kt": "package org.x.util\n\nfun clean(s: String): String = s.trim()\n",
    "app/test/ui/PrecipTest.kt": """package org.x.ui

import org.junit.Test
import org.x.util.clean

class PrecipTest {
    @Test
    fun parses() {
        hourlyRows(listOf(1))
        clean(" a ")
    }
}
""",
}


class ImportsTest(unittest.TestCase):
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
            fh.write('[project]\nname = "imp"\ninclude = ["**"]\n')
        cls.p = core.load(root, cfg, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def exact_callees(self, fid):
        return {dst for dst, _, ex in self.p.edges.get(fid, []) if ex}

    def test_exunit_blocks_are_test_functions(self):
        t = self.p.funcs["App.SettingsTest.test reads a key"]
        self.assertEqual(t.kind, "test")
        self.assertNotIn("App.SettingsTest.test no block", self.p.funcs)

    def test_brace_alias_resolves(self):
        self.assertIn("App.Settings.get", self.exact_callees("App.SettingsTest.test reads a key"))

    def test_capture_is_a_call(self):
        self.assertIn("App.Settings.get", self.exact_callees("App.SettingsTest.test captures"))

    def test_elixir_import(self):
        self.assertIn("App.Helpers.fixture", self.exact_callees("App.SettingsTest.setup"))

    def test_js_named_namespace_and_tsconfig_paths(self):
        t = "web/src/lib/fmt.test.test formats"
        self.assertEqual(self.p.funcs[t].kind, "test")
        self.assertEqual(self.exact_callees(t), {"web/src/lib/fmt.fmt", "web/src/lib/util/index.clamp"})

    def test_jsx_element_renders_its_component(self):
        self.assertIn("web/src/Card.Card", self.exact_callees("web/src/Card.test.test renders"))
        # the tag alone on its line, props below
        self.assertIn("web/src/Card.Card", self.exact_callees("web/src/Card.test.test renders multi-line"))

    def test_destructured_params_are_not_the_body(self):
        f = self.p.funcs["web/src/Stage.Stage"]
        self.assertEqual((f.start, f.end), (1, 8))

    def test_multiline_brace_alias(self):
        self.assertEqual(self.p.modules["App.SettingsTest"].aliases["Other"], "App.Other")

    def test_method_named_in_exactly_one_visible_module(self):
        f = self.p.funcs["web/src/store/panel.applyOp"]
        self.assertEqual((f.start, f.end), (6, 8))  # the implementation, not the type member
        # found, as a guess: only the store's generic type (`create<Store>`) says getState() is a Store,
        # and a name only one visible module has is no proof (it made `res.json()` Context.json)
        self.assertEqual(self.p.edges["web/src/store/panel.test.test applies"], [("web/src/store/panel.applyOp", 5, False)])
        self.assertIn("app/ui/Gestures.onTouch", self.exact_callees("app/test/ui/GesturesTest.taps"))

    def test_kotlin_package_and_function_import(self):
        t = "app/test/ui/PrecipTest.parses"
        self.assertEqual(self.p.funcs[t].kind, "test")
        self.assertEqual(self.exact_callees(t), {"app/ui/Precip.hourlyRows", "app/util/Text.clean"})


if __name__ == "__main__":
    unittest.main()
