"""The security pack's fifth field round (python3 -m unittest discover tests).

The field's shapes (tests/fixtures/flask_round5): an allow-list kept by membership and SQLAlchemy Core
statements, a route whose `render_pdf` is imported inside it while another route of the file imports
another `render_pdf`, file names with no separator left (a basename, both separators replaced) joined
to a temporary directory, and a fixed program's argument list handed to a helper.
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from seamcheck import core, security  # noqa: E402
from test_security_precision import TextHelpers, _sink_at, findings  # noqa: E402


class TheAppShape(unittest.TestCase):
    """The pack gave the fixture 14 errors at d0c8436; its injections are 3, its fixed programs 2 warnings."""

    def test_only_the_injections_are_errors(self):
        root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "flask_round5")
        p = core.load(root, use_cache=False)
        got = sorted((f.severity, *_sink_at(f.message)) for f in core.run_rules(p) if f.rule.startswith("security/"))
        self.assertEqual([("error", "app/inventory/routes.py", 62), ("error", "app/mail/attach.py", 50),
                          ("error", "app/ops/git.py", 22), ("warn", "app/mail/attach.py", 20),
                          ("warn", "app/ops/git.py", 11)], got)


class LocalImports(unittest.TestCase):
    def test_an_import_inside_the_function_wins_there(self):
        files = {
            "app/pdf.py": "import io\n\n\ndef render_pdf(name, **kw):\n    buf = io.BytesIO()\n    buf.write(name.encode())\n    return buf\n",
            "app/help.py": "def render_pdf(guide):\n    return open(guide).read()\n",
            "app/routes.py": ("from flask import request, send_file\n\n\n"
                              "def report():\n    from app.pdf import render_pdf\n\n"
                              "    out = render_pdf('r.html', q=request.args.get('q'))\n    return send_file(out)\n\n\n"
                              "def guide():\n    from app.help import render_pdf\n\n    return render_pdf('g')\n"),
        }
        self.assertEqual([], findings(files))


class Helpers(unittest.TestCase):
    """security.py's new pieces on their own (rules::security's Rust tests mirror these)."""

    def test_allow_lists_and_statements(self):
        dirty = lambda t: "tables" in t  # noqa: E731
        self.assertTrue(security.keeps_members("[name for name in tables if name in known]", dirty))
        self.assertFalse(security.keeps_members("[name for name in tables if name in tables]", dirty))
        self.assertFalse(security.keeps_members("[name for name in tables if other in known]", dirty))
        self.assertTrue(security.statement("known[table_name].select()"))
        self.assertTrue(security.statement("t.delete().where(t.c.id == x)"))
        self.assertFalse(security.statement("db.text(f'SELECT {x}')"))

    def test_separator_free(self):
        self.assertTrue(security.separator_free("f.replace('/', '_').replace('\\\\', '_')"))
        self.assertFalse(security.separator_free("f.replace('/', '_')"))
        self.assertTrue(security.separator_free("os.path.basename(_coerce_text(v)).strip()"))
        self.assertTrue(security.separator_free("helper(v) or 'attachment'", {"helper"}))
        self.assertFalse(security.separator_free("v or 'attachment'"))

    def test_argument_lists(self):
        c = TextHelpers.Ctx({"args"})
        self.assertEqual(["git_path", "args"], security.argv("[git_path] + args", c))
        self.assertTrue(security._program("shutil.which('git') or '/usr/bin/git'"))
        self.assertFalse(security._program("shutil.which(tool) or '/usr/bin/git'"))


if __name__ == "__main__":
    unittest.main()
