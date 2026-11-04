"""The security pack's sixth field round (python3 -m unittest discover tests).

The field's shapes (tests/fixtures/flask_round6): a call that passes `items=[]` from a route with a
tainted local of the same name (a keyword's name is no value read), and a sanitized file name built
into an f-string or a conditional before it is joined to a temporary directory.
"""
import os
import re
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from seamcheck import arch, core, security  # noqa: E402
from test_security_precision import TextHelpers, _sink_at, findings  # noqa: E402


class TheAppShape(unittest.TestCase):
    """The pack gave the fixture 5 errors at b1ffa78; its injections are 2."""

    def test_only_the_injections_are_errors(self):
        root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "flask_round6")
        p = core.load(root, use_cache=False)
        got = sorted((f.severity, *_sink_at(f.message)) for f in core.run_rules(p) if f.rule.startswith("security/"))
        self.assertEqual([("error", "app/upload.py", 31), ("error", "app/utils/reports.py", 3)], got)


_HELPER = ("def render(name, **context):\n    path = context.get('logo', 'logo.png')\n"
           "    with open(path, 'rb') as fh:\n        return name, fh.read()\n")


class KeywordNames(unittest.TestCase):
    def test_a_keyword_name_is_no_value(self):
        files = {
            "app/report.py": _HELPER,
            "app/routes.py": ("from flask import request\nfrom app.report import render\n\n\n"
                              "def low():\n    items = request.args.get('items')\n    print(items)\n"
                              "    return render('x.html', items=[])\n"),
        }
        self.assertEqual([], findings(files))

    def test_a_keyword_value_reaches_the_star_parameter(self):
        files = {
            "app/report.py": _HELPER,
            "app/routes.py": ("from flask import request\nfrom app.report import render\n\n\n"
                              "def logo():\n    return render('x.html', logo=request.args['logo'])\n"),
        }
        self.assertEqual(1, len(findings(files)))


class FreeValues(unittest.TestCase):
    def test_built_of_sanitized_values(self):
        files = {
            "app/upload.py": ("import os\nimport tempfile\nfrom flask import request\n\n\n"
                              "def upload():\n    safe = os.path.basename(request.form['name'])\n"
                              "    d = tempfile.mkdtemp()\n"
                              "    a = open(os.path.join(d, f'{safe}.pdf'), 'rb')\n"
                              "    b = open(os.path.join(d, safe if safe.endswith('.pdf') else safe + '.pdf'), 'rb')\n"
                              "    c = open(os.path.join(d, f'../{safe}'), 'rb')\n"
                              "    return a, b, c\n"),
        }
        self.assertEqual([11], [g[3] for g in findings(files)])


class Helpers(unittest.TestCase):
    """The new pieces on their own (rules::security's Rust tests mirror free_value)."""

    def test_free_value(self):
        c = TextHelpers.Ctx({"raw", "safe"}, {"safe": "os.path.basename(raw)"})
        self.assertTrue(security.free_value('f"{safe}.pdf"', c))
        self.assertTrue(security.free_value('safe if safe.lower().endswith(".pdf") else f"{safe}.pdf"', c))
        self.assertTrue(security.free_value('safe + ".pdf"', c))
        self.assertFalse(security.free_value('f"../{safe}"', c))
        self.assertFalse(security.free_value('f"a/{safe}"', c))
        self.assertFalse(security.free_value("raw", c))
        self.assertFalse(security.free_value('f"{raw}.pdf"', c))
        self.assertTrue(security.free_value("name", c))  # (not the request's)

    def test_a_keyword_name_is_not_read(self):
        src = re.compile(r"request\.")
        self.assertFalse(arch._dirty("render_pdf('x.html', items=[])", src, None, {"items"}))
        self.assertFalse(arch._dirty("items=[]", src, None, {"items"}))
        self.assertTrue(arch._dirty("render_pdf('x.html', rows=items)", src, None, {"items"}))
        self.assertTrue(arch._dirty("items == 3", src, None, {"items"}))


if __name__ == "__main__":
    unittest.main()
