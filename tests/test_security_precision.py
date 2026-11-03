"""The security pack on a Flask + Flask-SQLAlchemy + Flask-Login + Celery app with
PyQt desktop tools (python3 -m unittest discover tests).

A field run on such an app gave 104 errors, most of them not injections: an ORM
session's `get(Model, pk)` read as an outbound request, every `session.execute`
as raw SQL, `send_file(BytesIO(...))` as a path, a request to a configured host
as forgery, a Celery task's `self.request.args` and a Qt `QNetworkRequest` as
the web request, `os.path` as a tainted `path`. Each case here is that pattern
cut down, next to the injection of the same shape that must still be reported.
"""
import os
import re
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, security  # noqa: E402

HEAD = ("import io, os, re, subprocess, sys\nfrom io import BytesIO\nimport requests\n"
        "from flask import current_app, redirect, request, send_file, send_from_directory, url_for\n"
        "from sqlalchemy import select, text\nfrom app import db\nfrom app.models import Post, User\n\n\n")
MODELS = {"app/__init__.py": "from flask_sqlalchemy import SQLAlchemy\n\ndb = SQLAlchemy()\n",
          "app/models.py": ("from app import db\n\n\nclass User(db.Model):\n    def avatar(self):\n        return 1\n\n\n"
                            "class Post(db.Model):\n    def to_dict(self):\n        return {}\n")}


def findings(files: dict, extra: str = "") -> list[tuple[str, str, str, int, str]]:
    """(rule, severity, file, line, message) of the security pack's taint rules."""
    with tempfile.TemporaryDirectory() as root:
        for name, body in {**MODELS, **files}.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(textwrap.dedent(body))
        cfg = os.path.join(root, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "t"\ninclude = ["**"]\npacks = ["security"]\n' + extra)
        p = core.load(root, cfg, use_cache=False)
        got = [(f.rule.split("/")[-1], f.severity, *_sink_at(f.message), f.message) for f in core.run_rules(p)
               if "reaches" in f.message or f.rule == "config"]
        return sorted(got, key=lambda g: (g[2], g[3]))


def _sink_at(message: str) -> tuple[str, int]:
    """Where the sink is: (file, line) from "reaches x() at FILE:LINE: chain"."""
    m = re.search(r" at ([^ :]+):(\d+): ", message)
    return (m.group(1), int(m.group(2))) if m else ("", 0)


def routes(body: str) -> dict:
    return {"app/routes.py": HEAD + textwrap.dedent(body)}


class OrmSession(unittest.TestCase):
    def test_a_session_get_of_a_model_is_no_request(self):
        got = findings(routes("def post(pid):\n    p = db.session.get(Post, request.args.get('id', pid))\n    return p.to_dict()\n\n\n"
                              "def other():\n    s = db.session\n    return s.get(User, request.args['id'])\n"))
        self.assertEqual([], got)

    def test_a_query_object_and_bound_parameters_are_no_sql_injection(self):
        got = findings(routes("def search():\n    q = request.args.get('q', '')\n"
                              "    a = db.session.execute(select(User).where(User.name.like(f'%{q}%'))).scalars().all()\n"
                              "    b = db.session.execute(text('SELECT id FROM u WHERE name = :n'), {'n': q}).all()\n"
                              "    stmt = select(User).where(User.name == q)\n    c = db.session.execute(stmt)\n"
                              "    return [a, b, c]\n"))
        self.assertEqual([], got)

    def test_sql_text_built_from_the_value_is(self):
        got = findings(routes("def raw():\n    q = request.args['q']\n"
                              "    db.session.execute(text(f\"SELECT id FROM u WHERE name = '{q}'\"))\n\n\n"
                              "def raw2(cur):\n    sql = \"SELECT * FROM u WHERE id = \" + request.args['id']\n    cur.execute(sql)\n\n\n"
                              "def raw3(cur):\n    cur.execute(\"SELECT * FROM u WHERE id = '%s'\" % request.args['id'])\n"))
        self.assertEqual([("python-injection", "error", "app/routes.py", ln) for ln in (12, 17, 21)],
                         [g[:4] for g in got], got)

    def test_a_project_subclass_of_requests_session_is_an_http_client(self):
        api = ("import requests\n\n\nclass Api(requests.Session):\n    def fetch(self, url):\n        return self.get(url)\n")
        got = findings({"app/api.py": api, **routes("from app.api import Api\n\n\ndef proxy():\n    api = Api()\n"
                                                    "    return api.get(request.args['url'])\n")})
        self.assertEqual([("python-injection", "error", "app/routes.py")], [g[:3] for g in got], got)
        self.assertIn("api.get()", got[0][4])


class Redirects(unittest.TestCase):
    def test_url_for_a_declared_sanitizer_and_an_int_are_safe(self):
        files = {"app/safety.py": "def coerce_safe_redirect_target(t):\n    return t if t.startswith('/') else '/'\n",
                 **routes("from app.safety import coerce_safe_redirect_target\n\n\n"
                          "def back():\n    return redirect(url_for('main.user', tab=request.args.get('tab')))\n\n\n"
                          "def go():\n    return redirect(coerce_safe_redirect_target(request.args.get('next')))\n\n\n"
                          "def page():\n    n = request.args.get('n', 1, type=int)\n    return redirect('https://example.org/items?page=' + str(n))\n\n\n"
                          "def tab():\n    t = request.args.get('t', '')\n    if not t.isdigit():\n        return 'bad', 400\n"
                          "    return redirect('https://example.org/t/' + t)\n")}
        self.assertEqual([], findings(files, '[security]\nsanitizers = ["coerce_safe_redirect_target"]\n'))
        # without the declaration the helper is just a function the value goes through
        self.assertEqual(["python-injection"], [g[0] for g in findings(files)])

    def test_an_open_redirect_still_is(self):
        got = findings(routes("def jump():\n    return redirect(request.args.get('to'))\n"))
        self.assertEqual([("python-injection", "error")], [g[:2] for g in got])

    def test_sanitizers_must_be_a_list_of_names(self):
        p = type("P", (), {"config": {"security": {"sanitizers": "x"}}})()
        self.assertEqual([], security.sanitizers(p))


class Files(unittest.TestCase):
    def test_a_buffer_and_a_checked_name_are_no_path(self):
        got = findings(routes("NAME = re.compile(r'[a-z0-9_-]{1,64}\\.pdf')\n\n\n"
                              "def export():\n    fmt = request.args.get('fmt', 'pdf')\n    buf = io.BytesIO(b'x')\n"
                              "    if fmt == 'zip':\n        return send_file(BytesIO(fmt.encode()), download_name='x.zip')\n"
                              "    return send_file(buf, download_name=f'export.{fmt}')\n\n\n"
                              "def files():\n    name = request.args['name']\n    if not NAME.fullmatch(name):\n        return 'bad', 400\n"
                              "    return send_file(os.path.join(current_app.config['UPLOADS'], name))\n\n\n"
                              "def dl():\n    return send_from_directory(current_app.config['UPLOADS'], request.args['name'])\n"))
        self.assertEqual([], got)

    def test_a_positive_check_cleans_only_its_block(self):
        got = findings(routes("def files():\n    name = request.args['name']\n    if re.fullmatch(r'[a-z]+', name):\n"
                              "        send_file(os.path.join('/srv', name))\n    return send_file(os.path.join('/srv', name))\n"))
        self.assertEqual([14], [g[3] for g in got], got)

    def test_a_path_from_the_request_still_is(self):
        got = findings(routes("def raw():\n    path = os.path.join(current_app.config['UPLOADS'], request.args['f'])\n"
                              "    return send_file(path)\n"))
        self.assertEqual([("python-injection", "error")], [g[:2] for g in got])


class Outbound(unittest.TestCase):
    def test_a_configured_host_is_a_warning_and_a_request_url_an_error(self):
        got = findings(routes("def weather():\n    city = request.args.get('city')\n    base = current_app.config['API']\n"
                              "    return requests.get(f'{base}/forecast', params={'q': city}).json()\n\n\n"
                              "def fixed():\n    return requests.get('https://api.example.org/v1/' + request.args['id']).json()\n\n\n"
                              "def fetch():\n    url = request.args['url']\n    return requests.get(url).text\n\n\n"
                              "def host():\n    return requests.get('https://' + request.args['h'] + '/x').text\n"))
        self.assertEqual([("warn", 13), ("warn", 17), ("error", 22), ("error", 26)], [(g[1], g[3]) for g in got], got)


class Commands(unittest.TestCase):
    def test_an_argument_list_of_constants_and_server_paths_is_quiet(self):
        tasks = ("import os, subprocess\nfrom app import celery\n\n\n@celery.task(bind=True)\ndef build(self, name):\n"
                 "    args = self.request.args\n    out = os.path.join('/srv/reports', f'{self.request.id}.txt')\n"
                 "    subprocess.run(['/usr/bin/report', '--out', out, '--for', str(args[0])], check=True)\n")
        got = findings({"app/tasks.py": tasks, **routes(
            "def archive():\n    path = request.args.get('path', '')\n"
            "    subprocess.run(['tar', 'czf', os.path.join(current_app.instance_path, 'a.tgz'), '-C', current_app.root_path])\n"
            "    out = os.path.join(current_app.instance_path, 'r.pdf')\n"
            "    subprocess.run([sys.executable, 'render.py', '--out', out], check=True)\n    return path\n")})
        self.assertEqual([], got)

    def test_the_value_in_a_fixed_programs_arguments_is_a_warning_and_in_a_shell_an_error(self):
        got = findings(routes("def grep():\n    q = request.args['q']\n    subprocess.run(['grep', '-r', q, '/srv'])\n\n\n"
                              "def ping():\n    host = request.args['host']\n    subprocess.run(f'ping -c1 {host}', shell=True)\n\n\n"
                              "def prog():\n    subprocess.run([request.args['p'], '-x'])\n"))
        self.assertEqual([("warn", 12), ("error", 17), ("error", 21)], [(g[1], g[3]) for g in got], got)

    def test_a_row_looked_up_by_the_value_is_the_databases(self):
        got = findings(routes("def home():\n    user = User.query.filter_by(name=request.args['n']).first()\n"
                              "    os.system('du -sh ' + user.home)\n"))
        self.assertEqual([], got)


class SourceScope(unittest.TestCase):
    QT = ("import subprocess, sys\nfrom PyQt6.QtNetwork import QNetworkRequest\nfrom PyQt6.QtWidgets import QFileDialog\n\n\n"
          "class Viewer:\n    def fetch(self, url):\n        request = QNetworkRequest(url)\n"
          "        open(request.url().toString())\n        subprocess.run(request.headers, shell=True)\n\n"
          "    def pick(self):\n        path, _ = QFileDialog.getOpenFileName(self, 'Open')\n"
          "        subprocess.run(['xdg-open', path])\n        with open(path) as f:\n            return f.read()\n\n"
          "    def script(self):\n        name, _ = QFileDialog.getOpenFileName(self, 'Script')\n"
          "        subprocess.run(f'python3 {name}', shell=True)\n")

    def test_desktop_input_is_its_own_lower_source(self):
        got = findings({"tools/viewer.py": self.QT})
        self.assertEqual([("python-desktop-input", "warn", "tools/viewer.py", 20)], [g[:4] for g in got], got)

    def test_a_helper_a_route_reaches_still_reads_the_request(self):
        util = "import os\n\n\ndef run_it():\n    os.system('ping ' + request.args['h'])\n"
        got = findings({"app/util.py": util, **routes("from app.util import run_it\n\n\ndef ping():\n    run_it()\n")})
        self.assertEqual([("python-injection", "app/util.py")], [(g[0], g[2]) for g in got], got)


class JavaScript(unittest.TestCase):
    def test_a_relative_fetch_is_same_origin(self):
        js = ("const express = require('express');\nconst app = express();\n"
              "app.get('/a', async (req, res) => {\n  const r = await fetch(`/api/posts?page=${req.query.page}`);\n"
              "  res.send(await r.text());\n});\n"
              "app.get('/b', async (req, res) => {\n  const r = await fetch(req.query.url);\n  res.send(await r.text());\n});\n"
              "app.get('/c', async (req, res) => {\n  const r = await fetch('//' + req.query.host + '/x');\n  res.send(r);\n});\n")
        got = findings({"server.js": js})
        self.assertEqual([("js-injection", "error", 8), ("js-injection", "error", 12)], [(g[0], g[1], g[3]) for g in got], got)


class TheAppShape(unittest.TestCase):
    """tests/fixtures/flask_sqla: the field report's app cut down, an injection beside each look-alike
    (marked `TP` in its source). The pack once gave it 18 errors; the injections are 8."""

    def test_only_the_injections_are_errors(self):
        root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "flask_sqla")
        p = core.load(root, use_cache=False)
        got = sorted((f.severity, *_sink_at(f.message)) if "reaches" in f.message else (f.severity, f.file, f.line)
                     for f in core.run_rules(p) if f.rule.startswith("security/"))
        self.assertEqual([("error", "app/main/routes.py", ln) for ln in (58, 76, 106, 120, 144)]
                         + [("error", "run.py", 6), ("error", "server.js", 11), ("error", "server.js", 16),
                            ("warn", "app/main/routes.py", 113), ("warn", "tools/viewer.py", 31)], got)


class TextHelpers(unittest.TestCase):
    """security.py's pieces on their own (the Rust port's tests mirror these)."""

    class Ctx:
        def __init__(self, dirty: set, values: dict | None = None):
            self.names, self.values = dirty, values or {}

        def dirty(self, text):
            import re
            return any(re.search(rf"(?<![\w.]){n}(?!\w)", text) for n in self.names)

        def rhs(self, name):
            return (self.values[name], self) if name in self.values else None

    def test_url_taint(self):
        c = self.Ctx({"q", "u", "h"}, {"u": "request.args['u']"})
        for url, want in (("f'{base}/x?q={q}'", "tail"), ("'https://api.example.org/' + q", "tail"), ("u", "head"),
                          ("'https://' + h + '/x'", "head"), ("f'https://{h}/x'", "head"), ("`/api?q=${q}`", "relative"),
                          ("'/search?q=' + q", "relative"), ("'//' + h", "head"), ("'https://x.org/{}'.format(q)", "tail"),
                          ("'http://%s/x' % h", "head"), ("base", "clean"),
                          ("'https://api.x.com'\n        '/t?a={}'.format(q)", "tail")):
            self.assertEqual(want, security.url_taint(url, c), url)

    def test_sql_dirty(self):
        c = self.Ctx({"q", "s", "t"}, {"s": "select(User).where(User.n == q)", "t": "'SELECT ' + q"})
        for sql, want in (("text('SELECT :q')", False), ("text(f'SELECT {q}')", True), ("s", False), ("t", True),
                          ("q", True), ("'SELECT 1'", False), ("User.query.filter(User.n == q)", False)):
            self.assertEqual(want, security.sql_dirty(sql, c), sql)

    def test_guarded(self):
        self.assertEqual([("n", False)], security.guarded("not NAME.fullmatch(n)"))
        self.assertEqual([("n", True)], security.guarded("n.isdigit() and x"))
        self.assertEqual([("nxt", False)], security.guarded("not nxt or urlsplit(nxt).netloc != ''"))
        self.assertEqual([("fmt", False)], security.guarded("fmt not in ALLOWED"))
        self.assertEqual([("fmt", True)], security.guarded("fmt in ('a', 'b')"))
        self.assertEqual([("next", False)], security.guarded("next is None or not next.startswith('/')"))


if __name__ == "__main__":
    unittest.main()
