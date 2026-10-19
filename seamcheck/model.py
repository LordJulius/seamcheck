"""The shared model every language adapter fills in.

A codebase is a set of Modules; a Module holds Funcs; a Func knows its span,
its code (comments and string contents blanked, same line layout), and the
calls it makes. Rules and queries only ever see this model, so adding a
language means writing one adapter.
"""
from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class Call:
    name: str  # the called function's name
    qual: str  # what it was called on ("" = unqualified, "super", a module alias, a variable)
    line: int  # 1-based line in the file


@dataclass
class Func:
    module: str
    name: str
    file: str
    start: int  # 1-based, inclusive
    end: int  # 1-based, inclusive
    code: str  # the body with comments / string contents blanked
    calls: list[Call] = field(default_factory=list)
    refs: set[str] = field(default_factory=set)  # names referenced without a call (callbacks, "method" strings)
    # local variable -> what it holds, as the file wrote it down: "group:<id>"
    # (GDScript get_first_node_in_group, named in [project.groups]) or
    # "type:<Name>" (a declared type: Kotlin / TS `name: Type`, C++ `Type* name`)
    bound: dict[str, str] = field(default_factory=dict)
    recv: str = ""  # Kotlin extension function: the receiver type (`fun JSONObject.opt(`)
    kind: str = "func"  # "func", "top" (module-level code), "private"...
    id: str = ""  # "Module.name" (set in __post_init__; kept in sync when a module is renamed)

    def __post_init__(self) -> None:
        self.id = f"{self.module}.{self.name}"


@dataclass
class Module:
    name: str
    file: str
    lines: int
    raw: list[str]  # the original lines (for suppression comments, snippets)
    code: list[str]  # lines with comments / string contents blanked
    funcs: list[Func] = field(default_factory=list)
    parent: str = ""  # extends / parent module ("" = none)
    aliases: dict[str, str] = field(default_factory=dict)  # local name -> module name
    # Types this file declares (class / object / interface / enum / struct names),
    # for languages whose modules are named by path: `MapPrefs.set` names the file.
    decls: list[str] = field(default_factory=list)
    # Unqualified calls resolve through these, after the module's own functions:
    # `package` (Kotlin/Java: files of one package see each other), `uses`
    # (modules or "pkg:a.b" packages imported whole: Elixir `import`, Kotlin
    # `import a.b.*`) and `imported` (local name -> (module or "pkg:a.b", name):
    # JS `import {a as b} from "./x"`, Kotlin `import a.b.fn`).
    package: str = ""
    uses: list[str] = field(default_factory=list)
    imported: dict[str, tuple[str, str]] = field(default_factory=dict)
    # file-level declared types (Kotlin properties and constructor vals, TS class
    # fields): name -> "type:<Name>", for calls in every function of the file
    # that don't declare the name themselves. A name with two types is left out.
    bound: dict[str, str] = field(default_factory=dict)

    def func_at(self, line: int) -> Func | None:
        best = None
        for f in self.funcs:
            if f.start <= line <= f.end and (best is None or f.start >= best.start):
                best = f
        return best
