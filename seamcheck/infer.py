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
_MAX = 48  # how deep a chain, an `x = <expr>` or a return type may lead before we stop


class Types:
    """Per-project indexes, built once after imports are resolved."""

    def __init__(self, project):
        self.p = project
        self.by_type: dict[str, list[str]] = {}
        for m in project.modules.values():
            for d in m.decls:
                self.by_type.setdefault(d, []).append(m.name)
        for m in project.modules.values():  # `export { Hono as HonoBase }`: the class under its exported name
            if not m.owner and m.file.endswith(_JS_EXTS) and m.raw and "export" in (txt := "\n".join(m.raw)):
                for blk in re.finditer(r"\bexport\s*\{([^}]*)\}(?!\s*from)", txt):
                    for a, b in re.findall(r"([A-Za-z_$][\w$]*)\s+as\s+([A-Za-z_$][\w$]*)", blk.group(1)):
                        own = [mn for mn in self.by_type.get(a, ()) if project.modules[mn].file == m.file]
                        if own and a != b:
                            self.by_type.setdefault(b, []).extend(x for x in own if x not in self.by_type.get(b, []))
        for qname, cls in project.__dict__.get("_qml_names", {}).items():  # a C++ class registered under a QML name
            if qname != cls:
                self.by_type.setdefault(qname, []).extend(self.by_type.get(cls, []))
        # every name a project QML file declares (ids, properties): any other
        # bare name in QML is the framework's (Kirigami's `pageStack`)
        self.qml_names = {k for m in project.modules.values() if m.file.endswith(".qml") for k in m.bound} | \
            {f.name for m in project.modules.values() if m.file.endswith(".qml") for f in m.funcs}
        # library types project classes extend (GDScript's Node, Python's dict)
        self.extended = {m.parent for m in project.modules.values() if m.parent and m.parent not in project.modules} | \
            {b for m in project.modules.values() for b in m.bases if b not in project.modules}
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
        al = fm.bound.get(name, "") if name[:1].isupper() else ""
        if al.startswith("type:") and al[5:] != name:  # `using X = A.B;` / PHP `use A\\B as X;`: B, by another name
            name = al[5:]
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
                       lambda mn: mod.file.endswith(".gd") and self._godot(p.modules[mn].file) == self._godot(mod.file),
                       lambda mn: os.path.dirname(p.modules[mn].file) == os.path.dirname(mod.file)):
            got = [mn for mn in prim if narrow(mn)]
            if got:
                prim = got
                break
        else:
            if fm.file.endswith(_ES_MODULE) and all(self._es_module(p.modules[mn].file) for mn in prim):
                # an ES module sees another module's type only by importing it: not imported,
                # `Response` is the runtime's (not client/types.ts's interface Response)
                return [], False, False
        prims = {self._primary(mn) for mn in prim}
        mods = list(dict.fromkeys(prim + [mn for mn in exts if mn not in prim]))
        # one type built per platform (battery-macos.cpp / battery-win.cpp, one
        # directory, one name): a call reaches whichever is compiled, like an
        # interface reaches each implementation
        variants = len({os.path.dirname(p.modules[mn].file) for mn in prim}) == 1 and len(prims) > 1 and \
            len({p.modules[mn].decls[0] if p.modules[mn].owner and p.modules[mn].decls else name for mn in prim}) == 1
        return mods, len(prims) == 1 or variants, True

    def _godot(self, rel: str) -> str:
        """The Godot project a file is in (the nearest folder with a
        project.godot): a `class_name` is global to its project only."""
        d = os.path.dirname(rel)
        cache = self.__dict__.setdefault("_godot_roots", {})
        if d not in cache:
            up = d
            while up and not os.path.exists(os.path.join(self.p.root, up, "project.godot")):
                up = os.path.dirname(up)
            cache[d] = up
        return cache[d]

    def _es_module(self, rel: str) -> bool:
        """A TS/JS file that is a module (imports or exports), not an ambient
        `.d.ts` / script whose declarations are global."""
        key = ("\0esm", rel)
        if key not in self.cache:
            mods = self.p.by_file.get(rel) or []
            raw = "\n".join(mods[0].raw) if mods else ""
            self.cache[key] = rel.endswith(_ES_MODULE) and not rel.endswith(".d.ts") and \
                bool(re.search(r"^[ \t]*(?:import|export)\b", raw, re.M)) and "declare global" not in raw
        return self.cache[key]

    def _primary(self, mn: str) -> str:
        """Which type a module is part of: a C++ class's header and source are
        one; a C# partial class's files (and a Swift extension, a Rust impl)
        are one per namespace and name."""
        m = self.p.modules[mn]
        if m.file.endswith((".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx")):
            return os.path.splitext(m.file)[0] + "\0" + (m.decls[0] if m.owner and m.decls else "")  # Foo.h + Foo.cpp: one class
        if m.kind == "ext" and m.decls:
            # a Swift extension / Rust impl of the project's one type of that name is part of it
            own = [o for o in self.by_type.get(m.decls[0], ()) if self.p.modules[o].kind != "ext"]
            if len(own) == 1 and m.file.endswith((".swift", ".rs")):
                return self._primary(own[0])
            return f"{self._pkg_of(mn)}\0{m.decls[0]}"
        return mn

    def _pkg_of(self, mn: str) -> str:
        m = self.p.modules.get(mn)
        if m is None:
            return ""
        return self._file_mod(m).package

    def _imported(self, fm, name: str, mn: str) -> bool:
        m = self.p.modules[mn]
        own = self._file_mod(m).name
        w = fm.aliases.get(name) or (fm.imported.get(name, ("", ""))[0])
        if w and (w == mn or w == own or (w.startswith("pkg:") and self._pkg_of(mn) == w[4:])):
            return True
        if w in self.p.modules:  # through the imported module's re-exports (a crate root's `pub use`)
            from .core import _defining_module
            dm = _defining_module(self.p, w, name)
            if dm and (dm == mn or dm == own):
                return True
        if w:
            return False  # the import names another package's
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
            m = self.p.modules.get(mn)
            if m is not None and m.methods and m.file.endswith(".go") and name in m.methods:
                impls += [g for g in self._go_impls(m, name) if g not in impls]
            for sub in self.p.children.get(mn, ()):
                for part in [sub] + self.parts(self.p.modules[sub]):  # (a C++ subclass's .cpp half)
                    g = self.p.funcs.get(f"{part}.{name}")
                    if g and g not in impls:
                        impls.append(g)
        for mn in mods:  # Kotlin / C# / Swift extensions on the type, or on a supertype / interface it has
            for a in self.ancestors(mn):
                m = self.p.modules[a]
                for d in (m.decls[:1] if m.owner or m.kind == "ext" else m.decls[-1:]):
                    impls += [g for g in self.ext_fns.get((d, name), []) if g not in impls]
            if impls:
                return impls, False  # (extensions: the receiver's type is sure, each overload may be the one)
        return impls, False

    def one_type(self, funcs) -> bool:
        """These functions are one type's (overloads, or its parts)."""
        return len({self._primary(g.module) for g in funcs}) == 1

    def _go_impls(self, iface, name: str) -> list:
        """A Go interface's implementations: every project type whose methods
        (across its files) cover the interface's method set."""
        key = ("\0goimpl", iface.name)
        types_ = self.cache.get(key)
        if types_ is None:
            have: dict[tuple[str, str], set[str]] = {}
            where: dict[tuple[str, str], list[str]] = {}
            for m in self.p.modules.values():
                if m.owner and m.file.endswith(".go") and m.kind != "interface" and m.decls:
                    k = (self._pkg_of(m.name), m.decls[0])
                    have.setdefault(k, set()).update(f.name for f in m.funcs)
                    where.setdefault(k, []).append(m.name)
            need = set(iface.methods)
            types_ = [mods for k, mods in where.items() if need <= have[k]]
            self.cache[key] = types_
        return [g for mods in types_ for mn in mods if (g := self.p.funcs.get(f"{mn}.{name}"))]

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
        if held.startswith("mod:"):  # a component whose file names no type (`mousepad.qml`'s root id)
            return ("inst", (held[4:],), True) if held[4:] in self.p.modules else None
        if held.startswith("expr:"):
            return self.chain(mod, f, tuple(held[5:].split("\x1f")), depth + 1, self_mods=self_mods)
        if held.startswith("pack:"):
            return self.pack_param(held[5:])
        if held.startswith("fixture:"):
            g = self.fixture(mod, held[8:], f)
            return self.returns(g, None, depth + 1) if g is not None else None
        return None

    def pack_param(self, spec: str):
        """A framework-given parameter ("express/handler:res"): the module its
        pack names ("express:lib/response"), project code when the project is
        that package (express itself), else the library's."""
        pid, _, name = spec.rpartition(":")
        entry = self.p.params.get(pid)
        if not entry:
            return None  # the framework isn't in use here: just a parameter
        pkg, _, path = str(entry.get("types", {}).get(name, "")).partition(":")
        if not pkg:
            return None
        if pkg == self._package_name() and path in self.p.modules:  # (a class's file: an instance of it)
            return ("inst" if self.p.modules[path].kind == "class" else "static", (path,), True)
        return EXT

    def _rs_external(self) -> set:
        """The crates a Rust project uses but doesn't hold: std / core / alloc
        and every Cargo.toml dependency that isn't a crate of the workspace."""
        got = self.cache.get("\0rsext")
        if got is None:
            got = {"std", "core", "alloc"}
            own = set()
            for rel in self.p.files:
                if os.path.basename(rel) != "Cargo.toml":
                    continue
                try:
                    with open(os.path.join(self.p.root, rel), encoding="utf-8") as fh:
                        text = fh.read()
                except OSError:
                    continue
                pm = re.search(r"^\[package\][^\[]*?^name\s*=\s*\"([^\"]+)\"", text, re.M | re.S)
                if pm:
                    own.add(pm.group(1).replace("-", "_"))
                for sec in re.finditer(r"^\[(?:workspace\.)?(?:dev-|build-)?dependencies\]([^\[]*)", text, re.M):
                    got |= {k.replace("-", "_") for k in re.findall(r"^([A-Za-z0-9_-]+)\s*=", sec.group(1), re.M)}
                for sec in re.finditer(r"^\[(?:target\.[^\]]+\.)?(?:dev-|build-)?dependencies\.([A-Za-z0-9_-]+)\]", text, re.M):
                    got.add(sec.group(1).replace("-", "_"))
            got -= own
            self.cache["\0rsext"] = got
        return got

    def _package_name(self) -> str:
        if "\0pkgname" not in self.cache:
            import json
            try:
                with open(os.path.join(self.p.root, "package.json"), encoding="utf-8") as fh:
                    self.cache["\0pkgname"] = json.load(fh).get("name", "")
            except (OSError, ValueError, AttributeError):
                self.cache["\0pkgname"] = ""
        return self.cache["\0pkgname"]

    def fixture(self, mod, name: str, f=None):
        """The pytest fixture a test's parameter names: its own class's, else
        its file's (module level), else the nearest conftest.py up the tree.
        A fixture's parameter of its own name is the one it overrides."""
        d = os.path.dirname(mod.file)
        fm = self._file_mod(mod)
        scopes = ([mod.name] if mod.owner else []) + [fm.name]
        for rel in [os.path.join(d2, "conftest.py") for d2 in _parents(d)]:
            scopes += [m.name for m in self.p.by_file.get(rel, ()) if not m.owner]
        for mn in scopes:
            m = self.p.modules.get(mn)
            g = self.p.funcs.get(f"{mn}.{name}")
            if g is None or g is f or m is None:
                continue
            if any(re.match(r"\s*@(?:pytest\.)?fixture\b", m.raw[j]) for j in range(max(0, g.start - 4), g.start - 1)):
                return g
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
            if mod.file.endswith(_DYNAMIC) and ty in self.extended:
                return None  # `n: Node` in GDScript / Python: any project script that extends Node may be it
            return ("ext", (ty,), True) if ty[:1].isupper() or "." in ty or mod.file.endswith((".rs", ".swift")) or \
                (mod.file.endswith(".py") and ty in _PY_BUILTIN) else None
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
        if mod.file.endswith(".rs") and len(chain) > 1:
            # `grep::printer::StandardBuilder::new()`: a path ending at a type the project declares starts there
            k = max((i for i, sg in enumerate(chain[:-1]) if sg[:1].isupper() and sg in self.by_type), default=0)
            chain = chain[k:]
        if len(chain) > 1 and chain[-1] in _DOM_PROPS and mod.file.endswith(_JS_EXTS):
            return EXT  # `el.classList`, `el.style`: the DOM's, whatever `el` is
        cur = self.first(mod, f, chain[0], depth, self_mods)
        for seg in chain[1:]:
            if cur is None or is_ext(cur):
                if cur is not None and "[]" in chain[chain.index(seg):]:
                    return None  # an element of a library collection (`shapes[0]`): could be a project object
                return cur if cur is None else EXT
            cur = self.step(mod, cur, seg, depth)
        return cur

    def first(self, mod, f, seg: str, depth: int, self_mods=None):
        p = self.p
        fm = self._file_mod(mod)
        if seg in ("this", "self", "$this", "Self", "static"):
            own = self_mods or self.own_class(mod, f)
            if not own and f is not None and f.bound.get(seg, "").startswith("mod:"):
                return self.held(mod, f, f.bound[seg], depth + 1)  # a Python closure's captured `self`
            if not own:
                return None
            return ("static" if seg in ("Self", "static") else "inst", tuple(own), True)
        if fm.file.endswith(".rs") and seg in ("super", "crate"):  # (Rust's `self` is the receiver: `self.x()`)
            if seg == "super" and f is not None and re.search(r"^\s*(?:pub\s+)?mod\s+\w+\s*\{", "\n".join(fm.raw[:f.start]), re.M):
                return ("static", (fm.name,), True)  # `super::shell_quote()` in an inline `mod tests`: this file's module
            return None
        if fm.file.endswith(".rs") and re.fullmatch(_RS_PRIMS, seg):
            return EXT  # `u32::from(c)`: a primitive's
        if seg in ("super", "parent", "base", "super()"):  # (Python's `super().x()`)
            own = self.own_class(mod, f)
            if not own:
                return None
            sup = [b for b in self.ancestors(own[0])[1:2]]
            return ("inst", tuple(sup), True) if sup else (EXT if self.external_base(own[0]) else None)
        if seg.startswith("new "):
            return self.instance(mod, seg[4:-2], self_mods)
        if seg.endswith("()"):
            name = seg[:-2]
            held = f.bound.get(name, "") if f is not None else ""
            if held.startswith("call:"):  # a closure: calling it gives its body's value
                return self.chain(mod, f, tuple(held[5:].split("\x1f")), depth + 1, self_mods)
            g = self.bare(mod, f, name)
            if g is not None:
                got = self.returns(g, None, depth + 1)
                return got if got is not None else self._rb_named(mod, name)
            if mod.file.endswith(".rb") and len(got := {self.returns(h, [h.module], depth + 1) for h in self.method(self.own_class(mod, f) or [], name)[0]}) == 1:
                return got.pop()  # Ruby: a mixin's `site` that every includer's `site` returns alike (lang_ruby.py)
            if name in fm.external or (fm.file.endswith(".py") and name in _PY_BUILTIN_CALLS):
                return EXT  # (Python's `vars(x)`, `sorted(x)`: the language's own values)
            own = self.own_class(mod, f)
            if own and not fm.file.endswith(".py") and any(self.external_base(o) for o in own) \
                    and not self.method(own, name)[0]:
                return EXT  # inherited from a library supertype (Qt's `sourceModel()`)
            if name[:1].isupper():
                got = self.instance(mod, name, self_mods)  # `Foo(...)`: a constructor
                return got
            return self._rb_named(mod, name)
        if seg == "<literal>":
            return EXT  # a string / array / number literal: the language's own type
        if seg in ("(...)", "[]", "?", "!!"):
            return None
        if fm.bound.get(seg) == "qmlns:":
            return ("ns", (), True)
        if f is not None:
            held = f.bound.get(seg)
            if held:
                return self.held(mod, f, held, depth + 1, self_mods)
        # a bare name is a field only where `this` is implicit: never in Python,
        # JS/TS or PHP (`cli` in a Flask method is the module, not self.cli)
        own = self_mods or (None if fm.file.endswith(_EXPLICIT_SELF) else self.own_class(mod, f))
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
        if seg[:1].isupper() and seg in self.by_type and not fm.file.endswith(".py"):
            # a type the project declares, whatever module the import names
            # (Rust's `use grep_searcher::SearcherBuilder` names the crate root that re-exports it)
            mods, exact, declared = self.type_modules(mod, seg)
            if declared:
                return ("static", tuple(mods), exact)
        if seg in fm.aliases:
            target = fm.aliases[seg]
            if target.startswith("pkg:"):
                return ("pkg", (target[4:],), True)
            if target in p.modules:
                return ("static", (target,), True)
        if seg in fm.imported:
            from .core import _defining_module
            w, n = fm.imported[seg]
            vm = self._var_module(w, n)
            if vm is not None:  # an imported module-level variable (`from flask import current_app`)
                return self.held(vm[0], None, vm[0].bound[vm[1]], depth + 1)
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
        short = p.short.get(seg) if not (seg[:1].islower() and fm.file.endswith((".rb", ".rake", ".ru"))) else None
        if short:  # (a lowercase Ruby name is a value, never the file `site.rb`)
            return ("static", tuple(short), len(short) == 1)
        if seg[:1].isupper() and not fm.file.endswith((".ex", ".exs", ".gd")):
            return EXT  # a type or module the project doesn't declare
        if fm.file.endswith(".qml") and seg not in self.qml_names and seg not in ("parent", "model", "modelData", "index"):
            return EXT  # a name no project QML file declares: the window's / framework's
        if fm.file.endswith(".rs") and seg in self._rs_external():
            return EXT  # `std::...`, `serde_json::from_str`: a crate the project depends on, not one of its own
        if seg in _JS_GLOBALS and fm.file.endswith(_JS_EXTS):
            return EXT  # `console`, `window`, `document`: the runtime's (no project name took it)
        return self._rb_named(mod, seg)

    def _rb_named(self, mod, name: str):
        """Ruby, an untyped receiver named after a class (`site`, `@document`,
        `collection`): by the language's convention it holds that class, when
        the project has exactly one class of that name."""
        if not mod.file.endswith((".rb", ".rake", ".ru")):
            return None
        cls = "".join(w[:1].upper() + w[1:] for w in name.lstrip("@").split("_") if w)
        if not cls or not cls[0].isalpha():
            return None
        short = self.cache.get("\0rbshort")
        if short is None:  # `Jekyll::Site` is `Site` by its last name
            short = self.cache["\0rbshort"] = {}
            for d, mns in self.by_type.items():
                for mn in mns:
                    if self.p.modules[mn].file.endswith(".rb") and self.p.modules[mn].kind != "ext":
                        short.setdefault(d.rsplit("::", 1)[-1], set()).add(mn)
        mods = short.get(cls, set())
        return ("inst", tuple(mods), True) if len(mods) == 1 else None

    def _var_module(self, where: str, name: str, depth: int = 0):
        """(module, its name there) for a module-level variable, through re-exports."""
        m = self.p.modules.get(where)
        if m is None or depth > 4:
            return None
        if name in m.bound and not m.owner and m.file.endswith((".py",) + _JS_EXTS):
            return m, name
        if name in m.imported:
            w, n = m.imported[name]
            return self._var_module(w, n, depth + 1)
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

    def _getter(self, mods: list[str], name: str, depth: int):
        """`c.req` read through a getter (`get req(): HonoRequest`, Python's
        `@property`, Kotlin's `val x get()`): what the getter returns."""
        hits, on_type = self.method(mods, name)
        if not hits or not on_type:
            return None
        g = hits[0]
        raw = self.p.modules[g.module].raw
        head = " ".join(raw[max(0, g.start - 3):g.start])
        if re.search(r"\bget\s+" + re.escape(name) + r"\s*\(", head) or "@property" in head or \
                "@cached_property" in head or "@functools.cached_property" in head:
            return self.returns(g, mods, depth + 1)
        return None

    def _py_classes(self, mods, name: str) -> list[str]:
        """Python: the class a module (or package `__init__`) makes available
        as `name`, through its imports and `from x import *` re-exports."""
        from .core import _py_class
        return [c for mn in mods if mn in self.p.modules and self.p.modules[mn].file.endswith(".py")
                and not self.p.modules[mn].owner and (c := _py_class(self.p, self.p.modules[mn], name))]

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
                nested = [f"{mn}.{name}" for mn in mods if f"{mn}.{name}" in self.p.modules] or self._py_classes(mods, name)
                if nested:  # `Request.Builder()`: builds the nested class (`httpx.Client()`: the class the package exports)
                    return ("inst", tuple(nested), len(nested) == 1)
                hits = [h for mn in mods for h in _in_scope(self.p, mn, name)]
                if not hits:
                    hits, _ = self.method(list(mods), name)
            else:
                hits, _ = self.method(list(mods), name)
            if len(hits) == 1:
                return self.returns(hits[0], list(mods) if kind == "inst" else None, depth + 1)
            if hits and len({g.returns for g in hits}) == 1 and hits[0].returns:
                # overloads that all return the same type: whichever runs, that is the value
                return self.returns(hits[0], list(mods) if kind == "inst" else None, depth + 1)
            if not hits:
                if kind == "static" and seg == "new()" and mod.file.endswith((".gd", ".rb", ".rake", ".ru")):
                    return ("inst", mods, exact)  # `Foo.new()` (GDScript, Ruby): a Foo (no `new` of its own)
                if seg in UNWRAP:
                    return cur  # `.unwrap()` / `.get()` on an Option: the value inside
                if any(self.external_base(mn) for mn in mods):
                    return EXT
            return None
        # a field / property / nested type
        if kind == "static":
            inner = [f"{mn}.{seg}" for mn in mods if f"{mn}.{seg}" in self.p.modules] or self._py_classes(mods, seg)
            if inner:
                return ("static", tuple(inner), len(inner) == 1)
            for mn in mods:
                m = self.p.modules[mn]
                held = m.bound.get(seg)
                if held:
                    return self.held(m, None, held, depth + 1, self_mods=[mn])
                vm = self._var_module(mn, seg) if not m.owner else None
                if vm is not None:  # `flask.g`: the package re-exports globals.g
                    return self.held(vm[0], None, vm[0].bound[vm[1]], depth + 1)
            return None
        got = self.field(list(mods), seg, depth)
        if got is None:
            got = self._getter(list(mods), seg, depth)
        if got is UNKNOWN:
            return None
        if got is None and seg.startswith("__") and seg.endswith("__") and mod.file.endswith(".py"):
            return EXT  # `self.__dict__`, `cls.__name__`: Python's own attributes
        if got is None and seg in UNWRAP:
            return cur
        if got is None and any(self.external_base(mn) for mn in mods):
            return EXT
        return got


_RS_PRIMS = r"u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64|bool|char|str"
_ES_MODULE = (".ts", ".tsx", ".mts", ".cts")
_JS_EXTS = (".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".qml")
_JS_GLOBALS = {"console", "window", "document", "navigator", "localStorage", "sessionStorage", "globalThis",
               "location", "history", "process", "performance", "crypto",
               "vi", "jest", "expect", "cy"}  # (test runners' globals: vitest / jest `globals`, cypress)
_DOM_PROPS = {"classList", "style", "dataset"}
_EXPLICIT_SELF = (".py", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".php")
_PY_BUILTIN_CALLS = {"vars", "dict", "list", "set", "frozenset", "tuple", "str", "bytes", "bytearray", "sorted",
                     "reversed", "enumerate", "zip", "map", "filter", "range", "iter", "repr", "format", "getattr",
                     "type", "open", "globals", "locals", "dir", "int", "float", "bool", "len", "id", "hash"}
_PY_BUILTIN = {"dict", "list", "set", "frozenset", "tuple", "str", "bytes", "bytearray", "int", "float", "bool",
               "defaultdict", "deque", "OrderedDict", "Counter"}
_DYNAMIC = (".gd", ".py", ".js", ".mjs", ".cjs", ".rb", ".php", ".qml")
_GO_BUILTIN = {"string", "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64",
               "float32", "float64", "bool", "byte", "rune", "error", "any", "uintptr", "complex64", "complex128"}


def _parents(d: str) -> list[str]:
    out = [d]
    while d:
        d = d.rsplit("/", 1)[0] if "/" in d else ""
        out.append(d)
    return out


def resolve_bases(project) -> None:
    """Each class's written supertypes -> modules (`parent` = the first);
    a supertype the project doesn't declare marks the class `unresolved` (an
    inherited method may be the library's)."""
    t = Types(project)
    for m in list(project.modules.values()):
        if m.parent and not m.bases and m.parent not in project.modules and m.parent[:1].isupper() \
                and not m.file.endswith((".ex", ".exs", ".rb", ".rake", ".ru")):
            m.unresolved = True  # GDScript `extends CharacterBody3D`: an engine class
        if not m.bases or m.file.endswith((".rb", ".rake", ".ru", ".py")):  # Ruby, Python: resolved by their adapters
            continue
        got, missing = [], False
        fm = project.modules.get(m.owner, m) if m.owner else m
        for b in m.bases:
            if b in fm.external:  # `use std::fmt::Debug`: the library's, not a project type of that name
                missing = True
                continue
            mods, _, declared = t.type_modules(m, b)
            mods = [x for x in mods if x != m.name and project.modules[x].kind != "ext"] or [x for x in mods if x != m.name]
            if mods:
                got.append(mods[0])
            elif b in fm.external or (not declared and not _from_project(project, fm, b)):
                missing = True  # a library supertype (imported from outside, or declared nowhere)
        m.bases = got
        m.unresolved = missing
        if got and not m.parent:
            m.parent = got[0]


def _from_project(project, fm, name: str) -> bool:
    """`name` is imported from a module or package of the project (a re-export
    we couldn't follow), not from a library (`import java.util.ArrayList`)."""
    w = fm.aliases.get(name) or fm.imported.get(name, ("", ""))[0]
    if not w:
        return False
    if w.startswith("pkg:"):
        return w[4:] in project.packages
    return w in project.modules


_IDENT = re.compile(r"^[\w$]+$")
