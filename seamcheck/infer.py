"""Which class a call's receiver is: the type layer over the call graph.

Adapters record what the code writes down (a call's receiver chain, a
variable's declared type or the expression it was assigned, a field's type, a
function's declared return type, a class's supertypes). This module walks a
chain one step at a time over those facts:

  repo.find(id).save()   repo: Repo (a field)  ->  Repo.find returns Order  ->  Order.save

Every step is a fact the code states; when one is missing the walk stops and
the call falls back to the older rules (and, last, a name guess). A step that
lands on a library type (a name the project doesn't declare, an import from
outside it) makes the rest of the chain the library's too: `Path(p).open()`
reaches nothing in the project, rather than a guess at every `open`.
"""
from __future__ import annotations

import os
import re

from .chains import UNWRAP

UNKNOWN = ("unknown", (), False)  # declared, of a type the code doesn't say (a QML `property var`)
EXT = ("ext", (), True)  # a library value: every call on it is the library's (("ext", (type,), True) when named)


def is_ext(v) -> bool:
    return v is not None and v[0] == "ext"
_MAX = 6  # how deep an `x = <expr>` / a return type may lead before we stop


class Types:
    """Per-project indexes, built once after imports are resolved."""

    def __init__(self, project):
        self.p = project
        self.by_type: dict[str, list[str]] = {}
        for m in project.modules.values():
            for d in m.decls:
                self.by_type.setdefault(d, []).append(m.name)
        for qname, cls in project.__dict__.get("_qml_names", {}).items():  # a C++ class registered under a QML name
            if qname != cls:
                self.by_type.setdefault(qname, []).extend(self.by_type.get(cls, []))
        # every name a project QML file declares (ids, properties): any other
        # bare name in QML is the framework's (Kirigami's `pageStack`)
        self.qml_names = {k for m in project.modules.values() if m.file.endswith(".qml") for k in m.bound} | \
            {f.name for m in project.modules.values() if m.file.endswith(".qml") for f in m.funcs}
        self.cache: dict = {}
        self.ext_fns: dict[tuple[str, str], list] = {}
        for g in project.funcs.values():
            if g.recv and not g.module.endswith("." + g.recv):
                self.ext_fns.setdefault((g.recv, g.name), []).append(g)

    # --- types -> modules -----------------------------------------------------------

    def _file_mod(self, mod):
        return self.p.modules.get(mod.owner, mod) if mod.owner else mod

    def searchable(self, mn: str) -> bool:
        m = self.p.modules.get(mn)
        return bool(m) and (m.kind in ("class", "interface", "ext") or bool(m.owner))

    def type_modules(self, mod, name: str) -> tuple[list[str], bool, bool]:
        """The modules declaring type `name` as seen from `mod`: (modules,
        exact, declared in the project). Nearest first: the file, an import,
        the package / namespace / directory, then anywhere (exact only when
        there is one)."""
        key = (mod.name, name)
        hit = self.cache.get(key)
        if hit is not None:
            return hit
        hit = self._type_modules(mod, name)
        self.cache[key] = hit
        return hit

    def _type_modules(self, mod, name: str):
        p = self.p
        fm = self._file_mod(mod)
        if fm.file.endswith(".py"):
            from .core import _py_class
            cls = _py_class(p, mod, name)
            if cls:
                return [cls], True, True
        if "." in name:  # `pkg.Type`, `Outer.Inner`, `ns.Type`
            q, _, last = name.rpartition(".")
            target = fm.aliases.get(q.split(".")[-1], "")
            if target.startswith("pkg:"):
                mods = [mn for mn in self.by_type.get(last, ()) if self._pkg_of(mn) == target[4:]]
                return mods, len({self._primary(m) for m in mods}) <= 1 and bool(mods), bool(mods)
            if target in p.modules:
                inner = [mn for mn in self.by_type.get(last, ()) if p.modules[mn].file == p.modules[target].file]
                if inner:
                    return inner, True, True
            got = self.type_modules(mod, q.split(".")[-1])
            if got[0]:
                inner = [f"{mn}.{last}" for mn in got[0] if f"{mn}.{last}" in p.modules]
                if inner:
                    return inner, len(inner) == 1, True
            name = last
        cands = self.by_type.get(name, [])
        if not cands:
            return [], False, False
        exts = [mn for mn in cands if p.modules[mn].kind == "ext"]
        prim = [mn for mn in cands if p.modules[mn].kind != "ext"] or exts
        for narrow in (lambda mn: p.modules[mn].file == mod.file,
                       lambda mn: self._imported(fm, name, mn),
                       lambda mn: fm.package and self._pkg_of(mn) == fm.package,
                       lambda mn: self._in_uses(fm, mn),
                       lambda mn: os.path.dirname(p.modules[mn].file) == os.path.dirname(mod.file)):
            got = [mn for mn in prim if narrow(mn)]
            if got:
                prim = got
                break
        prims = {self._primary(mn) for mn in prim}
        mods = list(dict.fromkeys(prim + [mn for mn in exts if mn not in prim]))
        # one type built per platform (battery-macos.cpp / battery-win.cpp, one
        # directory, one name): a call reaches whichever is compiled, like an
        # interface reaches each implementation
        variants = len({os.path.dirname(p.modules[mn].file) for mn in prim}) == 1 and len(prims) > 1 and \
            len({p.modules[mn].decls[0] if p.modules[mn].owner and p.modules[mn].decls else name for mn in prim}) == 1
        return mods, len(prims) == 1 or variants, True

    def _primary(self, mn: str) -> str:
        m = self.p.modules[mn]
        if m.file.endswith((".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx")):
            return os.path.splitext(m.file)[0] + "\0" + (m.decls[0] if m.owner and m.decls else "")  # Foo.h + Foo.cpp: one class
        return m.file if m.kind == "ext" else mn

    def _pkg_of(self, mn: str) -> str:
        m = self.p.modules.get(mn)
        if m is None:
            return ""
        return self._file_mod(m).package

    def _imported(self, fm, name: str, mn: str) -> bool:
        m = self.p.modules[mn]
        own = self._file_mod(m).name
        w = fm.aliases.get(name) or (fm.imported.get(name, ("", ""))[0])
        if w and (w == mn or w == own):
            return True
        for u in fm.uses:
            if u == f"type:{name}" and name in m.decls:
                return True
        return False

    def _in_uses(self, fm, mn: str) -> bool:
        own = self._file_mod(self.p.modules[mn])
        for u in fm.uses:
            if u.startswith("pkg:") and own.package == u[4:]:
                return True
            if u == own.name:
                return True
        return False

    # --- methods ------------------------------------------------------------------

    def ancestors(self, mn: str) -> list[str]:
        """The module, the other parts of its type (a C++ class's header and
        .cpp, a Swift extension, a Rust impl in another file), then its
        supertypes, nearest first."""
        hit = self.cache.get(("\0anc", mn))
        if hit is not None:
            return hit
        out, todo, seen = [], [mn], set()
        while todo:
            cur = todo.pop(0)
            if cur in seen or cur not in self.p.modules:
                continue
            seen.add(cur)
            out.append(cur)
            m = self.p.modules[cur]
            todo[:0] = [x for x in self.parts(m) if x not in seen]
            todo += [b for b in m.bases if b in self.p.modules] + ([m.parent] if m.parent else [])
        self.cache[("\0anc", mn)] = out
        return out

    def parts(self, m) -> list[str]:
        """The other modules of the same type: a C++ class's header and source
        are one class; an `ext` module (a Swift extension, a Rust impl in
        another file) joins the type it extends, and that type gains it."""
        if not m.decls:
            return []
        key = ("\0parts", m.name)
        hit = self.cache.get(key)
        if hit is not None:
            return hit
        name = m.decls[0] if m.owner or m.kind == "ext" else m.decls[-1]
        cands = [x for x in self.by_type.get(name, ()) if x != m.name]
        if m.file.endswith((".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx")):
            own = self._primary(m.name)
            out = [x for x in cands if self._primary(x) == own]
        elif m.kind == "ext":
            out = [x for x in self.type_modules(m, name)[0] if x != m.name]
        else:
            out = [x for x in cands if self.p.modules[x].kind == "ext" and m.name in self.type_modules(self.p.modules[x], name)[0]]
        self.cache[key] = out
        return out

    def external_base(self, mn: str) -> bool:
        """A supertype the project doesn't declare: an inherited method may be
        the library's."""
        for a in self.ancestors(mn):
            m = self.p.modules[a]
            if getattr(m, "unresolved", False):
                return True
        return False

    def method(self, mods: list[str], name: str) -> tuple[list, bool]:
        """`name` on an instance of these modules' type: the definition on the
        type or its nearest supertype; else (an interface, an abstract
        method) every subtype's implementation. (funcs, found on the type)."""
        found = []
        for mn in mods:
            for a in self.ancestors(mn):
                g = self.p.funcs.get(f"{a}.{name}")
                if g:
                    if g not in found:
                        found.append(g)
                    break
        if found:
            return found, True
        impls = []
        for mn in mods:
            for sub in self.p.children.get(mn, ()):
                for part in [sub] + self.parts(self.p.modules[sub]):  # (a C++ subclass's .cpp half)
                    g = self.p.funcs.get(f"{part}.{name}")
                    if g and g not in impls:
                        impls.append(g)
        for mn in mods:  # Kotlin / C# / Swift extensions on the type
            m = self.p.modules[mn]
            for d in m.decls[:1]:
                impls += [g for g in self.ext_fns.get((d, name), []) if g not in impls]
        return impls, False

    def field(self, mods: list[str], name: str, depth: int):
        """A field's value; UNKNOWN when the class declares it without a type
        the code states (so it is not a library supertype's)."""
        for mn in mods:
            for a in self.ancestors(mn):
                m = self.p.modules[a]
                held = m.bound.get(name)
                if held:
                    got = self.held(m, None, held, depth + 1, self_mods=[mn])
                    return UNKNOWN if got is None else got
        return None

    # --- the walk -----------------------------------------------------------------

    def held(self, mod, f, held: str, depth: int, self_mods=None):
        """The value a binding holds: "type:T", "expr:<chain>", "group:<id>"."""
        if depth > _MAX:
            return None
        if held.startswith("type:"):
            return self.instance(mod, held[5:], self_mods)
        if held.startswith("expr:"):
            return self.chain(mod, f, tuple(held[5:].split("\x1f")), depth + 1, self_mods=self_mods)
        return None

    def instance(self, mod, ty: str, self_mods=None):
        if ty == "Self":
            return ("inst", tuple(self_mods), True) if self_mods else None
        if mod.file.endswith(".go"):
            mods = self._go_type(mod, ty)
            if mods is None:
                return EXT
            return ("inst", tuple(mods), True) if mods else None
        mods, exact, declared = self.type_modules(mod, ty)
        if not declared:
            return ("ext", (ty,), True) if ty[:1].isupper() or "." in ty or mod.file.endswith((".rs", ".swift")) else None
        return ("inst", tuple(mods), exact)

    def _go_type(self, mod, ty: str):
        """A Go type of the caller's package (or an imported one): its method
        modules; None when it is the standard library's / another module's."""
        fm = self._file_mod(mod)
        pkg = fm.package
        if "." in ty:
            q, _, ty = ty.partition(".")
            target = fm.aliases.get(q, "")
            if not target.startswith("pkg:"):
                return None
            pkg = target[4:]
        mods = [mn for mn in self.by_type.get(ty, ()) if self._pkg_of(mn) == pkg]
        return mods if mods else (None if ty in _GO_BUILTIN or not any(
            ty in self.p.modules[mn].decls for mn in self.p.packages.get(pkg, ()) if mn in self.p.modules) else [])

    def own_class(self, mod, f):
        if f is not None and f.recv and mod.file.endswith(".go"):
            return [mod.name]
        if mod.owner or mod.kind in ("class", "interface", "ext"):
            return [mod.name]
        return None

    def returns(self, g, self_mods, depth: int):
        if depth > _MAX or not g.returns:
            return None
        gm = self.p.modules.get(g.module)
        if gm is None:
            return None
        if g.returns.startswith("expr:"):
            return self.chain(gm, g, tuple(g.returns[5:].split("\x1f")), depth + 1, self_mods=self_mods or self.own_class(gm, g))
        if g.returns == "Self":
            mods = self_mods or self.own_class(gm, g)
            return ("inst", tuple(mods), True) if mods else None
        return self.instance(gm, g.returns, self_mods)

    def chain(self, mod, f, chain: tuple, depth: int = 0, self_mods=None):
        """The value a receiver chain evaluates to: ("inst" | "static",
        modules, exact), EXT, or None (unknown)."""
        if not chain or depth > _MAX:
            return None
        cur = self.first(mod, f, chain[0], depth, self_mods)
        for seg in chain[1:]:
            if cur is None or is_ext(cur):
                return cur if cur is None else EXT
            cur = self.step(mod, cur, seg, depth)
        return cur

    def first(self, mod, f, seg: str, depth: int, self_mods=None):
        p = self.p
        fm = self._file_mod(mod)
        if seg in ("this", "self", "$this", "Self", "static"):
            own = self_mods or self.own_class(mod, f)
            if not own:
                return None
            return ("static" if seg in ("Self", "static") else "inst", tuple(own), True)
        if seg in ("super", "parent", "base"):
            own = self.own_class(mod, f)
            if not own:
                return None
            sup = [b for b in self.ancestors(own[0])[1:2]]
            return ("inst", tuple(sup), True) if sup else (EXT if self.external_base(own[0]) else None)
        if seg.startswith("new "):
            return self.instance(mod, seg[4:-2], self_mods)
        if seg.endswith("()"):
            name = seg[:-2]
            g = self.bare(mod, f, name)
            if g is not None:
                return self.returns(g, None, depth + 1)
            if name in fm.external:
                return EXT
            own = self.own_class(mod, f)
            if own and not fm.file.endswith(".py") and any(self.external_base(o) for o in own) \
                    and not self.method(own, name)[0]:
                return EXT  # inherited from a library supertype (Qt's `sourceModel()`)
            if name[:1].isupper():
                got = self.instance(mod, name, self_mods)  # `Foo(...)`: a constructor
                return got
            return None
        if seg in ("(...)", "[]", "?", "!!"):
            return None
        if fm.bound.get(seg) == "qmlns:":
            return ("ns", (), True)
        if f is not None:
            held = f.bound.get(seg)
            if held:
                return self.held(mod, f, held, depth + 1, self_mods)
        own = self_mods or self.own_class(mod, f)
        if own:
            got = self.field(list(own), seg, depth)
            if got is UNKNOWN:
                return None
            if got is not None:
                return got
        held = mod.bound.get(seg) or fm.bound.get(seg)
        if held and not held.startswith("group:"):
            return self.held(fm, None, held, depth + 1)
        if seg in fm.external:
            return EXT
        if seg in fm.aliases:
            target = fm.aliases[seg]
            if target.startswith("pkg:"):
                return ("pkg", (target[4:],), True)
            if target in p.modules:
                return ("static", (target,), True)
        if seg in fm.imported:
            from .core import _defining_module
            w, n = fm.imported[seg]
            dm = _defining_module(p, w, n)
            if dm:
                inner = f"{dm}.{n}"
                return ("static", (inner if inner in p.modules else dm,), True)
        if seg[:1].isupper() or seg in self.by_type:
            mods, exact, declared = self.type_modules(mod, seg)
            if declared:
                return ("static", tuple(mods), exact)
        if seg in p.modules:
            return ("static", (seg,), True)
        short = p.short.get(seg)
        if short:
            return ("static", tuple(short), len(short) == 1)
        if seg[:1].isupper() and not fm.file.endswith((".ex", ".exs", ".gd")):
            return EXT  # a type or module the project doesn't declare
        if fm.file.endswith(".qml") and seg not in self.qml_names and seg not in ("parent", "model", "modelData", "index"):
            return EXT  # a name no project QML file declares: the window's / framework's
        return None

    def bare(self, mod, f, name: str):
        """The function a bare call `name()` reaches (one, or None)."""
        from .core import _in_scope
        fm = self._file_mod(mod)
        own = self.own_class(mod, f)
        if own and not fm.file.endswith(".py"):
            hits, _ = self.method(own, name)
            if len(hits) == 1:
                return hits[0]
        hits = _in_scope(self.p, fm.name, name)
        if not hits and name in fm.imported:
            w, n = fm.imported[name]
            hits = _in_scope(self.p, w, n)
        for w in ([] if hits else fm.uses):
            hits = _in_scope(self.p, w, name)
            if hits:
                break
        if not hits and fm.package:
            hits = _in_scope(self.p, "pkg:" + fm.package, name)
        return hits[0] if len(hits) == 1 else None

    def step(self, mod, cur, seg: str, depth: int):
        kind, mods, exact = cur
        if seg in ("?", "!!"):
            return cur
        if seg in ("[]", "(...)"):
            return None
        if kind == "ns":  # a QML module's namespace: `KDEConnect.Type`
            name = seg[:-2] if seg.endswith("()") else seg
            mods_, ex, declared = self.type_modules(mod, name)
            return ("static", tuple(mods_), ex) if declared else EXT
        if kind == "pkg":
            from .core import _in_scope
            if seg.endswith("()"):
                hits = _in_scope(self.p, "pkg:" + mods[0], seg[:-2])
                return self.returns(hits[0], None, depth + 1) if len(hits) == 1 else None
            got = [mn for mn in self.by_type.get(seg, ()) if self._pkg_of(mn) == mods[0]]
            return ("static", tuple(got), True) if got else None
        if seg.endswith("()"):
            name = seg[:-2]
            if kind == "static":
                from .core import _in_scope
                hits = [h for mn in mods for h in _in_scope(self.p, mn, name)]
                if not hits:
                    hits, _ = self.method(list(mods), name)
            else:
                hits, _ = self.method(list(mods), name)
            if len(hits) == 1:
                return self.returns(hits[0], list(mods) if kind == "inst" else None, depth + 1)
            if not hits:
                if seg in UNWRAP:
                    return cur  # `.unwrap()` / `.get()` on an Option: the value inside
                if any(self.external_base(mn) for mn in mods):
                    return EXT
            return None
        # a field / property / nested type
        if kind == "static":
            inner = [f"{mn}.{seg}" for mn in mods if f"{mn}.{seg}" in self.p.modules]
            if inner:
                return ("static", tuple(inner), len(inner) == 1)
            for mn in mods:
                m = self.p.modules[mn]
                held = m.bound.get(seg)
                if held:
                    return self.held(m, None, held, depth + 1, self_mods=[mn])
            return None
        got = self.field(list(mods), seg, depth)
        if got is UNKNOWN:
            return None
        if got is None and seg in UNWRAP:
            return cur
        if got is None and any(self.external_base(mn) for mn in mods):
            return EXT
        return got


_GO_BUILTIN = {"string", "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64",
               "float32", "float64", "bool", "byte", "rune", "error", "any", "uintptr", "complex64", "complex128"}


def resolve_bases(project) -> None:
    """Each class's written supertypes -> modules (`parent` = the first);
    a supertype the project doesn't declare marks the class `unresolved` (an
    inherited method may be the library's)."""
    t = Types(project)
    for m in list(project.modules.values()):
        if not m.bases:
            continue
        got, missing = [], False
        fm = project.modules.get(m.owner, m) if m.owner else m
        for b in m.bases:
            mods, _, declared = t.type_modules(m, b)
            mods = [x for x in mods if x != m.name and project.modules[x].kind != "ext"] or [x for x in mods if x != m.name]
            if mods:
                got.append(mods[0])
            elif b in fm.external or (not declared and b not in fm.imported and b not in fm.aliases):
                missing = True  # a library supertype (imported from outside, or declared nowhere)
        m.bases = got
        m.unresolved = missing
        if got and not m.parent:
            m.parent = got[0]


_IDENT = re.compile(r"^[\w$]+$")
