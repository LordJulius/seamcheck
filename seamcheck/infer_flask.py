"""Flask-SQLAlchemy and Flask-Login values the type walk can name (infer.Types).

`User.query` on a Flask-SQLAlchemy model (`class User(db.Model)`) is a query of
Users: its builders (`filter_by()`, `order_by()`, `all()`) keep it one, its
one-row readers (`first()`, `get()`, `first_or_404()`) give a User, anything
else is the library's. It travels as an EXT value tagged with the model
(ROWS), so every other consumer of a value treats it as the library's.
Flask-Login's `current_user` is the project's one `UserMixin` class.
"""
from __future__ import annotations

import re

from .model import PY_EXTS

ROWS = "\0rows"
_BUILDERS = {"filter()", "filter_by()", "order_by()", "group_by()", "having()", "join()", "outerjoin()", "options()",
             "limit()", "offset()", "distinct()", "where()", "select_from()", "params()", "execution_options()",
             "with_for_update()", "populate_existing()", "yield_per()", "enable_eagerloads()", "all()"}
_ONE = {"first()", "one()", "one_or_none()", "get()", "get_or_404()", "first_or_404()", "one_or_404()", "scalar()"}
_MODEL_BASE = re.compile(r"^(?:[A-Za-z_]\w*\.)?Model$")
_CURRENT_USER = re.compile(r"^\s*from\s+flask_login\s+import\s+[^\n]*\bcurrent_user\b")


def rows(mods) -> tuple:
    return ("ext", (ROWS,) + tuple(mods), True)


def is_rows(v) -> bool:
    return v is not None and v[0] == "ext" and v[1][:1] == (ROWS,)


def step(v, seg: str, ext):
    """A segment after a model's query."""
    mods = v[1][1:]
    if seg in _ONE:
        return ("inst", mods, len(mods) == 1)
    return v if seg in _BUILDERS else ext


def class_bases(m) -> list[str]:
    """A Python class module's supertypes as its `class X(...)` line writes them (the linker
    keeps only the project's)."""
    if not m.decls or not m.file.endswith(PY_EXTS):
        return []
    head = next((ln for ln in m.raw if re.match(rf"\s*class\s+{re.escape(m.decls[0])}\s*\(", ln)), "")
    bm = re.match(r"\s*class\s+\w+\s*\(([^)]*)\)", head)
    return [x.strip() for x in bm.group(1).split(",") if x.strip() and "=" not in x] if bm else []


def model(t, mods) -> bool:
    """Any of these classes is a Flask-SQLAlchemy model (a `db.Model` among its supertypes)."""
    return any(_MODEL_BASE.match(b) for mn in mods for a in t.ancestors(mn) for b in class_bases(t.p.modules[a]))


def login_user(t, fm):
    """`current_user` imported from flask_login: an instance of the project's one UserMixin class."""
    if not any(_CURRENT_USER.match(ln) for ln in fm.raw):
        return None
    key = "\0flask_user"
    if key not in t.cache:
        users = [mn for mn, m in t.p.modules.items() if m.owner and any(b.split(".")[-1] == "UserMixin" for b in class_bases(m))]
        t.cache[key] = ("inst", (users[0],), True) if len(users) == 1 else None
    return t.cache[key]
