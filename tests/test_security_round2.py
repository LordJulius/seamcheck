"""The security pack's second field round on a Flask app (python3 -m unittest discover tests).

What still read as injection after the first round: a sanitizer applied inside a
helper the sink's argument calls, a BytesIO a helper returns, an int with a range
check, an anchored `re.match`, a redirect to a literal host, a loop over a literal
list, a database client whose callers pass constant SQL, and a JS fetch helper
only ever called with same-origin paths. Each case is that pattern cut down,
beside the injection of the same shape that must still be reported.
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_security_precision import findings  # noqa: E402

HEAD = ("import io, os, re, subprocess, sqlite3\nimport requests\n"
        "from flask import redirect, request, send_file, url_for\nfrom app import db\n\n\n")


def py(body: str) -> dict:
    return {"app/routes.py": HEAD + body}


class SanitizedReturns(unittest.TestCase):
    def test_a_helper_whose_return_is_sanitized_is_clean_for_its_callers(self):
        safety = ("def coerce_same_origin_redirect_target(t, default, host):\n    return t if t.startswith('/') else default\n\n\n"
                  "def mode_url_for(endpoint, **kw):\n    return url_for(endpoint, **kw)\n")
        body = ("from app.safety import coerce_same_origin_redirect_target, mode_url_for\n\n\n"
                "def _ret():\n    t = request.form.get('return_to') or request.args.get('return_to')\n"
                "    return coerce_same_origin_redirect_target(t, None, request.host_url)\n\n\n"
                "def _panel_target(pid, return_to_url=None):\n    return mode_url_for('panel', pid=pid, next=return_to_url)\n\n\n"
                "def panel(pid):\n    return redirect(_panel_target(pid, return_to_url=_ret()))\n\n\n"
                "def panel2(pid):\n    target = _panel_target(pid, return_to_url=_ret())\n    return redirect(target)\n\n\n"
                "def raw_next():\n    return request.args.get('next')\n\n\n"
                "def bad():\n    return redirect(raw_next())\n")
        got = findings({"app/safety.py": safety, **py(body)},
                       '[security]\nsanitizers = ["coerce_same_origin_redirect_target"]\n')
        self.assertEqual([("error", 33)], [(g[1], g[3]) for g in got], got)  # only bad(): its helper returns the raw value


class BufferReturns(unittest.TestCase):
    def test_a_helper_that_returns_a_buffer_is_no_path(self):
        body = ("def _csv_bytes(rows) -> io.BytesIO:\n    return io.BytesIO(b''.join(rows))\n\n\n"
                "def _zip_bytes(rows):\n    buf = io.BytesIO()\n    buf.write(rows)\n    return buf\n\n\n"
                "def export():\n    rows = request.args.getlist('r')\n    buffer = _csv_bytes(rows)\n"
                "    send_file(buffer, mimetype='text/csv')\n    return send_file(_zip_bytes(rows), mimetype='application/zip')\n\n\n"
                "def _path(name):\n    return os.path.join('/srv', name)\n\n\n"
                "def raw():\n    return send_file(_path(request.args['f']))\n")
        got = findings(py(body))
        self.assertEqual([("error", 29)], [(g[1], g[3]) for g in got], got)


class CheckedValues(unittest.TestCase):
    def test_an_int_in_range_an_anchored_match_and_a_literal_host(self):
        body = ("_STATE_RE = re.compile(r'^[A-Za-z0-9_-]{16,128}$')\n_LOOSE = re.compile(r'[a-z]+')\n\n\n"
                "def callback():\n    port = int(request.args.get('port', ''))\n    if port < 1024 or port > 65535:\n"
                "        return 'bad', 400\n    state = request.args.get('state', '')\n    if not _STATE_RE.match(state):\n"
                "        return 'bad', 400\n    query = request.query_string.decode()\n"
                "    subprocess.run(f'notify {state}', shell=True)\n"
                "    return redirect(f'http://127.0.0.1:{port}/callback?{query}')\n\n\n"
                "def loose():\n    s = request.args.get('s', '')\n    if not _LOOSE.match(s):\n        return 'bad', 400\n"
                "    subprocess.run(f'notify {s}', shell=True)\n\n\n"
                "def hostile():\n    return redirect(f\"https://{request.args['h']}/x\")\n")
        got = findings(py(body))
        self.assertEqual([("error", 27), ("error", 31)], [(g[1], g[3]) for g in got], got)


class Constants(unittest.TestCase):
    def test_a_loop_over_a_literal_list_is_constant(self):
        body = ("def dump():\n    keep = request.args.get('keep')\n    tables = ['users', 'posts', 'tags']\n"
                "    for table_name in tables:\n        db.session.execute(db.text(f'SELECT * FROM {table_name}'))\n"
                "    for t in request.args.getlist('t'):\n        db.session.execute(db.text(f'SELECT * FROM {t}'))\n"
                "    return keep\n")
        got = findings(py(body))
        self.assertEqual([("error", 13)], [(g[1], g[3]) for g in got], got)

    def test_a_client_whose_callers_pass_constant_sql(self):
        client = ("import sqlite3\nSQL_ONE = 'SELECT * FROM u WHERE id = ?'\n\n\nclass Db:\n"
                  "    def query(self, sql, params=None):\n        cur = self.conn.cursor()\n"
                  "        cur.execute(sql, params or ())\n        return cur.fetchall()\n")
        body = ("from app.client import Db, SQL_ONE\n\n\ndef one():\n    d = Db()\n    uid = request.args['id']\n"
                "    d.query(SQL_ONE, (uid,))\n    ids = request.args.getlist('ids')\n"
                "    marks = ','.join('?' for _ in ids)\n    d.query(f'SELECT * FROM u WHERE id IN ({marks})', ids)\n"
                "    d.query('SELECT * FROM u WHERE id = ?', params=(uid,))\n    d.query('SELECT 1 WHERE ' + request.args['w'])\n")
        got = findings({"app/client.py": client, **py(body)})
        self.assertEqual(1, len(got), got)
        self.assertIn("one -> query", got[0][4])


class Kept(unittest.TestCase):
    def test_a_request_controlled_host_is_still_an_error(self):
        body = ("def version():\n    url = request.get_json()['url']\n"
                "    return requests.get(f'{url}/api/v1/version', timeout=5).text\n")
        self.assertEqual([("error", 9)], [(g[1], g[3]) for g in findings(py(body))])


class JsFetchHelper(unittest.TestCase):
    def test_a_parameter_its_callers_fill_with_same_origin_paths_is_clean(self):
        js = ("const express = require('express');\nconst app = express();\n"
              "async function requestJson(url, options) {\n  const r = await fetch(url, options || {});\n  return r.json();\n}\n"
              "app.post('/chat', async (req, res) => {\n  const body = req.body;\n"
              "  const a = await requestJson('/chat/api/send', { method: 'POST', body: JSON.stringify(body) });\n"
              "  const b = await requestJson(`/chat/api/thread/${body.id}`, {});\n  res.send([a, b]);\n});\n"
              "async function relay(url) {\n  return fetch(url);\n}\n"
              "app.get('/relay', async (req, res) => {\n  res.send(await relay(req.query.u));\n});\n")
        got = findings({"server.js": js})
        self.assertEqual([("js-injection", "error", 14)], [(g[0], g[1], g[3]) for g in got], got)


class RoundTwoHelpers(unittest.TestCase):
    """security.py's new pieces on their own (rules::security's Rust tests mirror these)."""

    def test_guards_ranges_and_anchored_matches(self):
        from seamcheck import security
        self.assertEqual([("port", False)], security.guarded("port < 1024 or port > 65535"))
        self.assertEqual([("state", False)], security.guarded("not _STATE_RE.match(state)", {"_STATE_RE"}))
        self.assertEqual([], security.guarded("not _LOOSE.match(state)", {"_STATE_RE"}))
        self.assertEqual([("s", True)], security.guarded("re.match(r'^[a-z]+$', s)"))

    def test_whole_calls_and_relative_paths(self):
        from seamcheck import security
        from test_security_precision import TextHelpers
        self.assertEqual("mode_url_for", security._whole_call("mode_url_for('a', x=b)"))
        self.assertIsNone(security._whole_call("f(a) + b"))
        c = TextHelpers.Ctx({"x"})
        self.assertEqual("head", security.url_taint("'/' + x", c))
        self.assertEqual("relative", security.url_taint("'/a/' + x", c))

        class P(TextHelpers.Ctx):
            def passed(self, name):
                return ["'/chat/api/send'", "`/chat/api/thread/${id}`"] if name == "url" else None
        self.assertEqual("relative", security.url_taint("url", P({"url"})))


if __name__ == "__main__":
    unittest.main()
