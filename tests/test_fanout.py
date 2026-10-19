"""`kind = "fanout"` (python3 -m unittest discover tests): one producer, many
views. Each case is a seam from elixir-pot: section types rendered by three
clients, panel hosts matched by substring (OS-D) or after dropping `_embed`
(OS-A), client paths vs `mix phx.routes` (`:id` / `${id}` / appended suffix)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "hub/spec.ex": "defmodule Spec do\n  @types ~w(\n    header\n    list\n    video\n  )\nend\n",
    "a/Spec.kt": 'enum class T(val w: String) {\n    H("header"),\n    L("list"),\n    ;\n}\nval x = "video"\n',
    "s/spec.tsx": 'function r(t: string) {\n  switch (t) {\n    case "header":\n    case "list":\n    case "chart":\n  }\n}\n',
    "d/host.cpp": 'QString kind(QString t) {\n  if (t.contains("head")) return "a";\n  if (t.contains("vid")) return "b";\n}\n',
    "hub/types.ex": "defmodule Types do\n  @known ~w(weather weather_embed quiz_question_embed)\nend\n",
    "a/Kinds.kt": 'fun from(t: String) = when (t.removeSuffix("_embed")) {\n    "weather" -> 1\n    "quiz" -> 2\n    else -> 0\n}\n',
    "c/api.ts": 'get(`/api/things/${id}/tracks`)\nget("/api/inbox" + qs)\nget("/api/nope")\n',
    "routes.txt": "GET /api/things/:id/tracks\nGET /api/inbox\n",
}

RULES = r"""[project]
name = "f"
include = ["**"]

[[rule]]
id = "sections"
kind = "fanout"
check = ["missing", "extra"]
source = { in = ["hub/spec.ex"], within = '(?s)@types ~w\((.*?)\)', pattern = '(\w+)', text = "raw" }
[rule.views.osa]
in = ["a/Spec.kt"]
within = '(?s)enum class T\b.*?\n\s*;'
pattern = '\("(\w+)"\)'
except = { video = "the phone plays video in its own player" }
[rule.views.surface]
in = ["s/spec.tsx"]
func = "r"
pattern = 'case "(\w+)":'
[rule.views.osd]
in = ["d/host.cpp"]
mtach = "exact"
within = '(?s)QString kind\(.*?\n\}'
pattern = 'contains\("(\w+)"\)'
match = "contains"
fallback = "generic host"

[[rule]]
id = "types"
kind = "fanout"
source = { in = ["hub/types.ex"], within = '@known ~w\((.*?)\)', pattern = '(\w+)', text = "raw" }
[rule.views.osa]
in = ["a/Kinds.kt"]
pattern = '"(\w+)" ->'
normalize = [['_embed$', ''], ['^quiz_question$', 'quiz']]

[[rule]]
id = "routes"
kind = "fanout"
source = { in = ["c/api.ts"], pattern = '''["'`](/api/(?:[\w/.-]|\$\{[^}]*\})*)(["'`]\s*\+)?''', rewrite = [['\$\{[^}]*\}', ':p']] }
[rule.views.hub]
run = "cat routes.txt"
pattern = '(?m)^\w+ (/api/\S*)'
rewrite = [[':\w+', ':p']]
match = "segments"
"""


class FanoutTest(unittest.TestCase):
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
            fh.write(RULES)
        cls.p = core.load(root, cfg, use_cache=False)
        cls.found = {}
        for f in core.run_rules(cls.p):
            cls.found.setdefault(f.rule, []).append(f.message)
        cls.rules = {r["id"]: r for r in cls.p.config["rule"]}

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def test_missing_extra_except_fallback(self):
        msgs = self.found["sections"]
        self.assertIn("surface does not handle 'video'", msgs)
        self.assertIn("surface handles 'chart', which the source never produces", msgs)
        self.assertIn("osd does not handle 'list' (falls back to generic host)", msgs)
        self.assertFalse(any("osa" in m for m in msgs))  # video is an intentional gap
        self.assertIn("views.osd: unknown key 'mtach' (did you mean 'match'?)", msgs)
        toks, views, cell = core.matrix(self.p, self.rules["sections"])
        self.assertEqual(cell[("video", "osa")], "except")
        self.assertEqual(cell[("header", "osd")], "ok")  # "head" is in "header"
        self.assertEqual(cell[("list", "osd")], "fallback")

    def test_normalize_before_lookup(self):
        self.assertNotIn("types", self.found)

    def test_routes_with_params_and_suffixes(self):
        # `${id}` meets `:id`; "/api/inbox" + qs is the inbox route; /api/nope is a 404
        self.assertEqual(self.found["routes"], ["hub does not handle '/api/nope'"])


if __name__ == "__main__":
    unittest.main()
