"""The security pack's seventh field round (python3 -m unittest discover tests).

The field's shapes (tests/fixtures/flask_round7): a comment line, a trailing comment or a docstring
between `pdf_buffer = render_pdf(...)` and `send_file(pdf_buffer)` (the value's text ran on into the
comment and lost the buffer summary), and a `render_pdf` imported under another name, in the
function or the file.
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from seamcheck import arch, core  # noqa: E402
from test_security_precision import _sink_at, findings  # noqa: E402


class TheAppShape(unittest.TestCase):
    """The pack gave the fixture 8 errors at 91453e9; its injections are 3, one of them (through the
    alias) newly seen."""

    def test_only_the_injections_are_errors(self):
        root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "flask_round7")
        p = core.load(root, use_cache=False)
        got = sorted((f.severity, *_sink_at(f.message)) for f in core.run_rules(p) if f.rule.startswith("security/"))
        self.assertEqual([("error", "app/inventory/tp.py", 18), ("error", "app/inventory/tp.py", 25),
                          ("error", "app/utils/reports.py", 3)], got)


_PDF = ("from io import BytesIO\n\n\ndef render_pdf(template_name, **context):\n    buf = BytesIO()\n"
        "    buf.write(template_name.encode())\n    buf.seek(0)\n    return buf  # the PDF\n")


def _route(between: str, head: str = "from app.pdf import render_pdf\n", call: str = "render_pdf") -> dict:
    return {
        "app/pdf.py": _PDF,
        "app/routes.py": ("from flask import request, send_file\n" + head + "\n\n"
                          "def export():\n    name = request.args.get('d', '')\n"
                          f"    pdf_buffer = {call}('x.html', x=name)\n" + between +
                          "    return send_file(pdf_buffer, download_name='x.pdf')\n"),
    }


class Comments(unittest.TestCase):
    def test_nothing_between_the_call_and_the_sink_counts(self):
        for between in ("", "    # Log export\n", "    \"\"\"Log export.\"\"\"\n", "\n", "    pass\n",
                        "    # one\n\n    # two\n"):
            with self.subTest(between=between):
                self.assertEqual([], findings(_route(between)))

    def test_a_trailing_comment(self):
        files = _route("")
        files["app/routes.py"] = files["app/routes.py"].replace("x=name)\n", "x=name)  # the report\n")
        self.assertEqual([], findings(files))

    def test_a_comment_keeps_a_real_path(self):
        files = {"app/routes.py": ("from flask import request\n\n\ndef read():\n    path = request.args['p']  # #1\n"
                                   "    # Read it\n    \"\"\"Still the request's.\"\"\"\n    return open(path).read()\n")}
        self.assertEqual([8], [g[3] for g in findings(files)])


class Aliases(unittest.TestCase):
    def test_imported_under_another_name(self):
        self.assertEqual([], findings(_route("", "from app.pdf import render_pdf as make_pdf\n", "make_pdf")))
        files = _route("", "", "rp")
        files["app/routes.py"] = files["app/routes.py"].replace(
            "    name = request", "    from app.pdf import render_pdf as rp\n    name = request")
        self.assertEqual([], findings(files))


class Helpers(unittest.TestCase):
    """arch's comment-blanked text on its own (rust/tests/security_paths.rs mirrors it)."""

    def test_uncommented(self):
        raw = ["x = f()  # note", "# Log export", "s = '#1'  // two", "t = \"\"\"", "# in the string", "\"\"\"  # é",
               "u = \"#{q}\" # c", "v = `a ${b}` /* c */"]
        code = ["x = f()        ", "            ", "s = '  '        ", "t = \"\"\"", "               ", "\"\"\"     ",
                "u = \"    \"    ", "v = `      `        "]
        self.assertEqual(["x = f()        ", "            ", "s = '#1'        ", "t = \"\"\"", "# in the string",
                          "\"\"\"     ", "u = \"#{q}\"    ", "v = `a ${b}`        "], arch._uncommented(raw, code))


if __name__ == "__main__":
    unittest.main()
