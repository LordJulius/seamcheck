//! The security pack's taint rules end to end (kind = "path" calling
//! rules::security), ported from test_security_precision.py's rule-level
//! classes (TextHelpers is ported in src/rules/security.rs).

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

// test_security_precision.py: TheAppShape.test_only_the_injections_are_errors
#[test]
fn only_the_injections_are_errors() {
    std::env::set_var("SEAMCHECK_RS_QUIET", "1");
    // (the Python suite's tests/fixtures/flask_sqla, copied so nothing is written beside it)
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/flask_sqla");
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
