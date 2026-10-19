"""GDScript adapter (Godot 4): one Module per .gd file."""
from __future__ import annotations

import os
import re

from .model import Call, Func, Module
from .scan import blank

EXT = (".gd",)
_FUNC = re.compile(r"^(\s*)(?:static\s+)?func\s+([A-Za-z_]\w*)")
_CALL = re.compile(r"(?:\b([A-Za-z_]\w*)\s*\.\s*)?\b([A-Za-z_]\w*)\s*\(")
_CLASS_NAME = re.compile(r"^class_name\s+(\w+)", re.M)
_EXTENDS = re.compile(r'^extends\s+(?:"([^"]+)"|(\w+))', re.M)
_PRELOAD = re.compile(r'^(?:static\s+)?(?:const|var)\s+(\w+)\s*(?::\s*\w+\s*)?:?=\s*(?:preload|load)\(\s*"([^"]+\.gd)"\s*\)', re.M)
# `var main := get_tree().get_first_node_in_group("match")` (any receiver of
# get_tree(), a type annotation or none); the group id is read from the line's
# string, since the code view has string contents blanked.
_GROUP_VAR = re.compile(r'\bvar\s+(\w+)\s*(?::\s*\w+\s*)?:?=\s*(?:[\w.]+\.)?get_tree\(\)\.get_first_node_in_group\(\s*"')
_CONNECT = re.compile(r"\.connect\(\s*([A-Za-z_]\w*)\s*[,)]")
# a function used as a value: `= fn`, `(fn,`, `, fn)`, `[fn]`, `: fn` (not followed by a call or member)
_VALUE = re.compile(r"(?:[=(,\[:]|\breturn)\s*([A-Za-z_]\w*)\s*(?=[,)\]}]|$)")
_CALLABLE = re.compile(r"Callable\(\s*[\w.]+\s*,\s*$")
_KEYWORDS = {
    "if", "elif", "else", "for", "while", "match", "return", "func", "and", "or", "not", "in", "is", "as",
    "await", "assert", "preload", "load", "print", "printerr", "push_error", "push_warning", "len", "range",
    "str", "int", "float", "bool", "Vector2", "Vector3", "Vector2i", "Vector3i", "Color", "Rect2", "Rect2i",
    "Array", "Dictionary", "PackedByteArray", "PackedStringArray", "PackedVector3Array", "PackedInt32Array",
    "PackedFloat32Array", "StringName", "NodePath", "Callable", "Transform3D", "Basis", "Quaternion", "AABB",
    "min", "max", "abs", "clamp", "clampf", "clampi", "lerp", "lerpf", "minf", "maxf", "mini", "maxi",
    "absf", "absi", "sign", "signf", "signi", "floor", "ceil", "round", "floorf", "ceilf", "roundf", "floori",
    "ceili", "roundi", "sqrt", "pow", "sin", "cos", "tan", "atan2", "deg_to_rad", "rad_to_deg", "randf",
    "randi", "randf_range", "randi_range", "wrapi", "wrapf", "snappedf", "is_instance_valid", "typeof",
    "move_toward", "posmod", "fposmod", "exp", "log", "var_to_str", "str_to_var", "hash", "super", "range",
    "Time", "OS", "Engine", "self", "lerp_angle", "smoothstep", "is_equal_approx", "is_zero_approx",
}
_INDIRECT = {"rpc", "rpc_id", "call", "call_deferred", "callv", "bind"}


def _module_name(path: str, text: str) -> str:
    m = _CLASS_NAME.search(text)
    return m.group(1) if m else os.path.splitext(os.path.basename(path))[0]


def parse(path: str, rel: str, text: str, res_of) -> list[Module]:
    """`res_of(res_path)` maps a "res://..." path to a module name (or "")."""
    code, strings = blank(text, "gdscript")
    raw = text.split("\n")
    lines = code.split("\n")
    mod = Module(_module_name(path, text), rel, len(raw), raw, lines)
    ext = _EXTENDS.search(text)
    if ext:
        mod.parent = res_of(ext.group(1)) if ext.group(1) else ext.group(2)
    for m in _PRELOAD.finditer(text):
        target = res_of(m.group(2))
        if target:
            mod.aliases[m.group(1)] = target
    # Function spans: a func runs until the next non-blank line indented at
    # or less than its own `func` line.
    starts = []
    for idx, ln in enumerate(lines):
        m = _FUNC.match(ln)
        if m:
            starts.append((idx, len(m.group(1).expandtabs(4)), m.group(2)))
    str_by_line: dict[int, list[str]] = {}
    for ln, s in strings:
        str_by_line.setdefault(ln, []).append(s)
    covered = set()
    for si, (idx, indent, name) in enumerate(starts):
        end = len(lines) - 1
        for j in range(idx + 1, len(lines)):
            t = lines[j]
            if t.strip() == "":
                continue
            ind = len(t) - len(t.lstrip())
            ind = len(t[:ind].expandtabs(4))
            if ind <= indent:
                end = j - 1
                break
        while end > idx and lines[end].strip() == "":
            end -= 1
        f = Func(mod.name, name, rel, idx + 1, end + 1, "\n".join(lines[idx:end + 1]))
        _collect(f, lines, idx, end, str_by_line)
        mod.funcs.append(f)
        covered.update(range(idx, end + 1))
    top_lines = [ln if i not in covered else "" for i, ln in enumerate(lines)]
    top = Func(mod.name, "<top>", rel, 1, len(lines), "\n".join(top_lines), kind="top")
    _collect(top, top_lines, 0, len(lines) - 1, str_by_line, skip=covered)
    mod.funcs.append(top)
    return [mod]


def _collect(f: Func, lines, a: int, b: int, str_by_line, skip=None) -> None:
    for i in range(a, b + 1):
        if skip and i in skip:
            continue
        t = lines[i]
        if i == a and f.kind != "top":
            t = t.split("(", 1)[1] if "(" in t else ""  # skip the def itself
        for m in _CALL.finditer(t):
            qual, name = m.group(1) or "", m.group(2)
            if name in _INDIRECT and qual:
                f.calls.append(Call(qual, "", i + 1))
                continue
            if not qual and name in _KEYWORDS:
                continue
            f.calls.append(Call(name, qual, i + 1))
        if f.kind != "top":
            g = _GROUP_VAR.search(t)
            if g and str_by_line.get(i + 1):
                f.bound[g.group(1)] = "group:" + str_by_line[i + 1][0]
        for m in _CONNECT.finditer(t):
            f.refs.add(m.group(1))
        for m in _VALUE.finditer(t):
            f.refs.add(m.group(1))
        for s in str_by_line.get(i + 1, []):
            if re.fullmatch(r"[A-Za-z_]\w*", s):
                f.refs.add(s)
