"""`kind = "lint"`: structural checks that know a language or UI toolkit.

A project still chooses what runs and where (`[[rule]] kind = "lint"`,
`lint = "<name>"`, `in`, `skip`, `options`); seamcheck holds only the
knowledge of what the shape means. Each check is narrow on purpose: a rule
that cries wolf gets waived and ignored.

  disabled-by-data          a row or list item whose enabled state comes from
                            the data's status (read, pending, settled...)
                            instead of busy/loading: taps on it are dead
  inert-control             a button with no handler, an empty click handler,
                            an uncontrolled setting input nothing reads
  qml-anchors-in-positioner an item anchored inside a Column/Row/Grid/Flow or
                            a Qt Quick Layout (Qt refuses and stops laying out)
  kotlin-init-order         an `init` block that uses (itself or through the
                            class's own functions) a property declared below it
  id-kind-mix               `a.requestId ?: b.challengeId`: one fallback chain
                            mixing ids of different things
  ets-owner                 an ETS table created outside a long-lived owner
                            (it dies with whichever process made it)

Plus `kind = "essential"`: declared controls must render unconditionally on
their bar (see `essential`).
"""
from __future__ import annotations

import os
import re

from . import structure as st

# --- shared -------------------------------------------------------------------------


def _lang(rel: str) -> str:
    ext = os.path.splitext(rel)[1]
    return {".kt": "kotlin", ".kts": "kotlin", ".tsx": "jsx", ".jsx": "jsx", ".ts": "js", ".js": "js",
            ".mjs": "js", ".qml": "qml", ".ex": "elixir", ".exs": "elixir"}.get(ext, "")


def _line(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def _balanced_end(text: str, i: int, stops: str = ",)\n") -> int:
    """From i, the end of an argument/expression: the first stop char at depth 0."""
    depth = 0
    while i < len(text):
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                return i
            depth -= 1
        elif depth == 0 and c in stops:
            return i
        i += 1
    return i


def _braces(code: str, project=None, rel: str = ""):
    """brace_tree, once per (file, load)."""
    if project is None:
        return st.brace_tree(code)
    key = (rel, "braces")
    hit = project.text_cache.get(key)
    if hit is None:
        hit = project.text_cache[key] = st.brace_tree(code)
    return hit


def _jsx(raw: str, project=None, rel: str = ""):
    if project is None:
        return st.jsx_tree(raw)
    key = (rel, "jsx")
    hit = project.text_cache.get(key)
    if hit is None:
        hit = project.text_cache[key] = st.jsx_tree(raw)
    return hit


# --- disabled-by-data ---------------------------------------------------------------

STATUS = r"unread|read|is_?read|seen|is_?seen|pending|is_?pending|settled|is_?settled|status|state|expired|is_?expired|" \
         r"approved|denied|rejected|resolved|archived|done|is_?done|completed|dismissed|handled|opened|answered|outcome"
BUSY = r"(?i)busy|loading|saving|sending|submitting|uploading|in_?flight|working|refreshing|fetching|syncing|" \
       r"connecting|offline|online|connected|paired|permission|allowed|editable|read_?only|can[A-Z_]"
TARGETS = r"(?i)(row|item|card|tile|entry|cell|delegate)$"


def _status_without_busy(expr: str, o: dict) -> str | None:
    """The data-status member the condition reads (`notice.unread`), unless
    the condition is about work in flight too."""
    if re.search(o.get("busy", BUSY), expr):
        return None
    status = re.compile(rf"(?i)^(?:{o.get('status', STATUS)})$")
    for m in re.finditer(r"\b([A-Za-z_]\w*)\s*\??\.\s*([A-Za-z_]\w*)", expr):
        owner, member = m.group(1), m.group(2)
        if owner[0].isupper():  # VState.Disabled, Theme.x: constants, not data
            continue
        if status.match(member):
            return f"{owner}.{member}"
    return None


def _top_level(code: str, tree, k: int) -> str:
    """A group's text without its nested groups (`VRow(onClick = x, trailing = { VButton(onClick = y) })`
    -> its own `onClick = x` only)."""
    n = tree[k]
    parts, pos = [], n.start + 1
    for c in n.children:
        parts.append(code[pos:tree[c].start])
        pos = tree[c].end
    parts.append(code[pos:n.end - 1])
    return " ".join(parts)


def _disabled_kotlin(rel, raw, code, o, project=None):
    if not re.search(r"(?<![\w.])(?:state|enabled)\s*=(?!=)", code):
        return []
    targets = re.compile(o.get("targets", TARGETS))
    tree = _braces(code, project, rel)
    out = []

    def callee(pos):
        k = st.innermost(tree, pos, ("(",))
        if k < 0:
            return ""
        m = re.search(r"([A-Za-z_]\w*)\s*$", st.strip_groups(tree[k].name))
        return m.group(1) if m else ""

    def expr_at(pos):
        return code[pos:_balanced_end(code, pos)]

    vals = {}
    for m in re.finditer(r"\b(?:val|var)\s+(\w+)\s*=(?!=)\s*", code):
        e = expr_at(m.end())
        if "Disabled" in e or m.group(1) in ("enabled", "isEnabled"):
            vals[m.group(1)] = (m.start(), e)
    for m in re.finditer(r"(?<![\w.])(state|enabled)\s*=(?!=)\s*", code):
        if re.search(r"\b(?:val|var)\s+$", code[max(0, m.start() - 8):m.start()]):
            continue
        fn = callee(m.start())
        if not targets.search(fn):
            continue
        k = st.innermost(tree, m.start(), ("(",))
        if not re.search(r"\bon(?:Click|LongClick|Tap|Open|Select)\s*=|\bclickable\b", _top_level(code, tree, k)):
            continue  # nothing to tap on the row itself: Disabled only dims it
        e = expr_at(m.end())
        where = m.start()
        name = e.strip()
        if name in vals:
            where, e = vals[name]
        if m.group(1) == "state" and "Disabled" not in e:
            continue
        hit = _status_without_busy(e, o)
        if hit:
            out.append((_line(code, where), f"{fn} is disabled by the data's state ({hit}), not by work in flight: "
                        f"a tap on it does nothing. Let it open; show the state in the row (a dot, a subtitle)."))
    return out


def _disabled_jsx(rel, raw, code, o, project=None):
    targets = re.compile(o.get("targets", TARGETS))
    out = []
    for n in _jsx(raw, project, rel):
        if n.kind != "el" or not n.name:
            continue
        head = st.jsx_attrs(raw, n)
        m = re.search(r"\bdisabled=\{", head)
        if not m:
            continue
        rowish = (n.name.lower() in ("li", "tr", "option") or targets.search(n.name)
                  or re.search(r"""(?:className|role)=["'{][^"'}]*\b\w*(?:row|item|card|entry|tile|listitem|option)\b""", head))
        if not rowish:
            continue
        a = n.start + m.end()
        e = raw[a:_balanced_end(raw, a, "")]
        hit = _status_without_busy(e, o)
        if hit:
            out.append((_line(raw, n.start), f"<{n.name}> is disabled by the data's state ({hit}), not by work in flight: "
                        f"a click on it does nothing. Let it open; show the state in the row."))
    return out


def _disabled_qml(rel, raw, code, o, project=None):
    if not re.search(r"(?m)^\s*enabled\s*:", code):
        return []
    targets = re.compile(o.get("targets", TARGETS))
    tree = _braces(code, project, rel)
    out = []
    for k, n in enumerate(tree):
        if n.kind != "{":
            continue
        typ = _qml_type(n.name)
        if not typ:
            continue
        delegated = any(_qml_prop(tree[a].name) == "delegate" or targets.search(_qml_type(tree[a].name) or "")
                        for a in st.ancestors(tree, k)[:3] if tree[a].kind == "{")
        if not ((targets.search(typ) and typ != "Item") or (typ in ("MouseArea", "TapHandler") and delegated)):
            continue
        for m in re.finditer(r"(?m)^\s*enabled\s*:\s*(.+)$", st.own_text(code, tree, k)):
            hit = _status_without_busy(m.group(1), o)
            if hit:
                pos = code.find(m.group(0).strip(), n.start)
                out.append((_line(code, pos), f"{typ} is disabled by the data's state ({hit}), not by work in flight: "
                            f"a click on it does nothing. Let it open; show the state in the row."))
    return out


def disabled_by_data(rel, raw, code, o, project=None):
    lang = _lang(rel)
    if lang == "kotlin":
        return _disabled_kotlin(rel, raw, code, o, project)
    if lang == "jsx":
        return _disabled_jsx(rel, raw, code, o, project)
    if lang == "qml":
        return _disabled_qml(rel, raw, code, o, project)
    return []


# --- inert-control ------------------------------------------------------------------

_HANDLER = re.compile(r"\bon[A-Z]\w*\s*=")
_EMPTY_JSX_HANDLER = re.compile(r"\b(on[A-Z]\w*)=\{\s*\(\s*\w*\s*\)\s*=>\s*(?:\{\s*\}|undefined|null|void 0)\s*\}")
_EMPTY_KT = re.compile(r"\b(onClick|onLongClick|onDoubleClick|onCheckedChange|onValueChange|onToggle|onSelect|onTap)\s*=\s*\{\s*\}"
                       r"|\.(clickable|combinedClickable)\s*(?:\([^()]*\))?\s*\{\s*\}")
_EMPTY_QML = re.compile(r"\b(on(?:Clicked|Tapped|Pressed|Toggled|Activated|Triggered|DoubleClicked))\s*:\s*\{\s*\}")


def inert_control(rel, raw, code, o, project=None):
    lang = _lang(rel)
    out = []
    if lang in ("jsx",):
        for n in _jsx(raw, project, rel):
            if n.kind != "el" or n.name not in ("button", "input", "select", "textarea"):
                continue
            head = st.jsx_attrs(raw, n)
            ln = _line(raw, n.start)
            if (m := _EMPTY_JSX_HANDLER.search(head)):
                out.append((ln, f"<{n.name}> {m.group(1)} does nothing (an empty handler): wire it or remove the control."))
                continue
            if re.search(r"\{\s*\.\.\.", head) or re.search(r"\bref=", head):
                continue
            if n.name == "button":
                if _HANDLER.search(head) or re.search(r"""\btype=["']submit|\bform=|\bdisabled(?:[\s/>]|=\{?true)""", head):
                    continue
                label = re.sub(r"\s+", " ", re.sub(r"<[^>]*>|\{[^}]*\}", " ", raw[n.head_end:n.end])).strip()
                label = label.removesuffix("</button>").strip()[:40]
                out.append((ln, f"<button>{label and ' ' + repr(label)} has no handler: it does nothing when clicked. "
                            f"Wire it (onClick) or remove it."))
            elif re.search(r"\bdefault(?:Value|Checked)\b", head) and not _HANDLER.search(head) \
                    and not re.search(r"\bname=", head):
                out.append((ln, f"<{n.name}> with a default value and no onChange, name or ref: "
                            f"changing it saves nothing. Load and save it (controlled value + onChange)."))
    elif lang == "kotlin":
        tree = None
        for m in _EMPTY_KT.finditer(code):
            what = m.group(1) or m.group(2)
            tree = tree or _braces(code, project, rel)
            k = st.innermost(tree, m.start(), ("(",))
            call = code[tree[k].start:tree[k].end] if k >= 0 else ""
            if re.search(r"\bindication\s*=\s*null|\benabled\s*=\s*false", call):
                continue  # a tap-eater over a gesture layer, or a deliberately disabled label
            out.append((_line(code, m.start()), f"{what} with an empty lambda: the control looks live and does nothing. "
                        f"Wire it, or drop the handler so it reads as static."))
    elif lang == "qml":
        for m in _EMPTY_QML.finditer(code):
            out.append((_line(code, m.start()), f"{m.group(1)}: {{}} does nothing: wire it or remove the handler."))
    return out


# --- qml-anchors-in-positioner ------------------------------------------------------

_POSITIONERS = {
    # positioner -> anchors it overrides (Qt: "Cannot specify ... anchors for items inside Column")
    "Column": {"fill", "centerIn", "top", "bottom", "verticalCenter", "baseline"},
    "Row": {"fill", "centerIn", "left", "right", "horizontalCenter"},
    "Grid": {"fill", "centerIn", "top", "bottom", "verticalCenter", "baseline", "left", "right", "horizontalCenter"},
    "Flow": {"fill", "centerIn", "top", "bottom", "verticalCenter", "baseline", "left", "right", "horizontalCenter"},
    "ColumnLayout": {"*"}, "RowLayout": {"*"}, "GridLayout": {"*"}, "StackLayout": {"*"},
}
_NOT_ITEMS = {"Repeater", "Timer", "Connections", "Binding", "Component", "QtObject", "TapHandler", "HoverHandler",
              "DragHandler", "WheelHandler", "PointHandler", "PinchHandler", "Behavior", "NumberAnimation",
              "SequentialAnimation", "ParallelAnimation", "PropertyAnimation", "ColorAnimation", "Shortcut",
              "ListModel", "ListElement", "State", "Transition", "Instantiator", "Keys", "Accessible", "Loader"}


def _qml_type(header: str) -> str:
    """`Rectangle`, `delegate: Rectangle`, `T.Button` -> the object's type ("" if not an object)."""
    m = re.search(r"(?:^|[\s:])((?:[A-Za-z_]\w*\.)*[A-Z]\w*)\s*$", header.strip())
    if not m or re.search(r"\b(function|if|else|for|while|switch|case|try|catch|return)\b", header):
        return ""
    return m.group(1)


def _qml_prop(header: str) -> str:
    """`delegate: Rectangle` -> delegate; a bare `Rectangle` -> ''."""
    m = re.match(r"\s*([a-z]\w*(?:\.\w+)*)\s*:", st.head_line(header))
    return m.group(1) if m else ""


def qml_anchors_in_positioner(rel, raw, code, o, project=None):
    if _lang(rel) != "qml" or "anchors" not in code or not re.search(r"\b(?:Column|Row|Grid|Flow|\w+Layout)\s*\{", code):
        return []
    tree = _braces(code, project, rel)
    out = []

    def positioned(k):
        """Children a positioner lays out: bare child objects, and a Repeater's delegate."""
        for c in tree[k].children:
            ch = tree[c]
            if ch.kind != "{":
                continue
            typ = _qml_type(ch.name)
            if not typ:
                continue
            prop = _qml_prop(ch.name)
            if typ.split(".")[-1] == "Repeater" and not prop:
                for d in ch.children:
                    dn = tree[d]
                    if dn.kind == "{" and _qml_type(dn.name) and _qml_prop(dn.name) in ("", "delegate"):
                        yield d
            elif not prop:
                yield c

    for k, n in enumerate(tree):
        if n.kind != "{":
            continue
        typ = _qml_type(n.name).split(".")[-1]
        if typ not in _POSITIONERS:
            continue
        banned = _POSITIONERS[typ]
        for c in positioned(k):
            ctyp = _qml_type(tree[c].name).split(".")[-1]
            if ctyp in _NOT_ITEMS:
                continue
            body = st.own_text(code, tree, c)
            used = set(re.findall(r"(?m)^\s*anchors\.(\w+)\s*:", body))
            for g in tree[c].children:  # anchors { fill: parent }
                if tree[g].kind == "{" and st.head_line(tree[g].name).startswith("anchors"):
                    used |= set(re.findall(r"(\w+)\s*:", code[tree[g].start:tree[g].end]))
            used = {a for a in used if not a.endswith("Margin") and a not in ("margins", "alignWhenCentered")}
            bad = sorted(used if "*" in banned else used & banned)
            if bad:
                out.append((_line(code, tree[c].start), f"{ctyp} inside a {typ} sets anchors.{', anchors.'.join(bad)}: "
                            f"the {typ} positions its children, so Qt refuses the anchor and stops laying out. "
                            + ("Use Layout.* properties instead." if typ.endswith("Layout")
                               else "Size it explicitly, or use TapHandler/HoverHandler instead of a filling MouseArea.")))
    return out


# --- kotlin-init-order --------------------------------------------------------------

_KT_PROP = re.compile(r"^\s*(?:(?:private|internal|protected|public|override|open|final|@\w+(?:\([^)]*\))?)\s+)*"
                      r"(lateinit\s+)?(?:val|var)\s+(?:<[^>]*>\s*)?([A-Za-z_]\w*)\b")
_KT_FUN = re.compile(r"\bfun\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?([A-Za-z_]\w*)\s*\(")


def kotlin_init_order(rel, raw, code, o, project=None):
    if _lang(rel) != "kotlin":
        return []
    if not re.search(r"(?m)^\s*init\s*\{", code):
        return []
    depth_limit = int(o.get("depth", 2))
    tree = _braces(code, project, rel)
    out = []
    for k, n in enumerate(tree):
        if n.kind != "{" or not re.search(r"\b(class|object)\b", st.strip_groups(n.name)) \
                or re.search(r"\b(fun|interface)\b", st.head_line(n.name)):
            continue
        body_children = [c for c in n.children if tree[c].kind == "{"]
        # properties declared at this class's own level, with their offsets
        props: dict[str, int] = {}
        pos = n.start + 1
        inits, funs = [], {}
        for c in body_children + [None]:
            seg_end = tree[c].start if c is not None else n.end - 1
            seg = code[pos:seg_end]
            for m in re.finditer(r"(?m)^.*$", seg):
                line = m.group(0)
                pm = _KT_PROP.match(line)
                rest = line[pm.end():] if pm else ""
                if pm and not pm.group(1) and "get()" not in rest and re.search(r"(?<![=!<>])=(?!=)|\bby\b", rest) \
                        and st.innermost(tree, pos + m.start() + pm.start(2), ("{", "(")) == k:
                    props.setdefault(pm.group(2), pos + m.start())
            if c is not None:
                head = st.head_line(tree[c].name)
                if head == "init":
                    inits.append(c)
                fm = _KT_FUN.search(head)
                if fm:
                    funs.setdefault(fm.group(1), []).append(c)
                pos = tree[c].end
        if not inits or not props:
            continue
        for c in inits:
            seen, frontier = set(), [(c, [])]
            used: dict[str, list[str]] = {}
            for _ in range(depth_limit + 1):
                nxt = []
                for b, path in frontier:
                    if b in seen:
                        continue
                    seen.add(b)
                    # a function's header too: `fun f() = synchronized(lock) { ... }`
                    text = code[(tree[b].hstart if b != c else tree[b].start):tree[b].end]
                    # names the block declares itself (locals, lambda and function parameters) shadow the class's
                    local = set(re.findall(r"\b(?:val|var)\s+([A-Za-z_]\w*)", text))
                    local |= set(re.findall(r"\{\s*([A-Za-z_][\w\s,:<>?]*?)\s*->", text))
                    local |= {w.strip() for grp in re.findall(r"\{\s*([\w\s,]+)->", text) for w in grp.split(",")}
                    local |= set(re.findall(r"([A-Za-z_]\w*)\s*:", tree[b].name))
                    for w in set(re.findall(r"(?<![\w.])([A-Za-z_]\w*)\b", text)) - local:
                        if w in props and props[w] > tree[c].start:
                            used.setdefault(w, path)
                        if w in funs:
                            for fb in funs[w]:
                                nxt.append((fb, path + [w]))
                frontier = nxt
            for w, path in sorted(used.items(), key=lambda kv: props[kv[0]]):
                via = f" (through {' -> '.join(path)})" if path else ""
                out.append((_line(code, tree[c].start), f"init uses {w}{via}, declared below it at line "
                            f"{_line(code, props[w])}: Kotlin initialises in source order, so {w} is still null/zero "
                            f"when init runs. Declare it above init."))
    return out


# --- id-kind-mix --------------------------------------------------------------------

_OPS = {"kotlin": r"\?:", "js": r"\?\?|\|\|", "jsx": r"\?\?|\|\|", "elixir": r"\|\|"}
# a named id (`notice.requestId`, `metadata["challenge_id"]`, `challengeId`) or a
# bare `.id` of some object (`dest.id`)
_ID_TOKEN = re.compile(r"""(?:\b([A-Za-z_]\w*)\s*(?:\??\.\s*|\[\s*["':]+))?\b([A-Za-z_]\w*?)_?(?:Id|_id|ID)\b(?!\s*\()"""
                       r"""|\b([A-Za-z_]\w*)\s*\??\.\s*id\b(?!\s*\()""")
# generic holders: `row.id` says nothing about what kind of id it is
_NO_OWNER = {"it", "this", "self", "item", "x", "e", "data", "props", "p", "opts", "args", "params", "attrs",
             "payload", "body", "json", "obj", "m", "v", "value", "row", "entry", "record", "result", "res", "hit",
             "el", "element", "model", "ref", "node", "n", "r", "t", "a", "b", "s", "o", "d", "i", "state", "ctx"}
GENERIC_WORDS = {"db", "local", "remote", "server", "client", "new", "old", "existing", "current", "selected",
                 "active", "raw", "primary", "stored", "preferred", "requested", "explicit", "default", "my", "the",
                 "to", "from", "query", "api", "main", "last", "next", "prev", "target", "source", "parent", "child",
                 "root", "base", "focus", "picked", "chosen", "real", "stable", "external", "internal", "public"}


def _words(name: str) -> set[str]:
    parts = re.findall(r"[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])", name.replace("_", " "))
    return {w.lower() for w in parts} - GENERIC_WORDS


def _ids(text: str, a: int, b: int) -> list[tuple[str, str, bool]]:
    """(owner, kind, named) for each id an operand names, in order."""
    out = []
    for m in _ID_TOKEN.finditer(text, a, b):
        if m.group(2) is not None:
            out.append((m.group(1) or "", m.group(2), True))
        elif m.group(3) not in _NO_OWNER:
            out.append((m.group(3), m.group(3), False))
    return out


_NOT_A_FALLBACK = re.compile(r"(?:^|[^!=<>])!(?!=)|===?|!==?|<=|>=|\$|#\{|`|\binstanceof\b|\btypeof\b")


def _left_start(text: str, p: int, ops: re.Pattern) -> int:
    """Where the operand left of the operator at p begins: an unbalanced
    opener, `,` `;` `=`, the previous chain operator, or a line break (unless
    the operator's own line continues the chain: Kotlin's `\n    ?: b`)."""
    depth, i = 0, p - 1
    while i >= 0:
        c = text[i]
        if c in ")]}":
            depth += 1
        elif c in "([{":
            if depth == 0:
                return i + 1
            depth -= 1
        elif depth == 0:
            if c in ",;=":
                return i + 1
            if c == "\n" and text[i + 1:p].strip():
                return i + 1  # a statement ended here; the operator is mid-line
            if (om := ops.match(text, i)) and om.end() <= p:
                return om.end()
        i -= 1
    return 0


def _right_end(text: str, p: int, ops: re.Pattern) -> int:
    depth, i = 0, p
    while i < len(text):
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                return i
            depth -= 1
        elif depth == 0 and (c in ",;" or c == "\n" or ops.match(text, i)):
            return i
        i += 1
    return i


def _mixes(left, right, named_pairs: bool) -> str | None:
    """Why two operands' ids are different things (None: they are not)."""
    (lo, lk, ln), (ro, rk, rn) = left, right
    if ln and rn:
        if not named_pairs:
            return None
        wl, wr = _words(lk), _words(rk)
        return None if not wl or not wr or wl & wr else f"a {lk!r} id with a {rk!r} id"
    if ln == rn:
        return None  # two bare `.id`s: nothing says what either is
    named, bare = (left, right) if ln else (right, left)
    if bare[0] == named[0]:
        return None  # one record's own id standing in for its other id
    wn, wb = _words(named[1]), _words(bare[0])
    if not wn or not wb or wn & wb:
        return None
    return f"a {named[1]!r} id with {bare[0]}.id"


def id_kind_mix(rel, raw, code, o, project=None):
    lang = _lang(rel)
    if lang not in _OPS:
        return []
    ops = re.compile(_OPS[lang])
    named_pairs = bool(o.get("named", False))
    # strings stay (metadata["challenge_id"]), comments go
    if project is not None:
        from .core import _text_for
        text = "\n".join(_text_for(project, rel, "nocomment"))
    else:
        from .scan import blank
        text, _ = blank(raw, "js" if lang in ("js", "jsx") else lang, keep_strings=True)
    out, done = [], set()
    for m in ops.finditer(text):
        p = m.start()
        ln = _line(text, p)
        if ln in done:
            continue
        la, rb = _left_start(text, p, ops), _right_end(text, m.end(), ops)
        lt, rt = text[la:p], text[m.end():rb]
        if _NOT_A_FALLBACK.search(lt) or _NOT_A_FALLBACK.search(rt):
            continue
        left, right = _ids(text, la, p), _ids(text, m.end(), rb)
        if not left or not right:
            continue
        why = _mixes(left[-1], right[0], named_pairs)
        if why:
            done.add(ln)
            out.append((ln, f"one fallback chain mixes {why}: when the first is missing the second stands in for it, "
                        f"and whoever reads it looks up the wrong thing (a sign-in challenge id passed as the request "
                        f"id said \"Request unavailable\"). Pass each id under its own name."))
    return out


# --- ets-owner ----------------------------------------------------------------------

_OWNER_FUNCS = re.compile(r"^(init|start|handle_\w+|terminate|code_change)$")
_PROCESS = re.compile(r"^\s*use\s+(?:GenServer|Supervisor|DynamicSupervisor|Agent|GenStage|Application|Oban\.Worker)\b"
                      r"|@behaviou?r\s+(?:GenServer|:gen_server|Supervisor|Application)\b", re.M)


def _is_owner(project, g, owners) -> bool:
    """A process callback (init, handle_*) of a module that is a process."""
    if not owners.match(g.name):
        return False
    return bool(_PROCESS.search(project.text(g.file)))


def _owner_path(project, f, owners, depth, tests) -> tuple[bool, str]:
    """(some path into f starts at a process callback, a product caller that isn't one)."""
    seen, frontier, culprit = {f.id}, [f], ""
    for _ in range(depth):
        nxt = []
        for g in frontier:
            for c, _, ex in project.redges.get(g.id, []):
                h = project.funcs.get(c)
                if not ex or h is None or c in seen or tests(h.file):
                    continue
                seen.add(c)
                if _is_owner(project, h, owners):
                    return True, ""
                if not culprit and (h.module != f.module or h.kind != "private"):
                    culprit = f"{h.module}.{h.name}"
                nxt.append(h)
        frontier = nxt
    return False, culprit


def ets_owner(rel, raw, code, o, project=None):
    if _lang(rel) != "elixir" or project is None:
        return []
    owners = re.compile(o.get("owners", _OWNER_FUNCS.pattern))
    tests = re.compile(o.get("tests", r"(^|/)test/|_test\.exs$")).search
    out = []
    mods = project.by_file.get(rel, [])
    for m in re.finditer(r":ets\.new\(", code):
        ln = _line(code, m.start())
        args = code[m.end():_balanced_end(code, m.end(), "")]
        if "heir" in args:
            continue
        f = None
        for mod in mods:
            f = mod.func_at(ln) or f
        if f is None or _is_owner(project, f, owners):
            continue
        owned, culprit = _owner_path(project, f, owners, int(o.get("depth", 5)), tests)
        if owned:
            continue
        who = f"reached from {culprit}" if culprit else "no process's init or handle_* reaches it"
        out.append((ln, f"ETS table created in {f.name} ({who}): a table dies with the process that made it, so "
                    f"whichever request comes first owns the cache and takes it down when it ends (PMTiles: \"the table "
                    f"identifier does not refer to an existing ETS table\"). Create it in a supervised owner's init "
                    f"(e.g. a small GenServer under the application) or pass :heir."))
    return out


# --- handle-info-catch-all ----------------------------------------------------------

_HANDLE_INFO = re.compile(r"^\s*def\s+handle_info\(", re.M)


def handle_info_catch_all(rel, raw, code, o, project=None):
    """A process that subscribes to a shared topic and matches handle_info
    clauses only on the messages it expects crashes on the first one it
    doesn't (Stocks and Weather on an auto-approve broadcast). With no
    handle_info at all, GenServer's default logs and ignores; once you define
    one, end with `def handle_info(msg, state)` that logs and ignores."""
    if _lang(rel) != "elixir":
        return []
    sub = re.compile(o.get("subscribe", r"\bsubscribe\(|:pg\.join\(|Registry\.register\("))
    mods = project.by_file.get(rel, []) if project else []
    own = False  # the process subscribes itself (init, a callback, a private helper), not a client API for callers
    for m in sub.finditer(code):
        f = None
        for mod in mods:
            f = mod.func_at(_line(code, m.start())) or f
        if f is None or f.kind == "private" or re.match(r"(init|handle_\w+|mount|join)$", f.name):
            own = True
            break
    if not own:
        return []
    heads = list(_HANDLE_INFO.finditer(code))
    if not heads:
        return []
    for m in heads:
        a = m.end()
        first = code[a:_balanced_end(code, a, ",)")].strip()
        rest = code[a:_balanced_end(code, a, "")]
        tail = code[a + len(rest):a + len(rest) + 40]
        if re.fullmatch(r"_?\w*", first) and not re.match(r"\)\s*when\b", tail):
            return []
    return [(_line(code, heads[0].start()), "subscribes to a topic, and no handle_info clause matches any message: "
             "the first message it doesn't expect (a shared topic carries others' too) is a FunctionClauseError "
             "that crashes it. End with `def handle_info(msg, state)` that logs and ignores.")]


# --- CSS: popups that can't be seen -----------------------------------------------

def _sheet(project, o):
    from . import core, css
    globs = o.get("css", ["**/*.css"])
    key = ("css-sheet", tuple(globs))
    hit = project.text_cache.get(key)
    if hit is None:
        files = [p for p in project.files if core.match_any(p, globs) and p.endswith(".css")]
        hit = css.index([(f, project.text(f)) for f in files])
        project.text_cache[key] = hit
    return hit


_STYLE_KEYS = re.compile(r"""\b(overflow|overflowX|overflowY|position|transform)\s*:\s*["']([^"']+)["']""")


def _class_attr(head: str) -> list[str]:
    """Class names a JSX opening tag's className can carry (string, template, cn(...) parts)."""
    from . import css
    m = re.search(r"\bclassName=", head)
    if not m:
        return []
    a = m.end()
    if a < len(head) and head[a] in "\"'":
        end = head.find(head[a], a + 1)
        return head[a + 1:end if end > 0 else len(head)].split()
    if a < len(head) and head[a] == "{":
        return css.classes_in(head[a:_balanced_end(head, a + 1, "") + 1])
    return []


def _el_props(sheet, raw, n):
    """A JSX element's classes and effective box properties (prop -> (value, rule, class)):
    its classes' base CSS, then its inline style."""
    head = st.jsx_attrs(raw, n)
    cls = _class_attr(head)
    props: dict = {}
    for c in cls:
        for k, (val, rule) in sheet.props(c).items():
            props[k] = (val, rule, c)
    sm = re.search(r"\bstyle=\{\{", head)
    if sm:
        for k, val in _STYLE_KEYS.findall(head[sm.end():]):
            k = re.sub(r"([A-Z])", lambda x: "-" + x.group(1).lower(), k)
            props[k] = (val, None, "style")
    return cls, props


def _flat(props: dict) -> dict:
    return {k: (v[0],) for k, v in props.items()}


def css_popup_clipped(rel, raw, code, o, project=None):
    """A popup (an absolutely positioned box placed outside its containing
    block: `bottom: calc(100% + 4px)`) with an ancestor at or above that
    containing block that clips overflow: it opens cropped to a sliver."""
    if _lang(rel) != "jsx" or project is None:
        return []
    from . import css
    sheet = _sheet(project, o)
    nodes = _jsx(raw, project, rel)
    out = []
    cache = {}

    def props(k):
        if k not in cache:
            cache[k] = _el_props(sheet, raw, nodes[k])
        return cache[k]

    for k, n in enumerate(nodes):
        if n.kind != "el" or "className=" not in raw[n.start:n.head_end]:
            continue
        cls, p = props(k)
        how = css.escapes(_flat(p))
        if not how:
            continue
        pop = next((c for c in cls if css.escapes(_flat(sheet.props(c)))), cls[0] if cls else n.name)
        chain = [a for a in st.ancestors(nodes, k) if nodes[a].kind == "el"]
        cb = None
        for a in chain:
            acls, ap = props(a)
            why = css.containing_block(_flat(ap))
            if why:
                cb = (a, acls, why)
                break
        if cb is None:
            continue  # its containing block is outside this file: nothing here to judge
        for a in chain[chain.index(cb[0]):]:
            acls, ap = props(a)
            clip = css.clips(_flat(ap))
            if clip:
                src = next((v for key, v in ap.items() if key.startswith("overflow") or key == "contain"), None)
                where = f" ({src[1].file}:{src[1].line})" if src and src[1] else ""
                holder = "." + src[2] if src and src[2] != "style" else f"<{nodes[a].name} style>"
                anchor = "." + cb[1][0] if cb[1] else f"<{nodes[cb[0]].name}>"
                out.append((_line(raw, n.start), f".{pop} opens outside its box ({how}), but {holder} "
                            f"({clip}){where} sits at or above its containing block {anchor} ({cb[2]}) and crops it "
                            f"to a sliver. Drop the clip, or move the popup out of the clipping box."))
                break
    return out


def css_overlay_order(rel, raw, code, o, project=None):
    """A full-cover overlay (a modal, an approval, a scrim) must draw above the
    shell's own layers (dock, rails, headers). Reported where the overlay's
    z-index is set."""
    if not rel.endswith(".css") or project is None:
        return []
    from . import css
    sheet = _sheet(project, o)
    over = re.compile(o.get("overlays", r"(?i)overlay|modal|dialog|scrim|backdrop|lightbox"))
    shell_rx = re.compile(o.get("shell", r"(?i)(^|-)(dock|rail|header|footer|toolbar|topbar|tabbar|nav|hub)(-|$)"))
    above = o.get("above")
    min_z = int(o.get("min_shell_z", 10))
    layers = []
    for c in sheet.base:
        p = sheet.props(c)
        z = css.z_index({k: (v,) for k, (v, _) in p.items()})
        pos = p.get("position", ("",))[0]
        if z is None or pos not in ("fixed", "absolute"):
            continue
        layers.append((c, z, p))
    if above:
        shell = [(c, z) for c, z, p in layers if c in above]
    else:
        shell = [(c, z) for c, z, p in layers if shell_rx.search(c) and z >= min_z
                 and not css.covers({k: (v,) for k, (v, _) in p.items()})]
    if not shell:
        return []
    top_c, top_z = max(shell, key=lambda t: t[1])
    out = []
    for c, z, p in layers:
        flat = {k: (v,) for k, (v, _) in p.items()}
        if not over.search(c) or not css.covers(flat) or p.get("pointer-events", ("",))[0] == "none":
            continue
        under = [(sc, sz) for sc, sz in shell if sz > z and sc != c]
        if not under:
            continue
        rule = p["z-index"][1]
        if rule.file != rel:
            continue
        names = ", ".join(f".{sc} ({sz})" for sc, sz in sorted(under, key=lambda t: -t[1])[:3])
        out.append((rule.line, f".{c} covers the screen at z-index {z}, under {names}: it opens behind them "
                    f"(the approval drew under the chat dock). Raise it above {top_z} (.{top_c})."))
    return out


# --- elixir-rescue-debug --------------------------------------------------------------

_LOUD = re.compile(r"\bLogger\.(?:warning|warn|error|info|notice|critical|alert|emergency)\b|\b(?:re)?raise\b|"
                   r":telemetry\.|\{:error\b|\bthrow\b|\bexit\(")


def elixir_rescue_debug(rel, raw, code, o, project=None):
    """A rescue (or catch) whose only trace is Logger.debug: production logs
    drop debug, so the failure vanishes. Hub Spotify (2026-10-14): saving the
    last device raised on every poll (to_string/1 on a map), the poll's rescue
    logged it at debug, and "resume on the last device" silently never worked."""
    if not rel.endswith((".ex", ".exs")) or "Logger.debug" not in code:
        return []
    lines = code.split("\n")
    out = []
    for i, line in enumerate(lines):
        m = re.match(r"^(\s*)(rescue|catch)\b", line)
        if not m:
            continue
        indent = len(m.group(1))
        body = []
        for j in range(i + 1, len(lines)):
            nxt = lines[j]
            if nxt.strip() and len(nxt) - len(nxt.lstrip()) <= indent:
                break
            body.append((j, nxt))
        text = "\n".join(b for _, b in body)
        if "Logger.debug" in text and not _LOUD.search(text):
            ln = next(j for j, b in body if "Logger.debug" in b)
            out.append((ln + 1, "this rescue only logs at debug, which production drops: the failure is invisible "
                                "(Spotify's last-device save raised on every poll and nobody saw it). Log at "
                                "warning, or return the error."))
    return out


LINTS = {
    "disabled-by-data": disabled_by_data,
    "inert-control": inert_control,
    "qml-anchors-in-positioner": qml_anchors_in_positioner,
    "kotlin-init-order": kotlin_init_order,
    "id-kind-mix": id_kind_mix,
    "ets-owner": ets_owner,
    "handle-info-catch-all": handle_info_catch_all,
    "css-popup-clipped": css_popup_clipped,
    "css-overlay-order": css_overlay_order,
    "elixir-rescue-debug": elixir_rescue_debug,
}
DEFAULT_IN = {
    "disabled-by-data": ["**/*.kt", "**/*.tsx", "**/*.jsx", "**/*.qml"],
    "inert-control": ["**/*.kt", "**/*.tsx", "**/*.jsx", "**/*.qml"],
    "qml-anchors-in-positioner": ["**/*.qml"],
    "kotlin-init-order": ["**/*.kt"],
    "id-kind-mix": ["**/*.kt", "**/*.ts", "**/*.tsx", "**/*.js", "**/*.ex"],
    "ets-owner": ["**/*.ex"],
    "handle-info-catch-all": ["**/*.ex"],
    "css-popup-clipped": ["**/*.tsx", "**/*.jsx"],
    "css-overlay-order": ["**/*.css"],
    "elixir-rescue-debug": ["**/*.ex"],
}
OPTIONS = {
    "disabled-by-data": {"status", "busy", "targets"},
    "inert-control": set(),
    "qml-anchors-in-positioner": set(),
    "kotlin-init-order": {"depth"},
    "id-kind-mix": {"named"},
    "ets-owner": {"owners", "depth", "tests"},
    "handle-info-catch-all": {"subscribe"},
    "css-popup-clipped": {"css"},
    "css-overlay-order": {"css", "overlays", "shell", "above", "min_shell_z"},
    "elixir-rescue-debug": set(),
}


# --- essential controls -------------------------------------------------------------

MENUS = r"(?i)\b(?:\w*Menu|Popover|Popup|Dropdown\w*|Dialog|Drawer|BottomSheet|ModalBottomSheet|Tooltip)\b|\b(?:v-menu|menu|popover|dropdown)\b"
_KT_BRANCH = re.compile(r"^(?:if\b|else\b|when\b)|->$|\?\.let$|\.let$|\.also$|AnimatedVisibility\b")


def essential(project, r) -> list[tuple[str, int, str]]:
    """Declared controls render on their bar, always.

    [[rule]] kind = "essential", in = [one file], bar = regex (marks the bar
    element / block: a className, an id, a function header), and
    controls = { name = 'marker regex' | { marker = '...', when = 'regex',
    disabled = false } }. Each control must exist, sit inside the bar, and
    have no conditional ancestor between it and the bar (`{cond && ...}`,
    `cond ? ... : ...`, `if (...) {`, `visible: ...`) and no menu ancestor
    (`menus`, a regex over tags / headers / class names). `when`: a condition
    allowed for that control (Abort shows only while busy). `disabled =
    false`: the control may never be disabled (Auto-approve while busy was
    the bug). Returns (file, line, message)."""
    out = []
    menus = re.compile(r.get("menus", MENUS))
    for rel in r["in"]:
        path = os.path.join(project.root, rel)
        if not os.path.isfile(path):
            out.append((rel, 0, f"essential-controls file is gone: {rel}"))
            continue
        raw = project.text(rel)
        lang = _lang(rel)
        bm = re.search(r["bar"], raw)
        if not bm:
            out.append((rel, 0, f"bar {r['bar']!r} not found in {rel}"))
            continue
        for name, spec in r["controls"].items():
            spec = {"marker": spec} if isinstance(spec, str) else spec
            msgs = []
            hits = list(re.finditer(spec["marker"], raw))
            if not hits:
                out.append((rel, _line(raw, bm.start()), f"essential control {name!r} is gone (no {spec['marker']!r})"))
                continue
            for hm in hits:
                if lang in ("jsx",):
                    why = _essential_jsx(raw, bm, hm, spec, menus)
                else:
                    from .scan import blank
                    code, _ = blank(raw, "kotlin" if lang == "kotlin" else "js")
                    why = _essential_braces(code, raw, bm, hm, spec, menus, lang)
                if not why:
                    break
                msgs.append((_line(raw, hm.start()), why))
            else:
                ln, why = msgs[0]
                out.append((rel, ln, f"essential control {name!r} {why}: it must sit on the bar unconditionally, "
                            f"or it can't be reached when it matters (an approval waiting mid-turn)."))
    return out


def _essential_jsx(raw, bm, hm, spec, menus):
    nodes = st.jsx_tree(raw)
    bar = st.innermost(nodes, bm.start(), ("el",))
    ctl = st.innermost(nodes, hm.start(), ("el",))
    if bar < 0 or ctl < 0:
        return "is not inside a JSX element"
    head = st.jsx_attrs(raw, nodes[ctl])
    if spec.get("disabled") is False and re.search(r"\bdisabled(?:=|[\s/>])", head):
        return "can be disabled (" + re.search(r"\bdisabled(?:=\{[^}]*\}|[^\s/>]*)", head).group(0) + ")"
    chain = st.ancestors(nodes, ctl)
    if bar not in chain and bar != ctl:
        return "is not inside the bar"
    for a in chain[:chain.index(bar)] if bar in chain else []:
        n = nodes[a]
        if n.kind == "expr":
            cond = raw[n.start + 1:nodes[ctl].start if not n.children else nodes[n.children[0]].start]
            cond = re.sub(r"\s+", " ", cond).strip()[:60]
            if spec.get("when") and re.search(spec["when"], cond):
                continue
            return f"renders only when {{{cond} ...}}"
        if n.kind == "el" and (menus.search(n.name) or menus.search(st.jsx_attrs(raw, n))):
            return f"is inside a menu (<{n.name} {st.jsx_attrs(raw, n)[len(n.name) + 1:][:50].strip()}...)"
    return ""


def _essential_braces(code, raw, bm, hm, spec, menus, lang):
    nodes = st.brace_tree(code)
    # the bar is the block whose header the marker is in (`fun VoiceChatComposer(...) {`),
    # else the innermost block around it (`id: composerRow` inside `Row {`)
    def opens(n) -> bool:  # nothing but the rest of this one statement between the marker and `{`
        rest, depth = [], 0
        for ch in code[bm.end():n.start]:
            if ch == "(":
                depth += 1
            elif ch == ")":
                if depth == 0:
                    rest = []  # the marker sat inside this group (`fun X(` ... `)`)
                else:
                    depth -= 1
            elif depth == 0:
                rest.append(ch)
        return "\n" not in "".join(rest).rstrip()
    heads = [k for k, n in enumerate(nodes) if n.kind == "{" and n.hstart <= bm.start() < n.start and opens(n)]
    bar = min(heads, key=lambda k: nodes[k].start - nodes[k].hstart) if heads else st.innermost(nodes, bm.start(), ("{",))
    ctl = st.innermost(nodes, hm.start(), ("{", "("))
    if bar < 0 or ctl < 0:
        return "is not inside the bar"
    chain = [ctl] + st.ancestors(nodes, ctl)
    if bar not in chain:
        return "is not inside the bar"
    for a in chain[:chain.index(bar)]:
        n = nodes[a]
        head = st.head_line(n.name)
        if lang == "qml":
            body = st.own_text(code, nodes, a) if n.kind == "{" else ""
            vm = re.search(r"(?m)^\s*visible\s*:\s*(.+)$", body)
            if vm and vm.group(1).strip() != "true" and not (spec.get("when") and re.search(spec["when"], vm.group(1))):
                return f"renders only when visible: {vm.group(1).strip()[:60]}"
            if spec.get("disabled") is False and re.search(r"(?m)^\s*enabled\s*:", body):
                return "can be disabled (enabled: ...)"
            if menus.search(head) or re.match(r"(Loader|Repeater)\b", head):
                return f"is inside {head[:50]}"
            continue
        if _KT_BRANCH.search(head):
            if spec.get("when") and re.search(spec["when"], n.name):
                continue
            cond = re.sub(r"\s+", " ", n.name.strip())[:60]
            return f"renders only when {cond}"
        if menus.search(head):
            return f"is inside a menu ({head[:50]})"
        if n.kind == "(" and spec.get("disabled") is False:
            args = code[n.start:n.end]
            if re.search(r"\benabled\s*=(?!=)", args) or "Disabled" in args:
                return "can be disabled (enabled = ...)"
    return ""


# the deploy / container / service seams (lint_infra.py)
from . import lint_infra as _infra  # noqa: E402

LINTS.update(_infra.LINTS)
DEFAULT_IN.update(_infra.DEFAULT_IN)
OPTIONS.update(_infra.OPTIONS)
# QML: what a file names is what the engine finds (lint_qml.py)
from . import lint_qml as _qml  # noqa: E402

LINTS.update(_qml.LINTS)
DEFAULT_IN.update(_qml.DEFAULT_IN)
OPTIONS.update(_qml.OPTIONS)
REPO_WIDE = set(_infra.REPO_WIDE) | _qml.REPO_WIDE
# file-local checks: their answer for a file depends only on that file (and its
# sibling: a lockfile's package.json, a .path unit's .service)
PER_FILE = set(_infra.PER_FILE) | {"qml-anchors-in-positioner", "kotlin-init-order", "id-kind-mix", "disabled-by-data",
                                       "elixir-rescue-debug"}

