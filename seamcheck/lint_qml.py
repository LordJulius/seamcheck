"""QML seams (kind = "lint"): what a QML file names must be what the engine
finds, on every Qt build. Each check reproduces a bug from elixir-pot's OS-D
(2026-10-12..10-16):

  qml-unknown-type       an object type no import, project file or C++ class
                         defines (`Sheet { }` in FoodBookDive.qml: the page
                         failed to load on the desk)
  qml-singleton-import   a file names a singleton of its own module without
                         importing the module (Qt 6.11.2 read the plain
                         Tokens.qml beside it, an empty object; 6.11.1 read the
                         singleton; the window went black on one machine)
  qml-js-shadows-type    a .js library named like a .qml type in the same
                         module (HubMove.js hid the HubMove singleton)

Modules come from CMake `qt_add_qml_module(... URI X ... QML_FILES ...)` and
from `qmldir` files; singletons from `QT_QML_SINGLETON_TYPE TRUE`, a qmldir
`singleton` line, `pragma Singleton`, and C++ `QML_SINGLETON`.
"""
from __future__ import annotations

import os
import re

from . import core

_HERE = os.path.dirname(os.path.abspath(__file__))
_IMPORT = re.compile(r"^\s*import\s+(\"[^\"]+\"|[\w.]+)(?:\s+[\d.]+)?(?:\s+as\s+(\w+))?", re.M)
# an object declaration: `Rectangle {`, `delegate: Item {`, `Behavior on x {`
_OBJECT = re.compile(r"^\s*(?:[a-z_]\w*(?:\.\w+)*\s*:\s*)?([A-Z]\w*)(?:\s+on\s+[\w.]+)?\s*\{", re.M)
_INLINE = re.compile(r"^\s*component\s+([A-Z]\w*)\s*:", re.M)
_CMAKE_MODULE = re.compile(r"qt_add_qml_module\s*\(([^)]*)\)", re.S)
_SINGLETON_PROP = re.compile(r"set_source_files_properties\s*\(([^)]*?)PROPERTIES[^)]*QT_QML_SINGLETON_TYPE\s+TRUE", re.S)
_CPP_ELEMENT = re.compile(r"\b(?:class|struct)\s+(?:\w+\s+)?(\w+)[^;{]*\{(?:(?!\b(?:class|struct)\s+\w+[^;{]*\{).)*?"
                          r"\bQML_(?:ELEMENT|SINGLETON|ANONYMOUS)\b", re.S)
_CPP_NAMED = re.compile(r"\bQML_NAMED_ELEMENT\s*\(\s*(\w+)\s*\)")
_CPP_REGISTER = re.compile(r"\bqmlRegister\w*\s*(?:<[^>]*>)?\s*\([^;]*?\"([A-Z]\w*)\"\s*[,)]")


def _qt_types() -> set[str]:
    """Every type a Qt module exports (QtQuick, Controls, Layouts, Shapes,
    WebEngine, Multimedia, ...), from Qt 6.11's qmltypes and qmldir files."""
    with open(os.path.join(_HERE, "qt_types.txt"), encoding="utf-8") as fh:
        return {line.strip() for line in fh if line.strip()}


class _Modules:
    """The project's QML modules: which files each lists, its singletons, and
    every type the project defines (QML files, inline components aside, C++)."""

    def __init__(self, project):
        root = project.root
        excl = core.DEFAULT_EXCLUDE + project.config.get("project", {}).get("exclude", [])
        self.qml = [r for r in core.list_files(root, ["**/*.qml", "**/*.js", "**/*.mjs"], excl)]
        self.module_of: dict[str, str] = {}       # file -> module URI
        self.singletons: dict[str, set[str]] = {}  # URI -> singleton type names
        self.uris: set[str] = set()
        self.types = {os.path.splitext(os.path.basename(r))[0] for r in self.qml if r.endswith(".qml")}
        for cm in core.list_files(root, ["**/CMakeLists.txt", "**/*.cmake"], excl):
            text = _read(root, cm)
            if "qt_add_qml_module" not in text and "QT_QML_SINGLETON_TYPE" not in text:
                continue
            base = os.path.dirname(cm)
            single = {os.path.splitext(os.path.basename(p.strip('"')))[0]
                      for m in _SINGLETON_PROP.finditer(text) for p in m.group(1).split() if p.strip('"').endswith(".qml")}
            for m in _CMAKE_MODULE.finditer(text):
                body = m.group(1)
                uri = re.search(r"\bURI\s+([\w.]+)", body)
                if not uri:
                    continue
                uri = uri.group(1)
                self.uris.add(uri)
                names = set()
                for tok in body.split():
                    tok = tok.strip('"')
                    if tok.endswith((".qml", ".js", ".mjs")):
                        names.add(os.path.splitext(os.path.basename(tok))[0])
                        if "$" not in tok:
                            self.module_of[os.path.normpath(os.path.join(base, tok))] = uri
                self.singletons.setdefault(uri, set()).update(single & names)
        for qd in core.list_files(root, ["**/qmldir"], excl):
            text = _read(root, qd)
            mod = re.search(r"^module\s+([\w.]+)", text, re.M)
            if not mod:
                continue
            uri, base = mod.group(1), os.path.dirname(qd)
            self.uris.add(uri)
            for line in text.splitlines():
                p = line.split()
                if len(p) >= 3 and p[-1].endswith((".qml", ".js")):
                    if p[0] == "singleton" and len(p) >= 4:
                        self.singletons.setdefault(uri, set()).add(p[1])
                    self.module_of.setdefault(os.path.normpath(os.path.join(base, p[-1])), uri)
        for rel in self.qml:
            if rel.endswith(".qml") and rel in self.module_of and re.search(r"^\s*pragma\s+Singleton\b",
                                                                            _read(root, rel), re.M):
                self.singletons.setdefault(self.module_of[rel], set()).add(
                    os.path.splitext(os.path.basename(rel))[0])
        for src in core.list_files(root, ["**/*.h", "**/*.hpp", "**/*.cpp", "**/*.cc"], excl):
            text = _read(root, src)
            if "QML_" in text or "qmlRegister" in text:
                self.types |= set(_CPP_ELEMENT.findall(text)) | set(_CPP_NAMED.findall(text)) \
                    | set(_CPP_REGISTER.findall(text))


def _read(root, rel) -> str:
    try:
        with open(os.path.join(root, rel), encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def _modules(project) -> _Modules:
    if getattr(project, "_qml_modules", None) is None:
        project._qml_modules = _Modules(project)
    return project._qml_modules


def _line(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def qml_unknown_type(rel, raw, code, o, project=None):
    if not rel.endswith(".qml") or project is None:
        return []
    mods = _modules(project)
    known = _qt_types() | mods.types | set(_INLINE.findall(raw)) | set(o.get("types", []))
    for imp, _ in _IMPORT.findall(raw):
        if imp.startswith('"') or imp.startswith(("Qt", "QML")) or imp in mods.uris:
            continue
        return []  # a module we can't see into (MapLibre, Kirigami): its types are unknown here
    out = []
    for m in _OBJECT.finditer(code):
        name = m.group(1)
        if name not in known:
            out.append((_line(code, m.start(1)), f"{name} is not a type any import, project file or C++ class "
                                                 f"defines: the page fails to load (Sheet {{ }} in FoodBookDive.qml "
                                                 f"left the desk's Food Book blank)"))
    return out


def qml_singleton_import(rel, raw, code, o, project=None):
    if not rel.endswith(".qml") or project is None:
        return []
    mods = _modules(project)
    uri = mods.module_of.get(rel)
    if not uri:
        return []
    own = os.path.splitext(os.path.basename(rel))[0]
    used = sorted(s for s in mods.singletons.get(uri, ()) if s != own and re.search(rf"\b{s}\.\w", code))
    imported = {i for i, _ in _IMPORT.findall(raw)}
    if not used or uri in imported:
        return []
    first = min(code.find(f"{s}.") for s in used if f"{s}." in code)
    return [(_line(code, max(first, 0)), f"uses {', '.join(used)} (singleton{'s' if len(used) > 1 else ''} of "
                                         f"{uri}) without `import {uri}`: which one it gets depends on the Qt build "
                                         f"(6.11.2 read the plain Tokens.qml beside it, an empty object, and the "
                                         f"window went black)")]


def qml_js_shadows_type(rel, raw, code, o, project=None):
    if not rel.endswith((".js", ".mjs")) or project is None:
        return []
    mods = _modules(project)
    stem = os.path.splitext(rel)[0]
    twin = stem + ".qml"
    if twin in mods.qml and (mods.module_of.get(rel) == mods.module_of.get(twin) or os.path.dirname(rel) == os.path.dirname(twin)):
        return [(1, f"{os.path.basename(rel)} shares its name with {os.path.basename(twin)}: the module lists both "
                    f"under one name and the library can hide the type (HubMove.js hid the HubMove singleton). "
                    f"Rename the library ({os.path.basename(stem)}Rules.js)")]
    return []


LINTS = {
    "qml-unknown-type": qml_unknown_type,
    "qml-singleton-import": qml_singleton_import,
    "qml-js-shadows-type": qml_js_shadows_type,
}
DEFAULT_IN = {
    "qml-unknown-type": ["**/*.qml"],
    "qml-singleton-import": ["**/*.qml"],
    "qml-js-shadows-type": ["**/*.js", "**/*.mjs"],
}
OPTIONS = {
    "qml-unknown-type": {"types"},
    "qml-singleton-import": set(),
    "qml-js-shadows-type": set(),
}
# QML lives outside many projects' [project] include (native/, ui/): read the repo
REPO_WIDE = {"qml-unknown-type", "qml-singleton-import", "qml-js-shadows-type"}
