//! The security pack's taint rules end to end (kind = "path" calling
//! rules::security), ported from test_security_precision.py's rule-level
//! and test_security_round2/3/4.py's rule-level classes (their text helpers are
//! ported in src/rules/security.rs).

mod common;

use common::write;
use seamcheck::project::Project;
use seamcheck::rules::run_rules;

const HEAD: &str = "import io, os, re, subprocess, sys\nfrom io import BytesIO\nimport requests\n\
from flask import current_app, redirect, request, send_file, send_from_directory, url_for\n\
from sqlalchemy import select, text\nfrom app import db\nfrom app.models import Post, User\n\n\n";
const MODELS: &[(&str, &str)] = &[
    ("app/__init__.py", "from flask_sqlalchemy import SQLAlchemy\n\ndb = SQLAlchemy()\n"),
    (
        "app/models.py",
        "from app import db\n\n\nclass User(db.Model):\n    def avatar(self):\n        return 1\n\n\nclass Post(db.Model):\n    def to_dict(self):\n        return {}\n",
    ),
];

type Got = Vec<(String, String, String, usize, String)>;

/// Where the sink is: (file, line) from "reaches x() at FILE:LINE: chain"
/// (test_security_precision._sink_at: the first ` at ([^ :]+):(\d+): `).
fn sink_at(message: &str) -> (String, usize) {
    for (k, _) in message.match_indices(" at ") {
        let rest = &message[k + 4..];
        let file: String = rest.chars().take_while(|c| *c != ' ' && *c != ':').collect();
        let tail = &rest[file.len()..];
        let Some(tail) = tail.strip_prefix(':') else { continue };
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !file.is_empty() && !digits.is_empty() && tail[digits.len()..].starts_with(": ") {
            return (file, digits.parse().unwrap());
        }
    }
    (String::new(), 0)
}

/// (rule, severity, file, line, message) of the security pack's taint rules (test_security_precision.findings).
fn findings(files: &[(&str, &str)], extra: &str) -> Got {
    std::env::set_var("SEAMCHECK_RS_QUIET", "1");
    let d = tempfile::tempdir().unwrap();
    write(d.path(), MODELS);
    write(d.path(), files);
    std::fs::write(
        d.path().join("seamcheck.toml"),
        format!("[project]\nname = \"t\"\ninclude = [\"**\"]\npacks = [\"security\"]\n{extra}"),
    )
    .unwrap();
    let p = Project::load(d.path(), &[]);
    let mut got: Got = run_rules(&p, None, None)
        .into_iter()
        .filter(|f| f.message.contains("reaches") || f.rule == "config")
        .map(|f| {
            let (file, line) = sink_at(&f.message);
            (f.rule.rsplit('/').next().unwrap().to_string(), f.severity.clone(), file, line, f.message.clone())
        })
        .collect();
    got.sort_by(|a, b| (&a.2, a.3).cmp(&(&b.2, b.3)));
    got
}

fn routes(body: &str) -> (String, String) {
    ("app/routes.py".into(), format!("{HEAD}{body}"))
}

fn run(files: Vec<(String, String)>, extra: &str) -> Got {
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    findings(&refs, extra)
}

fn sev_line(got: &Got) -> Vec<(String, usize)> {
    got.iter().map(|g| (g.1.clone(), g.3)).collect()
}

// test_security_precision.py: OrmSession.test_a_session_get_of_a_model_is_no_request
#[test]
fn a_session_get_of_a_model_is_no_request() {
    let got = run(
        vec![routes(
            "def post(pid):\n    p = db.session.get(Post, request.args.get('id', pid))\n    return p.to_dict()\n\n\n\
             def other():\n    s = db.session\n    return s.get(User, request.args['id'])\n",
        )],
        "",
    );
    assert_eq!(got, vec![]);
}

// test_security_precision.py: OrmSession.test_a_query_object_and_bound_parameters_are_no_sql_injection
#[test]
fn a_query_object_and_bound_parameters_are_no_sql_injection() {
    let got = run(
        vec![routes(
            "def search():\n    q = request.args.get('q', '')\n\
             \x20   a = db.session.execute(select(User).where(User.name.like(f'%{q}%'))).scalars().all()\n\
             \x20   b = db.session.execute(text('SELECT id FROM u WHERE name = :n'), {'n': q}).all()\n\
             \x20   stmt = select(User).where(User.name == q)\n    c = db.session.execute(stmt)\n    return [a, b, c]\n",
        )],
        "",
    );
    assert_eq!(got, vec![]);
}

// test_security_precision.py: OrmSession.test_sql_text_built_from_the_value_is
#[test]
fn sql_text_built_from_the_value_is() {
    let got = run(
        vec![routes(
            "def raw():\n    q = request.args['q']\n    db.session.execute(text(f\"SELECT id FROM u WHERE name = '{q}'\"))\n\n\n\
             def raw2(cur):\n    sql = \"SELECT * FROM u WHERE id = \" + request.args['id']\n    cur.execute(sql)\n\n\n\
             def raw3(cur):\n    cur.execute(\"SELECT * FROM u WHERE id = '%s'\" % request.args['id'])\n",
        )],
        "",
    );
    let at: Vec<(String, String, String, usize)> = got.iter().map(|g| (g.0.clone(), g.1.clone(), g.2.clone(), g.3)).collect();
    let want: Vec<(String, String, String, usize)> =
        [12, 17, 21].iter().map(|ln| ("python-injection".into(), "error".into(), "app/routes.py".into(), *ln)).collect();
    assert_eq!(at, want, "{got:?}");
}

// test_security_precision.py: OrmSession.test_a_project_subclass_of_requests_session_is_an_http_client
#[test]
fn a_project_subclass_of_requests_session_is_an_http_client() {
    let api = "import requests\n\n\nclass Api(requests.Session):\n    def fetch(self, url):\n        return self.get(url)\n";
    let got = run(
        vec![
            ("app/api.py".into(), api.into()),
            routes("from app.api import Api\n\n\ndef proxy():\n    api = Api()\n    return api.get(request.args['url'])\n"),
        ],
        "",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!((got[0].0.as_str(), got[0].1.as_str(), got[0].2.as_str()), ("python-injection", "error", "app/routes.py"));
    assert!(got[0].4.contains("api.get()"));
}

fn redirect_files() -> Vec<(String, String)> {
    vec![
        ("app/safety.py".into(), "def coerce_safe_redirect_target(t):\n    return t if t.startswith('/') else '/'\n".into()),
        routes(
            "from app.safety import coerce_safe_redirect_target\n\n\n\
             def back():\n    return redirect(url_for('main.user', tab=request.args.get('tab')))\n\n\n\
             def go():\n    return redirect(coerce_safe_redirect_target(request.args.get('next')))\n\n\n\
             def page():\n    n = request.args.get('n', 1, type=int)\n    return redirect('https://example.org/items?page=' + str(n))\n\n\n\
             def tab():\n    t = request.args.get('t', '')\n    if not t.isdigit():\n        return 'bad', 400\n\
             \x20   return redirect('https://example.org/t/' + t)\n",
        ),
    ]
}

// test_security_precision.py: Redirects.test_url_for_a_declared_sanitizer_and_an_int_are_safe
#[test]
fn url_for_a_declared_sanitizer_and_an_int_are_safe() {
    assert_eq!(run(redirect_files(), "[security]\nsanitizers = [\"coerce_safe_redirect_target\"]\n"), vec![]);
    // without the declaration the helper is just a function the value goes through
    let rules: Vec<String> = run(redirect_files(), "").into_iter().map(|g| g.0).collect();
    assert_eq!(rules, vec!["python-injection".to_string()]);
}

// test_security_precision.py: Redirects.test_an_open_redirect_still_is
#[test]
fn an_open_redirect_still_is() {
    let got = run(vec![routes("def jump():\n    return redirect(request.args.get('to'))\n")], "");
    assert_eq!(got.iter().map(|g| (g.0.as_str(), g.1.as_str())).collect::<Vec<_>>(), vec![("python-injection", "error")]);
}

// test_security_precision.py: Files.test_a_buffer_and_a_checked_name_are_no_path
#[test]
fn a_buffer_and_a_checked_name_are_no_path() {
    let got = run(
        vec![routes(
            "NAME = re.compile(r'[a-z0-9_-]{1,64}\\.pdf')\n\n\n\
             def export():\n    fmt = request.args.get('fmt', 'pdf')\n    buf = io.BytesIO(b'x')\n\
             \x20   if fmt == 'zip':\n        return send_file(BytesIO(fmt.encode()), download_name='x.zip')\n\
             \x20   return send_file(buf, download_name=f'export.{fmt}')\n\n\n\
             def files():\n    name = request.args['name']\n    if not NAME.fullmatch(name):\n        return 'bad', 400\n\
             \x20   return send_file(os.path.join(current_app.config['UPLOADS'], name))\n\n\n\
             def dl():\n    return send_from_directory(current_app.config['UPLOADS'], request.args['name'])\n",
        )],
        "",
    );
    assert_eq!(got, vec![]);
}

// test_security_precision.py: Files.test_a_positive_check_cleans_only_its_block
#[test]
fn a_positive_check_cleans_only_its_block() {
    let got = run(
        vec![routes(
            "def files():\n    name = request.args['name']\n    if re.fullmatch(r'[a-z]+', name):\n\
             \x20       send_file(os.path.join('/srv', name))\n    return send_file(os.path.join('/srv', name))\n",
        )],
        "",
    );
    assert_eq!(got.iter().map(|g| g.3).collect::<Vec<_>>(), vec![14], "{got:?}");
}

// test_security_precision.py: Files.test_a_path_from_the_request_still_is
#[test]
fn a_path_from_the_request_still_is() {
    let got = run(
        vec![routes("def raw():\n    path = os.path.join(current_app.config['UPLOADS'], request.args['f'])\n    return send_file(path)\n")],
        "",
    );
    assert_eq!(got.iter().map(|g| (g.0.as_str(), g.1.as_str())).collect::<Vec<_>>(), vec![("python-injection", "error")]);
}

// test_security_precision.py: Outbound.test_a_configured_host_is_a_warning_and_a_request_url_an_error
#[test]
fn a_configured_host_is_a_warning_and_a_request_url_an_error() {
    let got = run(
        vec![routes(
            "def weather():\n    city = request.args.get('city')\n    base = current_app.config['API']\n\
             \x20   return requests.get(f'{base}/forecast', params={'q': city}).json()\n\n\n\
             def fixed():\n    return requests.get('https://api.example.org/v1/' + request.args['id']).json()\n\n\n\
             def fetch():\n    url = request.args['url']\n    return requests.get(url).text\n\n\n\
             def host():\n    return requests.get('https://' + request.args['h'] + '/x').text\n",
        )],
        "",
    );
    assert_eq!(
        sev_line(&got),
        vec![("warn".into(), 13), ("warn".into(), 17), ("error".into(), 22), ("error".into(), 26)],
        "{got:?}"
    );
}

// test_security_precision.py: Commands.test_an_argument_list_of_constants_and_server_paths_is_quiet
#[test]
fn an_argument_list_of_constants_and_server_paths_is_quiet() {
    let tasks = "import os, subprocess\nfrom app import celery\n\n\n@celery.task(bind=True)\ndef build(self, name):\n\
                 \x20   args = self.request.args\n    out = os.path.join('/srv/reports', f'{self.request.id}.txt')\n\
                 \x20   subprocess.run(['/usr/bin/report', '--out', out, '--for', str(args[0])], check=True)\n";
    let got = run(
        vec![
            ("app/tasks.py".into(), tasks.into()),
            routes(
                "def archive():\n    path = request.args.get('path', '')\n\
                 \x20   subprocess.run(['tar', 'czf', os.path.join(current_app.instance_path, 'a.tgz'), '-C', current_app.root_path])\n\
                 \x20   out = os.path.join(current_app.instance_path, 'r.pdf')\n\
                 \x20   subprocess.run([sys.executable, 'render.py', '--out', out], check=True)\n    return path\n",
            ),
        ],
        "",
    );
    assert_eq!(got, vec![]);
}

// test_security_precision.py: Commands.test_the_value_in_a_fixed_programs_arguments_is_a_warning_and_in_a_shell_an_error
#[test]
fn the_value_in_a_fixed_programs_arguments_is_a_warning_and_in_a_shell_an_error() {
    let got = run(
        vec![routes(
            "def grep():\n    q = request.args['q']\n    subprocess.run(['grep', '-r', q, '/srv'])\n\n\n\
             def ping():\n    host = request.args['host']\n    subprocess.run(f'ping -c1 {host}', shell=True)\n\n\n\
             def prog():\n    subprocess.run([request.args['p'], '-x'])\n",
        )],
        "",
    );
    assert_eq!(sev_line(&got), vec![("warn".into(), 12), ("error".into(), 17), ("error".into(), 21)], "{got:?}");
}

// test_security_precision.py: Commands.test_a_row_looked_up_by_the_value_is_the_databases
#[test]
fn a_row_looked_up_by_the_value_is_the_databases() {
    let got = run(
        vec![routes("def home():\n    user = User.query.filter_by(name=request.args['n']).first()\n    os.system('du -sh ' + user.home)\n")],
        "",
    );
    assert_eq!(got, vec![]);
}

const QT: &str = "import subprocess, sys\nfrom PyQt6.QtNetwork import QNetworkRequest\nfrom PyQt6.QtWidgets import QFileDialog\n\n\n\
class Viewer:\n    def fetch(self, url):\n        request = QNetworkRequest(url)\n\
\x20       open(request.url().toString())\n        subprocess.run(request.headers, shell=True)\n\n\
\x20   def pick(self):\n        path, _ = QFileDialog.getOpenFileName(self, 'Open')\n\
\x20       subprocess.run(['xdg-open', path])\n        with open(path) as f:\n            return f.read()\n\n\
\x20   def script(self):\n        name, _ = QFileDialog.getOpenFileName(self, 'Script')\n\
\x20       subprocess.run(f'python3 {name}', shell=True)\n";

// test_security_precision.py: SourceScope.test_desktop_input_is_its_own_lower_source
#[test]
fn desktop_input_is_its_own_lower_source() {
    let got = findings(&[("tools/viewer.py", QT)], "");
    let at: Vec<(String, String, String, usize)> = got.iter().map(|g| (g.0.clone(), g.1.clone(), g.2.clone(), g.3)).collect();
    assert_eq!(at, vec![("python-desktop-input".into(), "warn".into(), "tools/viewer.py".into(), 20)], "{got:?}");
}

// test_security_precision.py: SourceScope.test_a_helper_a_route_reaches_still_reads_the_request
#[test]
fn a_helper_a_route_reaches_still_reads_the_request() {
    let util = "import os\n\n\ndef run_it():\n    os.system('ping ' + request.args['h'])\n";
    let got = run(
        vec![("app/util.py".into(), util.into()), routes("from app.util import run_it\n\n\ndef ping():\n    run_it()\n")],
        "",
    );
    assert_eq!(
        got.iter().map(|g| (g.0.as_str(), g.2.as_str())).collect::<Vec<_>>(),
        vec![("python-injection", "app/util.py")],
        "{got:?}"
    );
}

// test_security_precision.py: JavaScript.test_a_relative_fetch_is_same_origin
#[test]
fn a_relative_fetch_is_same_origin() {
    let js = "const express = require('express');\nconst app = express();\n\
              app.get('/a', async (req, res) => {\n  const r = await fetch(`/api/posts?page=${req.query.page}`);\n\
              \x20 res.send(await r.text());\n});\n\
              app.get('/b', async (req, res) => {\n  const r = await fetch(req.query.url);\n  res.send(await r.text());\n});\n\
              app.get('/c', async (req, res) => {\n  const r = await fetch('//' + req.query.host + '/x');\n  res.send(r);\n});\n";
    let got = findings(&[("server.js", js)], "");
    assert_eq!(
        got.iter().map(|g| (g.0.as_str(), g.1.as_str(), g.3)).collect::<Vec<_>>(),
        vec![("js-injection", "error", 8), ("js-injection", "error", 12)],
        "{got:?}"
    );
}

/// (severity, file, line) of the security findings on one of the Python suite's fixtures
/// (tests/fixtures/NAME, copied so nothing is written beside it).
fn fixture_findings(name: &str) -> Vec<(String, String, usize)> {
    std::env::set_var("SEAMCHECK_RS_QUIET", "1");
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures").join(name);
    let d = tempfile::tempdir().unwrap();
    copy(&src, d.path());
    let p = Project::load(d.path(), &[]);
    let mut got: Vec<(String, String, usize)> = run_rules(&p, None, None)
        .into_iter()
        .filter(|f| f.rule.starts_with("security/"))
        .map(|f| {
            if f.message.contains("reaches") {
                let (file, line) = sink_at(&f.message);
                (f.severity, file, line)
            } else {
                (f.severity, f.file, f.line)
            }
        })
        .collect();
    got.sort();
    got
}

// test_security_precision.py: TheAppShape.test_only_the_injections_are_errors
#[test]
fn only_the_injections_are_errors() {
    let got = fixture_findings("flask_sqla");
    let mut want: Vec<(String, String, usize)> =
        [58, 76, 106, 120, 144].iter().map(|ln| ("error".to_string(), "app/main/routes.py".to_string(), *ln)).collect();
    want.extend([
        ("error".into(), "run.py".into(), 6),
        ("error".into(), "server.js".into(), 11),
        ("error".into(), "server.js".into(), 16),
        ("warn".into(), "app/main/routes.py".into(), 113),
        ("warn".into(), "tools/viewer.py".into(), 31),
    ]);
    assert_eq!(got, want);
}

fn copy(from: &std::path::Path, to: &std::path::Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
            copy(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

// ---- test_security_round2.py: the second field round's rule-level classes ----

const HEAD2: &str = "import io, os, re, subprocess, sqlite3\nimport requests\n\
from flask import redirect, request, send_file, url_for\nfrom app import db\n\n\n";

fn py(body: &str) -> (String, String) {
    ("app/routes.py".into(), format!("{HEAD2}{body}"))
}

// test_security_round2.py: SanitizedReturns.test_a_helper_whose_return_is_sanitized_is_clean_for_its_callers
#[test]
fn a_helper_whose_return_is_sanitized_is_clean_for_its_callers() {
    let safety = "def coerce_same_origin_redirect_target(t, default, host):\n    return t if t.startswith('/') else default\n\n\n\
                  def mode_url_for(endpoint, **kw):\n    return url_for(endpoint, **kw)\n";
    let body = "from app.safety import coerce_same_origin_redirect_target, mode_url_for\n\n\n\
                def _ret():\n    t = request.form.get('return_to') or request.args.get('return_to')\n\
                \x20   return coerce_same_origin_redirect_target(t, None, request.host_url)\n\n\n\
                def _panel_target(pid, return_to_url=None):\n    return mode_url_for('panel', pid=pid, next=return_to_url)\n\n\n\
                def panel(pid):\n    return redirect(_panel_target(pid, return_to_url=_ret()))\n\n\n\
                def panel2(pid):\n    target = _panel_target(pid, return_to_url=_ret())\n    return redirect(target)\n\n\n\
                def raw_next():\n    return request.args.get('next')\n\n\n\
                def bad():\n    return redirect(raw_next())\n";
    let got = run(
        vec![("app/safety.py".into(), safety.into()), py(body)],
        "[security]\nsanitizers = [\"coerce_same_origin_redirect_target\"]\n",
    );
    // only bad(): its helper returns the raw value
    assert_eq!(sev_line(&got), vec![("error".into(), 33)], "{got:?}");
}

// test_security_round2.py: BufferReturns.test_a_helper_that_returns_a_buffer_is_no_path
#[test]
fn a_helper_that_returns_a_buffer_is_no_path() {
    let body = "def _csv_bytes(rows) -> io.BytesIO:\n    return io.BytesIO(b''.join(rows))\n\n\n\
                def _zip_bytes(rows):\n    buf = io.BytesIO()\n    buf.write(rows)\n    return buf\n\n\n\
                def export():\n    rows = request.args.getlist('r')\n    buffer = _csv_bytes(rows)\n\
                \x20   send_file(buffer, mimetype='text/csv')\n    return send_file(_zip_bytes(rows), mimetype='application/zip')\n\n\n\
                def _path(name):\n    return os.path.join('/srv', name)\n\n\n\
                def raw():\n    return send_file(_path(request.args['f']))\n";
    let got = run(vec![py(body)], "");
    assert_eq!(sev_line(&got), vec![("error".into(), 29)], "{got:?}");
}

// test_security_round2.py: CheckedValues.test_an_int_in_range_an_anchored_match_and_a_literal_host
#[test]
fn an_int_in_range_an_anchored_match_and_a_literal_host() {
    let body = "_STATE_RE = re.compile(r'^[A-Za-z0-9_-]{16,128}$')\n_LOOSE = re.compile(r'[a-z]+')\n\n\n\
                def callback():\n    port = int(request.args.get('port', ''))\n    if port < 1024 or port > 65535:\n\
                \x20       return 'bad', 400\n    state = request.args.get('state', '')\n    if not _STATE_RE.match(state):\n\
                \x20       return 'bad', 400\n    query = request.query_string.decode()\n\
                \x20   subprocess.run(f'notify {state}', shell=True)\n\
                \x20   return redirect(f'http://127.0.0.1:{port}/callback?{query}')\n\n\n\
                def loose():\n    s = request.args.get('s', '')\n    if not _LOOSE.match(s):\n        return 'bad', 400\n\
                \x20   subprocess.run(f'notify {s}', shell=True)\n\n\n\
                def hostile():\n    return redirect(f\"https://{request.args['h']}/x\")\n";
    let got = run(vec![py(body)], "");
    assert_eq!(sev_line(&got), vec![("error".into(), 27), ("error".into(), 31)], "{got:?}");
}

// test_security_round2.py: Constants.test_a_loop_over_a_literal_list_is_constant
#[test]
fn a_loop_over_a_literal_list_is_constant() {
    let body = "def dump():\n    keep = request.args.get('keep')\n    tables = ['users', 'posts', 'tags']\n\
                \x20   for table_name in tables:\n        db.session.execute(db.text(f'SELECT * FROM {table_name}'))\n\
                \x20   for t in request.args.getlist('t'):\n        db.session.execute(db.text(f'SELECT * FROM {t}'))\n\
                \x20   return keep\n";
    let got = run(vec![py(body)], "");
    assert_eq!(sev_line(&got), vec![("error".into(), 13)], "{got:?}");
}

// test_security_round2.py: Constants.test_a_client_whose_callers_pass_constant_sql
#[test]
fn a_client_whose_callers_pass_constant_sql() {
    let client = "import sqlite3\nSQL_ONE = 'SELECT * FROM u WHERE id = ?'\n\n\nclass Db:\n\
                  \x20   def query(self, sql, params=None):\n        cur = self.conn.cursor()\n\
                  \x20       cur.execute(sql, params or ())\n        return cur.fetchall()\n";
    let body = "from app.client import Db, SQL_ONE\n\n\ndef one():\n    d = Db()\n    uid = request.args['id']\n\
                \x20   d.query(SQL_ONE, (uid,))\n    ids = request.args.getlist('ids')\n\
                \x20   marks = ','.join('?' for _ in ids)\n    d.query(f'SELECT * FROM u WHERE id IN ({marks})', ids)\n\
                \x20   d.query('SELECT * FROM u WHERE id = ?', params=(uid,))\n    d.query('SELECT 1 WHERE ' + request.args['w'])\n";
    let got = run(vec![("app/client.py".into(), client.into()), py(body)], "");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].4.contains("one -> query"), "{got:?}");
}

// test_security_round2.py: Kept.test_a_request_controlled_host_is_still_an_error
#[test]
fn a_request_controlled_host_is_still_an_error() {
    let body = "def version():\n    url = request.get_json()['url']\n\
                \x20   return requests.get(f'{url}/api/v1/version', timeout=5).text\n";
    assert_eq!(sev_line(&run(vec![py(body)], "")), vec![("error".into(), 9)]);
}

// test_security_round2.py: JsFetchHelper.test_a_parameter_its_callers_fill_with_same_origin_paths_is_clean
#[test]
fn a_parameter_its_callers_fill_with_same_origin_paths_is_clean() {
    let js = "const express = require('express');\nconst app = express();\n\
              async function requestJson(url, options) {\n  const r = await fetch(url, options || {});\n  return r.json();\n}\n\
              app.post('/chat', async (req, res) => {\n  const body = req.body;\n\
              \x20 const a = await requestJson('/chat/api/send', { method: 'POST', body: JSON.stringify(body) });\n\
              \x20 const b = await requestJson(`/chat/api/thread/${body.id}`, {});\n  res.send([a, b]);\n});\n\
              async function relay(url) {\n  return fetch(url);\n}\n\
              app.get('/relay', async (req, res) => {\n  res.send(await relay(req.query.u));\n});\n";
    let got = findings(&[("server.js", js)], "");
    assert_eq!(
        got.iter().map(|g| (g.0.as_str(), g.1.as_str(), g.3)).collect::<Vec<_>>(),
        vec![("js-injection", "error", 14)],
        "{got:?}"
    );
}

// ---- test_security_round3.py / test_security_round4.py: the rule-level classes (generated from the
// Python tests' own inputs and findings by the security lane; each names the test it ports) ----

// test_security_round3.py: FilledBuffers.test_a_buffer_written_to_is_still_a_buffer
#[test]
fn round3_a_buffer_written_to_is_still_a_buffer() {
    let files: &[(&str, &str)] = &[
        ("app/pdf.py", "import io\nfrom flask import render_template\n\n\ndef render_pdf(template_name, **context):\n    html = render_template(template_name, **context)\n    buf = io.BytesIO()\n    buf.write(html.encode())\n    buf.seek(0)\n    return buf\n"),
        ("app/routes.py", "import io, os\nfrom io import BytesIO\nfrom pathlib import Path\nfrom urllib.parse import urlencode\nfrom sqlalchemy import text\nfrom flask import redirect, request, send_file, url_for\nfrom app import db\n\n\nfrom app.pdf import render_pdf\n\n\ndef invoice():\n    inv = request.args.get('id')\n    pdf_buffer = render_pdf('invoice.html', inv=inv)\n    return send_file(pdf_buffer, mimetype='application/pdf', download_name=f'inv-{inv}.pdf')\n\n\ndef notes():\n    data = request.args['d'].encode()\n    buffer = io.BytesIO()\n    buffer.write(data)\n    buffer.seek(0)\n    return send_file(buffer, mimetype='text/plain')\n\n\ndef rows():\n    buffer = BytesIO()\n    for r in request.args.getlist('r'):\n        buffer.write(r.encode())\n    return send_file(buffer, as_attachment=True)\n\n\ndef swapped():\n    buffer = io.BytesIO()\n    buffer = request.args['p']\n    return send_file(buffer)\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
        ("python-injection".into(), "error".into(), "app/routes.py".into(), 37),
    ];
    assert_eq!(got, want);
}

// test_security_round3.py: ConfinedPaths.test_a_path_a_method_checks_under_its_roots
#[test]
fn round3_a_path_a_method_checks_under_its_roots() {
    let files: &[(&str, &str)] = &[
        ("app/ctl.py", "from pathlib import Path\n\n\nclass HostController:\n    roots = [Path('/srv/a'), Path('/srv/b')]\n\n    def _is_allowed_path(self, path):\n        resolved = Path(path).resolve()\n        for root in self.roots:\n            try:\n                resolved.relative_to(root)\n                return resolved\n            except ValueError:\n                continue\n        raise PermissionError(path)\n\n    def resolve_managed_path(self, raw, must_exist=False):\n        path = self._is_allowed_path(raw)\n        if must_exist and not path.exists():\n            raise FileNotFoundError(raw)\n        return path\n\n    def loose(self, raw):\n        return Path(raw).resolve()\n\n\nhost_controller = HostController()\n"),
        ("app/routes.py", "import io, os\nfrom io import BytesIO\nfrom pathlib import Path\nfrom urllib.parse import urlencode\nfrom sqlalchemy import text\nfrom flask import redirect, request, send_file, url_for\nfrom app import db\n\n\nfrom app.ctl import host_controller\n\n\ndef download():\n    path = host_controller.resolve_managed_path(request.args['path'], must_exist=True)\n    return send_file(path)\n\n\ndef direct():\n    return send_file(host_controller.resolve_managed_path(request.args['path']))\n\n\ndef resolved_only():\n    return send_file(host_controller.loose(request.args['path']))\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
        ("python-injection".into(), "error".into(), "app/routes.py".into(), 23),
    ];
    assert_eq!(got, want);
}

// test_security_round3.py: Redirects.test_a_same_site_url_helper_with_the_query_after_it
#[test]
fn round3_a_same_site_url_helper_with_the_query_after_it() {
    let files: &[(&str, &str)] = &[
        ("app/routes.py", "import io, os\nfrom io import BytesIO\nfrom pathlib import Path\nfrom urllib.parse import urlencode\nfrom sqlalchemy import text\nfrom flask import redirect, request, send_file, url_for\nfrom app import db\n\n\ndef mode_query_url(endpoint, include_request_args=False, **values):\n    q = request.args.to_dict(flat=False) if include_request_args else {}\n    q.pop('mode', None)\n    return url_for(endpoint, **values) + '?' + urlencode(q, doseq=True)\n\n\ndef index():\n    return redirect(mode_query_url('x.index', include_request_args=True, view='y'))\n\n\ndef index2():\n    target = _go()\n    return redirect(target)\n\n\ndef _go():\n    return mode_query_url('x.index', include_request_args=True)\n\n\ndef query_host():\n    return request.args['h'] + '?' + urlencode(request.args)\n\n\ndef bad():\n    return redirect(query_host())\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
        ("python-injection".into(), "error".into(), "app/routes.py".into(), 34),
    ];
    assert_eq!(got, want);
}

// test_security_round3.py: Redirects.test_back_to_the_referer_is_a_warning
#[test]
fn round3_back_to_the_referer_is_a_warning() {
    let files: &[(&str, &str)] = &[
        ("app/safety.py", "def safe_target(t):\n    return t if t and t.startswith('/') else '/'\n"),
        ("app/routes.py", "import io, os\nfrom io import BytesIO\nfrom pathlib import Path\nfrom urllib.parse import urlencode\nfrom sqlalchemy import text\nfrom flask import redirect, request, send_file, url_for\nfrom app import db\n\n\nfrom app.safety import safe_target\n\n\ndef back():\n    return redirect(request.referrer or url_for('index'))\n\n\ndef back2():\n    ref = request.headers.get('Referer')\n    return redirect(ref or '/')\n\n\ndef checked():\n    return redirect(safe_target(request.referrer))\n\n\ndef nxt():\n    return redirect(request.args.get('next') or request.referrer)\n\n\ndef real():\n    return redirect(request.form.get('return_to'))\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "[security]\nsanitizers = [\"safe_target\"]\n").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
        ("python-injection".into(), "error".into(), "app/routes.py".into(), 27),
        ("python-injection".into(), "error".into(), "app/routes.py".into(), 31),
        ("python-injection".into(), "warn".into(), "app/routes.py".into(), 14),
        ("python-injection".into(), "warn".into(), "app/routes.py".into(), 19),
    ];
    assert_eq!(got, want);
}

// test_security_round3.py: Constants.test_a_literal_list_loop_nested_in_try_and_with
#[test]
fn round3_a_literal_list_loop_nested_in_try_and_with() {
    let files: &[(&str, &str)] = &[
        ("app/routes.py", "import io, os\nfrom io import BytesIO\nfrom pathlib import Path\nfrom urllib.parse import urlencode\nfrom sqlalchemy import text\nfrom flask import redirect, request, send_file, url_for\nfrom app import db\n\n\nfrom zipfile import ZipFile\n\n\ndef dump():\n    fmt = request.args.get('fmt', 'csv')\n    tables: list[str] = [\n        'customer',  # people\n        'department',\n        'fuel_surcharge',\n    ]\n    buf = BytesIO()\n    try:\n        with ZipFile(\n            buf, 'w'\n        ) as zf:\n            for table_name in tables:\n                rows = db.session.execute(\n                    db.text(\n                        f\"\"\"SELECT *\n                        FROM {table_name}\"\"\"\n                    )\n                ).mappings().all()\n                zf.writestr(f'{table_name}.{fmt}', str(rows))\n    except Exception:\n        return 'bad', 500\n    return send_file(buf, download_name=f'dump.{fmt}')\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
    ];
    assert_eq!(got, want);
}

// test_security_round3.py: Constants.test_constant_sql_shapes_through_a_fetch_helper
#[test]
fn round3_constant_sql_shapes_through_a_fetch_helper() {
    let files: &[(&str, &str)] = &[
        ("app/routes.py", "import io, os\nfrom io import BytesIO\nfrom pathlib import Path\nfrom urllib.parse import urlencode\nfrom sqlalchemy import text\nfrom flask import redirect, request, send_file, url_for\nfrom app import db\n\n\ndef _header_sql_select() -> str:\n    return \"\"\"a, b, c\"\"\"\n\n\ndef b2w_fetchall(sql, params, conn=None):\n    cur = conn.cursor()\n    cur.execute(sql, params)\n    return cur.fetchall()\n\n\ndef lookup():\n    exact = request.args.getlist('x')\n    placeholders = ','.join(['%s'] * len(exact))\n    header_from = f'FROM header h'\n    order_sql = 'ORDER BY a' if request.args.get('o') else ''\n    sql = f\"\"\"SELECT {_header_sql_select()} {header_from} WHERE x IN ({placeholders}) {order_sql}\"\"\"\n    return b2w_fetchall(sql, tuple(exact), conn=db)\n\n\ndef purge():\n    conditions, params = [], {}\n    if request.form.get('wo'):\n        conditions.append('work_order_id = :wo')\n        params['wo'] = request.form['wo']\n    db.session.execute(text(f\"DELETE FROM invoice_work_orders WHERE {' AND '.join(conditions)}\"), params)\n\n\ndef bad():\n    c = request.args['c']\n    return b2w_fetchall(f'SELECT * FROM t WHERE {c}', (), conn=db)\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
        ("python-injection".into(), "error".into(), "app/routes.py".into(), 16),
    ];
    assert_eq!(got, want);
}

// test_security_round3.py: JsIife.test_a_fetch_helper_inside_an_iife_called_from_nested_closures
#[test]
fn round3_a_fetch_helper_inside_an_iife_called_from_nested_closures() {
    let files: &[(&str, &str)] = &[
        ("static/chat.js", "(function () {\n  async function requestJson(url, options) {\n    const r = await fetch(url, options || {});\n    return r.json();\n  }\n  function bind(root, req) {\n    root.querySelectorAll('[data-thread]').forEach((el) => {\n      el.addEventListener('click', async () => {\n        await requestJson(`/chat/api/${el.dataset.thread}/messages`, { method: 'GET' });\n      });\n    });\n    root.addEventListener('submit', async () => {\n      await requestJson('/chat/api/send', { method: 'POST', body: JSON.stringify(req.body) });\n    });\n  }\n  bind(document.body, window.state);\n})();\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
    ];
    assert_eq!(got, want);
}

// test_security_round4.py: Stores.test_a_session_key_written_with_request_data
#[test]
fn round4_a_session_key_written_with_request_data() {
    let files: &[(&str, &str)] = &[
        ("app/routes.py", "import json\nfrom flask import request, session\nfrom sqlalchemy import text\nfrom app import db\n\n\ndef keep():\n    session['note'] = json.dumps(request.get_json())\n    session['who'] = 'x'\n    return 'ok'\n\n\ndef use():\n    note = json.loads(session['note'])\n    db.session.execute(text(f\"SELECT {note['c']} FROM t\"))\n\n\ndef fine():\n    who = session.get('who')\n    db.session.execute(text(f\"SELECT * FROM {who}\"))\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
        ("python-injection".into(), "error".into(), "app/routes.py".into(), 15),
    ];
    assert_eq!(got, want);
}

// test_security_round3.py: TheAppShape.test_only_the_injections_are_errors
#[test]
fn round3_only_the_injections_are_errors() {
    let got = fixture_findings("flask_round3");
    let want: Vec<(String, String, usize)> = vec![
        ("error".into(), "app/main/files.py".into(), 40),
        ("error".into(), "app/main/files.py".into(), 55),
        ("warn".into(), "app/main/files.py".into(), 50),
    ];
    assert_eq!(got, want);
}

// test_security_round4.py: TheAppShape.test_only_the_injections_are_errors
#[test]
fn round4_only_the_injections_are_errors() {
    let got = fixture_findings("flask_round4");
    let want: Vec<(String, String, usize)> = vec![
        ("error".into(), "app/tp.py".into(), 19),
        ("error".into(), "app/tp.py".into(), 37),
        ("error".into(), "app/tp.py".into(), 40),
    ];
    assert_eq!(got, want);
}


// ---- test_security_round5.py: the rule-level classes (generated as above) ----

// test_security_round5.py: LocalImports.test_an_import_inside_the_function_wins_there
#[test]
fn round5_an_import_inside_the_function_wins_there() {
    let files: &[(&str, &str)] = &[
        ("app/pdf.py", "import io\n\n\ndef render_pdf(name, **kw):\n    buf = io.BytesIO()\n    buf.write(name.encode())\n    return buf\n"),
        ("app/help.py", "def render_pdf(guide):\n    return open(guide).read()\n"),
        ("app/routes.py", "from flask import request, send_file\n\n\ndef report():\n    from app.pdf import render_pdf\n\n    out = render_pdf('r.html', q=request.args.get('q'))\n    return send_file(out)\n\n\ndef guide():\n    from app.help import render_pdf\n\n    return render_pdf('g')\n"),
    ];
    let mut got: Vec<(String, String, String, usize)> = findings(files, "").into_iter().map(|g| (g.0, g.1, g.2, g.3)).collect();
    got.sort();
    let want: Vec<(String, String, String, usize)> = vec![
    ];
    assert_eq!(got, want);
}

// test_security_round5.py: TheAppShape.test_only_the_injections_are_errors
#[test]
fn round5_only_the_injections_are_errors() {
    let got = fixture_findings("flask_round5");
    let want: Vec<(String, String, usize)> = vec![
        ("error".into(), "app/inventory/routes.py".into(), 62),
        ("error".into(), "app/mail/attach.py".into(), 50),
        ("error".into(), "app/ops/git.py".into(), 22),
        ("warn".into(), "app/mail/attach.py".into(), 20),
        ("warn".into(), "app/ops/git.py".into(), 11),
    ];
    assert_eq!(got, want);
}

