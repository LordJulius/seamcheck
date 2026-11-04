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
_SAME_SITE = re.compile(r"""^\s*[fbru]{0,2}["'`]/[^/\\"'`]""", re.I)
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
               r"(?P<rx>[A-Za-z_]\w*)\.match\(\s*(?P<n6>[A-Za-z_]\w*)\s*[,)]",
               r"""(?:re\.)?match\(\s*[rbuRBU]{0,2}(?:"\^[^"\n]*\$"|'\^[^'\n]*\$')\s*,\s*(?P<n7>[A-Za-z_]\w*)\s*[,)]""",
               r"(?:url_has_allowed_host_and_scheme|is_safe_url|is_safe_redirect)\(\s*(?:url\s*=\s*)?(?P<n4>[A-Za-z_]\w*)\s*[,)]")
_VALIDATOR = re.compile("|".join(f"(?:{v})" for v in _VALIDATORS))
# a comparison with a number: the value is one (`if port < 1024 or port > 65535: abort(400)`)
_RANGE = re.compile(r"(?<![\w.])([A-Za-z_]\w*)\s*(?:<=?|>=?)\s*-?\d|-?\d[\w.]*\s*(?:<=?|>=?)\s*([A-Za-z_]\w*)(?![\w.(\[])")
_ANCHORED = re.compile(r"""^([A-Za-z_]\w*)\s*=\s*re\.compile\(\s*[rbuRBU]{0,2}(["'])(?:\^|\\A)[^"'\n]*(?:\$|\\Z|\\z)\2""")
_NETLOC = re.compile(r"\b(?:urlsplit|urlparse)\(\s*([A-Za-z_]\w*)\s*\)\.netloc\b")
_ALLOWED = re.compile(r"(?<![\w.])([A-Za-z_]\w*)\s+(not\s+)?in\s+(?:[A-Z][A-Z0-9_]*\b|[\[({])")


def guarded(cond: str, anchored=frozenset()) -> list[tuple[str, bool]]:
    """The names a Python `if` condition checks: (name, inside), inside = the value is checked
    within the block only (`if x.isdigit():`), else from the `if` on (`if not x.isdigit(): return`,
    `if x not in ALLOWED: abort(400)`, `if urlsplit(x).netloc: x = "/"`). `anchored`: the file's
    compiled patterns anchored at both ends (`_STATE_RE = re.compile(r"^[a-z]{16}$")`), whose
    `.match(x)` is a full match."""
    out = []
    for m in _VALIDATOR.finditer(cond):
        if m.group("rx") and m.group("rx") not in anchored:
            continue
        name = next(v for k, v in m.groupdict().items() if v and k != "rx")
        negated = bool(re.search(r"\bnot\s+$", cond[:m.start()]))
        out.append((name, not negated))
    out += [(m.group(1), False) for m in _NETLOC.finditer(cond)]
    out += [(m.group(1) or m.group(2), False) for m in _RANGE.finditer(cond)]
    for m in _ALLOWED.finditer(cond):
        if m.group(1) not in ("not", "and", "or", "is", "if"):
            out.append((m.group(1), not m.group(2)))
    seen, uniq = set(), []
    for name, inside in out:
        if name not in seen:
            seen.add(name)
            uniq.append((name, inside))
    return uniq


def anchored(project, rel: str) -> frozenset:
    """A file's module-level `NAME = re.compile(r"^...$")`: patterns whose `.match` is a full match."""
    cache = project.__dict__.setdefault("_anchored", {})
    if rel not in cache:
        mods = project.by_file.get(rel) or []
        cache[rel] = frozenset(m.group(1) for m in map(_ANCHORED.match, mods[0].raw if mods else []) if m)
    return cache[rel]


_JS_EXTS = (".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx")
_RETURN = re.compile(r"^[ \t]*return\b[ \t]*(.*)$", re.M)
_CONST = re.compile(r"""^(?:None|True|False|-?\d[\w.]*|null|undefined|true|false|[rbuRBU]{0,2}"(?:[^"\\$#]|\\.)*"|[rbuRBU]{0,2}'(?:[^'\\]|\\.)*')?;?$""")
_BUFFER_ANN = re.compile(r"->\s*['\"]?(?:io\.)?(?:BytesIO|StringIO)\b")


def returns(project, code, clean) -> tuple[set, set]:
    """What functions return, by name (every function of that name alike): (sanitized, buffers).
    Sanitized: each `return` is a constant or a call to a sanitizer (the rule's `unless`, url_for, a
    declared one) or to a function that is sanitized itself; buffers: annotated `-> BytesIO`, or each
    `return` builds one or names a local assigned one, by name (every function of it alike) and by
    function id (for a call resolved exactly). `code(f, raw)`: a function's own code."""
    key = (clean[0].pattern if clean else "", len(project.funcs))
    cache = project.__dict__.setdefault("_returns", {})
    if key not in cache:
        cache[key] = _returns(project, code, clean)
    return cache[key]


def _returns(project, code, clean) -> tuple[set, set]:
    cands = [f for f in project.funcs.values() if f.kind != "top" and f.file.endswith(PY_EXTS + _JS_EXTS)]
    rets = {}
    for f in cands:
        body = code(f, True)
        if "return" not in body:
            continue
        exprs = [_ret_expr(body, m) for m in _RETURN.finditer(body)]
        if exprs:
            rets[f.id] = (f, body, exprs)
    by_name = {}
    for f in project.funcs.values():
        by_name.setdefault(f.name, []).append(f.id)
    sanit = set()
    call = clean[0] if clean else None
    for _ in range(6):  # (a wrapper of a wrapper: a few rounds reach the fixed point)
        grown = False
        for fid, (f, _body, exprs) in rets.items():
            if f.name in sanit:
                continue
            ok = all(_CONST.match(e) or _sanitizing_call(e, call, sanit) for e in exprs)
            if ok and all(i in rets and all(_CONST.match(e) or _sanitizing_call(e, call, sanit) for e in rets[i][2])
                          for i in by_name.get(f.name, [])):
                sanit.add(f.name)
                grown = True
        if not grown:
            break
    buffers = {fid for fid in rets if _returns_buffer(*rets[fid])}  # (by id: what a call resolved exactly gives)
    buffers |= {name for name, ids in by_name.items() if ids and all(i in buffers for i in ids)}
    return sanit, buffers


def _ret_expr(body: str, m) -> str:
    """A `return`'s whole expression (on to the line its brackets close)."""
    text = m.group(1)
    k = m.end()
    while _depth(text) > 0 and k < len(body):
        nl = body.find("\n", k + 1)
        nl = len(body) if nl < 0 else nl
        text += "\n" + body[k + 1:nl]
        k = nl
    return re.sub(r"\s*#[^\n]*$", "", text.strip()).rstrip(";").strip()


def _depth(text: str) -> int:
    d = 0
    for ch in re.sub(r"""(["'])(?:(?!\1)[^\\]|\\.)*\1""", "", text):
        d += ch in "([{"
        d -= ch in ")]}"
    return d


def _whole_call(e: str) -> str | None:
    """The called name when e is one call, `name(...)` / `x.name(...)` with nothing after it."""
    m = re.match(r"(?:await\s+)?(?:[A-Za-z_$][\w$]*\.)*([A-Za-z_$][\w$]*)\s*\(", e)
    if not m:
        return None
    d, j = 0, m.end() - 1
    while j < len(e):
        d += e[j] in "([{"
        d -= e[j] in ")]}"
        if d == 0:
            break
        j += 1
    return m.group(1) if e[j + 1:].strip() == "" else None


def _sanitizing_call(e: str, call, sanit: set) -> bool:
    name = _whole_call(e)
    if name is None:
        return False
    return name in sanit or bool(call and call.match(e[e.find(name):]))


def _returns_buffer(f, body: str, exprs: list[str]) -> bool:
    head = body.split("\n", 3)
    if any(_BUFFER_ANN.search(ln) for ln in head[:3] if ":" in ln):
        return True
    for e in exprs:
        if _BUFFER.match(e):
            continue
        if re.fullmatch(r"[A-Za-z_]\w*", e) and re.search(rf"^[ \t]*{re.escape(e)}[ \t]*=[ \t]*(?:io\.)?(?:BytesIO|StringIO)\s*\(", body, re.M):
            continue
        return False
    return True


_RESOLVES = re.compile(r"\.resolve\s*\(|\b(?:os\.path\.)?(?:realpath|abspath)\s*\(")
_WITHIN = re.compile(r"\.(?:is_)?relative_to\s*\(|\bcommonpath\s*\(")
_REJECTS = re.compile(r"\braise\b|\babort\s*\(|\bthrow\b|\breturn\s+(?:None|False|null|false)\b")
_URL_BUILDERS = ("url_for", "reverse", "reverse_lazy")


def summaries(project, code) -> tuple[set, set, set]:
    """What else functions give back, by name (every function of that name alike): (confined,
    same_site, free). Free: each `return` is a name with no path separator (a basename, both
    separators replaced, `name or "attachment"` of one). Confined: it resolves a path and checks it is under a root (`.resolve()`, then
    `.relative_to(root)` / `.is_relative_to(root)` / `commonpath`, raising or returning None when
    it isn't), or calls such a function: what it returns is no path the request can move.
    Same-site: each `return` is a URL whose scheme, host and path are the site's (`url_for(...)
    + "?" + urlencode(q)`, a literal "/path", such a function's result), so request data in it is
    only the query. Each set holds the names (every function of that name alike) and the function ids
    (for a call resolved exactly). `code(f, raw)`: a function's own code."""
    key = len(project.funcs)
    cache = project.__dict__.setdefault("_summaries", {})
    if key not in cache:
        cache[key] = _summaries(project, code)
    return cache[key]


def _summaries(project, code) -> tuple[set, set, set]:
    cands = [f for f in project.funcs.values() if f.kind != "top" and f.file.endswith(PY_EXTS + _JS_EXTS)]
    by_name = {}
    for f in project.funcs.values():
        by_name.setdefault(f.name, []).append(f.id)
    bodies = {f.id: (f, code(f, True)) for f in cands}

    def named(ids: set) -> set:  # the names every function of which is in ids
        return {f.name for fid in ids for f in (bodies[fid][0],) if all(i in ids for i in by_name.get(f.name, []))}

    conf = {fid for fid, (f, body) in bodies.items() if ("relative_to" in body or "commonpath" in body)
            and _WITHIN.search(body) and _RESOLVES.search(body) and _REJECTS.search(body)}
    for _ in range(6):  # (a method that calls the check: `resolve_managed_path` -> `self._is_allowed_path`)
        if not conf:
            break
        names = named(conf)
        calls = re.compile(rf"(?<![\w$])(?:{'|'.join(map(re.escape, sorted(names)))})\s*\(") if names else None
        grown = {fid for fid, (f, body) in bodies.items() if fid not in conf
                 and (any(ex and dst in conf for dst, _ln, ex in project.edges.get(fid, [])) or calls and calls.search(body))}
        if not grown:
            break
        conf |= grown

    # (only a function with a URL-ish return can be one: url_for, reverse, a "/path", or a call to one found)
    def site_rets(body):
        es = [_ret_expr(body, m) for m in _RETURN.finditer(body)]
        return es if es and all(_same_site_ret(e, None) is not False for e in es) else None
    rets = {fid: es for fid, (f, body) in bodies.items()
            if "return" in body and ("url_for" in body or "reverse" in body or _SITE_HINT.search(body))
            and (es := site_rets(body))}
    same = set()
    for _ in range(6):
        names = named(same)
        grown = {fid for fid in rets if fid not in same and all(_same_site_ret(e, names) for e in rets[fid])}
        if not grown:
            break
        same |= grown
        new = named(same) - names
        if new:
            calls = re.compile(rf"(?<![\w$])(?:{'|'.join(map(re.escape, sorted(new)))})\s*\(")
            rets.update({fid: es for fid, (f, body) in bodies.items() if fid not in rets and "return" in body
                         and calls.search(body) and (es := site_rets(body))})
    # a name with no separator: each return is a basename, both separators replaced, a literal with
    # none, or a local assigned one (`name = os.path.basename(v).strip(); return name or "attachment"`)
    free = set()
    cands_ = {fid: (body, [_ret_expr(body, m) for m in _RETURN.finditer(body)]) for fid, (f, body) in bodies.items()
              if "return" in body and ("basename" in body or "replace" in body or "secure_filename" in body or ".name" in body)}
    for _ in range(6):
        names = named(free)
        grown = {fid for fid, (body, es) in cands_.items() if fid not in free and es and all(_free_ret(e, body, names) for e in es)}
        if not grown:
            break
        free |= grown
    return conf | named(conf), same | named(same), free | named(free)


def _free_ret(e: str, body: str, names) -> bool:
    """A return expression with no separator in it, its bare names read as their assignment in body."""
    parts = []
    for p in _split_top_or(e):
        n = _bare(p)
        if n is not None:
            got = re.findall(rf"^[ \t]*{re.escape(n)}[ \t]*=(?!=)[ \t]*(.+)$", body, re.M)
            if len(got) != 1:
                return False
            p = got[0]
        parts.append(p)
    return all(separator_free(p, names) for p in parts)


_SITE_HINT = re.compile(r"""\breturn\b[ \t]*[fbru]{0,2}["'`]/[^/]""", re.I)


def _same_site_ret(e: str, same):
    """The return expression is a same-site URL; with `same` None, None when it could be one (a
    call, once its callee is known) and False when it can't."""
    if e in ("None", "null", "undefined", ""):
        return True
    lead = _split_top(e, "+")[0].strip() if "+" in e else e
    if _SAME_SITE.match(lead) is not None or _whole_call(lead) in _URL_BUILDERS:
        return True
    if same is None:
        return None if _whole_call(e) is not None else False
    return _whole_call(e) in same


def via(a: str, ctx, names, depth: int = 0) -> bool:
    """a is a call to one of `names` (by name or by the call's exact targets), or a name last assigned one."""
    a = a.strip()
    if _summarised(a, ctx, names):
        return True
    name = _bare(a)
    if name is None or depth > 6:
        return False
    sub = ctx.rhs(name)
    return sub is not None and via(sub[0], sub[1], names, depth + 1)


# where the user came from: `request.referrer`, `request.headers.get("Referer")`, Django's `META["HTTP_REFERER"]`
_REFERER = re.compile(r"""\brequest\.referrer\b|\b(?:req|request)\.(?:headers|META|environ)(?:\.get\s*\(|\[)\s*['"](?:referr?er|http_referer)['"][^)\]]*[)\]]"""
                      r"""|\breq\.headers\.referr?er\b|\breq\.(?:get|header)\s*\(\s*['"]referr?er['"]\s*\)""", re.I)


def referer_only(a: str, ctx, depth: int = 0) -> bool:
    """The request's only part in a is the Referer header (`request.referrer or url_for("index")`,
    or a name assigned it): the page the user came from, not a link someone else wrote."""
    seen = bool(_REFERER.search(a))
    rest = _REFERER.sub("None", a)
    for name in sorted(set(re.findall(r"(?<![\w.$])[A-Za-z_]\w*", rest))):
        if not ctx.dirty(name):
            continue
        sub = ctx.rhs(name)
        if sub is None or depth > 6 or not referer_only(sub[0], sub[1], depth + 1):
            return False
        seen = True
        rest = re.sub(rf"(?<![\w.$]){re.escape(name)}(?!\w)", "None", rest)
    return seen and not ctx.dirty(rest)


# a string literal's start: Python's prefixes (f-strings), C#'s `$"` / `@$"`, a triple quote
_STR_START = re.compile(r"""(?<![\w$])(?:[rRbBuUfF]{1,2}|\$@?|@\$?)?(?:"{3}|'{3}|["'`])""")
# what a non-f string interpolates: Ruby / Elixir `#{x}`, `${x}`, PHP `{$x}` and `$x->y`, Swift `\(x)`
_INTERP = re.compile(r"#\{[^}]*\}|\$\{[^}]*\}|\{\$[^}]*\}|\$[A-Za-z_]\w*(?:->\w+|\[[^\]]*\])*|\\\([^)]*\)")


def code_parts(text: str) -> str:
    """text with each string literal reduced to what it interpolates (an f-string's `{x}`, a JS
    template's `${x}`, Ruby's `#{x}`, PHP's `$x`, C#'s `$"{x}"`): the words inside a string
    are not names (`f"FROM t c ON c.id"` holds no `c`). A string cut off keeps the rest."""
    out, i, n = [], 0, len(text)
    while i < n:
        m = _STR_START.search(text, i)
        if not m:
            out.append(text[i:])
            break
        out.append(text[i:m.start()])
        tok = m.group(0)
        q = tok[-3:] if tok[-3:] in ('"' * 3, "'" * 3) else tok[-1]
        pre = tok[:-len(q)].lower()
        braces = "f" in pre or "$" in pre or q == "`"  # (`{x}` / `${x}` interpolate)
        verbatim = "@" in pre
        end = _string_end(text, m.end(), q, braces, verbatim)
        if end is None:
            out.append(text[m.start():])
            break
        body = text[m.end():end - len(q)]
        if braces:
            kept = _brace_parts(body, q == "`")
        else:
            kept = _INTERP.findall(body) if q[0] == '"' else []
        out.append(" " + " ".join(kept) + " ")
        i = end
    return "".join(out)


def _string_end(text: str, j: int, q: str, braces: bool, verbatim: bool) -> int | None:
    """Where the string whose body starts at j ends (after its closing quote); None if it doesn't."""
    depth, n = 0, len(text)
    while j < n:
        ch = text[j]
        if braces and depth == 0 and q != "`" and text.startswith("{{", j):
            j += 2
            continue
        if braces and ch == "{" and (q != "`" or text[j - 1:j] == "$"):
            depth += 1
        elif braces and ch == "}" and depth > 0:
            depth -= 1
        elif depth == 0:
            if verbatim and len(q) == 1 and text.startswith(q * 2, j):
                j += 2
                continue
            if ch == "\\" and not verbatim:
                j += 2
                continue
            if text.startswith(q, j):
                return j + len(q)
        j += 1
    return None


def _brace_parts(body: str, dollar: bool) -> list[str]:
    """An f-string's `{x}` (a template's `${x}`) contents, outermost first."""
    kept, depth, start, k = [], 0, 0, 0
    while k < len(body):
        ch = body[k]
        if depth == 0 and not dollar and body.startswith("{{", k):
            k += 2
            continue
        if ch == "{" and (not dollar or depth > 0 or body[k - 1:k] == "$"):
            depth += 1
            if depth == 1:
                start = k + 1
        elif ch == "}" and depth > 0:
            depth -= 1
            if depth == 0:
                kept.append(body[start:k])
        k += 1
    return kept


def _mask(s: str) -> str:
    """s with what is inside strings and brackets blanked (the same length)."""
    out, d, q, i = [], 0, "", 0
    while i < len(s):
        ch = s[i]
        if q:
            if ch == "\\":
                out.append("  "[:len(s) - i])
                i += 2
                continue
            q = "" if ch == q else q
            out.append(" ")
        elif ch in "\"'`":
            q = ch
            out.append(" ")
        elif ch in "([{":
            d += 1
            out.append(" ")
        elif ch in ")]}":
            d -= 1
            out.append(" ")
        else:
            out.append(ch if d == 0 else " ")
        i += 1
    return "".join(out)[:len(s)]


def branches(e: str) -> list[str]:
    """The values a conditional expression can take (`c ? '/a' : '/b'`, `'/a' if c else '/b'`);
    [e] when it is none."""
    e = e.strip()
    masked = _mask(e)
    py = re.search(r"\sif\s", masked)
    if py:
        el = re.search(r"\selse\s", masked[py.end():])
        if el:
            return branches(e[:py.start()]) + branches(e[py.end() + el.end():])
    k = next((k for k, ch in enumerate(masked) if ch == "?" and masked[k + 1:k + 2] not in (".", "?") and masked[k - 1:k] != "?"), None)
    if k is not None:
        c = masked.find(":", k)
        if c > 0:
            return branches(e[k + 1:c]) + branches(e[c + 1:])
    return [e]


def same_site(e: str) -> bool:
    """Every value e can take is a same-origin path (`"/a"`, `` `/a/${id}` ``, `c ? "/a" : "/b"`)."""
    return all(_SAME_SITE.match(b) for b in branches(e))


def _summarised(e: str, ctx, names) -> bool:
    """e is one call to a function the summary names: by name (every function of it alike), or by
    the functions the call resolves to exactly (`ctx.callees`, when the ctx has it)."""
    n = _whole_call(e.strip())
    if n is None:
        return False
    if n in names:
        return True
    ids = ctx.callees(n) if hasattr(ctx, "callees") else []
    return bool(ids) and all(i in names for i in ids)


def stored(project, stores: list, source, code, taint, dirty):
    """A rule's `stores` (a request-scoped store's names: Flask's `session`): a key some function
    writes the request's value to (`session["backup"] = json.dumps(data)`) holds the request's value
    wherever it is read (`json.loads(session["backup"])`, `session.get("backup")`), so the source
    grows to read it, a few rounds deep. `code(f)`: a function's own raw code; `taint(f, source)` its
    taint; `dirty(text, source, tainted)`: the text carries the value."""
    names = "|".join(map(re.escape, stores))
    write = re.compile(rf"""^[ \t]*(?<![\w.])(?:{names})\[\s*(['"])([^'"\\\n]+)\1\s*\]\s*=(?!=)\s*(.+)$""", re.M)
    base, keys = source, set()
    for _ in range(3):  # (a key read back and written under another)
        grown = False
        for f in project.funcs.values():
            if f.kind == "top":
                continue
            text = code(f)
            if "[" not in text or not write.search(text) or not source.search(text):
                continue
            t = None
            for m in write.finditer(text):
                if m.group(2) in keys:
                    continue
                t = t or taint(f, source)
                if dirty(m.group(3), source, t.at(f.start + text.count("\n", 0, m.start()))):
                    keys.add(m.group(2))
                    grown = True
        if not grown:
            break
        k = "|".join(map(re.escape, sorted(keys)))
        source = re.compile(rf"""(?:{base.pattern})|(?<![\w.])(?:{names})(?:\[\s*['"](?:{k})['"]\s*\]|\.(?:get|pop)\(\s*['"](?:{k})['"])""")
    return source


# an SQLAlchemy Core statement as the call's last step (`known[name].select()`, `t.delete().where(...)`):
# a query object, no SQL text
_STATEMENT_TAIL = re.compile(r"\.(?:select|insert|update|delete|where|filter|filter_by|values|order_by|limit|offset|"
                             r"join|outerjoin|group_by|having|returning|with_only_columns)\s*\((?:[^()]|\([^()]*\))*\)\s*$", re.S)
# a comprehension that keeps only members of something (`[t for t in tables if t in known]`)
_KEEP_MEMBERS = re.compile(r"^\s*[\[{(]\s*([A-Za-z_]\w*)\s+for\s+([A-Za-z_]\w*)\s+in\s+(.+?)\s+if\s+([A-Za-z_]\w*)\s+in\s+"
                           r"([A-Za-z_][\w.]*(?:\[[^\]]*\])?)\s*[\]})]\s*$", re.S)
# a path component with no separator left: `os.path.basename(x)`, `Path(x).name`, `secure_filename(x)`,
# both separators replaced (`x.replace("/", "_").replace("\\", "_")`)
_BASENAME = re.compile(r"^(?:os\.path\.|posixpath\.|ntpath\.)?basename\s*\(|^(?:pathlib\.)?(?:Pure)?Path\s*\(.*\)\.name\s*$|^(?:\w+\.)?secure_filename\s*\(", re.S)
_TRIM = re.compile(r"(?:\.(?:strip|lstrip|rstrip|lower|upper|casefold)\(\s*(?:" + _STR + r")?\s*\))+\s*$")
_JOIN = re.compile(r"^(?:os\.path\.|posixpath\.|ntpath\.)join\s*\((.*)\)\s*$", re.S)


def keeps_members(rhs: str, dirty) -> bool:
    """rhs keeps only the members of a clean collection (`[t for t in tables if t in known]`): an allow-list.
    `dirty(text)`: the text carries the value."""
    m = _KEEP_MEMBERS.match(rhs)
    return bool(m and m.group(1) == m.group(2) == m.group(4) and not dirty(m.group(5)))


def statement(a: str) -> bool:
    """a builds an SQLAlchemy Core statement (its last step `.select()` / `.delete()` / `.where(...)`), no SQL text."""
    a = a.strip()
    return not _TEXT_CALL.match(a) and "text(" not in a and bool(_STATEMENT_TAIL.search(a))


def separator_free(e: str, names=frozenset()) -> bool:
    """e can't hold a path separator: a basename, a name with both separators replaced, a literal with
    none, `x or "attachment"` of those, or a call to a function in `names` (that returns one)."""
    e = _TRIM.sub("", e.strip()).strip()
    parts = _split_top_or(e)
    if len(parts) > 1:
        return all(separator_free(p, names) for p in parts)
    if _BASENAME.match(e) or _whole_call(e) in names:
        return True
    if _literal(e):
        return "/" not in e and "\\\\" not in e
    reps = re.findall(r"""\.replace\(\s*[bu]?(["'])(/|\\\\)\1\s*,""", e)
    return {r[1] for r in reps} >= {"/", "\\\\"}


def free_value(e: str, ctx, free=frozenset(), depth: int = 0) -> bool:
    """The value can't hold a path separator (separator_free), followed through the names it was
    assigned and through what it is built of: an f-string or `+` of such values and literals with
    no `/`, `\\` or `..`, each branch of a conditional (`n if n.endswith(".pdf") else f"{n}.pdf"`)."""
    e = e.strip()
    if separator_free(e) or _summarised(e, ctx, free):
        return True
    if depth > 6:
        return False
    bs = branches(e)
    if len(bs) > 1:
        return all(free_value(b, ctx, free, depth + 1) for b in bs)
    parts = _parts(e)
    if parts and (len(parts) > 1 or parts[0][0] == "expr"):
        return all(not re.search(r"[/\\]|\.\.", t) if kind == "lit" else free_value(t, ctx, free, depth + 1) for kind, t in parts)
    name = _bare(e)
    if name is not None:
        if not ctx.dirty(name):
            return True  # (not the request's: what it holds is the program's own)
        sub = ctx.rhs(name)
        return sub is not None and free_value(sub[0], sub[1], free, depth + 1)
    return False


def _split_top_or(e: str) -> list[str]:
    masked = _mask(e)
    cuts = [m.start() for m in re.finditer(r"\s(?:or|\|\|)\s", masked)]
    if not cuts:
        return [e]
    out, k = [], 0
    for c in cuts:
        out.append(e[k:c])
        k = c + len(re.match(r"\s(?:or|\|\|)\s", masked[c:]).group(0))
    return out + [e[k:]]


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
        self.redirect = pats("redirect_sinks")
        self.buffers: set = set()  # functions that return an in-memory buffer (set by the path rule)
        self.confined: set = set()  # functions that return a path checked to be under a root (summaries)
        self.same_site: set = set()  # functions that return a same-site URL (summaries)
        self.free: set = set()  # functions that return a name with no path separator (summaries)
        self.lookups = re.compile(r["lookups"]) if r.get("lookups") else None
        self.severity = r.get("severity", "error")
        self.url_severity = r.get("url_severity", "warn")
        self.argv_severity = r.get("argv_severity", "warn")
        self.referer_severity = r.get("referer_severity", "warn")

    def kind(self, text: str) -> str | None:
        for k, ps in (("sql", self.sql), ("url", self.url), ("path", self.path), ("shell", self.shell), ("redirect", self.redirect)):
            if any(p.search(text) for p in ps):
                return k
        return None

    def any(self) -> bool:
        return bool(self.sql or self.url or self.path or self.shell or self.redirect)

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
            safe = self.buffers | self.confined
            return (self.severity, "") if first is not None and path_dirty(first, ctx, 0, safe, self.free) else None
        if kind == "redirect":  # an open redirect needs the value in the scheme or host (a literal host can't leave)
            first = next((kws[k] for k in ("location", "url", "redirect_to") if k in kws), pos[0] if pos else None)
            if first is None or via(first, ctx, self.same_site) or url_taint(first, ctx) != "head":
                return None
            if referer_only(first, ctx):  # (back where the user came from: worth a look, not someone else's link)
                return (self.referer_severity, " (a redirect to the Referer)") if self.referer_severity != "off" else None
            return self.severity, ""
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
            if elems is None and not shell and _bare(first) and hasattr(ctx, "passed") and ctx.rhs(_bare(first)) is None:
                given = ctx.passed(_bare(first))  # `_run(args)`: what every caller passes, a fixed program's list each
                if given and all((g := argv(x, _NO_CTX)) and _program(g[0]) for x in given):
                    return (self.argv_severity, " (an argument of a fixed program, no shell)") \
                        if self.argv_severity != "off" else None
            if elems is None:
                return (self.severity, "") if first and ctx.dirty(first) else None
            if not elems or not _program(elems[0]) and ctx.dirty(elems[0]):
                return (self.severity, "") if elems else None
            if any(ctx.dirty(e) for e in elems[1:]):
                return (self.argv_severity, " (an argument of a fixed program, no shell)") \
                    if self.argv_severity != "off" else None
            return None
        return None


def _program(e: str) -> bool:
    """A fixed program: a literal, `sys.executable`, `shutil.which("git")`, or `shutil.which("git") or "/usr/bin/git"`."""
    return all(_PROGRAM.match(p) for p in _split_top_or(e.strip()))


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
    if _QUERY_OBJECT.match(a) or statement(a):
        return False
    name = _bare(a)
    if name is not None:
        sub = ctx.rhs(name)
        return True if sub is None or depth > 6 else sql_dirty(sub[0], sub[1], depth + 1)
    return True


def path_dirty(a: str, ctx, depth: int = 0, buffers=frozenset(), free=frozenset()) -> bool:
    """The path argument carries the value (an in-memory buffer is no path, nor what a function
    that returns one or a confined path gives, nor a name with no separator in it: a basename,
    both separators replaced, a function in `free`; `os.path.join` is read part by part;
    `ctx.assigned(name)`, when the ctx has it: the latest plain assignment past calls that add
    to the object)."""
    a = a.strip()
    if _BUFFER.match(a) or not ctx.dirty(a) or _summarised(a, ctx, buffers):
        return False
    if free_value(a, ctx, free):
        return False  # (`os.path.basename(x)`, both separators replaced, `f"{name}.pdf"` of one: a name in its directory)
    j = _JOIN.match(a)
    if j and depth <= 6:  # `os.path.join(tmp, name)`: each part
        return any(path_dirty(x, ctx, depth + 1, buffers, free) for x in _split_args(j.group(1)))
    name = _bare(a)
    if name is not None:
        made = ctx.assigned(name) if hasattr(ctx, "assigned") else None
        if made is not None and (_BUFFER.match(made[0].strip()) or _summarised(made[0], made[1], buffers)):
            return False  # (`buffer = io.BytesIO()`, then `buffer.write(data)`: still a buffer)
        sub = ctx.rhs(name)
        return True if sub is None or depth > 6 else path_dirty(sub[0], sub[1], depth + 1, buffers, free)
    return True


def argv(a: str, ctx, depth: int = 0) -> list[str] | None:
    """An argument list's elements (a list or tuple literal, or a name assigned one), else None."""
    a = a.strip()
    parts = _split_top(a, "+")
    if len(parts) > 1:  # `[git_path] + args`: the first part says the program, the rest are its arguments
        head = argv(parts[0], ctx, depth + 1)
        if not head or depth >= 6:
            return None
        rest = []
        for p in parts[1:]:
            got = argv(p, ctx, depth + 1)
            rest += got if got is not None else [p.strip()]
        return head + rest
    if a[:1] in "[(" and a[-1:] in "])":
        return _split_args(a[1:-1])
    name = _bare(a)
    if name is not None and depth < 6:
        sub = ctx.rhs(name)
        if sub is not None:
            return argv(sub[0], sub[1], depth + 1)
    return None


class _NoCtx:
    """A caller's text read on its own: nothing tainted, no assignments."""

    def dirty(self, _text):
        return False

    def rhs(self, _name):
        return None


_NO_CTX = _NoCtx()


def url_taint(a: str, ctx, depth: int = 0) -> str:
    """Where the value is in a URL: "head" (its scheme or host, or all of it), "tail" (only
    the path or query of a fixed host), "relative" (a same-origin path), or "clean"."""
    a = a.strip()
    if same_site(a):
        return "relative"
    name = _bare(a)
    if name is not None and depth < 6 and hasattr(ctx, "passed") and ctx.rhs(name) is None:
        given = ctx.passed(name)  # a parameter: what every caller passes (`requestJson('/chat/api/x', ...)`)
        if given and all(same_site(g) for g in given):
            return "relative"
    if not ctx.dirty(a):
        return "clean"
    parts = _parts(a)
    if parts is None:  # a name, a call: follow a name to its value, else the value may be all of it
        name = _bare(a)
        sub = ctx.rhs(name) if name is not None and depth < 6 else None
        return "head" if sub is None else url_taint(sub[0], sub[1], depth + 1)
    if parts[0][0] == "lit" and re.match(r"/[^/\\]", parts[0][1]):
        return "relative"
    if parts[0] == ("lit", "/") and len(parts) > 1:
        return "head" if ctx.dirty(parts[1][1]) else "tail"  # (`"/" + x`: x may start with another `/`)
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
