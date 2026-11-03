"""Flask-SQLAlchemy and Flask-Login values in the call graph (python3 -m unittest discover tests).

A Flask app's project calls were 70% exact on a field run: `User.query...first()`,
`db.session.get(User, pk)` and `current_user` were values nothing could type, so
every call on a user was a name guess. Model.query is a query of that model, a
lookup that names the model gives one, Flask-Login's proxy is the UserMixin class;
a blueprint's `@bp.route` handler is an entry point, not dead code.
"""
import os
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, tidy  # noqa: E402

MODELS = ("from flask_login import UserMixin\nfrom app import db\n\n\nclass User(UserMixin, db.Model):\n"
          "    def avatar(self):\n        return 1\n\n    def get(self):\n        return 2\n\n\n"
          "class Post(db.Model):\n    def summary(self):\n        return 3\n\n\nclass Plain:\n    def summary(self):\n        return 4\n")
ROUTES = ("from flask import Blueprint\nfrom flask_login import current_user\nfrom sqlalchemy import select\n"
          "from app import db\nfrom app.models import Plain, Post, User\n\nbp = Blueprint('main', __name__)\n\n\n"
          "@bp.route('/u/<name>')\ndef user(name):\n    u = User.query.filter_by(name=name).first_or_404()\n    u.avatar()\n"
          "    p = db.session.get(Post, 1)\n    p.summary()\n    v = db.session.scalar(select(User).where(User.name == name))\n"
          "    v.avatar()\n    current_user.avatar()\n    for w in User.query.all():\n        w.avatar()\n"
          "    User.query.get(1)\n    q = Plain.query\n    return q\n")


def load(files: dict):
    root = tempfile.mkdtemp()
    for name, body in files.items():
        path = os.path.join(root, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as fh:
            fh.write(textwrap.dedent(body))
    return core.load(root, use_cache=False)


class FlaskTypes(unittest.TestCase):
    def setUp(self):
        self.p = load({"app/__init__.py": "from flask_sqlalchemy import SQLAlchemy\n\ndb = SQLAlchemy()\n",
                       "app/models.py": MODELS, "app/routes.py": ROUTES})

    def edges(self):
        return sorted((ln, d.split(".", 1)[1], ex) for d, ln, ex in self.p.edges.get("app/routes.user", []))

    def test_queries_lookups_and_the_login_proxy_are_typed(self):
        got = self.edges()
        for ln, target in ((13, "User.avatar"), (15, "Post.summary"), (17, "User.avatar"), (18, "User.avatar")):
            self.assertIn((ln, target, True), got, got)

    def test_a_querys_own_methods_are_the_librarys(self):
        self.assertFalse([e for e in self.edges() if e[0] == 21], "User.query.get is not User.get")

    def test_a_blueprint_route_is_an_entry_point(self):
        self.assertTrue(tidy.is_entry(self.p, self.p.funcs["app/routes.user"]))


if __name__ == "__main__":
    unittest.main()
