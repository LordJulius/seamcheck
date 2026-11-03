"""The security pack's fourth field round (python3 -m unittest discover tests).

The field's own code, cut down (tests/fixtures/flask_round4): a PDF buffer from a helper an app
keeps two of, a lookup whose SQL text is built from constants beside a length, a JS fetch helper
called with same-origin paths and a ternary of two, and two injections the field once doubted: a
table list from the request, and an upload's keys read back from the session in another handler.
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from seamcheck import core, security  # noqa: E402
from test_security_precision import TextHelpers, _sink_at, findings  # noqa: E402

HEAD = "import json\nfrom flask import request, session\nfrom sqlalchemy import text\nfrom app import db\n\n\n"


class TheAppShape(unittest.TestCase):
    """The pack gave the fixture 4 errors and a warning at 37bb6b4 and missed the session's two."""

    def test_only_the_injections_are_errors(self):
        root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "flask_round4")
        p = core.load(root, use_cache=False)
        got = sorted((f.severity, *_sink_at(f.message)) for f in core.run_rules(p) if f.rule.startswith("security/"))
        self.assertEqual([("error", "app/tp.py", 19), ("error", "app/tp.py", 37), ("error", "app/tp.py", 40)], got)


class Stores(unittest.TestCase):
    def test_a_session_key_written_with_request_data(self):
        body = ("def keep():\n    session['note'] = json.dumps(request.get_json())\n    session['who'] = 'x'\n    return 'ok'\n\n\n"
                "def use():\n    note = json.loads(session['note'])\n    db.session.execute(text(f\"SELECT {note['c']} FROM t\"))\n\n\n"
                "def fine():\n    who = session.get('who')\n    db.session.execute(text(f\"SELECT * FROM {who}\"))\n")
        got = findings({"app/routes.py": HEAD + body})
        self.assertEqual([("error", 15)], [(g[1], g[3]) for g in got], got)


class Helpers(unittest.TestCase):
    """security.py's new pieces on their own (rules::security's Rust tests mirror these)."""

    def test_code_parts(self):
        self.assertEqual("", security.code_parts('f"""FROM t h\n JOIN c ON c.Id"""').strip())
        got = security.code_parts("f\"SELECT {a} {b['k']} {{x}}\" + 'q' + \"#{params[:id]} $id\" + `/x/${y}`")
        self.assertEqual(["a", "b['k']", "+", "+", "#{params[:id]}", "$id", "+", "y"], got.split())
        self.assertEqual('x + "cut', security.code_parts('x + "cut'))  # (cut off: the rest counts)
        self.assertEqual(["q"], security.code_parts('$"SELECT {q}"').split())
        self.assertEqual([], security.code_parts('@"C:\\dir"').split())

    def test_branches_and_same_site(self):
        self.assertEqual(["'/a'", "'/b'"], security.branches("c ? '/a' : '/b'"))
        self.assertEqual(["'/a'", "'/b'"], security.branches("'/a' if c else '/b'"))
        self.assertEqual(["a?.b"], security.branches("a?.b"))
        self.assertTrue(security.same_site("state.x ? '/chat/?include_archived=1' : '/chat/'"))
        self.assertFalse(security.same_site("c ? '/a' : u"))

    def test_summarised_by_the_calls_target(self):
        class C(TextHelpers.Ctx):
            def callees(self, name):
                return ["app/pdf.render_pdf"] if name == "render_pdf" else []
        c = C(set())
        self.assertTrue(security._summarised("render_pdf('x.html', po=po)", c, {"app/pdf.render_pdf"}))
        self.assertFalse(security._summarised("other('x')", c, {"app/pdf.render_pdf"}))
        self.assertTrue(security._summarised("helper(1)", c, {"helper"}))


if __name__ == "__main__":
    unittest.main()
