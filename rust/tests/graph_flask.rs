//! Flask-SQLAlchemy and Flask-Login values in the call graph, ported from
//! tests/test_flask_types.py.

mod common;

use common::write;
use seamcheck::graph::Graph;
use seamcheck::project::Project;

const MODELS: &str = "from flask_login import UserMixin\nfrom app import db\n\n\nclass User(UserMixin, db.Model):\n    def avatar(self):\n        return 1\n\n    def get(self):\n        return 2\n\n\nclass Post(db.Model):\n    def summary(self):\n        return 3\n\n\nclass Plain:\n    def summary(self):\n        return 4\n";
const ROUTES: &str = "from flask import Blueprint\nfrom flask_login import current_user\nfrom sqlalchemy import select\nfrom app import db\nfrom app.models import Plain, Post, User\n\nbp = Blueprint('main', __name__)\n\n\n@bp.route('/u/<name>')\ndef user(name):\n    u = User.query.filter_by(name=name).first_or_404()\n    u.avatar()\n    p = db.session.get(Post, 1)\n    p.summary()\n    v = db.session.scalar(select(User).where(User.name == name))\n    v.avatar()\n    current_user.avatar()\n    for w in User.query.all():\n        w.avatar()\n    User.query.get(1)\n    q = Plain.query\n    return q\n";

fn edges() -> Vec<(usize, String, bool)> {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        &[
            ("app/__init__.py", "from flask_sqlalchemy import SQLAlchemy\n\ndb = SQLAlchemy()\n"),
            ("app/models.py", MODELS),
            ("app/routes.py", ROUTES),
        ],
    );
    let p = Project::load(d.path(), &[]);
    let g = Graph::load(&p);
    let mut v: Vec<(usize, String, bool)> = g
        .edges
        .get("app/routes.user")
        .map(|es| es.iter().map(|(t, ln, ex)| (*ln, t.split_once('.').unwrap().1.to_string(), *ex)).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// test_flask_types.FlaskTypes.test_queries_lookups_and_the_login_proxy_are_typed
#[test]
fn queries_lookups_and_the_login_proxy_are_typed() {
    let got = edges();
    for (ln, target) in [(13, "User.avatar"), (15, "Post.summary"), (17, "User.avatar"), (18, "User.avatar")] {
        assert!(got.contains(&(ln, target.to_string(), true)), "{ln} {target}: {got:?}");
    }
}

/// test_flask_types.FlaskTypes.test_a_querys_own_methods_are_the_librarys
#[test]
fn a_querys_own_methods_are_the_librarys() {
    assert!(edges().iter().all(|e| e.0 != 21), "User.query.get is not User.get");
}
