"""Python (indentation) and brace languages: JS/TS, Rust, Kotlin, Java, C#,
Go. One Module per file (named by its path). Function spans come from
indentation (Python) or brace matching (the rest), on blanked code.
"""
from __future__ import annotations

import re

from .model import Call, Func, Module
from .scan import blank

# extension -> (scanner family, function-declaration regexes)
_ID = r"[A-Za-z_$][\w$]*"
_JS = [
    re.compile(rf"\bfunction\s*\*?\s*({_ID})\s*\("),
    re.compile(rf"\b(?:const|let|var)\s+({_ID})\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*(?::\s*[^=]+)?=>|{_ID}\s*=>)"),
    # object-literal members (`applyOp: (op) => {`): a zustand store, a handler table
    re.compile(rf"^[ \t]*({_ID})[ \t]*:[ \t]*(?:async[ \t]+)?(?:function\b|\([^()\n]*\)[ \t]*(?::[^=\n]+)?=>|{_ID}[ \t]*=>)", re.M),
    re.compile(rf"^\s*(?:(?:public|private|protected|static|async|get|set|override|readonly|export|default)\s+)*({_ID})\s*(?:<[^>\n]*>)?\s*\([^;\n]*\)\s*(?::\s*[^{{;\n]+)?\{{", re.M),
]
_RUST = [re.compile(r"\bfn\s+([A-Za-z_]\w*)")]
_KOTLIN = [re.compile(r"\bfun\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?([A-Za-z_]\w*)\s*\(")]
# (one line, a bounded run of type/modifier words: an unbounded one backtracked
# exponentially on long Java files)
# The parameters may span lines and the body may open on the next line
# (Allman braces, the C# default: every C# method was missed); a C# body may
# be `=> expr;`, a constructor may chain `: base(x)`.
_STMT = r"(?!(?:return|new|else|throw|await|yield|case|goto|using|lock|fixed|checked|unchecked)\b)"
_JAVA = [re.compile(rf"^[ \t]*(?:@\w+(?:\([^)\n]*\))?[ \t]+)*{_STMT}(?:[\w<>\[\],.?]+[ \t]+){{1,6}}([A-Za-z_]\w*)[ \t]*"
                    r"\((?:[^;{}()]|\([^()]*\))*\)[ \t]*(?:throws [\w., ]+)?\s*\{", re.M)]
_CS = [re.compile(rf"^[ \t]*(?:\[[^\]\n]*\][ \t]*)*{_STMT}(?:[\w<>\[\],.?]+[ \t]+){{1,6}}([A-Za-z_]\w*)[ \t]*(?:<[^>\n]*>)?[ \t]*"
                  r"\((?:[^;{}()]|\([^()]*\))*\)[ \t]*(?::[ \t]*(?:base|this)[ \t]*\((?:[^;{}()]|\([^()]*\))*\))?"
                  r"(?:\s*where[^{;=]*)?\s*(?:\{|=>)", re.M)]
_GO = [re.compile(r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)\s*\(", re.M)]
# C++: a definition line has at least one type word before the name, is not a
# statement (return / delete / emit...), and has a body (a declaration or a
# `QString s(x);` constructor call ends in `;` and is dropped). `Class::name`
# keeps the method name; the class is a declared type for qualifier lookup.
_CPP = [re.compile(r"^[ \t]*(?!(?:return|else|delete|new|throw|case|co_return|co_await|emit|Q_EMIT|goto|using|typedef|if|for|while|switch)\b)"
                   r"(?:template[ \t]*<[^>\n]*>[ \t]*)?(?:[A-Za-z_][\w:<>,\*&~]*[ \t]+){1,6}[\*&]*(?:\w+::)*(~?[A-Za-z_]\w*)[ \t]*\(", re.M),
        re.compile(r"^(?:\w+::)+(~?[A-Za-z_]\w*)[ \t]*\(", re.M)]  # out-of-line Class::Class( constructors
# QML: JS functions, signal handlers (`onClicked: {...}` or one line), and
# component instances (`Button {` is a call to Button.qml) handled below.
_QML = [re.compile(rf"\bfunction\s+({_ID})\s*\("),
        re.compile(r"^[ \t]*(on[A-Z]\w*)[ \t]*:", re.M)]
LANGS = {
    ".js": ("js", _JS), ".jsx": ("js", _JS), ".mjs": ("js", _JS), ".cjs": ("js", _JS),
    ".ts": ("js", _JS), ".tsx": ("js", _JS), ".mts": ("js", _JS),
    ".rs": ("rust", _RUST), ".kt": ("kotlin", _KOTLIN), ".kts": ("kotlin", _KOTLIN),
    ".java": ("c", _JAVA), ".cs": ("c", _CS), ".go": ("c", _GO),
    ".cpp": ("c", _CPP), ".cc": ("c", _CPP), ".cxx": ("c", _CPP), ".hpp": ("c", _CPP), ".hh": ("c", _CPP),
    ".h": ("c", _CPP), ".qml": ("js", _QML),
}
# type declarations (Kotlin, Java, C#, TS, Rust, Go): their names qualify calls
_DECL = re.compile(r"\b(?:class|object|interface|enum|struct|record|trait|type)\s+([A-Z][\w]*)")
# Declared types (no inference): `name: Type` in Kotlin / TS declarations and
# parameters, `Type* name` / `Type& name` in C++. The type is one Capitalized
# word (a generic `List<Foo>` is List, outside the project: its calls resolve
# to nothing, which is right).
_KT_TYPED = re.compile(r"(?:\b(?:val|var)\s+|[(,{]\s*|^\s*)([a-z_]\w*)\s*:\s*([A-Z]\w*)(?![\w.])", re.M)
_KT_FIELD = re.compile(r"\b(?:val|var)\s+([a-z_]\w*)\s*:\s*([A-Z]\w*)(?![\w.])")
# built by its constructor: `val prefs = OsaPrefs(ctx)`, `const hub = new HubClient(url)`
_KT_NEW = re.compile(r"\b(?:val|var)\s+([a-z_]\w*)\s*=\s*([A-Z]\w*)\s*\(")
_TS_NEW = re.compile(r"(?:\b(?:const|let|var)\s+|^\s*(?:(?:private|public|protected|readonly|static)\s+)*)"
                     r"([A-Za-z_$][\w$]*)\s*=\s*new\s+([A-Z][\w$]*)\s*[(<]", re.M)
_TS_TYPED = re.compile(r"(?:\b(?:const|let|var)\s+|[(,]\s*|^\s*)([A-Za-z_$][\w$]*)\s*\??\s*:\s*([A-Z][\w$]*)(?![\w$.<\[])", re.M)
_TS_FIELD = re.compile(r"^\s*(?:(?:private|public|protected|readonly|static|declare)\s+)*([a-z_$][\w$]*)\s*[?!]?\s*:\s*([A-Z][\w$]*)(?![\w$.<\[])\s*[;=]", re.M)
_TS_PARAM_FIELD = re.compile(r"\b(?:private|public|protected|readonly)\s+([a-z_$][\w$]*)\s*\??\s*:\s*([A-Z][\w$]*)(?![\w$.<\[])")
# `Client client(...);` at a statement start, `QString name` as a parameter
_CPP_VALUE = re.compile(r"(?:^|[(,])\s*(?:const\s+)?([A-Z]\w*)\s+([a-z_]\w*)\s*(?=[({;=,)])", re.M)
_CPP_TYPED = re.compile(r"(?<![\w:>])([A-Z]\w*)\s*[\*&]+\s*([a-z_]\w*)\s*(?=[,;=)\[{]|$)", re.M)
# a C++ class member in a header: `BoardModel* m_board = nullptr;`, `Hub m_hub;`
_CPP_FIELD = re.compile(r"^\s*(?:(?:mutable|static|const|inline)\s+)*([A-Z]\w*)\s*[\*&]?\s*([a-z_]\w*)\s*(?:=[^;]*|\{[^;]*\})?;")
_KT_RECV = r"\bfun\s+(?:<[^>]*>\s*)?([A-Z]\w*)(?:<[^>]*>)?\??\.%s\s*\("
_ALLCAPS = re.compile(r"[A-Z0-9_]+$")


# Java / C#: `Foo foo` (a local, a parameter), `var x = new Foo(`, and class
# fields with their modifiers (`private readonly Foo _foo;`)
_JV_NEW = re.compile(r"\b(?:var|val|final)\s+([a-z_]\w*)\s*=\s*new\s+([A-Z]\w*)\s*[(<{]")
_JV_FIELD = re.compile(r"^\s*(?:(?:public|private|protected|internal|static|readonly|final|volatile|transient)\s+)*"
                       r"([A-Z]\w*)(?:<[^>;]*>)?\??\s+([a-z_]\w*)\s*(?:=[^;]*)?;")
# Go: `c *Command` / `var x Foo` (receivers, parameters, vars), `x := &Foo{`, and
# struct fields (`flags *flagSet`). Go types may be lowercase (unexported).
_GO_TYPED = re.compile(r"(?:\bvar\s+|[(,]\s*)([A-Za-z_]\w*)\s+\*?([A-Za-z_]\w*)(?=\s*[,)=]|\s*$)", re.M)
_GO_NEW = re.compile(r"\b([A-Za-z_]\w*)\s*:?=\s*&?([A-Za-z_]\w*)\s*\{")
_GO_FIELD = re.compile(r"^\s*([A-Za-z_]\w*)\s+\*?([A-Za-z_]\w*)\s*(?:`[^`]*`)?\s*$")
_GO_KEYWORDS = {"return", "go", "defer", "case", "range", "package", "import", "type", "func", "else", "chan", "map",
                "struct", "interface", "break", "continue", "var", "const"}


def _typed(tk: str, text: str) -> list[tuple[str, str]]:
    """(name, type) declarations in a stretch of blanked code."""
    if tk == "kotlin":
        return [(m.group(1), m.group(2)) for pat in (_KT_TYPED, _KT_NEW) for m in pat.finditer(text)]
    if tk == "js":
        return [(m.group(1), m.group(2)) for pat in (_TS_TYPED, _TS_NEW) for m in pat.finditer(text)]
    if tk in ("cpp", "jvm"):
        out = [(m.group(2), m.group(1)) for pat in ((_CPP_TYPED, _CPP_VALUE) if tk == "cpp" else (_CPP_VALUE,))
               for m in pat.finditer(text) if not _ALLCAPS.match(m.group(1))]
        if tk == "jvm":
            out += [(m.group(1), m.group(2)) for m in _JV_NEW.finditer(text)]
        return out
    if tk == "go":
        return [(m.group(1), m.group(2)) for pat in (_GO_TYPED, _GO_NEW) for m in pat.finditer(text)
                if m.group(1) not in _GO_KEYWORDS and m.group(2) not in _GO_KEYWORDS]
    return []


_TEST_ATTR = re.compile(r"@Test\b|\[(?:Fact|Theory|Test|TestMethod|TestCase|DataTestMethod)\b|#\[(?:[\w:]+::)?test\b")
_JS_TEST = re.compile(r"^[ \t]*(?:it|test)(?:\.(?:only|skip|todo|concurrent|each\([^)]*\)))?[ \t]*\(", re.M)
# Imports, read from the raw text (the blanked code has lost the paths). JS/TS
# specifiers are kept as "js:<file dir>\0<spec>" for core to resolve against
# the files (and tsconfig `paths`) once everything is parsed.
_JS_IMPORT = re.compile(r"""^[ \t]*import[ \t]+(?!type\b)([^;'"]*?)\s+from\s+["']([^"']+)["']""", re.M)
_JS_REEXPORT_SKIP = re.compile(r"^\s*type\s+")
_KT_PACKAGE = re.compile(r"^[ \t]*package[ \t]+([\w.]+)", re.M)
_KT_IMPORT = re.compile(r"^[ \t]*import[ \t]+([\w.]+?)(\.\*)?(?:[ \t]+as[ \t]+(\w+))?[ \t]*;?[ \t]*$", re.M)
_JSX = re.compile(r"<(?:([A-Za-z_$][\w$]*)\.)?([A-Z][\w$]*)(?=[\s/>]|$)")
_NOT_FUNC = {"if", "for", "while", "switch", "catch", "return", "function", "else", "do", "try", "with", "new",
             "typeof", "match", "loop", "when", "sizeof", "constructor_", "super"}
_CALL = re.compile(rf"(?:({_ID})\s*(?:\.|::|\?\.|->)\s*)?\b({_ID})\s*(?:::<[^()]*>|<[\w\s,<>\[\]]*>)?\s*\(")
_CALL_SKIP = _NOT_FUNC | {"catch", "require", "import", "print", "println", "Some", "Ok", "Err", "Box", "Vec",
                          "String", "assert", "setOf", "listOf", "mapOf"}

_PY_BLOCK = re.compile(r"^(\s*)(?:async\s+)?(def|class)\s+([A-Za-z_]\w*)")
_PY_CALL = re.compile(r"(?:\b([A-Za-z_]\w*)\s*\.\s*)?\b([A-Za-z_]\w*)\s*\(")
_PY_SKIP = {"if", "elif", "while", "for", "return", "and", "or", "not", "in", "is", "print", "len", "range",
            "str", "int", "float", "bool", "list", "dict", "set", "tuple", "isinstance", "super", "lambda",
            "min", "max", "abs", "sorted", "enumerate", "zip", "open", "any", "all", "sum", "map", "filter",
            "getattr", "setattr", "hasattr", "type", "repr", "round", "with", "assert", "yield", "await", "except",
            "def", "class", "del", "raise", "import", "from", "as", "async", "else", "try", "finally", "global",
            "nonlocal", "pass", "case", "match", "issubclass", "iter", "next", "id", "hash", "vars", "dir",
            "callable", "format", "frozenset", "bytes", "bytearray", "object", "reversed", "divmod", "pow", "chr",
            "ord", "hex", "oct", "bin", "input", "exec", "eval", "compile", "globals", "locals", "memoryview",
            "property", "staticmethod", "classmethod", "slice", "complex", "breakpoint", "aiter", "anext"}
_PY_IMPORT = re.compile(r"^[ \t]*import[ \t]+([\w. \t,]+)$", re.M)
_PY_FROM = re.compile(r"^[ \t]*from[ \t]+(\.*[\w.]*)[ \t]+import[ \t]+(\([^)]*\)|[^\n]+)", re.M)
# `x = Foo(`, `x: Foo` (a parameter or a local); `self.x = Foo(` / `self.x: Foo` for the class
_PY_NEW = re.compile(r"(?:^|[;(,]|\bself\.)[ \t]*([A-Za-z_]\w*)[ \t]*(?::[^=\n]*)?=[ \t]*((?:[a-z_]\w*\.)*[A-Z]\w*)[ \t]*\(", re.M)
_PY_ANN = re.compile(r"(?:^|[(,]|\bself\.)[ \t]*([A-Za-z_]\w*)[ \t]*:[ \t]*([A-Z]\w*)(?![\w.\[])", re.M)
_PY_SELF_ATTR = re.compile(r"\bself\.([A-Za-z_]\w*)[ \t]*(?::[ \t]*([A-Z]\w*)(?![\w.\[])[^=\n]*)?(?:=[ \t]*((?:[a-z_]\w*\.)*[A-Z]\w*)[ \t]*\()?")
# a function used as a value: `key=fn`, `(fn,`, `[fn]`, `return fn`, `callback=self.on_x`
_PY_VALUE = re.compile(r"(?:[=(,\[:]|\breturn)\s*(?:self\.|cls\.)?([A-Za-z_]\w*)\s*(?=[,)\]}]|$)", re.M)


def _modname(rel: str) -> str:
    return rel.rsplit(".", 1)[0]


def _py_test_file(rel: str) -> bool:
    base = rel.rsplit("/", 1)[-1]
    return (base.startswith("test_") or base.endswith("_test.py") or base in ("tests.py", "conftest.py")
            or "/tests/" in "/" + rel or "/test/" in "/" + rel)


def _py_imports(mod: Module, rel: str, code: str) -> None:
    """`import a.b as c`, `from .x import y as z`, `from x import *`, kept as
    "py:<dir>\0<spec>" for core to resolve against the project's files."""
    d = rel.rsplit("/", 1)[0] if "/" in rel else ""
    where = lambda spec: f"py:{d}\0{spec}"  # noqa: E731
    for m in _PY_IMPORT.finditer(code):
        for part in m.group(1).split(","):
            bits = part.split()
            if not bits:
                continue
            path = bits[0]
            if len(bits) == 3 and bits[1] == "as":
                mod.aliases[bits[2]] = where(path)
                continue
            # `import a.b.c` binds `a`; `a.b.c.f()` reads as a call on `c`
            mod.aliases.setdefault(path.split(".")[0], where(path.split(".")[0]))
            if "." in path:
                mod.aliases.setdefault(path.rsplit(".", 1)[1], where(path))
    for m in _PY_FROM.finditer(code):
        spec, names = m.group(1), m.group(2).strip().strip("()")
        for part in names.split(","):
            bits = part.split()
            if not bits:
                continue
            if bits[0] == "*":
                mod.uses.append(where(spec))
                mod.reexports.append(where(spec))
                continue
            local = bits[2] if len(bits) == 3 and bits[1] == "as" else bits[0]
            mod.imported[local] = (where(spec), bits[0])


def parse_python(path: str, rel: str, text: str, res_of=None) -> list[Module]:
    """One Module for the file, and one per class (`<file>.<Class>`, its
    parent the first base): a method is `self.x()`'s target, and two classes'
    `get` are two functions (they used to overwrite each other: 15-20% of a
    Python project's functions were lost)."""
    code, strings = blank(text, "python")
    raw, lines = text.split("\n"), code.split("\n")
    fm = Module(_modname(rel), rel, len(raw), raw, lines)
    _py_imports(fm, rel, code)
    blocks = []  # (start, end, indent, kind, name)
    for i, ln in enumerate(lines):
        m = _PY_BLOCK.match(ln)
        if not m:
            continue
        indent = len(m.group(1).expandtabs(4))
        end = len(lines) - 1
        for j in range(i + 1, len(lines)):
            t = lines[j]
            if t.strip() == "":
                continue
            if len(t[:len(t) - len(t.lstrip())].expandtabs(4)) <= indent and not t.lstrip().startswith((")", "]")):
                end = j - 1
                break
        while end > i and lines[end].strip() == "":
            end -= 1
        blocks.append((i, end, indent, m.group(2), m.group(3)))

    def enclosing(k: int) -> int:
        """Index of the innermost block holding block k (-1: the file)."""
        a, _, ind, _, _ = blocks[k]
        for j in range(k - 1, -1, -1):
            b0, b1, bi, _, _ = blocks[j]
            if b0 < a <= b1 and bi < ind:
                return j
        return -1

    outer = [enclosing(k) for k in range(len(blocks))]
    test_file = _py_test_file(rel)
    mods = {"": fm}
    owner_of: dict[int, str] = {}  # block -> module name its defs belong to
    for k, (a, b, ind, kind, name) in enumerate(blocks):
        if kind != "class":
            continue
        chain, j = [name], outer[k]
        while j >= 0:
            if blocks[j][3] == "class":
                chain.append(blocks[j][4])
            j = outer[j]
        mname = fm.name + "." + ".".join(reversed(chain))
        owner_of[k] = mname
        if mname in mods:
            continue
        cm = Module(mname, rel, len(raw), raw, lines, owner=fm.name, decls=[name])
        head = " ".join(x.strip() for x in lines[a:min(b, a + 5) + 1])
        bm = re.match(r"\s*class\s+\w+\s*\(([^)]*)\)", head)
        if bm:
            bases = [x.strip() for x in bm.group(1).split(",") if x.strip() and "=" not in x]
            bases = [x for x in bases if x not in ("object", "ABC", "Protocol", "Generic") and not x.startswith("Generic[")]
            if bases:
                cm.parent = "py:" + bases[0].split("[")[0]
        mods[mname] = cm
    inside = [False] * len(lines)
    funcs: dict[tuple[str, str], Func] = {}
    for k, (a, b, ind, kind, name) in enumerate(blocks):
        if kind != "def":
            continue
        for i in range(a, b + 1):
            inside[i] = True
        o = outer[k]
        mname = owner_of[o] if o >= 0 and blocks[o][3] == "class" else fm.name
        is_test = test_file and name.startswith("test") and (o < 0 or blocks[o][3] == "class")
        kind_ = "test" if is_test else "private" if name.startswith("_") and not name.startswith("__") else "func"
        f = funcs.get((mname, name))
        if f is None:
            f = Func(mname, name, rel, a + 1, b + 1, "\n".join(lines[a:b + 1]), kind=kind_)
            funcs[(mname, name)] = f
            (mods.get(mname) or fm).funcs.append(f)
        elif b - a > f.end - f.start:  # `@property x` and its `@x.setter`: one function, the longest span
            f.start, f.end, f.code = a + 1, b + 1, "\n".join(lines[a:b + 1])
        _py_collect(f, lines, a, b, first=True)
    top = Func(fm.name, "<top>", rel, 1, len(lines), "", kind="top")
    for i, ln in enumerate(lines):
        if not inside[i] and ln.strip() and not ln.lstrip().startswith(("class ", "import ", "from ")):
            _py_collect(top, lines, i, i, first=False)
    fm.funcs.append(top)
    # declared types: the class's `self.x = Foo()` / `self.x: Foo` for its methods
    for k, (a, b, ind, kind, name) in enumerate(blocks):
        if kind != "class":
            continue
        cm = mods[owner_of[k]]
        seen: dict[str, set[str]] = {}
        for i in range(a, b + 1):
            for m in _PY_SELF_ATTR.finditer(lines[i]):
                ty = m.group(2) or m.group(3)
                if ty:
                    seen.setdefault(m.group(1), set()).add(ty)
        for nm, tys in seen.items():
            if len(tys) == 1:
                cm.bound.setdefault(nm, "type:" + next(iter(tys)))
    for cm in mods.values():
        if cm is not fm:
            # bare names in a class resolve through its file: share its imports
            cm.aliases, cm.imported, cm.uses = fm.aliases, fm.imported, fm.uses
    return [fm] + [m for m in mods.values() if m is not fm]


def _py_collect(f: Func, lines: list[str], a: int, b: int, first: bool) -> None:
    for i in range(a, b + 1):
        t = lines[i]
        if first and i == a:
            t = t.split("(", 1)[1] if "(" in t else ""
        for m in _PY_CALL.finditer(t):
            name, q = m.group(2), m.group(1) or ""
            if not q:
                before = t[:m.start()].rstrip()
                if re.search(r"\bsuper\(\s*\)\s*\.$", before):
                    q = "super"
                elif before.endswith("."):
                    q = "."  # `x().name(` / `a[0].name(`: a method on something we can't name
                elif name in _PY_SKIP:
                    continue
            f.calls.append(Call(name, q, i + 1))
        if f.kind != "top":
            for m in _PY_NEW.finditer(t):
                f.bound[m.group(1)] = "type:" + m.group(2)
            for m in _PY_ANN.finditer(t):
                f.bound.setdefault(m.group(1), "type:" + m.group(2))
        for m in _PY_VALUE.finditer(t):
            f.refs.add(m.group(1))


_KT_CONT_END = ("=", "+", "-", "*", "/", "%", "&&", "||", "?:", "(", "[", ",", "->", ".", "?.", "!", "<", ">", "..")
_KT_CONT_START = (".", "?.", "?:", "&&", "||", "+", "-", "*", "/", "%", "->", ")", "]", "}", "..", "as ", "as?", "is ", "!is ", "in ", "!in ", "else")


def _kotlin_body(code: str, after_paren: int, n: int) -> tuple[int | None, int | None]:
    """Kotlin function body from just after the opening `(` of its parameters.

    Returns (block_start, None) for `{ ... }` (matched by the caller),
    (None, end) for `= expr`, or (None, None) for a declaration with no body.
    The parameter list is matched in full: Compose screens have 60 parameters,
    and a length cap on the signature lost the whole body. An expression body
    ends at the first newline at depth 0 that does not continue the
    expression; the old scan ran on to the next `{`, the NEXT function's.
    """
    k, depth = after_paren, 1
    while k < n and depth > 0:  # the parameter list (lambdas and defaults included)
        ch = code[k]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        k += 1
    # return type / where clause, then the body
    depth = 0
    while k < n:
        ch = code[k]
        if ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        elif depth <= 0 and ch == "{":
            return k, None
        elif depth <= 0 and ch == "=" and code[k + 1:k + 2] not in ("=", ">") and code[k - 1:k] not in ("!", "<", ">", "="):
            return None, _kotlin_expr_end(code, k + 1, n)
        elif depth <= 0 and ch == "\n":
            rest = code[k + 1:k + 200].lstrip()
            if not rest.startswith(("{", "=", ":", "where")) and not code[:k].rstrip().endswith((":", ",")):
                return None, None  # abstract / interface declaration
        k += 1
    return None, None


def _kotlin_expr_end(code: str, k: int, n: int) -> int:
    """End of an expression body: a newline at depth 0 that the next line does
    not continue (and that does not end on an operator)."""
    depth = 0
    while k < n:
        ch = code[k]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
            if depth < 0:  # closed an enclosing scope: the expression ended before it
                return k - 1
        elif ch == "\n" and depth <= 0:
            here = code[code.rfind("\n", 0, k) + 1:k].rstrip()
            nxt_end = code.find("\n", k + 1)
            nxt = code[k + 1:nxt_end if nxt_end != -1 else n].lstrip()
            if here == "" or not (here.endswith(_KT_CONT_END) or nxt.startswith(_KT_CONT_START)):
                return k
        k += 1
    return n - 1


def _real(lines: list[str], text: str, pos: int) -> bool:
    """The match at `pos` is code, not inside a comment (blanked there)."""
    ln = text.count("\n", 0, pos)
    return ln < len(lines) and lines[ln].strip().startswith("import")


def _js_imports(mod: Module, rel: str, text: str, lines: list[str]) -> None:
    d = rel.rsplit("/", 1)[0] if "/" in rel else ""
    for m in _JS_IMPORT.finditer(text):
        if not _real(lines, text, m.start()):
            continue
        clause, where = m.group(1).strip(), f"js:{d}\0{m.group(2)}"
        named = re.search(r"\{([^}]*)\}", clause)
        if named:
            for part in named.group(1).split(","):
                part = _JS_REEXPORT_SKIP.sub("", part.strip())
                if not part:
                    continue
                src, _, local = part.partition(" as ")
                mod.imported[(local or src).strip()] = (where, src.strip())
            clause = clause[:named.start()] + clause[named.end():]
        ns = re.search(r"\*\s+as\s+([A-Za-z_$][\w$]*)", clause)
        if ns:
            mod.aliases[ns.group(1)] = where
            clause = clause[:ns.start()] + clause[ns.end():]
        default = re.match(r"\s*([A-Za-z_$][\w$]*)", clause)
        if default:
            # a default export: a function (called bare) or an object (X.method)
            mod.imported[default.group(1)] = (where, default.group(1))
            mod.aliases[default.group(1)] = where


def _kt_imports(mod: Module, text: str, lines: list[str]) -> None:
    pm = _KT_PACKAGE.search(text)
    if pm:
        mod.package = pm.group(1)
    for m in _KT_IMPORT.finditer(text):
        if not _real(lines, text, m.start()):
            continue
        path, star, alias = m.group(1), m.group(2), m.group(3)
        if star:
            mod.uses.append(f"pkg:{path}")
            continue
        pkg, _, last = path.rpartition(".")
        if last[:1].islower():  # a top-level function (or property)
            mod.imported[alias or last] = (f"pkg:{pkg}", last)
        else:  # a type: its file is in scope for method-name matches
            mod.uses.append(f"type:{last}")


_CS_NAMESPACE = re.compile(r"^[ \t]*namespace[ \t]+([\w.]+)", re.M)
_CS_USING = re.compile(r"^[ \t]*(?:global[ \t]+)?using[ \t]+(?:static[ \t]+)?([\w.]+)[ \t]*;", re.M)


def _cs_imports(mod: Module, code: str) -> None:
    """C#: a file's namespace is its package (its types see each other), and
    each `using A.B;` brings that namespace's files into scope."""
    nm = _CS_NAMESPACE.search(code)
    if nm:
        mod.package = nm.group(1)
    for m in _CS_USING.finditer(code):
        mod.uses.append(f"pkg:{m.group(1)}")


def parse_brace(path: str, rel: str, text: str, res_of=None) -> list[Module]:
    ext = "." + rel.rsplit(".", 1)[-1]
    fam, pats = LANGS[ext]
    code, strings = blank(text, fam)
    raw, lines = text.split("\n"), code.split("\n")
    mod = Module(_modname(rel), rel, len(raw), raw, lines)
    line_start = [0]
    for ln in lines[:-1]:
        line_start.append(line_start[-1] + len(ln) + 1)

    def line_of(pos: int) -> int:
        lo, hi = 0, len(line_start) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if line_start[mid] <= pos:
                lo = mid
            else:
                hi = mid - 1
        return lo

    mod.decls = sorted({d.group(1) for d in _DECL.finditer(code)})
    if fam == "js":
        _js_imports(mod, rel, text, lines)
    elif fam == "kotlin" or ext == ".java":
        _kt_imports(mod, text, lines)
    elif ext == ".cs":
        _cs_imports(mod, code)
    seen: set[tuple[int, str]] = set()
    found = []
    kotlin = fam == "kotlin"
    cpp = ext in (".cpp", ".cc", ".cxx", ".hpp", ".hh", ".h")
    qml = ext == ".qml"
    for pat in pats:
        for m in pat.finditer(code):
            name = m.group(1)
            if name in _NOT_FUNC:
                continue
            start_line = line_of(m.start(1))
            if (start_line, name) in seen:
                continue
            seen.add((start_line, name))
            # Body: the first { after the signature (at paren depth 0), matched;
            # or, for `=> expr` / `= expr`, the rest of the statement.
            # (a match ending at `name(` is inside the parameters: a destructured
            # `({ a, b }: {...}) {` must not read as the body)
            k, n = m.end(), len(code)
            if code[m.end() - 1] == "{":
                k -= 1  # the declaration matched the body's `{`: the scan must see it (it took the next `;` as the end)
            depth = 1 if code[m.end() - 1] == "(" else 0
            body_start, body_end = None, None
            if qml and name.startswith("on"):
                # a handler: `onX: {` block, or the rest of the line
                nl = code.find("\n", m.end())
                nl = n if nl < 0 else nl
                brace = code.find("{", m.end(), nl)
                if brace >= 0 and not code[m.end():brace].strip():
                    k = brace
                    depth = 0
                    body_start = brace
                else:
                    body_end = nl - 1 if nl > 0 else m.end()
                    k = n
            if kotlin:
                body_start, body_end = _kotlin_body(code, m.end(), n)
                if body_start is None and body_end is None:
                    body_end = m.end()
                k = n  # skip the generic scan
            limit = min(n, m.end() + 4000)  # a signature is never longer than this
            while k < limit:
                ch = code[k]
                if ch in "([":
                    depth += 1
                elif ch in ")]":
                    depth -= 1
                elif depth <= 0 and ch == "{":
                    body_start = k
                    break
                elif depth <= 0 and ch == ";":
                    body_end = k  # a declaration without a body (trait/interface) or an expression body
                    break
                elif depth <= 0 and ch == "\n" and "=>" in code[m.start():k] and not code[max(m.start(), k - 8):k].rstrip().endswith("=>"):
                    body_end = k
                    break
                k += 1
            if body_start is not None:
                d, k = 0, body_start
                while k < n:
                    if code[k] == "{":
                        d += 1
                    elif code[k] == "}":
                        d -= 1
                        if d == 0:
                            break
                    k += 1
                body_end = k
            if cpp and body_start is None:
                continue  # a declaration, prototype or `Type var(args);`: not a function
            if qml and name.startswith("on") and body_start is None:
                body_end = code.find("\n", m.end())  # `onX: expr` on one line
                body_end = n - 1 if body_end < 0 else body_end
            if body_end is None:
                body_end = min(n - 1, m.end() + 200)
            found.append((start_line, line_of(body_end), name))
    tests: set[tuple[int, str]] = set()
    if fam == "js":
        # it("...") / test("...") blocks (vitest, jest, node:test): anonymous
        # callbacks, so the declaration patterns never saw them and a test file
        # had no calls. Named by their label, spanning to the matching `)`.
        first_str: dict[int, str] = {}
        for ln, st in strings:
            first_str.setdefault(ln - 1, st)
        n = len(code)
        for m in _JS_TEST.finditer(code):
            start_line = line_of(m.start())
            label = first_str.get(start_line, f"line {start_line + 1}")
            k, depth = m.end() - 1, 0
            while k < n:
                if code[k] in "([{":
                    depth += 1
                elif code[k] in ")]}":
                    depth -= 1
                    if depth == 0:
                        break
                k += 1
            name = f"test {label}"
            found.append((start_line, line_of(k), name))
            tests.add((start_line, name))
    jsx = ext in (".tsx", ".jsx")
    # One function per name (ids are module.name). Same-named spans merge: the
    # longest is the span (a type member `applyOp: (op: Op) => void;` loses to
    # its implementation), and the calls of every span are kept.
    spans: dict[str, list[tuple[int, int]]] = {}
    for a, b, name in sorted(found):
        spans.setdefault(name, []).append((a, b))
    # declared types: per line (a function takes the ones on its lines), and
    # file-level ones from the lines outside every function (+ TS constructor
    # parameter properties)
    typed_by_line: dict[int, list[tuple[str, str]]] = {}
    tk = "cpp" if cpp else "jvm" if ext in (".java", ".cs") else "go" if ext == ".go" else fam
    if tk in ("kotlin", "js", "cpp", "jvm", "go"):
        for i, ln in enumerate(lines):
            if ":" in ln or tk != fam or "=" in ln:  # `x: T`, or `val x = T(...)` / `new T(...)`
                got = _typed(tk, ln)
                if got:
                    typed_by_line[i] = got
        inside = [False] * len(lines)
        for a, b, _ in found:
            for i in range(a, min(b + 1, len(lines))):
                inside[i] = True
        fields: dict[str, set[str]] = {}
        for i, ln in enumerate(lines):
            pats_ = []
            if fam == "kotlin":
                pats_ = [] if inside[i] else [_KT_FIELD, _KT_NEW]
            elif fam == "js":
                pats_ = [_TS_PARAM_FIELD] + ([] if inside[i] else [_TS_FIELD, _TS_NEW])
            elif tk == "jvm" and not inside[i]:
                for m in _JV_FIELD.finditer(ln):
                    fields.setdefault(m.group(2), set()).add(m.group(1))
            elif tk == "go" and not inside[i]:
                for m in _GO_FIELD.finditer(ln):
                    if m.group(1) not in _GO_KEYWORDS and m.group(2) not in _GO_KEYWORDS:
                        fields.setdefault(m.group(1), set()).add(m.group(2))
            elif cpp and not inside[i]:
                for m in _CPP_FIELD.finditer(ln):
                    if not _ALLCAPS.match(m.group(1)) and m.group(1) not in ("Q_OBJECT", "Q_PROPERTY"):
                        fields.setdefault(m.group(2), set()).add(m.group(1))
            for pat_ in pats_:
                for m in pat_.finditer(ln):
                    fields.setdefault(m.group(1), set()).add(m.group(2))
        mod.bound = {k: "type:" + next(iter(v)) for k, v in fields.items() if len(v) == 1}
    for name, ss in sorted(spans.items(), key=lambda kv: kv[1][0]):
        a, b = max(ss, key=lambda s: s[1] - s[0])
        # JUnit / kotlin.test: @Test on the declaration or the lines just above it
        # JUnit / kotlin.test `@Test`, xUnit / NUnit / MSTest `[Fact]`, Rust `#[test]`
        annotated = fam in ("kotlin", "c", "rust") and any(_TEST_ATTR.search(raw[j]) for j in range(max(0, a - 3), a + 1))
        f = Func(mod.name, name, rel, a + 1, b + 1, "\n".join(lines[a:b + 1]),
                 kind="test" if (a, name) in tests or annotated else "func")
        if kotlin:
            rm = re.search(_KT_RECV % re.escape(name), lines[a])
            if rm:
                f.recv = rm.group(1)
        seen_lines: set[int] = set()
        for sa, sb in ss:
            name_col = max(0, lines[sa].find(name))  # the return type before it is no call (`Task<int> Run(`)
            for i in range(sa, sb + 1):
                if i in seen_lines:
                    continue
                seen_lines.add(i)
                for nm_, ty in typed_by_line.get(i, ()):
                    f.bound[nm_] = "type:" + ty
                for m in _CALL.finditer(lines[i]):
                    nm = m.group(2)
                    if nm in _CALL_SKIP or (i == sa and (nm == name or m.start(2) < name_col)):
                        continue
                    # `x().name(` / `a?.b.name(`: a method on something we can't
                    # name, qualified "." (not unqualified: that means in scope)
                    q = m.group(1) or ("." if lines[i][:m.start(2)].rstrip().endswith((".", "->")) else "")
                    f.calls.append(Call(nm, q, i + 1))
                if jsx:  # `<Widget ...>` renders (calls) the component Widget
                    for m in _JSX.finditer(lines[i]):
                        f.calls.append(Call(m.group(2), m.group(1) or "", i + 1))
        mod.funcs.append(f)
    top = Func(mod.name, "<top>", rel, 1, len(lines), "", kind="top")
    if ext == ".qml":
        # `Button {` instantiates Button.qml: a use of that component
        for i, ln in enumerate(lines):
            cm = re.match(r"[ \t]*([A-Z]\w*)[ \t]*\{", ln)
            if cm:
                top.calls.append(Call(cm.group(1), cm.group(1), i + 1))
                top.refs.add(cm.group(1))
    mod.funcs.append(top)
    return [mod]
