"""Just enough CSS to answer "where does this class's box go".

`index(texts)` reads stylesheets into per-class rules. A rule's selector is
classified by its last compound selector (the element it styles):

- base:    `.cls` alone, or in context (`.shell .cls`): applies whenever the class does
- variant: `.cls.is-open`, `.cls[data-x]`: applies only sometimes
- state:   `:hover`, `:focus`, `::before`...: transient or another box (ignored)

`props(cls)` folds base rules in source order (`!important` wins), so the
last `.hitl-overlay { position: fixed; z-index: 90 }` beats the first.
Custom properties from `:root` resolve in `z-index: var(--z-dock)`.
"""
from __future__ import annotations

import re
from dataclasses import dataclass, field

_COMMENT = re.compile(r"/\*.*?\*/", re.S)
_STATE = re.compile(r":(?!not\b|is\b|where\b|root\b)[\w-]+|::[\w-]+")


@dataclass
class Rule:
    file: str
    line: int
    selector: str
    decls: dict[str, tuple[str, bool]]  # prop -> (value, important)
    order: int


@dataclass
class Sheet:
    base: dict[str, list[Rule]] = field(default_factory=dict)
    variant: dict[str, list[Rule]] = field(default_factory=dict)
    vars: dict[str, str] = field(default_factory=dict)

    def props(self, cls: str) -> dict[str, tuple[str, Rule]]:
        """Effective base declarations of a class: prop -> (value, the rule that set it)."""
        out: dict[str, tuple[str, Rule, bool]] = {}
        for r in sorted(self.base.get(cls, []), key=lambda r: r.order):
            for p, (v, imp) in r.decls.items():
                if p in out and out[p][2] and not imp:
                    continue
                out[p] = (v, r, imp)
        return {p: (self.resolve(v), r) for p, (v, r, _) in out.items()}

    def resolve(self, value: str) -> str:
        for _ in range(4):
            m = re.search(r"var\(\s*(--[\w-]+)\s*(?:,\s*([^)]*))?\)", value)
            if not m:
                break
            value = value[:m.start()] + self.vars.get(m.group(1), (m.group(2) or "").strip()) + value[m.end():]
        return value.strip()


def _blocks(text: str, start: int, end: int):
    """(prelude, body_start, body_end) for each top-level `prelude { body }` in text[start:end]."""
    i, pre = start, start
    while i < end:
        c = text[i]
        if c == "{":
            depth, j = 1, i + 1
            while j < end and depth:
                if text[j] == "{":
                    depth += 1
                elif text[j] == "}":
                    depth -= 1
                j += 1
            yield text[pre:i].strip(), i + 1, j - 1
            i = pre = j
            continue
        if c == ";" or c == "}":
            pre = i + 1
        i += 1


def _decls(body: str) -> dict[str, tuple[str, bool]]:
    out = {}
    for part in body.split(";"):
        if ":" not in part or "{" in part:
            continue
        p, _, v = part.partition(":")
        p, v = p.strip().lower(), v.strip()
        imp = v.endswith("!important")
        out[p] = (v.removesuffix("!important").strip(), imp)
    return out


def index(texts: list[tuple[str, str]]) -> Sheet:
    """Read stylesheets [(file, text)] into a Sheet."""
    sheet = Sheet()
    order = 0

    def walk(rel, text, a, b):
        nonlocal order
        for prelude, s, e in _blocks(text, a, b):
            if prelude.startswith("@"):
                if re.match(r"@(media|supports|layer|container|document)\b", prelude):
                    walk(rel, text, s, e)
                continue  # @keyframes, @font-face
            body = text[s:e]
            if "{" in body:  # CSS nesting: take the outer declarations only
                body = re.sub(r"[^;{}]*\{[^{}]*\}", "", body)
            decls = _decls(body)
            line = text.count("\n", 0, s) + 1
            for sel in prelude.split(","):
                sel = sel.strip()
                if sel in (":root", "html", ":root:not([data-theme=\"light\"])"):
                    for p, (v, _) in decls.items():
                        if p.startswith("--"):
                            sheet.vars.setdefault(p, v)
                    continue
                last = re.split(r"\s*[ >+~]\s*", sel)[-1]
                if _STATE.search(last):
                    continue
                classes = re.findall(r"\.([\w-]+)", last)
                if not classes:
                    continue
                order += 1
                rule = Rule(rel, line, sel, decls, order)
                if len(classes) == 1 and re.fullmatch(r"\.[\w-]+", last):
                    sheet.base.setdefault(classes[0], []).append(rule)
                else:
                    for c in classes:
                        sheet.variant.setdefault(c, []).append(rule)

    for rel, text in texts:
        clean = _COMMENT.sub(lambda m: re.sub(r"[^\n]", " ", m.group(0)), text)
        walk(rel, clean, 0, len(clean))
    return sheet


def classes_in(attr_value: str) -> list[str]:
    """Class names in a JSX className value: string and template literal parts
    (`command-dock${x ? " is-minimized" : ""}` -> command-dock, is-minimized)."""
    out = []
    for lit in re.findall(r'"([^"]*)"|\'([^\']*)\'|`([^`]*)`', attr_value):
        s = next((x for x in lit if x), "")
        s = re.sub(r"\$\{[^}]*\}", " ", s)
        out += re.findall(r"-?[A-Za-z_][\w-]*", s)
    return out


CLIPS = re.compile(r"^(hidden|clip|auto|scroll)\b")
CONTAINS = re.compile(r"\b(paint|layout|strict|content)\b")
OUTSIDE = re.compile(r"^(?:calc\(\s*)?100%")


def clips(p: dict) -> str:
    for k in ("overflow", "overflow-x", "overflow-y"):
        if k in p and CLIPS.match(p[k][0]):
            return f"{k}: {p[k][0]}"
    if "contain" in p and CONTAINS.search(p["contain"][0]):
        return f"contain: {p['contain'][0]}"
    return ""


def containing_block(p: dict) -> str:
    """Why this box is the containing block of absolutely positioned descendants ('' = it isn't)."""
    pos = p.get("position", ("static",))[0]
    if pos in ("relative", "absolute", "fixed", "sticky"):
        return f"position: {pos}"
    for k in ("transform", "filter", "perspective"):
        if k in p and p[k][0] not in ("none", ""):
            return f"{k}: {p[k][0]}"
    if "contain" in p and CONTAINS.search(p["contain"][0]):
        return f"contain: {p['contain'][0]}"
    return ""


def escapes(p: dict) -> str:
    """An absolutely positioned box placed outside its containing block (`bottom: 100%`): a popup."""
    if p.get("position", ("",))[0] != "absolute":
        return ""
    for k in ("top", "bottom", "left", "right"):
        if k in p and OUTSIDE.match(p[k][0]):
            return f"{k}: {p[k][0]}"
    return ""


def covers(p: dict) -> bool:
    """A layer over its whole containing block: inset: 0, or all four edges at 0."""
    zero = lambda v: re.fullmatch(r"0(px|%|rem)?", v.strip()) is not None  # noqa: E731
    if "inset" in p and all(zero(x) for x in p["inset"][0].split()):
        return True
    edges = {k: p[k][0] for k in ("top", "bottom", "left", "right") if k in p}
    full_w = ("left" in edges and zero(edges["left"]) and (("right" in edges and zero(edges["right"]))
              or p.get("width", ("",))[0] in ("100%", "100vw")))
    full_h = ("top" in edges and zero(edges["top"]) and (("bottom" in edges and zero(edges["bottom"]))
              or p.get("height", ("",))[0] in ("100%", "100vh", "100dvh")))
    return full_w and full_h


def z_index(p: dict) -> int | None:
    v = p.get("z-index", ("",))[0]
    m = re.fullmatch(r"-?\d+", v.strip())
    return int(v) if m else None
