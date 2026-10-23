"""GDScript adapter (Godot 4): one Module per .gd file."""
from __future__ import annotations

import os
import re

from .chains import expression, receiver, type_name
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
# declared types: `var x: Timer`, `var x := Foo.new()`, `x: Foo` parameters, `-> Foo`
_TYPED_VAR = re.compile(r"\bvar\s+([A-Za-z_]\w*)\s*:\s*([A-Z][\w.]*(?:\[[^\]]*\])?)")
_INFER_VAR = re.compile(r"\bvar\s+([A-Za-z_]\w*)\s*:?=(?!=)")
_PARAM = re.compile(r"[(,]\s*([A-Za-z_]\w*)\s*:\s*([A-Z][\w.]*)")
_CAST = re.compile(r"[^\n#]*?\sas\s+([A-Z]\w*)[ \t]*(?:#[^\n]*)?$", re.M)
_RETURNS = re.compile(r"\)\s*->\s*([A-Z][\w.]*)\s*:")


def _types(held: dict, text: str) -> None:
    for m in _TYPED_VAR.finditer(text):
        t = type_name(m.group(2))
        if t and not held.get(m.group(1), "").startswith("group:"):  # (a group binding says more than `Node`)
            held[m.group(1)] = "type:" + t
        em = re.match(r"(?:Array|PackedArray)\[\s*([A-Z][\w.]*)\s*\]$", m.group(2))
        if em:  # `var frames: Array[Frame]`: frames[i] is a Frame
            held[m.group(1) + "[]"] = "type:" + em.group(1)
        dm = re.match(r"Dictionary\[\s*[\w.]+\s*,\s*([A-Z][\w.]*)\s*\]$", m.group(2))
        if dm:  # `Dictionary[String, Layer]`: a value is a Layer
            held[m.group(1) + "[]"] = "type:" + dm.group(1)
    for m in _INFER_VAR.finditer(text):
        cast = _CAST.match(text, m.end())  # `var x := $Path/To as Type`
        if cast and m.group(1) not in held:
            held[m.group(1)] = "type:" + cast.group(1)
            continue
        ch = expression(text, m.end())
        if ch and m.group(1) not in held:
            held[m.group(1)] = "expr:" + "\x1f".join(ch)


def _module_name(path: str, text: str) -> str:
    m = _CLASS_NAME.search(text)
    return m.group(1) if m else os.path.splitext(os.path.basename(path))[0]


def _res(rel: str, path: str, res_of) -> str:
    """The module a script path names: `res://x/y.gd` from the Godot project
    `rel` is in (any folder above it may hold its project.godot), or
    `"../motion.gd"` relative to `rel` itself."""
    here = os.path.dirname(rel)
    if not path.startswith("res://"):
        return res_of("res://" + os.path.normpath(os.path.join(here, path)).replace(os.sep, "/"))
    tail = path[6:]
    while True:
        got = res_of("res://" + (here + "/" if here else "") + tail)
        if got or not here:
            return got
        here = os.path.dirname(here)


def parse(path: str, rel: str, text: str, res_of) -> list[Module]:
    """`res_of(res_path)` maps a "res://..." path to a module name (or "")."""
    code, strings = blank(text, "gdscript")
    raw = text.split("\n")
    lines = code.split("\n")
    mod = Module(_module_name(path, text), rel, len(raw), raw, lines, kind="class")
    if _CLASS_NAME.search(text):
        mod.decls = [mod.name]  # `class_name Player`: a type other scripts name
    ext = _EXTENDS.search(text)
    if ext:
        mod.parent = _res(rel, ext.group(1), res_of) if ext.group(1) else ext.group(2)
    for m in _PRELOAD.finditer(text):
        target = _res(rel, m.group(2), res_of)
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
        # a signature over several lines (`func f(\n\timage: Image,\n) -> void:`): the body after it
        hdr_end, depth = idx, 0
        for j in range(idx, min(len(lines), idx + 40)):
            depth += lines[j].count("(") - lines[j].count(")")
            hdr_end = j
            if depth <= 0:
                break
        header = " ".join(lines[idx:hdr_end + 1])
        for j in range(hdr_end + 1, len(lines)):
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
        for m in _PARAM.finditer(header):
            f.bound["" + m.group(1)] = "type:" + m.group(2)
        _types(f.bound, f.code)
        rm = _RETURNS.search(header)
        if rm:
            f.returns = type_name(rm.group(1))
        mod.funcs.append(f)
        covered.update(range(idx, end + 1))
    top_lines = [ln if i not in covered else "" for i, ln in enumerate(lines)]
    _types(mod.bound, "\n".join(top_lines))  # the script's own variables, for its every function
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
        cut = len(lines[i]) - len(t)
        for m in _CALL.finditer(t):
            qual, name = m.group(1) or "", m.group(2)
            if name in _INDIRECT and qual:
                f.calls.append(Call(qual, "", i + 1))
                continue
            if not qual and name in _KEYWORDS:
                continue
            chain = receiver(lines[i], cut + m.start(2)) if qual or t[:m.start(2)].rstrip().endswith(".") else ()
            if chain and not qual:
                qual = "."
            f.calls.append(Call(name, qual, i + 1, chain, cut + m.start(2)))
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
