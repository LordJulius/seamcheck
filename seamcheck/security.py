"""What a taint `path` rule's sink, source and sanitizers mean (the security pack).

`arch.path` finds a sink a request value reaches; this module says what that
sink does with which argument, so the value counts only where it does harm:

- `sql_sinks`: the first argument is SQL text. It is injection only when that
  text is built from the value (an f-string, `%`, `+`, `.format`, `text()` of
  one), directly or through the names it was assigned to. A query object
  (`select(User).where(User.name == q)`, `User.query`) or bound parameters
  (`text("... :q"), {"q": q}`) never are.
- `url_sinks`: the first argument (or `url=`) is a URL. The value in its scheme
  or host is request forgery (the rule's severity); a host the code fixes (a
  constant, a config value) with the value only in the path or the parameters
  is `url_severity` (warn); a relative, same-origin URL (`"/api/x?q=" + q`) is
  nothing. Python: an ORM session's `get(Model, pk)` (`db.session` of `db =
  SQLAlchemy()`) is no request at all, and a project class that subclasses
  `requests.Session` / `httpx.Client` is one.
- `path_sinks`: only the first argument is a path, and an in-memory buffer
  (`BytesIO(...)`, a name assigned one) is none.
- `shell_sinks`: an argument list with a fixed program and no `shell=True`
  (`subprocess.run(["convert", src, out])`) runs no shell: the value in its
  other arguments is `argv_severity` (warn; "off" drops it).

`[security] sanitizers = ["coerce_safe_redirect_target"]` in seamcheck.toml
adds the project's own cleaning functions to every taint rule's `unless`.
`source_scope` (a regex over a file's text) keeps a source to the files that
use the framework that defines it (`from flask import request`) and to the
functions they reach: a desktop tool's `QNetworkRequest` named `request` is not
the web request.
"""
from __future__ import annotations

import re

from .model import PY_EXTS

_STR = r"""[rRbBuU]{0,2}(?:"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')"""
_FSTR = re.compile(r"""^[rRbB]?[fF][rRbB]?(["'])(.*)\1$""", re.S)
_PLAIN = re.compile(r"""^[rRbBuU]{0,2}(["'])((?:(?!\1)[^\\]|\\.)*)\1$""", re.S)
_TEMPLATE = re.compile(r"^`(.*)`$", re.S)
_SAME_SITE = re.compile(r"""^\s*[fbru]{0,2}["'`]/(?![/\\])""", re.I)
# a query object, never SQL text: SQLAlchemy's builders, a model's query, Django's manager
_QUERY_OBJECT = re.compile(r"(?:[A-Za-z_]\w*\.)*(?:select|insert|update|delete|union|union_all|exists|values|"
                           r"lateral|intersect|except_)\s*\(|[A-Z]\w*\.(?:query|objects|__table__)\b")
_TEXT_CALL = re.compile(r"(?:[A-Za-z_]\w*\.)*(?:text|literal_column|SQL)\s*\((.*)\)\s*$", re.S)
_BUFFER = re.compile(r"(?:io\.)?(?:BytesIO|StringIO)\s*\(|(?:tempfile\.)?(?:SpooledTemporaryFile|TemporaryFile)\s*\(")
_SHELL_KW = re.compile(r"^\s*shell\s*=\s*(?!False\b|None\b|0\b)")
_PROGRAM = re.compile(r"""^\s*(?:[rRbBuU]{0,2}(?:"[^"$\\{]*"|'[^'$\\{]*')|sys\.executable|shutil\.which\(\s*""" + _STR + r"\s*\))\s*$")
_URL_KWS = ("url", "uri", "endpoint")
_PATH_KWS = ("path_or_file", "filename_or_fp", "file", "path", "filename")
_SQL_KWS = ("sql", "statement", "query", "stmt")
# an ORM session built by hand (`Session(engine)`, `sessionmaker(bind=e)()`)
_ORM_RHS = re.compile(r"^(?:[A-Za-z_]\w*\.)?(?:Session|sessionmaker|scoped_session)\s*\(")
_HTTP_BASES = re.compile(r"^(?:requests\.(?:sessions\.)?Session|httpx\.(?:Async)?Client|aiohttp\.ClientSession)$")
# (a bare `Session` / `Client` base counts when the file imports it from the HTTP library, not SQLAlchemy's)
_HTTP_IMPORT = re.compile(r"^\s*from\s+(?:requests|httpx|aiohttp)(?:\.\w+)*\s+import\s+.*\b(Session|AsyncClient|Client|ClientSession)\b")
_HTTP_RHS = re.compile(r"^\s*(?:requests\.(?:sessions\.)?Session|httpx\.(?:Async)?Client|aiohttp\.ClientSession)\s*\(")


# a check that lets only a safe value through (Python): `x.isdigit()`, `NAME.fullmatch(x)`,
# `re.fullmatch(p, x)`, `url_has_allowed_host_and_scheme(x, ...)`; with `not` before it the
# `if` turns the value away (clean after it), without it the block holds a checked value
_VALIDATORS = (r"(?P<n1>[A-Za-z_]\w*)\.(?:isdigit|isnumeric|isdecimal|isalnum|isidentifier)\(\s*\)",
               r"(?:re\.)?fullmatch\(\s*[^,()]+(?:\([^()]*\))?\s*,\s*(?P<n2>[A-Za-z_]\w*)\s*[,)]",
               r"(?<![\w.])(?!re\.)[A-Za-z_][\w.]*\.fullmatch\(\s*(?P<n3>[A-Za-z_]\w*)\s*[,)]",
               r"(?P<n5>[A-Za-z_]\w*)\.startswith\(",
               r"(?:url_has_allowed_host_and_scheme|is_safe_url|is_safe_redirect)\(\s*(?:url\s*=\s*)?(?P<n4>[A-Za-z_]\w*)\s*[,)]")
_VALIDATOR = re.compile("|".join(f"(?:{v})" for v in _VALIDATORS))
_NETLOC = re.compile(r"\b(?:urlsplit|urlparse)\(\s*([A-Za-z_]\w*)\s*\)\.netloc\b")
_ALLOWED = re.compile(r"(?<![\w.])([A-Za-z_]\w*)\s+(not\s+)?in\s+(?:[A-Z][A-Z0-9_]*\b|[\[({])")


def guarded(cond: str) -> list[tuple[str, bool]]:
    """The names a Python `if` condition checks: (name, inside), inside = the value is checked
    within the block only (`if x.isdigit():`), else from the `if` on (`if not x.isdigit(): return`,
    `if x not in ALLOWED: abort(400)`, `if urlsplit(x).netloc: x = "/"`)."""
    out = []
    for m in _VALIDATOR.finditer(cond):
        name = next(v for k, v in m.groupdict().items() if v)
        negated = bool(re.search(r"\bnot\s+$", cond[:m.start()]))
        out.append((name, not negated))
    out += [(m.group(1), False) for m in _NETLOC.finditer(cond)]
    for m in _ALLOWED.finditer(cond):
        if m.group(1) not in ("not", "and", "or", "is", "if"):
            out.append((m.group(1), not m.group(2)))
    seen, uniq = set(), []
    for name, inside in out:
        if name not in seen:
            seen.add(name)
            uniq.append((name, inside))
    return uniq


def sanitizers(project) -> list[str]:
    """`[security] sanitizers`: the project's own functions whose return value is clean."""
    got = (project.config.get("security", {}) or {}).get("sanitizers", [])
    return [s for s in got if isinstance(s, str) and s] if isinstance(got, list) else []


def scoped(project, starts: list, scope, depth: int) -> list:
    """The entry points a `source_scope` keeps: functions in files whose text matches it
    and the functions those reach by exact calls (a helper that reads the request)."""
    inside = {rel for rel in project.by_file if scope.search(_file_text(project, rel))}
    reach = {fid for fid, f in project.funcs.items() if f.file in inside}
    frontier = list(reach)
    for _ in range(depth):
        nxt = []
        for fid in frontier:
            for dst, _ln, exact in project.edges.get(fid, []):
                if exact and dst not in reach and dst in project.funcs:
                    reach.add(dst)
                    nxt.append(dst)
        frontier = nxt
    return [f for f in starts if f.id in reach]


def _file_text(project, rel: str) -> str:
    mods = project.by_file.get(rel) or []
    return "\n".join(mods[0].raw) if mods else ""


class Sinks:
    """A rule's categorised sinks and what each makes of a call's arguments."""

    def __init__(self, r: dict):
        def pats(key):
            v = r.get(key, [])
            return [re.compile(p) for p in (v if isinstance(v, list) else [v])]
        self.sql, self.url, self.path, self.shell = pats("sql_sinks"), pats("url_sinks"), pats("path_sinks"), pats("shell_sinks")
        self.lookups = re.compile(r["lookups"]) if r.get("lookups") else None
        self.severity = r.get("severity", "error")
        self.url_severity = r.get("url_severity", "warn")
        self.argv_severity = r.get("argv_severity", "warn")

    def kind(self, text: str) -> str | None:
        for k, ps in (("sql", self.sql), ("url", self.url), ("path", self.path), ("shell", self.shell)):
            if any(p.search(text) for p in ps):
                return k
        return None

    def any(self) -> bool:
        return bool(self.sql or self.url or self.path or self.shell)

    def lookup(self, rhs: str) -> bool:
        """An assignment from a database lookup holds what the database returned, not the value."""
        return bool(self.lookups and self.lookups.match(rhs.strip()))

    def verdict(self, kind: str, args: list[str], ctx) -> tuple[str, str] | None:
        """(severity, note) when the call's arguments carry the value the way this sink does
        harm with, else None. `ctx.dirty(text)`: the text carries the value at the call;
        `ctx.rhs(name)`: the latest assignment of a tainted name, a ctx for its line (None: a
        parameter or unknown)."""
        pos = [a for a in args if not _kwarg(a)]
        kws = {k: v for k, v in (_kwarg(a) for a in args if _kwarg(a))}
        if kind == "sql":
            first = next((kws[k] for k in _SQL_KWS if k in kws), pos[0] if pos else None)
            return (self.severity, "") if first is not None and sql_dirty(first, ctx) else None
        if kind == "path":
            first = next((kws[k] for k in _PATH_KWS if k in kws), pos[0] if pos else None)
            return (self.severity, "") if first is not None and path_dirty(first, ctx) else None
        if kind == "url":
            first = next((kws[k] for k in _URL_KWS if k in kws), pos[0] if pos else None)
            how = url_taint(first, ctx) if first is not None else "clean"
            if how == "relative":
                return None
            if how == "head":
                return self.severity, ""
            if how == "tail" or any(ctx.dirty(a) for a in args if a is not first):
                return (self.url_severity, " (a fixed host; request data in the path or parameters)") \
                    if self.url_severity != "off" else None
            return None
        if kind == "shell":
            shell = any(_SHELL_KW.match(a) for a in args)
            first = pos[0] if pos else (kws.get("args") or "")
            elems = None if shell else argv(first, ctx)
            if not shell and self.argv_severity == "off":
                return None  # (only a shell counts: the desktop user running the program they picked)
            if elems is None:
                return (self.severity, "") if first and ctx.dirty(first) else None
            if not elems or not _PROGRAM.match(elems[0]) and ctx.dirty(elems[0]):
                return (self.severity, "") if elems else None
            if any(ctx.dirty(e) for e in elems[1:]):
                return (self.argv_severity, " (an argument of a fixed program, no shell)") \
                    if self.argv_severity != "off" else None
            return None
        return None


def _kwarg(a: str):
    m = re.match(r"\s*([A-Za-z_]\w*)\s*=(?!=)\s*(.*)$", a, re.S)
    return (m.group(1), m.group(2)) if m else None


def _bare(a: str) -> str | None:
    a = a.strip()
    return a if re.fullmatch(r"[$@]?[A-Za-z_]\w*", a) else None


def _literal(a: str) -> bool:
    a = a.strip()
    return bool(_PLAIN.match(a)) and not re.search(r"#\{|\$\{", a)


def sql_dirty(a: str, ctx, depth: int = 0) -> bool:
    """The SQL text is built from the value (not a constant, a query object or a bound parameter)."""
    a = a.strip()
    if _literal(a) or not ctx.dirty(a):
        return False
    m = _TEXT_CALL.match(a)
    if m:  # `text("... :id")` is constant, `text(f"... {q}")` isn't
        inner = _split_args(m.group(1))
        return bool(inner) and sql_dirty(inner[0], ctx, depth + 1)
    if _QUERY_OBJECT.match(a):
        return False
    name = _bare(a)
    if name is not None:
        sub = ctx.rhs(name)
        return True if sub is None or depth > 6 else sql_dirty(sub[0], sub[1], depth + 1)
    return True


def path_dirty(a: str, ctx, depth: int = 0) -> bool:
    """The path argument carries the value (an in-memory buffer is no path)."""
    a = a.strip()
    if _BUFFER.match(a) or not ctx.dirty(a):
        return False
    name = _bare(a)
    if name is not None:
        sub = ctx.rhs(name)
        return True if sub is None or depth > 6 else path_dirty(sub[0], sub[1], depth + 1)
    return True


def argv(a: str, ctx, depth: int = 0) -> list[str] | None:
    """An argument list's elements (a list or tuple literal, or a name assigned one), else None."""
    a = a.strip()
    if a[:1] in "[(" and a[-1:] in "])":
        return _split_args(a[1:-1])
    name = _bare(a)
    if name is not None and depth < 6:
        sub = ctx.rhs(name)
        if sub is not None:
            return argv(sub[0], sub[1], depth + 1)
    return None


def url_taint(a: str, ctx, depth: int = 0) -> str:
    """Where the value is in a URL: "head" (its scheme or host, or all of it), "tail" (only
    the path or query of a fixed host), "relative" (a same-origin path), or "clean"."""
    a = a.strip()
    if _SAME_SITE.match(a):
        return "relative"
    if not ctx.dirty(a):
        return "clean"
    parts = _parts(a)
    if parts is None:  # a name, a call: follow a name to its value, else the value may be all of it
        name = _bare(a)
        sub = ctx.rhs(name) if name is not None and depth < 6 else None
        return "head" if sub is None else url_taint(sub[0], sub[1], depth + 1)
    if parts[0][0] == "lit" and re.match(r"/(?![/\\])", parts[0][1]):
        return "relative"
    # the head: up to the end of the authority (`scheme://host:port`), or with no scheme up to
    # the first / ? # (a base URL held in a name: `f"{base}/forecast"`)
    head, seen = [], ""
    for kind, text in parts:
        if kind == "expr":
            head.append(text)
            seen += "\0"
            continue
        seen += text
        if _authority_closed(seen):
            break
    for h in head:
        if not ctx.dirty(h):
            continue
        name = _bare(h)
        sub = ctx.rhs(name) if name is not None and depth < 6 else None
        if sub is None or url_taint(sub[0], sub[1], depth + 1) == "head":
            return "head"
    return "tail"


def _authority_closed(seen: str) -> bool:
    if re.match(r"^(?:[A-Za-z][\w+.-]*:)?//", seen):
        return bool(re.match(r"^(?:[A-Za-z][\w+.-]*:)?//[^/?#]*[/?#]", seen))
    if re.match(r"^[A-Za-z][\w+.-]*:?/?$", seen):
        return False  # (a scheme still being written: `"https:" + "//" + host`)
    return bool(re.match(r"^[^/?#]*[/?#]", seen))


def _parts(a: str) -> list[tuple[str, str]] | None:
    """A string expression as literal and expression parts: an f-string, a JS template,
    a `+` concatenation, `"...".format(...)` or `"..." % x`; None for anything else."""
    a = _join_adjacent(a)
    m = _FSTR.match(a)
    if m and m.group(1) * 3 not in a:
        return _interpolated(m.group(2), "{")
    m = _TEMPLATE.match(a)
    if m and "`" not in m.group(1):
        return _interpolated(m.group(1), "${")
    m = _PLAIN.match(a)
    if m:
        return [("lit", m.group(2))]
    terms = _split_top(a, "+")
    if len(terms) > 1:
        out = []
        for t in terms:
            sub = _parts(t.strip())
            out += sub if sub is not None else [("expr", t.strip())]
        return out
    fm = re.match(r"""^([rRbBuU]{0,2}(["'])(?:(?!\2)[^\\]|\\.)*\2)\s*\.format\s*\((.*)\)$""", a, re.S)
    if fm:
        lit = _PLAIN.match(fm.group(1)).group(2)
        vals = _split_args(fm.group(3))
        out, k = [], 0
        for piece in re.split(r"(\{[^{}]*\})", lit):
            if re.fullmatch(r"\{[^{}]*\}", piece):
                out.append(("expr", vals[k] if k < len(vals) else fm.group(3)))
                k += 1
            elif piece:
                out.append(("lit", piece))
        return out
    pm = re.match(r"""^([rRbBuU]{0,2}(["'])(?:(?!\2)[^\\]|\\.)*\2)\s*%\s*(.+)$""", a, re.S)
    if pm:
        lit = _PLAIN.match(pm.group(1)).group(2)
        vals = pm.group(3).strip()
        vals_ = _split_args(vals[1:-1]) if vals[:1] == "(" and vals[-1:] == ")" else [vals]
        out, k = [], 0
        for piece in re.split(r"(%[-+ #0]*\d*(?:\.\d+)?[sdirfx])", lit):
            if re.fullmatch(r"%[-+ #0]*\d*(?:\.\d+)?[sdirfx]", piece):
                out.append(("expr", vals_[k] if k < len(vals_) else vals))
                k += 1
            elif piece:
                out.append(("lit", piece))
        return out
    return None


def _join_adjacent(a: str) -> str:
    """Python's adjacent literals as one: `'https://host' '/x?a={}'.format(v)`."""
    lit = r"""[rRuU]?(?:"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')"""
    m = re.match(rf"^((?:{lit}\s*){{2,}})(?=$|\.|\s*[%+])", a)
    if not m:
        return a
    pieces = [_PLAIN.match(x).group(2) for x in re.findall(lit, m.group(1))]
    joined = '"' + "".join(pieces).replace('"', '\\"') + '"'
    return joined + a[m.end():]


def _interpolated(body: str, opener: str) -> list[tuple[str, str]]:
    out, lit, i = [], "", 0
    while i < len(body):
        if opener == "{" and body.startswith("{{", i) or body.startswith("}}", i) and opener == "{":
            lit += body[i]
            i += 2
            continue
        if body.startswith(opener, i):
            j, d = i + len(opener), 1
            while j < len(body) and d:
                d += body[j] == "{"
                d -= body[j] == "}"
                j += 1
            if lit:
                out.append(("lit", lit))
                lit = ""
            expr = body[i + len(opener):j - 1]
            out.append(("expr", re.split(r"(?<![=!<>])[!:](?![=])", expr)[0] if opener == "{" else expr))
            i = j
            continue
        lit += body[i]
        i += 1
    if lit:
        out.append(("lit", lit))
    return out


def _split_top(s: str, sep: str) -> list[str]:
    """s split at `sep` outside brackets and strings."""
    out, cur, d, q, i = [], "", 0, "", 0
    while i < len(s):
        ch = s[i]
        if q:
            cur += ch
            if ch == "\\" and i + 1 < len(s):
                cur += s[i + 1]
                i += 2
                continue
            if ch == q:
                q = ""
        elif ch in "\"'`":
            q = ch
            cur += ch
        elif ch in "([{":
            d += 1
            cur += ch
        elif ch in ")]}":
            d -= 1
            cur += ch
        elif ch == sep and d == 0:
            out.append(cur)
            cur = ""
        else:
            cur += ch
        i += 1
    return out + [cur]


def _split_args(s: str) -> list[str]:
    return [a for a in (x.strip() for x in _split_top(s, ",")) if a]


def receiver(project, f, c, line: str, rhs) -> str | None:
    """Python: what a `.get` / `.execute` is called on. "orm" for an ORM session (`db.session`
    with `db = SQLAlchemy()`, a name assigned `db.session` or `Session(engine)`), "http" for an
    HTTP client (`requests.Session()`, a project class that subclasses one, `self` inside one),
    else None."""
    if not f.file.endswith(PY_EXTS):
        return None
    m = re.search(rf"([A-Za-z_][\w.]*?)\s*\.\s*{re.escape(c.name)}\s*\(", line)
    if not m:
        return None
    recv = m.group(1)
    parts = recv.split(".")
    if parts[0] == "self" and len(parts) == 1:
        own = project.modules.get(f.module)
        return "http" if own is not None and _http_class(project, own) else None
    if len(parts) >= 2 and parts[-1] == "session" and parts[0] in _sqlalchemy_names(project, f):
        return "orm"
    if len(parts) == 1:
        sub = rhs(parts[0])
        if sub is not None:
            val = sub[0].strip()
            if _HTTP_RHS.match(val):
                return "http"
            cm = re.match(r"([A-Z]\w*)\s*\(", val)
            if cm and any(_http_class(project, project.modules[mn]) for mn in project.short.get(cm.group(1), ())
                          if mn in project.modules):
                return "http"
            root = re.match(r"([A-Za-z_]\w*)\.session\b", val)
            if _ORM_RHS.match(val) or root and root.group(1) in _sqlalchemy_names(project, f):
                return "orm"
    return None


def _http_class(project, m, depth: int = 0) -> bool:
    """A project class that subclasses an HTTP client (its `class X(...)` line, as written: the
    linker drops a library base)."""
    if depth > 8 or m is None or not m.decls:
        return False
    head = next((ln for ln in m.raw if re.match(rf"\s*class\s+{re.escape(m.decls[0])}\s*\(", ln)), "")
    bm = re.match(r"\s*class\s+\w+\s*\(([^)]*)\)", head)
    imported = {im.group(1) for im in map(_HTTP_IMPORT.match, m.raw) if im}
    for b in (x.strip() for x in (bm.group(1).split(",") if bm else []) if "=" not in x):
        if _HTTP_BASES.match(b) or b in imported:
            return True
        for mn in project.short.get(b.split(".")[-1], ()):
            if mn in project.modules and mn != m.name and _http_class(project, project.modules[mn], depth + 1):
                return True
    return False


def _sqlalchemy_names(project, f) -> set[str]:
    """The names a file can see bound to `SQLAlchemy()` (its own, or imported from the module that builds it)."""
    got = getattr(project, "_sqlalchemy_names", None)
    if got is None:
        got = {}
        for rel, mods in project.by_file.items():
            if rel.endswith(PY_EXTS) and mods:
                for ln in mods[0].raw:
                    am = re.match(r"^([A-Za-z_]\w*)\s*(?::[^=]+)?=\s*(?:flask_sqlalchemy\.)?SQLAlchemy\s*\(", ln)
                    if am:
                        got.setdefault(rel, set()).add(am.group(1))
        project._sqlalchemy_names = got  # (once per load: every file's module-level lines)
    own = set(got.get(f.file, ()))
    fm = project.by_file.get(f.file, [None])[0]
    if fm is not None and got:
        names = {n for ns in got.values() for n in ns}
        own |= {local for local, (_w, n) in fm.imported.items() if n in names}
    return own
