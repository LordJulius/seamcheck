"""The security pack's third field round on a Flask app (python3 -m unittest discover tests).

What still read as injection after the second round: a buffer a function fills with
`buffer.write(...)` before sending it, a path a controller method checks under its
roots, a redirect to a URL a helper builds from url_for with the request's query
after it, and a redirect back to the Referer (now a warning). Each case is that
pattern cut down, beside the injection of the same shape that must still be reported.
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from seamcheck import core  # noqa: E402
from test_security_precision import TextHelpers, _sink_at, findings  # noqa: E402

HEAD = ("import io, os\nfrom io import BytesIO\nfrom pathlib import Path\nfrom urllib.parse import urlencode\n"
        "from sqlalchemy import text\nfrom flask import redirect, request, send_file, url_for\nfrom app import db\n\n\n")


def py(body: str) -> dict:
    return {"app/routes.py": HEAD + body}


def at(got) -> list:
    return [(g[1], g[3]) for g in got]


class FilledBuffers(unittest.TestCase):
    def test_a_buffer_written_to_is_still_a_buffer(self):
        pdf = ("import io\nfrom flask import render_template\n\n\ndef render_pdf(template_name, **context):\n"
               "    html = render_template(template_name, **context)\n    buf = io.BytesIO()\n    buf.write(html.encode())\n"
               "    buf.seek(0)\n    return buf\n")
        body = ("from app.pdf import render_pdf\n\n\n"
                "def invoice():\n    inv = request.args.get('id')\n    pdf_buffer = render_pdf('invoice.html', inv=inv)\n"
                "    return send_file(pdf_buffer, mimetype='application/pdf', download_name=f'inv-{inv}.pdf')\n\n\n"
                "def notes():\n    data = request.args['d'].encode()\n    buffer = io.BytesIO()\n    buffer.write(data)\n"
                "    buffer.seek(0)\n    return send_file(buffer, mimetype='text/plain')\n\n\n"
                "def rows():\n    buffer = BytesIO()\n    for r in request.args.getlist('r'):\n        buffer.write(r.encode())\n"
                "    return send_file(buffer, as_attachment=True)\n\n\n"
                "def swapped():\n    buffer = io.BytesIO()\n    buffer = request.args['p']\n    return send_file(buffer)\n")
        got = findings({"app/pdf.py": pdf, **py(body)})
        self.assertEqual([("error", 37)], at(got), got)  # only swapped(): the name was reassigned to the value


class ConfinedPaths(unittest.TestCase):
    def test_a_path_a_method_checks_under_its_roots(self):
        ctl = ("from pathlib import Path\n\n\nclass HostController:\n    roots = [Path('/srv/a'), Path('/srv/b')]\n\n"
               "    def _is_allowed_path(self, path):\n        resolved = Path(path).resolve()\n        for root in self.roots:\n"
               "            try:\n                resolved.relative_to(root)\n                return resolved\n"
               "            except ValueError:\n                continue\n        raise PermissionError(path)\n\n"
               "    def resolve_managed_path(self, raw, must_exist=False):\n        path = self._is_allowed_path(raw)\n"
               "        if must_exist and not path.exists():\n            raise FileNotFoundError(raw)\n        return path\n\n"
               "    def loose(self, raw):\n        return Path(raw).resolve()\n\n\nhost_controller = HostController()\n")
        body = ("from app.ctl import host_controller\n\n\n"
                "def download():\n    path = host_controller.resolve_managed_path(request.args['path'], must_exist=True)\n"
                "    return send_file(path)\n\n\n"
                "def direct():\n    return send_file(host_controller.resolve_managed_path(request.args['path']))\n\n\n"
                "def resolved_only():\n    return send_file(host_controller.loose(request.args['path']))\n")
        got = findings({"app/ctl.py": ctl, **py(body)})
        self.assertEqual([("error", 23)], at(got), got)  # resolve() alone confines nothing


class Redirects(unittest.TestCase):
    def test_a_same_site_url_helper_with_the_query_after_it(self):
        body = ("def mode_query_url(endpoint, include_request_args=False, **values):\n"
                "    q = request.args.to_dict(flat=False) if include_request_args else {}\n    q.pop('mode', None)\n"
                "    return url_for(endpoint, **values) + '?' + urlencode(q, doseq=True)\n\n\n"
                "def index():\n    return redirect(mode_query_url('x.index', include_request_args=True, view='y'))\n\n\n"
                "def index2():\n    target = _go()\n    return redirect(target)\n\n\n"
                "def _go():\n    return mode_query_url('x.index', include_request_args=True)\n\n\n"
                "def query_host():\n    return request.args['h'] + '?' + urlencode(request.args)\n\n\n"
                "def bad():\n    return redirect(query_host())\n")
        got = findings(py(body))
        self.assertEqual([("error", 34)], at(got), got)

    def test_back_to_the_referer_is_a_warning(self):
        body = ("from app.safety import safe_target\n\n\n"
                "def back():\n    return redirect(request.referrer or url_for('index'))\n\n\n"
                "def back2():\n    ref = request.headers.get('Referer')\n    return redirect(ref or '/')\n\n\n"
                "def checked():\n    return redirect(safe_target(request.referrer))\n\n\n"
                "def nxt():\n    return redirect(request.args.get('next') or request.referrer)\n\n\n"
                "def real():\n    return redirect(request.form.get('return_to'))\n")
        safety = "def safe_target(t):\n    return t if t and t.startswith('/') else '/'\n"
        got = findings({"app/safety.py": safety, **py(body)}, '[security]\nsanitizers = ["safe_target"]\n')
        self.assertEqual([("warn", 14), ("warn", 19), ("error", 27), ("error", 31)], at(got), got)
        self.assertIn("(a redirect to the Referer)", got[0][4])

    def test_referer_severity_off_drops_it(self):
        from seamcheck import security
        c = TextHelpers.Ctx({"request"})
        on = security.Sinks({"redirect_sinks": ["^redirect$"]})
        off = security.Sinks({"redirect_sinks": ["^redirect$"], "referer_severity": "off"})
        self.assertEqual(("warn", " (a redirect to the Referer)"), on.verdict("redirect", ["request.referrer"], c))
        self.assertIsNone(off.verdict("redirect", ["request.referrer"], c))
        self.assertEqual(("error", ""), off.verdict("redirect", ["request.args['next']"], c))


class Constants(unittest.TestCase):
    def test_a_literal_list_loop_nested_in_try_and_with(self):
        body = ("from zipfile import ZipFile\n\n\ndef dump():\n    fmt = request.args.get('fmt', 'csv')\n"
                "    tables: list[str] = [\n        'customer',  # people\n        'department',\n        'fuel_surcharge',\n    ]\n"
                "    buf = BytesIO()\n    try:\n        with ZipFile(\n            buf, 'w'\n        ) as zf:\n"
                "            for table_name in tables:\n                rows = db.session.execute(\n                    db.text(\n"
                "                        f\"\"\"SELECT *\n                        FROM {table_name}\"\"\"\n                    )\n"
                "                ).mappings().all()\n                zf.writestr(f'{table_name}.{fmt}', str(rows))\n"
                "    except Exception:\n        return 'bad', 500\n    return send_file(buf, download_name=f'dump.{fmt}')\n")
        self.assertEqual([], findings(py(body)))

    def test_constant_sql_shapes_through_a_fetch_helper(self):
        body = ("def _header_sql_select() -> str:\n    return \"\"\"a, b, c\"\"\"\n\n\n"
                "def b2w_fetchall(sql, params, conn=None):\n    cur = conn.cursor()\n    cur.execute(sql, params)\n    return cur.fetchall()\n\n\n"
                "def lookup():\n    exact = request.args.getlist('x')\n    placeholders = ','.join(['%s'] * len(exact))\n"
                "    header_from = f'FROM header h'\n    order_sql = 'ORDER BY a' if request.args.get('o') else ''\n"
                "    sql = f\"\"\"SELECT {_header_sql_select()} {header_from} WHERE x IN ({placeholders}) {order_sql}\"\"\"\n"
                "    return b2w_fetchall(sql, tuple(exact), conn=db)\n\n\n"
                "def purge():\n    conditions, params = [], {}\n    if request.form.get('wo'):\n"
                "        conditions.append('work_order_id = :wo')\n        params['wo'] = request.form['wo']\n"
                "    db.session.execute(text(f\"DELETE FROM invoice_work_orders WHERE {' AND '.join(conditions)}\"), params)\n\n\n"
                "def bad():\n    c = request.args['c']\n    return b2w_fetchall(f'SELECT * FROM t WHERE {c}', (), conn=db)\n")
        got = findings(py(body))
        self.assertEqual(1, len(got), got)
        self.assertIn("bad -> b2w_fetchall", got[0][4])


class JsIife(unittest.TestCase):
    def test_a_fetch_helper_inside_an_iife_called_from_nested_closures(self):
        js = ("(function () {\n  async function requestJson(url, options) {\n    const r = await fetch(url, options || {});\n"
              "    return r.json();\n  }\n  function bind(root, req) {\n"
              "    root.querySelectorAll('[data-thread]').forEach((el) => {\n      el.addEventListener('click', async () => {\n"
              "        await requestJson(`/chat/api/${el.dataset.thread}/messages`, { method: 'GET' });\n      });\n    });\n"
              "    root.addEventListener('submit', async () => {\n"
              "      await requestJson('/chat/api/send', { method: 'POST', body: JSON.stringify(req.body) });\n    });\n  }\n"
              "  bind(document.body, window.state);\n})();\n")
        self.assertEqual([], findings({"static/chat.js": js}))


class TheAppShape(unittest.TestCase):
    """tests/fixtures/flask_round3: the round's shapes in one Flask blueprint, an injection beside each
    look-alike (marked `TP`). The pack gave it 6 errors before this round; the injections are 2."""

    def test_only_the_injections_are_errors(self):
        root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "flask_round3")
        p = core.load(root, use_cache=False)
        got = sorted((f.severity, *_sink_at(f.message)) for f in core.run_rules(p) if f.rule.startswith("security/"))
        self.assertEqual([("error", "app/main/files.py", 40), ("error", "app/main/files.py", 55),
                          ("warn", "app/main/files.py", 50)], got)


class RoundThreeHelpers(unittest.TestCase):
    """security.py's new pieces on their own (rules::security's Rust tests mirror these)."""

    def test_referer_only(self):
        from seamcheck import security
        c = TextHelpers.Ctx({"request", "ref", "nxt"}, {"ref": "request.headers.get('Referer')"})
        self.assertTrue(security.referer_only("request.referrer or url_for('index')", c))
        self.assertTrue(security.referer_only("ref or '/'", c))
        self.assertTrue(security.referer_only("request.META.get('HTTP_REFERER', '/')", c))
        self.assertFalse(security.referer_only("request.args.get('next') or request.referrer", c))
        self.assertFalse(security.referer_only("nxt", c))

    def test_summaries_by_name(self):
        import tempfile
        from seamcheck import arch, security
        with tempfile.TemporaryDirectory() as root:
            os.makedirs(os.path.join(root, "app"))
            with open(os.path.join(root, "app", "h.py"), "w") as fh:
                fh.write("from pathlib import Path\nfrom flask import url_for\n\n\n"
                         "def check(p):\n    r = Path(p).resolve()\n    if not r.is_relative_to('/srv'):\n        raise ValueError(p)\n    return r\n\n\n"
                         "def managed(p):\n    return check(p)\n\n\n"
                         "def loose(p):\n    return Path(p).resolve()\n\n\n"
                         "def mq(e, q):\n    return url_for(e) + '?' + q\n\n\n"
                         "def go(q):\n    return mq('x', q)\n\n\n"
                         "def off(q):\n    return q + '/x'\n")
            p = core.load(root, None, use_cache=False)
            confined, same = security.summaries(p, lambda g, raw=False: arch._own_code(p, g, raw))
        self.assertEqual({"check", "managed", "app/h.check", "app/h.managed"}, confined)  # (names and function ids)
        self.assertEqual({"mq", "go", "app/h.mq", "app/h.go"}, same)

    def test_same_site_returns(self):
        from seamcheck import security
        self.assertTrue(security._same_site_ret("url_for(endpoint, **values) + '?' + urlencode(q)", set()))
        self.assertTrue(security._same_site_ret("f'/x?{q}'", set()))
        self.assertTrue(security._same_site_ret("helper(a)", {"helper"}))
        self.assertIsNone(security._same_site_ret("helper(a)", None))
        self.assertFalse(security._same_site_ret("request.args['h'] + '?'", set()))
        self.assertFalse(security._same_site_ret("'//evil' + q", set()))


if __name__ == "__main__":
    unittest.main()
