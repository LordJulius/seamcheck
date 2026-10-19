"""Elixir adapter: one Module per defmodule (nested modules included)."""
from __future__ import annotations

import re

from .model import Call, Func, Module
from .scan import blank

EXT = (".ex", ".exs")
_DEFMOD = re.compile(r"^(\s*)defmodule\s+([A-Z][\w.]*)\s+do\b")
_DEF = re.compile(r"^(\s*)(defp?|defmacrop?|defguardp?|defdelegate)\s+([a-z_]\w*[?!]?)")
# ExUnit blocks are functions too (kind "test"): without them a test module had
# no calls at all and nothing could say what a test exercises. `describe` stays
# a container so the tests inside it are found one by one.
_TEST = re.compile(r"^(\s*)(test|property|setup_all|setup)\b")
_TEST_NAME = re.compile(r"""^\s*(?:test|property)\s+(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)')""")
# (segments spelled out: a greedy `[\w.]*` ate the dot of `A.{B, C}`, which then
# aliased "" to "A." and every call through B or C resolved to nothing)
_ALIAS = re.compile(r"^\s*alias\s+([A-Z]\w*(?:\.[A-Z]\w*)*)(?:\.\{([^}]*)\})?(?:\s*,\s*as:\s*([A-Z]\w*))?")
_IMPORT = re.compile(r"^\s*import\s+([A-Z]\w*(?:\.[A-Z]\w*)*|__MODULE__)")
_QCALL = re.compile(r"\b([A-Z][\w.]*|__MODULE__)\.([a-z_]\w*[?!]?)\s*\(")
_LCALL = re.compile(r"(?<![.\w:&])([a-z_]\w*[?!]?)\s*\(")
_PIPE = re.compile(r"\|>\s*(?:([A-Z][\w.]*|__MODULE__)\.)?([a-z_]\w*[?!]?)")
_CAPTURE = re.compile(r"&(?:([A-Z][\w.]*|__MODULE__)\.)?([a-z_]\w*[?!]?)/\d")
# `start_supervised!({Mod, opts})` / `start_supervised(Mod)` starts Mod: a call to its start_link
_CHILD = re.compile(r"\bstart_supervised!?\(\s*\{?\s*([A-Z][\w.]*|__MODULE__)")
_KEYWORDS = {
    "def", "defp", "defmacro", "defmacrop", "defmodule", "defstruct", "defimpl", "defprotocol", "defguard",
    "if", "unless", "case", "cond", "fn", "for", "with", "quote", "unquote", "receive", "try", "raise", "rescue",
    "catch", "after", "else", "do", "end", "when", "and", "or", "not", "in", "import", "require", "use", "alias",
    "is_nil", "is_atom", "is_binary", "is_list", "is_map", "is_integer", "is_float", "is_number", "is_tuple",
    "is_function", "is_boolean", "is_pid", "length", "hd", "tl", "elem", "put_elem", "map_size", "tuple_size",
    "byte_size", "to_string", "inspect", "send", "self", "spawn", "throw", "exit", "super", "match?",
    "sigil_r", "sigil_s", "sigil_w", "sigil_c", "abs", "max", "min", "round", "trunc", "div", "rem", "apply",
    "struct", "update_in", "put_in", "get_in", "then", "tap", "binding", "dbg",
}


def parse(path: str, rel: str, text: str, res_of=None) -> list[Module]:
    code, strings = blank(text, "elixir")
    raw = text.split("\n")
    lines = code.split("\n")
    # Module spans from indentation: a module ends at the `end` at its indent.
    mods: list[tuple[int, int, str]] = []  # (start, indent, name)
    spans: list[tuple[str, int, int, int]] = []  # (name, start, end, indent)
    stack: list[tuple[int, int, str]] = []
    for i, ln in enumerate(lines):
        m = _DEFMOD.match(ln)
        if m:
            ind = len(m.group(1))
            name = m.group(2)
            if stack and not name.startswith(stack[-1][2] + "."):
                name = f"{stack[-1][2]}.{name}"
            stack.append((i, ind, name))
            continue
        s = ln.strip()
        if s == "end" and stack:
            ind = len(ln) - len(ln.lstrip())
            if ind == stack[-1][1]:
                a, ai, nm = stack.pop()
                spans.append((nm, a, i, ai))
    for a, ai, nm in stack:
        spans.append((nm, a, len(lines) - 1, ai))
    if not spans:
        base = rel.rsplit("/", 1)[-1].rsplit(".", 1)[0]
        spans.append((base, 0, len(lines) - 1, -2))
    str_by_line: dict[int, list[str]] = {}
    for ln, s in strings:
        str_by_line.setdefault(ln, []).append(s)
    out = []
    for name, a, b, ind in spans:
        mod = Module(name, rel, len(raw), raw, lines)
        # aliases declared inside this module
        for i in range(a, b + 1):
            ln = lines[i]
            if re.match(r"^\s*alias\s.*\{[^}]*$", ln):  # alias A.{\n  B,\n  C\n}
                j = i
                while j < b and "}" not in lines[j]:
                    j += 1
                ln = " ".join(x.strip() for x in lines[i:j + 1])
            m = _ALIAS.match(ln)
            if not m:
                continue
            base, multi, as_ = m.group(1), m.group(2), m.group(3)
            if multi:
                for part in multi.split(","):
                    p = part.strip()
                    if p:
                        mod.aliases[p.split(".")[-1]] = f"{base}.{p}"
            else:
                mod.aliases[as_ or base.split(".")[-1]] = base
        mod.aliases["__MODULE__"] = name
        for i in range(a, b + 1):
            m = _IMPORT.match(lines[i])
            if m:
                mod.uses.append(_resolve_qual(m.group(1), mod))
        # functions directly inside (skip nested module bodies)
        nested = [(x, y) for (n2, x, y, i2) in spans if x > a and y <= b and n2 != name]
        i = a + 1
        while i < b:
            if any(x <= i <= y for x, y in nested):
                i += 1
                continue
            m = _DEF.match(lines[i])
            t_m = None if m else _TEST.match(lines[i])
            if t_m and not re.search(r"\bdo\s*$", lines[i]):
                t_m = None  # `setup :fun` / `test "x"` without a block: not a body
            if not m and not t_m:
                i += 1
                continue
            if t_m:
                dind = len(t_m.group(1))
                kind = "test"
                if t_m.group(2) in ("test", "property"):
                    nm = _TEST_NAME.match(raw[i])
                    label = (nm.group(1) or nm.group(2)) if nm else f"line {i + 1}"
                    fname = f"test {label}"
                else:
                    fname = t_m.group(2)
            else:
                dind = len(m.group(1))
                kind = "private" if m.group(2).endswith("p") and m.group(2) != "defdelegate" else "func"
                fname = m.group(3)
            end = i
            for j in range(i + 1, b):
                t = lines[j]
                if t.strip() == "":
                    continue
                tind = len(t) - len(t.lstrip())
                if tind < dind or (tind == dind and not t.strip().startswith(("end", "rescue", "catch", "after", "else"))):
                    break
                end = j
                if tind == dind and t.strip() == "end":
                    break
            f = Func(name, fname, rel, i + 1, end + 1, "\n".join(lines[i:end + 1]), kind=kind)
            _collect(f, mod, lines, i, end, str_by_line)
            # multiple clauses of one function: merge into the first
            prev = next((g for g in mod.funcs if g.name == f.name), None)
            if prev:
                prev.end = max(prev.end, f.end)
                prev.calls += f.calls
                prev.refs |= f.refs
                prev.code += "\n" + f.code
            else:
                mod.funcs.append(f)
            i = end + 1
        top = Func(name, "<top>", rel, a + 1, b + 1, "", kind="top")
        mod.funcs.append(top)
        out.append(mod)
    return out


def _resolve_qual(q: str, mod: Module) -> str:
    if not q:
        return ""
    head, _, rest = q.partition(".")
    if head in mod.aliases:
        return mod.aliases[head] + ("." + rest if rest else "")
    return q


def _after_head(t: str) -> str:
    """A def line without its head (`def name(args)`): the parameters aren't
    calls, the body after `do:` is. (It used to cut at the first "(", which in
    `def index, do: {App.get(1), App.emit()}` was App.get's: the call was lost.)"""
    m = _DEF.match(t) or _TEST.match(t)
    if not m:
        return t
    rest = t[m.end():]
    if rest.startswith("("):
        depth = 0
        for k, ch in enumerate(rest):
            depth += ch == "("
            depth -= ch == ")"
            if depth == 0:
                return rest[k + 1:]
        return ""  # parameters continue on the next line
    return rest


def _collect(f: Func, mod: Module, lines, a: int, b: int, str_by_line) -> None:
    for i in range(a, b + 1):
        t = lines[i]
        if i == a:
            t = _after_head(t)
        for m in _QCALL.finditer(t):
            f.calls.append(Call(m.group(2), _resolve_qual(m.group(1), mod), i + 1))
        for m in _LCALL.finditer(t):
            if m.group(1) not in _KEYWORDS:
                f.calls.append(Call(m.group(1), "", i + 1))
        for m in _PIPE.finditer(t):
            if not m.group(1) and m.group(2) in _KEYWORDS:
                continue
            f.calls.append(Call(m.group(2), _resolve_qual(m.group(1) or "", mod), i + 1))
        for m in _CAPTURE.finditer(t):
            # `&evaluate/1` / `&Mod.fun/2` handed to Enum.map & co. is a call
            f.refs.add(m.group(2))
            f.calls.append(Call(m.group(2), _resolve_qual(m.group(1) or "", mod), i + 1))
        for m in _CHILD.finditer(t):
            f.calls.append(Call("start_link", _resolve_qual(m.group(1), mod), i + 1))
        for s in str_by_line.get(i + 1, []):
            if re.fullmatch(r"[a-z_]\w*[?!]?", s):
                f.refs.add(s)
