"""Exact edges where the code says what a call reaches, and no guess where it
can't be ours (python3 -m unittest discover tests). Each case is a shape that
was a name guess in elixir-pot on 2026-10-19 (114k of 210k edges were guesses;
many crossed languages or landed in test files)."""
import os
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402


def load(files: dict):
    d = tempfile.TemporaryDirectory()
    for name, body in files.items():
        path = os.path.join(d.name, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as fh:
            fh.write(textwrap.dedent(body))
    cfg = os.path.join(d.name, "seamcheck.toml")
    with open(cfg, "w") as fh:
        fh.write('[project]\nname = "t"\ninclude = ["**"]\n')
    return d, core.load(d.name, cfg, use_cache=False)


def edges(p, caller_suffix: str) -> dict:
    """{callee id: exact} for the one function whose id ends with caller_suffix."""
    [fid] = [f for f in p.funcs if f.endswith(caller_suffix)]
    return {t: ex for t, _, ex in p.edges.get(fid, [])}


class ResolutionTest(unittest.TestCase):
    def test_a_guess_never_crosses_languages_or_lands_in_a_test(self):
        # Kotlin's emptyMap() was guessed into C++, vitest's expect() into OS-D's ControlChecks.cpp,
        # a canvas ctx.arc() onto a test file's mock
        d, p = load({
            "app/Screen.kt": "package app\n\nfun screen() {\n    val m = emptyMap()\n}\n",
            "web/lib.ts": "export function emptyMap() { return {} }\n",
            "osd/Lean.cpp": "QVariantMap emptyMap() { return {}; }\n",
            "osd/paint.js": "function paint(ctx) {\n  ctx.arc(1, 2, 3)\n}\n",
            "osd/test/paint.test.js": "function arc() {}\n",
        })
        with d:
            self.assertEqual(edges(p, ".screen"), {})
            self.assertEqual(edges(p, ".paint"), {})

    def test_an_elixir_sigil_is_not_a_call(self):
        d, p = load({"lib/a.ex": "defmodule A do\n  def kinds, do: ~w(a b c)\nend\n",
                     "lib/b.ex": "defmodule B do\n  def w(x), do: x\nend\n"})
        with d:
            self.assertEqual(edges(p, "A.kinds"), {})

    def test_use_brings_in_what_its_using_block_imports(self):
        # every ConnCase / DataCase helper (await_task_supervisor!) was a guess
        d, p = load({
            "test/support/data_case.ex": """\
                defmodule App.DataCase do
                  use ExUnit.CaseTemplate

                  using do
                    quote do
                      import Ecto.Query
                      import App.DataCase
                    end
                  end

                  def await_tasks!, do: :ok
                end
                """,
            "test/session_test.exs": """\
                defmodule App.SessionTest do
                  use App.DataCase

                  test "the session ends" do
                    await_tasks!()
                    from(x in "t")
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "test the session ends"), {"App.DataCase.await_tasks!": True})

    def test_a_qml_context_property_has_its_cpp_type(self):
        # 385 `continuity.refreshDive()` calls in OS-D's QML were guesses
        d, p = load({
            "src/ContinuityClient.cpp": "void ContinuityClient::refreshDive(QString id) {}\n",
            "src/Other.cpp": "void Other::refreshDive(QString id) {}\n",
            "src/main.cpp": """\
                int main() {
                  ContinuityClient continuity;
                  engine.rootContext()->setContextProperty(QStringLiteral("continuity"), &continuity);
                }
                """,
            "qml/Dive.qml": "import QtQuick\nItem {\n  function load() { continuity.refreshDive(\"food\") }\n}\n",
        })
        with d:
            self.assertEqual(edges(p, ".load"), {"src/ContinuityClient.refreshDive": True})

    def test_a_files_own_extension_shadows_the_others(self):
        # each generated *Api.kt declares its own private JSONObject.textOrNull
        files = {f"api/{n}Api.kt": f"package api\n\nprivate fun JSONObject.textOrNull(k: String): String? = null\n\n"
                                   f"fun parse{n}(o: JSONObject) = o.textOrNull(\"x\")\n" for n in ("Weather", "Places", "Ops")}
        d, p = load(files)
        with d:
            [weather] = [t for t in edges(p, ".parseWeather")]
            self.assertTrue(weather.endswith("WeatherApi.textOrNull"))
            self.assertTrue(edges(p, ".parseWeather")[weather])

    def test_a_namespace_names_where_a_cpp_call_lives(self):
        d, p = load({
            "src/api/ClaimApi.h": "namespace veridian::api {\ninline Claim parseClaim(QJsonObject o) { return {}; }\n}\n",
            "src/legacy/Claim.cpp": "Claim parseClaim(QJsonObject o) { return {}; }\n",
            "src/Session.cpp": "void Session::claim() {\n  auto c = veridian::api::parseClaim(obj);\n}\n",
        })
        with d:
            got = edges(p, "Session.claim")
            self.assertEqual([t for t, ex in got.items() if ex], [t for t in got if "api/" in t or "ClaimApi" in t])
            self.assertEqual(len(got), 1)

    def test_a_helper_the_caller_defines_is_exact(self):
        d, p = load({
            "web/Canvas.tsx": """\
                export function Canvas() {
                  const nudge = (dx: number) => {
                    return dx
                  }
                  return nudge(1)
                }
                """,
            "web/other.ts": "export function nudge(dx: number) { return dx }\n",
        })
        with d:
            got = edges(p, "Canvas")
            self.assertEqual([ex for t, ex in got.items() if t.endswith("nudge")], [True])

    def test_a_local_built_by_its_constructor_has_that_type(self):
        d, p = load({
            "app/Prefs.kt": "package app\n\nclass Prefs {\n    fun load(): Int = 1\n}\n",
            "app/Cache.kt": "package app\n\nclass Cache {\n    fun load(): Int = 2\n}\n",
            "app/Use.kt": "package app\n\nfun boot() {\n    val prefs = Prefs()\n    prefs.load()\n}\n",
        })
        with d:
            self.assertEqual(edges(p, ".boot"), {"app/Prefs.load": True})


if __name__ == "__main__":
    unittest.main()
