"""Slint adapter (`.slint`): one Module per file.

Each `component` / `global` is a Func named for it (its body is the element
tree, so the near-duplicate check reads whole element bodies), whose calls
are the elements it instantiates (`Press { }`, `rail := Rail { }`,
`inherits Base`) and the functions it calls. A `function` inside a component
or global is a Func named `Owner.name`; `Shell.navigate()` from another file
reaches `Shell.navigate` through `import { Shell } from "state.slint"`.

What Rust and the Slint compiler see, not the file: an `export`ed window,
global, struct or enum and a `public function` are the generated Rust API
(`slint::include_modules!`), so they are roots, never dead. A plain exported
component is a library part: dead when nothing instantiates it. Callbacks
and properties are interface, not code.
"""
from __future__ import annotations

import os
import re

from .lambdas import matching
from .model import Call, Func, Module
from .scan import blank

EXT = (".slint",)

_ID = r"[A-Za-z_][\w-]*"
_DECL = re.compile(rf"^[ \t]*(export[ \t]+)?(component|global)[ \t]+({_ID})(?:[ \t]+inherits[ \t]+({_ID}))?[ \t]*\{{", re.M)
_TYPE_DECL = re.compile(rf"^[ \t]*(?:export[ \t]+)?(?:struct|enum)[ \t]+({_ID})", re.M)
_FUNC = re.compile(rf"(?<![\w-])((?:(?:pure|public|protected)[ \t]+)*)function[ \t]+({_ID})[ \t]*\(")
_EXPORT_LIST = re.compile(r"^[ \t]*export[ \t]*\{([^}]*)\}[ \t]*(?:from[ \t]*\"([^\"]+)\")?[ \t]*;?", re.M)
# an element: `Name {`, `id := Name {`, `if cond: Name {`, `for x in xs: Name {`
_ELEMENT = re.compile(rf"(?:^|[;{{}}:])[ \t]*(?:{_ID}[ \t]*:=[ \t]*)?([A-Z][\w-]*(?:\.[A-Z][\w-]*)?)[ \t]*\{{", re.M)
_ID_ELEMENT = re.compile(rf"(?<![\w-])({_ID})[ \t]*:=[ \t]*([A-Z][\w-]*)[ \t]*\{{")
_CALL = re.compile(rf"(?:(?<![\w.-])({_ID})[ \t]*\.[ \t]*)?(?<![\w-])({_ID})[ \t]*\(")
_CAP = re.compile(r"(?<![\w.-])([A-Z][\w-]*)")
# functions the language provides
_BUILTIN = {"min", "max", "abs", "round", "floor", "ceil", "sqrt", "pow", "log", "exp", "sin", "cos", "tan", "asin", "acos",
            "atan", "atan2", "mod", "clamp", "rgb", "rgba", "hsv", "cubic-bezier", "linear-gradient", "radial-gradient",
            "conic-gradient", "darker", "brighter", "transparentize", "mix", "with-alpha", "to-float", "to-string", "parse-int",
            "is-float", "animation-tick", "debug", "init", "if", "for", "return", "image-url", "@image-url", "@tr",
            "@linear-gradient", "@radial-gradient", "@conic-gradient", "@children", "@debug", "fabs", "sign", "lerp",
            "length", "set-selection-offsets", "select-all", "clear-selection", "copy", "cut", "paste", "focus", "show", "hide",
            "close", "clear-focus", "has-focus", "ceiling", "trunc", "fract", "lines", "deg"}
_KEYWORDS = {"if", "for", "in", "else", "return", "animate", "states", "transitions", "when", "out", "in-out", "pure",
             "public", "protected", "callback", "property", "function", "component", "global", "struct", "enum", "import",
             "export", "inherits", "root", "self", "parent"}


def _modname(rel: str) -> str:
    return rel.rsplit(".", 1)[0]


def _where(rel: str, spec: str) -> str:
    d = rel.rsplit("/", 1)[0] if "/" in rel else ""
    return f"sl:{d}\0{spec}"


def parse(path: str, rel: str, text: str, res_of=None) -> list[Module]:
    code, _ = blank(text, "slint")
    raw, lines = text.split("\n"), code.split("\n")
    mod = Module(_modname(rel), rel, len(raw), raw, lines)
    starts = [0]
    for ln in lines[:-1]:
        starts.append(starts[-1] + len(ln) + 1)

    def line_of(pos: int) -> int:
        lo, hi = 0, len(starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if starts[mid] <= pos:
                lo = mid
            else:
                hi = mid - 1
        return lo + 1

    # `import` paths are strings, which the blanked code lost: read them from the text
    for m in re.finditer(r"""^[ \t]*import[ \t]*\{([^}]*)\}[ \t]*from[ \t]*"([^"]+)"[ \t]*;""", text, re.M):
        if not lines[text.count("\n", 0, m.start())].strip().startswith("import"):
            continue  # inside a comment
        where = _where(rel, m.group(2))
        for part in m.group(1).split(","):
            src, _, local = part.strip().partition(" as ")
            if src.strip():
                mod.imported[(local or src).strip()] = (where, src.strip())
    local_types = {m.group(1) for m in _TYPE_DECL.finditer(code)}
    mod.decls = sorted(local_types)
    top = Func(mod.name, "<top>", rel, 1, len(lines), "", kind="top")

    decls = []
    for m in _DECL.finditer(code):
        close = matching(code, m.end() - 1)
        if close < 0:
            continue
        decls.append((m, close))
    declared = {m.group(3) for m, _ in decls}
    mod.decls = sorted(set(mod.decls) | declared)
    for m in _EXPORT_LIST.finditer(code):
        if m.group(2) is None:  # `export { A, B }` of this file's own names
            for part in m.group(1).split(","):
                src = part.strip().partition(" as ")[0].strip()
                if src in declared or src in local_types:
                    top.refs.add(src)
    for tm in _TYPE_DECL.finditer(code):
        if re.match(r"[ \t]*export\b", code[tm.start():tm.end()]):
            top.refs.add(tm.group(1))

    for m, close in decls:
        exported, kind, name, base = bool(m.group(1)), m.group(2), m.group(3), m.group(4)
        body0 = m.end() - 1
        owner_name = name
        if kind == "global" and exported or exported and base in ("Window", "Dialog", "PopupWindow"):
            top.refs.add(name)  # the generated Rust API
        funcs = []  # (name, start offset, end offset, name offset, public)
        for fm in _FUNC.finditer(code, body0, close):
            fb = code.find("{", fm.end(), close)
            if fb < 0:
                continue
            fe = matching(code, fb)
            if fe < 0:
                continue
            funcs.append((fm.group(2), fm.start(), fe, fm.start(2), "public" in fm.group(1), fb))
        # the owner's own text: its function bodies are their own Funcs
        own = list(code[m.start():close + 1])
        for _n, fs, fe, _no, _pub, _fb in funcs:
            for k in range(fs - m.start(), fe - m.start() + 1):
                if own[k] != "\n":
                    own[k] = " "
        own_code = "".join(own)
        fnames = {n for n, *_ in funcs}
        ids = {i.group(1): i.group(2) for i in _ID_ELEMENT.finditer(code, body0, close)}
        cf = Func(mod.name, name, rel, line_of(m.start()), line_of(close), code[m.start():close + 1])
        _collect(cf, own_code[body0 - m.start():], body0, line_of, name, fnames, ids, mod, declared, local_types, base, kind)
        mod.funcs.append(cf)
        for n, fs, fe, no, public, fb in funcs:
            fc = code[fs:fe + 1]
            f = Func(mod.name, f"{name}.{n}", rel, line_of(fs), line_of(fe), fc, kind="func")
            _collect(f, code[fb:fe + 1], fb, line_of, name, fnames, ids, mod, declared, local_types, "", "function")
            mod.funcs.append(f)
            if public:
                top.refs.add(f.name)  # callable from Rust: `ui.invoke_name()`
    mod.funcs.append(top)
    return [mod]


def _collect(f: Func, text: str, base: int, line_of, owner: str, fnames: set[str], ids: dict[str, str],
             mod: Module, declared: set[str], local_types: set[str], inherits: str | None, kind: str) -> None:
    def at(i: int) -> int:
        return line_of(base + i)

    if inherits:
        f.calls.append(Call(inherits, "", at(0)))
    for m in _ELEMENT.finditer(text):
        el = m.group(1)
        if "." in el:  # `Shell.Sub {`: a namespaced element, its last part
            el = el.split(".")[-1]
        f.calls.append(Call(el, "", at(m.start(1))))
        f.refs.add(el)
    for m in _CAP.finditer(text):
        nm = m.group(1)
        if nm in declared or nm in local_types or nm in mod.imported:
            f.refs.add(nm)
    for m in _CALL.finditer(text):
        qual, name = m.group(1) or "", m.group(2)
        if name in _KEYWORDS and not qual or (not qual and name in _BUILTIN):
            continue
        line = at(m.start(2))
        if qual in ("root", "self", "") or qual == owner:
            if name in fnames:
                f.calls.append(Call(f"{owner}.{name}", "", line))
            continue
        if qual in ids and ids[qual] in declared | set(mod.imported):
            ty = ids[qual]
            f.calls.append(Call(f"{ty}.{name}", "", line))
            if ty in mod.imported:
                where, orig = mod.imported[ty]
                mod.imported[f"{ty}.{name}"] = (where, f"{orig}.{name}")
            continue
        if qual[:1].isupper() and (qual in declared or qual in mod.imported):
            f.calls.append(Call(f"{qual}.{name}", "", line))
            if qual in mod.imported:
                where, orig = mod.imported[qual]
                mod.imported[f"{qual}.{name}"] = (where, f"{orig}.{name}")
            continue
