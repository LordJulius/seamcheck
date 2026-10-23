"""Python (indentation) and brace languages: JS/TS, Rust, Kotlin, Java, C#,
Go. One Module per file (named by its path). Function spans come from
indentation (Python) or brace matching (the rest), on blanked code.
"""
from __future__ import annotations

import os
import re

from . import lambdas
from .chains import _top_commas, elem_type, expression, generic_name, receiver, type_name
from .model import Call, Func, Module
from .scan import blank

# extension -> (scanner family, function-declaration regexes)
_ID = r"[A-Za-z_$][\w$]*"
# one TS type, as a function type's result reads: `void`, `Promise<T>`, `Foo[]`
_TS_TYPE = r"(?:void|string|number|boolean|unknown|any|never|null|undefined|bigint|object|[A-Z][\w$.]*(?:<[^()\n]*>)?(?:\[\])?)"
_JS = [
    re.compile(rf"\bfunction\s*\*?\s*({_ID})\s*\("),
    # `exports.foo = function (`, `app.use = function (`, `Foo.prototype.bar = async function (`:
    # an anonymous function stored as a property is called by that property's name
    re.compile(rf"^[ \t]*(?:{_ID}\.)+({_ID})[ \t]*=[ \t]*(?:async[ \t]+)?function[ \t]*\*?[ \t]*\(", re.M),
    # `res.set = res.header = function header(`: each name in the chain is the function
    re.compile(rf"^[ \t]*(?:{_ID}\.)+({_ID})[ \t]*=\s*(?:(?:{_ID}\.)+{_ID}[ \t]*=\s*)+(?:async[ \t]+)?function\b[^(\n]*\(", re.M),
    # `this.use = (arg, ...handlers) => {` in a constructor: a member of the class
    re.compile(rf"^[ \t]*this\.({_ID})[ \t]*=[ \t]*(?:async[ \t]+)?(?:\((?:[^()]|\([^()]*\))*\)[ \t]*(?::[^=\n]+?)?=>|{_ID}[ \t]*=>)", re.M),
    # (the annotation may be a function type: `const mergePath: (...p: string[]) => string = (`)
    re.compile(rf"\b(?:const|let|var)\s+({_ID})\s*(?::(?:[^=;]|=>){{1,300}}?)?=\s*(?:async\s+)?"
               rf"(?:function\b|\((?:[^()]|\([^()]*\))*\)\s*(?::\s*[^=]+)?=>|{_ID}\s*=>)"),
    # object-literal members (`applyOp: (op) => {`): a zustand store, a handler table
    # (not a parameter's function type: `fn: (...args: T) => void,`)
    re.compile(rf"^[ \t]*({_ID})[ \t]*:[ \t]*(?:async[ \t]+)?(?:function\b|\([^()\n]*\)[ \t]*(?::[^=\n]+)?=>|{_ID}[ \t]*=>)"
               rf"(?![ \t]*{_TS_TYPE}(?:[ \t]*[|&][ \t]*{_TS_TYPE})*[ \t]*(?:[,;)]|$))", re.M),
    # a method `name(params) {`; its parameters hold no string (`describe('x', function () {` is a call)
    # (a generic may nest: `mutateElement<T extends Mutable<Element>>(`; the parameters may span lines)
    re.compile(rf"^[ \t]*(?:(?:public|private|protected|static|async|get|set|override|readonly|export|default)\s+)*({_ID})[ \t]*"
               rf"(?:<[^<>\n]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>[^<>\n]*)*>)?\s*\((?:[^;()'\"`]|\([^;()'\"`]*\))*\)\s*(?::\s*[^{{;\n]+)?\{{", re.M),
    # class fields holding an arrow function (`private onPointerDown = (e) => {`,
    # `static create = <T,>(\n  {a, b}: Opts,\n) => {`): a React class
    # component's handlers, none of which were functions
    re.compile(rf"^[ \t]*(?:(?:public|private|protected|static|readonly|override|declare|export|default|const|let|var)[ \t]+)*({_ID})[ \t]*[?!]?[ \t]*"
               rf"(?::[^=\n<]*(?:<[^;]{{0,800}}?>[^=\n]*)?)?=[ \t]*(?:async[ \t]+)?(?:<[^()]*?>[ \t]*)?"
               rf"(?:\((?:[^()]|\([^()]*\))*\)[ \t]*(?::[^=;]+?)?=>|{_ID}[ \t]*=>)", re.M),
    # a method's start only (`static createElement<T extends X>(\n  {a}: {b?: number;} = {},\n): R & {...} {`):
    # the body scan below matches the brackets, and without a body it is a call
    re.compile(rf"(?P<tsstart>)^[ \t]*(?:(?:public|private|protected|static|async|get|set|override|abstract)[ \t]+)*({_ID})[ \t]*"
               rf"(?:<[^<>\n]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>[^<>\n]*)*>)?[ \t]*\(", re.M),
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
_CS = [re.compile(rf"^[ \t]*(?:\[[^\]\n]*\][ \t]*)*{_STMT}(?:(?:[\w<>\[\],.?]+|[\w.]*<?\((?:[^()\n;{{}}=]|\([^()\n]*\))*\)[>?\[\]]*)[ \t]+){{1,6}}([A-Za-z_]\w*)[ \t]*(?:<[^>\n]*>)?[ \t]*"
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
# Swift: `func name(` / `func name<T>(`, and initializers (`Foo(...)` runs `init`)
_SWIFT_MODS = r"(?:(?:@\w+(?:\([^)\n]*\))?|public|private|internal|fileprivate|open|static|class|final|override|convenience|required|mutating|nonmutating|nonisolated|dynamic|optional|package)\s+)*"
_SWIFT = [re.compile(r"\bfunc\s+([A-Za-z_]\w*)\s*(?:<[^>\n]*>)?\s*\("),
          re.compile(rf"^[ \t]*{_SWIFT_MODS}(init)\s*[?!]?\s*(?:<[^>\n]*>)?\s*\(", re.M)]
# PHP: `function name(` (methods, functions; `__construct` is what `new Foo` runs)
_PHP = [re.compile(r"\bfunction\s+&?\s*([A-Za-z_]\w*)\s*\(")]
_QML = [re.compile(rf"\bfunction\s+({_ID})\s*\("),
        re.compile(r"^[ \t]*(on[A-Z]\w*)[ \t]*:", re.M)]
LANGS = {
    ".js": ("js", _JS), ".jsx": ("js", _JS), ".mjs": ("js", _JS), ".cjs": ("js", _JS),
    ".ts": ("js", _JS), ".tsx": ("js", _JS), ".mts": ("js", _JS),
    ".rs": ("rust", _RUST), ".kt": ("kotlin", _KOTLIN), ".kts": ("kotlin", _KOTLIN),
    ".java": ("c", _JAVA), ".cs": ("c", _CS), ".go": ("c", _GO),
    ".c": ("c", _CPP), ".cpp": ("c", _CPP), ".cc": ("c", _CPP), ".cxx": ("c", _CPP), ".hpp": ("c", _CPP), ".hh": ("c", _CPP),
    ".h": ("c", _CPP), ".qml": ("js", _QML), ".swift": ("swift", _SWIFT), ".php": ("php", _PHP),
}
# type declarations (Kotlin, Java, C#, TS, Rust, Go): their names qualify calls
_DECL = re.compile(r"\b(?:class|object|interface|enum|struct|record|trait|type)\s+([A-Z][\w]*)")
_TS_ALIAS = re.compile(r"\btype\s+([A-Z]\w*)\s*(?:<[^=;]*?>)?\s*=\s*[|&]?\s*([A-Za-z_$][\w$]*)")
_TS_BUILTIN_TYPES = {"Map", "Set", "WeakMap", "WeakSet", "Array", "ReadonlyArray", "ReadonlyMap", "ReadonlySet", "Record",
                     "Promise", "string", "number", "boolean", "bigint", "symbol", "object", "unknown", "any", "never",
                     "Uint8Array", "Uint16Array", "Uint32Array", "Float32Array", "Float64Array", "ArrayBuffer", "DataView",
                     "Date", "RegExp", "Error", "Function", "Blob", "File", "URL", "Headers", "Request", "Response",
                     "HTMLElement", "Element", "Node", "Event", "MouseEvent", "KeyboardEvent", "PointerEvent", "Iterable"}
# Declared types (no inference): `name: Type` in Kotlin / TS declarations and
# parameters, `Type* name` / `Type& name` in C++. The type is one Capitalized
# word (a generic `List<Foo>` is List, outside the project: its calls resolve
# to nothing, which is right).
_KT_TYPED = re.compile(r"(?:\b(?:val|var)\s+|[(,{]\s*|^\s*)([a-z_]\w*)\s*:\s*([A-Z]\w*(?:\.[A-Z]\w*)*)(?![\w.])", re.M)
_KT_FIELD = re.compile(r"\b(?:val|var)\s+([a-z_]\w*)\s*:\s*([A-Z]\w*(?:\.[A-Z]\w*)*)(?![\w.])")
# built by its constructor: `val prefs = OsaPrefs(ctx)`, `const hub = new HubClient(url)`
_KT_NEW = re.compile(r"\b(?:val|var)\s+([a-z_]\w*)\s*=\s*([A-Z]\w*)\s*\(")
_TS_NEW = re.compile(r"(?:\b(?:const|let|var)\s+|^\s*(?:(?:private|public|protected|readonly|static)\s+)*)"
                     r"([A-Za-z_$][\w$]*)\s*=\s*new\s+([A-Z][\w$]*)\s*[(<]", re.M)
_TS_TYPED = re.compile(r"(?:\b(?:const|let|var)\s+|[(,]\s*|^\s*)([A-Za-z_$][\w$]*)\s*\??\s*:\s*([A-Z][\w$]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>)?)(?![\w$.<\[])", re.M)
_TS_FIELD = re.compile(r"^\s*(?:(?:private|public|protected|readonly|static|declare)\s+)*([a-z_$][\w$]*)\s*[?!]?\s*:\s*([A-Z][\w$]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>)?)(?![\w$.<\[])\s*[;=]", re.M)
# a module-level `export const utf8Encoder: TextEncoder = new TextEncoder()`
_TS_CONST = re.compile(r"^\s*(?:export\s+)?(?:declare\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*:\s*([A-Z][\w$]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>)?)(?![\w$.<\[])", re.M)
_TS_INSTANCE_TYPE = re.compile(r"^\s*(?:(?:private|public|protected|readonly|static|declare)\s+)*([a-z_$][\w$]*)\s*[?!]?\s*:\s*"
                               r"InstanceType\s*<\s*typeof\s+([A-Z][\w$]*)\s*>\s*[;,]?\s*$", re.M)
_TS_PARAM_FIELD = re.compile(r"\b(?:private|public|protected|readonly)\s+([a-z_$][\w$]*)\s*\??\s*:\s*([A-Z][\w$]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>)?)(?![\w$.<\[])")
# `Client client(...);` at a statement start, `QString name` as a parameter
_CPP_VALUE = re.compile(r"(?:^|[(,])\s*(?:(?:const|final|this|using|await\s+using|out|ref|in|params|scoped|readonly)\s+)*"
                        r"([A-Z]\w*(?:\.[A-Z]\w*)*(?:<[^;=(){}]*>)?)\s+([a-z_]\w*)\s*(?=[({;=,)]|:(?!:)|in\s)", re.M)
_CPP_TYPED = re.compile(r"(?<![\w:>])([A-Z]\w*)\s*[\*&]+\s*([a-z_]\w*)\s*(?=[,;=)\[{]|:(?!:)|$)", re.M)
# a C++ class member in a header: `BoardModel* m_board = nullptr;`, `Hub m_hub;`
# `Q_PROPERTY(SortFilterRoomTreeModel *model READ model CONSTANT)`: QML's `obj.model`
_Q_PROPERTY = re.compile(r"^\s*Q_PROPERTY\(\s*(?:const\s+)?((?:[\w:]+)(?:<[^()]*?>)?)\s*[\*&]?\s*([A-Za-z_]\w*)\s+(?:READ|MEMBER|BINDABLE)\b")
_CPP_FIELD = re.compile(r"^\s*(?:(?:mutable|static|const|inline)\s+)*((?:std::|[A-Z])[\w:]*(?:<[^;()]*>)?)\s*[\*&]?\s*([a-z_]\w*)\s*(?:=[^;]*|\{[^;]*\})?;")
_KT_RECV = r"\bfun\s+(?:<[^>]*>\s*)?([A-Z]\w*)(?:<[^>]*>)?\??\.%s\s*\("
_ALLCAPS = re.compile(r"[A-Z0-9_]+$")


# Java / C#: `Foo foo` (a local, a parameter), `var x = new Foo(`, and class
# fields with their modifiers (`private readonly Foo _foo;`)
_JV_NEW = re.compile(r"\b(?:var|val|final)\s+([a-z_]\w*)\s*=\s*new\s+([A-Z]\w*)\s*[(<{]")
_JV_FIELD = re.compile(r"^\s*(?:(?:public|private|protected|internal|static|readonly|final|volatile|transient)\s+)*"
                       r"([A-Z]\w*)(?:<[^>;]*>)?\??\s+([a-z_]\w*)\s*(?:=[^;]*)?;")
# C#: fields and properties (`public static Pool Shared { get; } = ...`, `Foo<T> _x;`, `Bar Baz => ...`)
_CS_MEMBER = re.compile(r"^\s*(?:\[[^\]\n]*\]\s*)*(?:(?:public|private|protected|internal|static|readonly|virtual|override|abstract|sealed|"
                        r"new|required|const|volatile|unsafe|extern|partial)\s+)*([A-Z][\w.]*(?:<[^;={}()]*>)?\??)\s+([A-Za-z_]\w*)\s*"
                        r"(?:\{\s*(?:get|set|init|private|protected|internal)\b|=>|=(?![=>])|;)")
# Go: `c *Command` / `var x Foo` (receivers, parameters, vars), `x := &Foo{`, and
# struct fields (`flags *flagSet`). Go types may be lowercase (unexported).
_GO_TYPED = re.compile(r"(?:\bvar\s+|[(,]\s*)([A-Za-z_]\w*)\s+\*?((?:[A-Za-z_]\w*\.)?[A-Za-z_]\w*)(?=\s*[,)=]|\s*$)", re.M)
_GO_NEW = re.compile(r"\b([A-Za-z_]\w*)\s*:?=\s*(?:&?((?:[A-Za-z_]\w*\.)?[A-Za-z_]\w*)\s*\{|new\(\s*((?:[A-Za-z_]\w*\.)?[A-Za-z_]\w*)\s*\))")
_GO_FIELD = re.compile(r"^\s*(?:var\s+)?([A-Za-z_]\w*)\s+\*?((?:[A-Za-z_]\w*\.)?[A-Za-z_]\w*)\s*(?:`[^`]*`)?\s*$")
_GO_ELEMS = re.compile(r"\b([A-Za-z_]\w*)\s+(?:\[\d*\]|map\[[^\]\n]+\])\s*\*?((?:[A-Za-z_]\w*\.)?[A-Z]\w*)\b")
_GO_KEYWORDS = {"return", "go", "defer", "case", "range", "package", "import", "type", "func", "else", "chan", "map",
                "struct", "interface", "break", "continue", "var", "const"}


# Rust: `x: &mut Foo` (a parameter, a `let`), `let x = Foo::new(` / `Foo {`
_RS_PRIM = r"(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64|bool|char|str)"
_RS_TYPED = re.compile(r"(?:\blet\s+(?:mut\s+)?|[(,]\s*(?:mut\s+)?)([a-z_]\w*)\s*:\s*&?(?:'\w+\s+)?(?:mut\s+)?"
                       r"((?:[a-z_]\w*::)*[A-Z]\w*|\[[^\]\n]*\]|" + _RS_PRIM + r")(?![\w:])")
_RS_NEW = re.compile(r"\blet\s+(?:mut\s+)?([a-z_]\w*)\s*(?::[^=]+)?=\s*([A-Z]\w*)\s*(?:::\s*(?:new|default|with_\w+|from\w*|builder)\s*\(|\{)")


_GEN_C = re.compile(r"(?<![\w>.])([A-Za-z_][\w:.]*)\s*<((?:[^<>;=(){}]|<(?:[^<>]|<[^<>]*>)*>)*)>\s*[\*&]*\s*([A-Za-z_]\w*)\s*(?=[;={(,)\[:]|$)", re.M)
_GEN_K = re.compile(r"([A-Za-z_$][\w$]*)\s*[?!]?\s*:\s*(?:readonly\s+)?([\w.$]+)\s*<((?:[^<>;=]|<(?:[^<>]|<[^<>]*>)*>)*)>")
_ARR_TS = re.compile(r"([A-Za-z_$][\w$]*)\s*[?!]?\s*:\s*(?:readonly\s+)?([A-Z][\w$.]*)\s*\[\s*\]")
_NOT_CONTAINER = {"template", "static_cast", "dynamic_cast", "reinterpret_cast", "const_cast", "return", "new", "typename"}


def _elements(tk: str, text: str) -> list[tuple[str, str]]:
    """("name[]", element type) for a container declaration: `QHash<QString,
    Player*> m_players`, `List<Foo> xs`, `xs: Map<K, Foo>`, TS `xs: Foo[]`.
    The element is the last type argument (a map's value)."""
    if tk == "go":  # `cmds []*Command`, `flags map[string]*Flag`: their elements (a map's values)
        return [(m.group(1) + "[]", m.group(2)) for m in _GO_ELEMS.finditer(text) if m.group(1) not in _GO_KEYWORDS]
    if tk not in ("cpp", "jvm", "kotlin", "js", "swift") or "<" not in text and "[" not in text:
        return []
    out = []
    if tk in ("cpp", "jvm"):
        hits = [(m.group(3), m.group(1), m.group(2)) for m in _GEN_C.finditer(text) if m.group(1) not in _NOT_CONTAINER]
    else:
        hits = [(m.group(1), m.group(2), m.group(3)) for m in _GEN_K.finditer(text)]
        out += [(m.group(1) + "[]", t) for m in _ARR_TS.finditer(text) if (t := type_name(m.group(2)))]
    for name, cont, args in hits:
        depth, last = 0, 0
        for k, ch in enumerate(args):
            depth += ch in "<([{"
            depth -= ch in ">)]}"
            if ch == "," and depth == 0:
                last = k + 1
        elem = type_name(args[last:])
        if elem and elem != type_name(f"{cont}<{args}>") and elem[:1].isupper():
            out.append((name + "[]", elem))
    return out


def _typed(tk: str, text: str) -> list[tuple[str, str]]:
    """(name, type) declarations in a stretch of blanked code."""
    if tk == "kotlin":
        return [(m.group(1), m.group(2)) for pat in (_KT_TYPED, _KT_NEW) for m in pat.finditer(text)]
    if tk == "js":
        return [(m.group(1), t) for pat in (_TS_TYPED, _TS_NEW) for m in pat.finditer(text)
                if (t := type_name(m.group(2)) if "<" in m.group(2) else m.group(2))]  # (`Router<string>`: a Router)
    if tk in ("cpp", "jvm"):
        out = [(m.group(2), t) for pat in ((_CPP_TYPED, _CPP_VALUE) if tk == "cpp" else (_CPP_VALUE,))
               for m in pat.finditer(text) if not _ALLCAPS.match(m.group(1))
               and (t := (generic_name(m.group(1)) if tk == "jvm" else type_name(m.group(1))))]
        if tk == "jvm":
            out += [(m.group(1), m.group(2)) for m in _JV_NEW.finditer(text)]
        return out
    if tk == "rust":
        return [(m.group(1), m.group(2)) for pat in (_RS_TYPED, _RS_NEW) for m in pat.finditer(text)
                if m.group(2) not in ("Self",)]
    if tk == "go":
        return [(m.group(1), m.group(2) or m.group(3)) for pat in (_GO_TYPED, _GO_NEW) for m in pat.finditer(text)
                if m.group(1) not in _GO_KEYWORDS and m.group(2) not in _GO_KEYWORDS]
    if tk == "swift":
        return [(m.group(1), t) for pat in (_SW_TYPED, _SW_NEW) for m in pat.finditer(text) if (t := type_name(m.group(2)))]
    if tk == "php":
        return [(m.group(2), t) for m in _PHP_TYPED.finditer(text) if (t := type_name(m.group(1)))] + \
            [(m.group(1), t) for m in _PHP_NEW.finditer(text) if (t := type_name(m.group(2)))]
    return []


# Swift `let x: Foo`, `(with client: Client)`, `let x = Foo(`; PHP `(Foo $x`, `$x = new Foo(`
_SW_TYPED = re.compile(r"(?:\b(?:let|var)\s+|[(,]\s*(?:[A-Za-z_]\w*\s+)?)([a-z_]\w*)\s*:\s*(?:inout\s+|@escaping\s+|@\w+\s+)*\(?\s*(?:any\s+|some\s+)?"
                       r"([A-Z][\w.]*(?:<[^<>=\n]*>)?[?!]?)(?![\w.(])")
_SW_NEW = re.compile(r"\b(?:let|var)\s+([a-z_]\w*)\s*=\s*(?:try[?!]?\s+|await\s+)*([A-Z]\w*)\s*\(")
_PHP_TYPED = re.compile(r"(?:[(,]\s*|\b(?:public|private|protected|readonly)\s+)(?:(?:public|private|protected|readonly)\s+)*"
                        r"\??([A-Z\\][\\\w]*)\s+&?(?:\.\.\.)?(\$\w+)")
_PHP_NEW = re.compile(r"(\$\w+)\s*=\s*new\s+([\\\w]+)")


_TEST_ATTR = re.compile(r"@Test\b|\[(?:Fact|Theory|Test|TestMethod|TestCase|DataTestMethod)\b|#\[(?:[\w:]+::)?test\b")
_JS_TEST = re.compile(r"^[ \t]*(?:it|test)(?:\.(?:only|skip|todo|concurrent|each\([^)]*\)))?[ \t]*\(", re.M)
# Imports, read from the raw text (the blanked code has lost the paths). JS/TS
# specifiers are kept as "js:<file dir>\0<spec>" for core to resolve against
# the files (and tsconfig `paths`) once everything is parsed.
_JS_IMPORT = re.compile(r"""^[ \t]*import[ \t]+(?!type\b)([^;'"]*?)\s+from\s+["']([^"']+)["']""", re.M)
_JS_REEXPORT_SKIP = re.compile(r"^\s*type\s+")
_KT_PACKAGE = re.compile(r"^[ \t]*package[ \t]+([\w.]+)", re.M)
_KT_IMPORT = re.compile(r"^[ \t]*import[ \t]+(?:static[ \t]+)?([\w.]+?)(\.\*)?(?:[ \t]+as[ \t]+(\w+))?[ \t]*;?[ \t]*$", re.M)
_JSX = re.compile(r"<(?:([A-Za-z_$][\w$]*)\.)?([A-Z][\w$]*)(?=[\s/>]|$)")
_NOT_FUNC = {"if", "for", "while", "switch", "catch", "return", "function", "else", "do", "try", "with", "new",
             "typeof", "match", "loop", "when", "sizeof", "constructor_", "super"}
_CALL = re.compile(rf"(?:({_ID})\s*(?:\.|::|\?\.|->)\s*)?\b({_ID})\s*(?:::<[^()]*>|<[\w\s,<>\[\]]*>)?\s*\(")
_CALL_SKIP = _NOT_FUNC | {"async", "await", "catch", "require", "import", "print", "println", "Some", "Ok", "Err", "Box", "Vec",
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
_PY_ANN = re.compile(r"^[ \t]*([A-Za-z_]\w*)[ \t]*:[ \t]*([^=\n#]+?)[ \t]*(?:=|#|$)", re.M)
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
                cm.bases = ["py:" + x.split("[")[0].strip() for x in bases]
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
        if mname == fm.name and o >= 0 and blocks[o][3] == "def":
            # a closure in a method (`def decorator(f):` in `add_template_global`): its
            # `self` is the method's, an instance of that class
            j = o
            while j >= 0 and blocks[j][3] == "def":
                j = outer[j]
            if j >= 0 and blocks[j][3] == "class" and owner_of.get(j):
                f.bound.setdefault("self", "mod:" + owner_of[j])
        nested = {i for (a2, b2, _, kind2, _) in blocks if kind2 == "def" and a < a2 <= b for i in range(a2, b2 + 1)}
        _py_collect(f, lines, a, b, first=True, skip=nested)
        _py_types(f, lines, a, b, test_file, fixture=any(
            re.match(r"\s*@(?:pytest\.)?fixture\b", lines[j]) for j in range(max(0, a - 3), a)))
    top = Func(fm.name, "<top>", rel, 1, len(lines), "", kind="top")
    for i, ln in enumerate(lines):
        if not inside[i] and ln.strip() and not ln.lstrip().startswith(("class ", "import ", "from ")):
            _py_collect(top, lines, i, i, first=False)
            if not ln[:1].isspace():  # a module-level variable (`app = Flask(__name__)`, `current_app: Flask = ...`)
                am = re.match(r"([A-Za-z_]\w*)\s*:\s*([^=]+?)\s*(?:=|$)", ln)
                if am and type_name(am.group(2)):
                    fm.bound.setdefault(am.group(1), "type:" + type_name(am.group(2)))
                for nm_, held in _assignments("py", ln):
                    fm.bound.setdefault(nm_, held)
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
        # the class body's own attributes (`session_interface: SessionInterface = ...`,
        # `json_provider_class = DefaultJSONProvider`), lines at the body's indent
        body_ind = None
        for i in range(a + 1, b + 1):
            ln = lines[i]
            if not ln.strip():
                continue
            ind_i = len(ln) - len(ln.lstrip())
            if body_ind is None:
                body_ind = ind_i
            if ind_i != body_ind:
                continue
            am = re.match(r"\s*([A-Za-z_]\w*)\s*:\s*([^=\n#]+?)\s*(?:=|#|$)", ln)
            if am and (ty := type_name(am.group(2))) and not ty.startswith(("ClassVar", "t.ClassVar")):
                seen.setdefault(am.group(1), set()).add(ty)
                continue
            vm = re.match(r"\s*([a-z_]\w*)\s*=\s*((?:[a-z_]\w*\.)*[A-Z]\w*)\s*\(", ln)
            if vm:
                seen.setdefault(vm.group(1), set()).add(vm.group(2))
        for nm, tys in seen.items():
            if len(tys) == 1:
                cm.bound.setdefault(nm, "type:" + next(iter(tys)))
        # `self.app = app` with `app: Flask` a parameter, `self.x = self.make()`
        for g in cm.funcs:
            for m in re.finditer(r"\bself\.([A-Za-z_]\w*)\s*(?::[^=\n]*)?=(?!=)", g.code):
                ch = expression(g.code, m.end())
                if not ch or m.group(1) in cm.bound:
                    continue
                held = g.bound.get(ch[0], "") if len(ch) == 1 else ""
                if held.startswith("type:"):
                    cm.bound[m.group(1)] = held
                elif ch[0] in ("self", "cls") or ch[0][:1].isupper() or ch[0].endswith("()") or \
                        ch[0] in fm.aliases or ch[0] in fm.imported:
                    cm.bound[m.group(1)] = "expr:" + "\x1f".join(ch)
    for cm in mods.values():
        if cm is not fm:
            # bare names in a class resolve through its file: share its imports
            cm.aliases, cm.imported, cm.uses = fm.aliases, fm.imported, fm.uses
    return [fm] + [m for m in mods.values() if m is not fm]


def _py_types(f: Func, lines: list[str], a: int, b: int, test_file: bool, fixture: bool) -> None:
    """What a Python function returns (`-> Foo`, else its last `return` /
    a fixture's `yield`), what its locals are assigned, and in a test file
    each plain parameter: the pytest fixture of that name."""
    head, k = lines[a], a
    while "):" not in head and ")->" not in head.replace(" ", "") and k < b:
        k += 1
        head += " " + lines[k].strip()
    rm = re.search(r"\)\s*->\s*([^:]+?)\s*:\s*$", head)
    ann = rm.group(1) if rm else ""
    if ann and any(re.match(r"\s*@(?:contextlib\.)?(?:async)?contextmanager\b", lines[j]) for j in range(max(0, a - 3), a)):
        # a @contextmanager's `-> Iterator[Response]`: `with f() as r` gives a Response
        ann = re.sub(r"^(?:[\w.]+\.)?(?:Async)?(?:Iterator|Generator)\[(.*)\]$", r"\1", ann.strip()).split(",")[0]
    if ann and type_name(ann):
        f.returns = type_name(ann)
    sig = head[head.find("(") + 1:]
    depth, last, parts = 0, 0, []
    for k, ch in enumerate(sig):  # the parameters, split at top-level commas
        depth += ch in "([{"
        depth -= ch in ")]}"
        if depth < 0 or (ch == "," and depth == 0):
            parts.append(sig[last:k])
            last = k + 1
            if depth < 0:
                break
    for prm in parts:
        pm = re.match(r"\s*([A-Za-z_]\w*)\s*:\s*([^=]+?)\s*(?:=.*)?$", prm, re.S)
        if pm and (ty := type_name(pm.group(2))):
            f.bound.setdefault(pm.group(1), "type:" + ty)  # `app: flask.Flask`, `options: dict[str, t.Any]`
    for m in re.finditer(r"[(,]\s*(\*\*?)([A-Za-z_]\w*)", head):
        f.bound[m.group(2)] = "type:dict" if m.group(1) == "**" else "type:tuple"  # `**kwargs`, `*args`
    body = "\n".join(lines[a:b + 1])
    if not f.returns:
        rets = re.findall(r"^[ \t]*(?:return|yield)[ \t]+(.+)$", body, re.M) if fixture else \
            re.findall(r"^[ \t]*return[ \t]+(.+)$", body, re.M)
        if rets:
            if all(r.strip() in ("self", "cls") for r in rets):
                f.returns = "Self"  # a builder step
            else:
                ch = expression(rets[-1], 0)
                if ch:
                    f.returns = "expr:" + "\x1f".join(ch)
    for nm_, held in _assignments("py", body):
        if nm_ != f.name or held.split("\x1f")[0] not in (f"expr:{f.name}", f"expr:{f.name}()"):  # (`app = create_app()` in `def app()`)
            f.bound.setdefault(nm_, held)
    for m in re.finditer(r"\bwith[ \t]+((?:[^:\n]|\([^()]*\))+?)\s+as[ \t]+([A-Za-z_]\w*)[ \t]*[:,)]", body):
        ch = expression(m.group(1) + "\n", 0)  # `with httpx.Client() as client:` (a context manager gives itself)
        if ch:
            f.bound.setdefault(m.group(2), "expr:" + "\x1f".join(ch))
    if test_file:
        pm = re.search(r"\(([^)]*)\)", head)
        for prm in (pm.group(1).split(",") if pm else []):
            prm = prm.strip()
            if re.match(r"^[a-z_]\w*$", prm) and prm not in ("self", "cls", "request"):
                f.bound.setdefault(prm, "fixture:" + prm)


def _py_collect(f: Func, lines: list[str], a: int, b: int, first: bool, skip: set[int] | None = None) -> None:
    body = "\n".join(lines[a:b + 1])
    off = 0
    for i in range(a, b + 1):
        t = lines[i]
        if skip and i in skip:  # a nested def's lines: its own function's calls
            off += len(lines[i]) + 1
            continue
        cut = 0
        if (first and i == a) or _PY_BLOCK.match(t):
            cut = t.index("(") + 1 if "(" in t else len(t)  # a (nested) def's own name is no call
            t = t[cut:]
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
            chain = receiver(body, off + cut + m.start(2)) if q and q != "super" else ("super()",) if q == "super" else ()
            if chain == ("(...)",) and t[:m.start()].rstrip().endswith(("}.", "].")):
                chain = ("<literal>",)  # `{"a": 1}.get(k)`, `[...].count(x)`: Python's own
            if chain and q != ".":
                q = chain[-1] if re.match(r"^\w+$", chain[-1]) else "."
            f.calls.append(Call(name, q, i + 1, chain, cut + m.start(2)))
        if f.kind != "top":
            for m in _PY_NEW.finditer(t):
                f.bound[m.group(1)] = "type:" + m.group(2)
            for m in _PY_ANN.finditer(t):
                if (ty := type_name(m.group(2))):  # `x: flask.Flask`, `y: dict[str, Any] = {}`, `z: Optional[Foo]`
                    f.bound.setdefault(m.group(1), "type:" + ty)
        for m in _PY_VALUE.finditer(t):
            f.refs.add(m.group(1))
        off += len(lines[i]) + 1


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


_CJS_REQUIRE = re.compile(r"""(?:\b(?:const|let|var)\s+|,\s*)([A-Za-z_$][\w$]*|\{[^}]*\})\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)\s*(\.\s*([A-Za-z_$][\w$]*))?""")
_CJS_EXPORT = re.compile(r"""^[ \t]*(?:exports[ \t]*=[ \t]*)?module\.exports[ \t]*=[ \t]*(?:exports[ \t]*=[ \t]*)?(?:require\(\s*['"]([^'"]+)['"]\s*\)|([A-Za-z_$][\w$]*)[ \t]*;?[ \t]*$)""", re.M)


def _cjs(mod: Module, rel: str, text: str) -> None:
    """CommonJS: `const x = require("./x")` (the module, and calling it calls
    what it exports), `const { a, b: c } = require("./x")`, and what a file
    exports (`module.exports = createApplication`, `= require("./lib/x")`)."""
    d = rel.rsplit("/", 1)[0] if "/" in rel else ""
    base = os.path.basename(rel)
    for m in _CJS_REQUIRE.finditer(text):
        where = f"js:{d}\0{m.group(2)}"
        local = m.group(1)
        if local.startswith("{"):
            for part in local[1:-1].split(","):
                src, _, alias = part.strip().partition(":")
                if src.strip():
                    mod.imported.setdefault((alias or src).strip(), (where, src.strip()))
        elif m.group(4):  # `const Router = require("./router").Router`
            mod.imported.setdefault(local, (where, m.group(4)))
        else:
            mod.aliases.setdefault(local, where)
            mod.imported.setdefault(local, (where, "default"))
    for m in _CJS_EXPORT.finditer(text):
        if m.group(1):
            mod.imported["default"] = (f"js:{d}\0{m.group(1)}", "default")
        elif m.group(2):
            mod.imported["default"] = (f"js:{d}\0./{base}", m.group(2))


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


_JS_REEXPORT = re.compile(r"""^[ \t]*export[ \t]+(?!type\b)(\*|\{[^}]*\})[ \t]*from[ \t]*["']([^"']+)["']""", re.M)


_JS_EXPORT_AS = re.compile(r"^[ \t]*export[ \t]*\{([^}]*)\}[ \t]*;?[ \t]*$", re.M)


def _js_reexports(mod: Module, rel: str, text: str, lines: list[str]) -> None:
    """A barrel's `export * from "./x"` and `export { a, b as c } from "./y"`:
    a name looked up here leads on to where it is defined. A file's own
    `export { Hono as HonoBase }` too (here, under another name)."""
    d = rel.rsplit("/", 1)[0] if "/" in rel else ""
    for m in _JS_EXPORT_AS.finditer(text):
        ln = text.count("\n", 0, m.start())
        if ln < len(lines) and lines[ln].strip().startswith("export"):
            for part in m.group(1).split(","):
                src, _, local = _JS_REEXPORT_SKIP.sub("", part.strip()).partition(" as ")
                if local.strip() and src.strip() and local.strip() != "default":
                    mod.imported.setdefault(local.strip(), (f"js:{d}\0./{os.path.basename(rel)}", src.strip()))
    for m in _JS_REEXPORT.finditer(text):
        ln = text.count("\n", 0, m.start())
        if ln >= len(lines) or not lines[ln].strip().startswith("export"):
            continue  # inside a comment
        where = f"js:{d}\0{m.group(2)}"
        if m.group(1) == "*":
            mod.reexports.append(where)
            continue
        for part in m.group(1)[1:-1].split(","):
            part = _JS_REEXPORT_SKIP.sub("", part.strip())
            if part:
                src, _, local = part.partition(" as ")
                mod.imported.setdefault((local or src).strip(), (where, src.strip()))


_RS_USE = re.compile(r"^[ \t]*(pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([^;]+);", re.M)


def _rs_tree(spec: str, prefix: list[str]) -> list[tuple[list[str], str]]:
    """`a::{b, c::d as e, self, f::*}` -> ([path...], local name) pairs; a
    glob's local name is "*"."""
    spec = spec.strip()
    out: list[tuple[list[str], str]] = []
    if "{" in spec:
        head, _, rest = spec.partition("{")
        body = rest[:rest.rfind("}")]
        base = prefix + [x for x in head.strip().rstrip(":").split("::") if x]
        depth, cur = 0, ""
        for ch in body + ",":
            if ch == "," and depth == 0:
                if cur.strip():
                    out += _rs_tree(cur, base)
                cur = ""
                continue
            depth += ch == "{"
            depth -= ch == "}"
            cur += ch
        return out
    path, _, alias = spec.partition(" as ")
    parts = prefix + [x.strip() for x in path.split("::") if x.strip()]
    if not parts:
        return out
    if parts[-1] == "self":
        parts = parts[:-1]
        if not parts:
            return out
    local = alias.strip() or parts[-1]
    return out + [(parts, "*" if local == "*" else local)]


def _rs_imports(mod: Module, rel: str, code: str) -> None:
    """Rust `use` declarations, kept as "rs:<file>\0<a::b>" for core to resolve
    against the workspace's crates; `super::` / `crate::` / `self::` too."""
    w = lambda parts: f"rs:{rel}\0{'::'.join(parts)}"  # noqa: E731
    for m in _RS_USE.finditer(code):
        for parts, local in _rs_tree(re.sub(r"\s+", " ", m.group(2)), []):
            if local == "*":
                mod.uses.append(w(parts[:-1]))
                if m.group(1):
                    mod.reexports.append(w(parts[:-1]))
                continue
            if local == "_":
                continue
            mod.aliases[local] = w(parts)
            if len(parts) > 1:
                mod.imported[local] = (w(parts[:-1]), parts[-1])
    for q in ("super", "crate"):
        mod.aliases.setdefault(q, w([q]))


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
        else:  # a type: its file is in scope for method-name matches, and its package says which one
            mod.uses.append(f"type:{last}")
            mod.imported.setdefault(alias or last, (f"pkg:{pkg}", last))


_CS_NAMESPACE = re.compile(r"^[ \t]*namespace[ \t]+([\w.]+)", re.M)
_CS_USING_ALIAS = re.compile(r"^[ \t]*(?:global[ \t]+)?using[ \t]+([A-Za-z_]\w*)[ \t]*=[ \t]*([\w.]+)[ \t]*;", re.M)
_CS_USING = re.compile(r"^[ \t]*(?:global[ \t]+)?using[ \t]+(?:static[ \t]+)?([\w.]+)[ \t]*;", re.M)


def _cs_imports(mod: Module, code: str) -> None:
    """C#: a file's namespace is its package (its types see each other), and
    each `using A.B;` brings that namespace's files into scope."""
    nm = _CS_NAMESPACE.search(code)
    if nm:
        mod.package = nm.group(1)
    for m in _CS_USING.finditer(code):
        mod.uses.append(f"pkg:{m.group(1)}")
    for m in _CS_USING_ALIAS.finditer(code):  # `using Assert = Tests.XUnitAssert;`: that type, by another name
        mod.bound[m.group(1)] = "type:" + m.group(2).rsplit(".", 1)[-1]


_SW_DECL = re.compile(r"\b(?:class|struct|enum|protocol|actor|typealias)\s+([A-Z]\w*)")
_SW_IMPORT = re.compile(r"^[ \t]*(?:@\w+(?:\([^)\n]*\))?[ \t]+)*import[ \t]+(?:(?:class|struct|enum|protocol|func|var|let|typealias)[ \t]+)?(\w+)", re.M)


def _swift_imports(mod: Module, rel: str, code: str) -> None:
    """Swift: a module (a SwiftPM target, `Sources/<Target>/**`) is one scope,
    its files see each other's types and functions; `import X` brings target
    X's in. Outside Sources/ and Tests/ a top folder stands for the target."""
    parts = rel.split("/")
    for i, p_ in enumerate(parts[:-1]):
        if p_ in ("Sources", "Tests", "Source") and i + 2 < len(parts):
            mod.package = parts[i + 1]
            break
    else:
        mod.package = parts[0] if len(parts) > 1 else ""
    for m in _SW_IMPORT.finditer(code):
        mod.uses.append(f"pkg:{m.group(1)}")


_PHP_NS = re.compile(r"^[ \t]*namespace[ \t]+([\w\\]+)[ \t]*[;{]", re.M)
_PHP_USE = re.compile(r"^[ \t]*use[ \t]+(?:function[ \t]+|const[ \t]+)?([^;]+);", re.M)


def _php_imports(mod: Module, text: str, code: str) -> None:
    """PHP: a file's namespace is its package; `use A\\B\\C;` names type C of
    namespace A\\B (`as D`: by another name), `use A\\{B, C}` several."""
    nm = _PHP_NS.search(code)
    if nm:
        mod.package = nm.group(1).strip("\\").replace("\\", ".")
    for m in _PHP_USE.finditer(code):
        if "(" in m.group(1) or "$" in m.group(1):
            continue  # a closure's `use ($x)`, a trait's `use Foo;` inside a class (the class handles that)
        spec = re.sub(r"\s+", " ", m.group(1)).strip()
        group = re.match(r"([\w\\]*)\\\{([^}]*)\}", spec)
        items = [(group.group(1) + "\\" + x.strip()) for x in group.group(2).split(",") if x.strip()] if group else spec.split(",")
        for it in items:
            am = re.match(r"\s*\\?([\w\\]+)(?:\s+as\s+(\w+))?\s*$", it)
            if not am:
                continue
            full = am.group(1).replace("\\", ".")
            ns, _, name = full.rpartition(".")
            local = am.group(2) or name
            if ns:
                mod.aliases.setdefault(local, "pkg:" + ns)
            if am.group(2) and am.group(2) != name:
                mod.bound[local] = "type:" + name


def _pack_signatures() -> list[tuple[str, list[str], re.Pattern, re.Pattern | None]]:
    """Every pack's [[param]] parameter lists (`(req, res` after an optional
    `err`), as a function's or callback's parameters; with `in_call`, only a
    callback passed to a call of one of those names (`.get('/', (c) =>`)."""
    from .config import available_packs, pack_params
    out = []
    for pid, entry in pack_params(available_packs()).items():
        if entry.get("kind") == "returns":
            continue
        names = list(entry.get("names", []))
        if not names:
            continue
        lst = (r"\(\s*(?:[A-Za-z_$][\w$]*\s*,\s*)?" + r"\s*,\s*".join(map(re.escape, names))
               + (r"\s*(?:,[^()]*)?\)" if len(names) > 1 else r"\s*\)"))
        fn = rf"(?:\bfunction\b[\s\w$]*{lst}|(?:\basync\s*)?{lst}\s*(?::[^=;]+?)?=>)"
        calls = entry.get("in_call")
        call_rx = re.compile(r"\.(?:" + "|".join(map(re.escape, calls)) + r")\s*(?:<[^()]*?>)?\s*\(") if calls else None
        out.append((pid, names, re.compile(fn), call_rx))
    return out


_PACK_SIGS = _pack_signatures()


def _pack_params(f: Func) -> None:
    """Parameters a framework hands this function or a callback in it
    (Express's `(req, res, next)`): "pack:<pack>/<id>:<name>", typed by core
    when that pack is in force."""
    for pid, names, rx, call_rx in _PACK_SIGS:
        if names[0] not in f.code:
            continue
        if call_rx is None:
            hit = rx.search(f.code)
        else:  # the callback must be an argument of one of those calls
            hit = None
            for cm in call_rx.finditer(f.code):
                depth, k = 1, cm.end()
                while k < len(f.code) and depth:
                    depth += {"(": 1, ")": -1}.get(f.code[k], 0)
                    k += 1
                if rx.search(f.code, cm.end(), k):
                    hit = True
                    break
        if hit:
            for n in names:
                f.bound.setdefault(n, f"pack:{pid}:{n}")


# --- classes, return types, assignments (the type layer; see chains.py) ------------

_CLASS = re.compile(
    r"(?<![\w.$@])(?:(?:data|sealed|abstract|open|inner|value|enum|annotation|final|public|private|protected|internal"
    r"|static|partial|export|default|readonly|ref|unsafe|pub(?:\([^)]*\))?|fileprivate|indirect|declare|fun)\s+)*"
    r"(class|interface|struct|record|object|trait|protocol|extension|union)\s+([A-Za-z_$][\w$]*)")
_COMPANION = re.compile(r"\bcompanion\s+object\b(?:\s+\w+)?")
_RS_IMPL = re.compile(r"\bimpl\s*(?:<[^{}]*?>)?\s*(?:([\w:]+)(?:<[^{}]*?>)?\s+for\s+)?([\w:]+)")
_BASE_SKIP = {"public", "private", "protected", "virtual", "internal", "extends", "implements", "where", "class",
              "struct", "new", "final", "sealed", "open", "abstract", "override", "in", "out", "reified", "const",
              "Sendable", "Equatable", "Hashable", "Codable", "Identifiable", "CustomStringConvertible"}
_CPP_EXTS = (".cpp", ".cc", ".cxx", ".hpp", ".hh", ".h", ".c")


def _classes(code: str, ext: str, fam: str) -> list[dict]:
    """Class-like declarations: name, bases as written, header text, and the
    body's character span (None for a body-less `class Foo(val a: A)`)."""
    out = []
    n = len(code)
    if fam == "rust":
        matches = [(m, "impl") for m in _RS_IMPL.finditer(code)]
        matches += [(m, "type") for m in re.finditer(r"(?<![\w.])(?:pub(?:\([^)]*\))?\s+)?(struct|enum|trait|union)\s+([A-Za-z_]\w*)", code)]
    else:
        matches = [(m, "type") for m in _CLASS.finditer(code)]
        if ext in (".swift", ".php"):  # Swift / PHP enums have methods; a Swift actor is a class
            matches += [(m, "type") for m in re.finditer(r"(?<![\w.$@])(?:(?:public|private|internal|fileprivate|indirect|final)\s+)*"
                                                         r"(enum|actor)\s+([A-Z]\w*)", code)]
        if fam == "kotlin":
            matches += [(m, "companion") for m in _COMPANION.finditer(code)]
    for m, how in matches:
        if how == "impl":
            name, bases, kw = m.group(2).split("::")[-1], ([m.group(1).split("::")[-1]] if m.group(1) else []), "impl"
        elif how == "companion":
            name, bases, kw = "", [], "companion"
        else:
            kw, name = m.group(1), m.group(2)
            qn = re.match(r"(?:\s*::\s*[A-Za-z_]\w*)+", code[m.end():m.end() + 200])
            if qn:  # `struct Device::DevicePrivate {`: the nested type's own name
                name = qn.group(0).rsplit("::", 1)[-1].strip()
            sq = re.match(r"(?:\.[A-Za-z_]\w*)+", code[m.end():m.end() + 200]) if ext == ".swift" and kw == "extension" else None
            if sq:  # `extension NameSpecification.Element {`: the nested type's own name
                name = sq.group(0).rsplit(".", 1)[-1]
            if _ALLCAPS.match(name):  # `class KDECONNECTCORE_EXPORT NetworkPacket`: an export macro, then the name
                nm2 = re.match(r"\s+([A-Za-z_]\w*)", code[m.end():m.end() + 200])
                if nm2 and not _ALLCAPS.match(nm2.group(1)):
                    name = nm2.group(1)
            if kw == "object" and fam not in ("kotlin",):
                continue
            if kw == "protocol" and ext not in (".swift",):
                continue
            bases = []
        # header: to the body's `{` at depth 0, or to the end of the declaration
        k, depth, body = m.end(), 0, None
        while k < n and k - m.end() < 3000:
            ch = code[k]
            if ch in "(<[":
                depth += 1
            elif ch in ")>]":
                depth -= 1 if not (ch == ">" and code[k - 1] == "-") else 0
            elif depth <= 0 and ch == "{":
                body = k
                break
            elif depth <= 0 and ch == ";":
                break
            elif depth <= 0 and ch == "\n" and fam in ("kotlin", "js") and code[k + 1:k + 200].lstrip()[:1] not in ("{", ":", ",", ""):
                nxt = code[k + 1:k + 200].lstrip()
                if not code[:k].rstrip().endswith((",", ":", "(")) and not (
                        fam == "kotlin" and re.match(r"(?:@\w+\s*|(?:private|internal|public|protected)\s+)*constructor\b|@\w+\s*$|@\w+\s*\n", nxt)):
                    break  # (Kotlin: `class Builder\n  @JvmOverloads\n  constructor(...) {` goes on)
            elif ch == "=" and depth <= 0 and fam in ("js", "c") and how == "type":
                break  # `type X = ...` / a declaration, not a class body
            k += 1
        hdr = code[m.end():body if body is not None else k]
        if how == "type":
            h = re.sub(r"\bwhere\b.*", "", hdr, flags=re.S)
            h = re.sub(r"<[^<>]*(?:<[^<>]*>[^<>]*)*>", "", h)  # generic arguments
            h2 = re.sub(r"\([^()]*(?:\([^()]*\)[^()]*)*\)", "", h)  # Kotlin primary constructors, `Bar()` calls
            tail = re.split(r"\bextends\b|\bimplements\b|:", h2, maxsplit=1)
            if len(tail) == 2:
                bases = [b.split("::")[-1].split(".")[-1].split("\\")[-1] for b in re.findall(r"[A-Za-z_$][\w$.:\\]*", tail[1])]
                bases = [b for b in bases if b not in _BASE_SKIP]
                if ext == ".cs":  # `: Policy<TResult>, IAsyncPolicy`: a generic base by its arity
                    raw_tail = re.split(r":", re.sub(r"\bwhere\b.*", "", hdr, flags=re.S), maxsplit=1)
                    if len(raw_tail) == 2:
                        depth, last, parts = 0, 0, []
                        for k2, ch2 in enumerate(raw_tail[1] + ","):
                            depth += ch2 in "<(["
                            depth -= ch2 in ">)]"
                            if ch2 == "," and depth == 0:
                                parts.append(raw_tail[1][last:k2])
                                last = k2 + 1
                        gb = [generic_name(x).split(".")[-1] for x in parts if generic_name(x)]
                        bases = [b for b in gb if b.split("`")[0] not in _BASE_SKIP] or bases
        span = None
        if body is not None:
            d, j = 0, body
            while j < n:
                if code[j] == "{":
                    d += 1
                elif code[j] == "}":
                    d -= 1
                    if d == 0:
                        break
                j += 1
            span = (body, j)
            if ext == ".php":  # `use SomeTrait;` in the body: the trait's methods are the class's
                for tm in re.finditer(r"^[ \t]*use[ \t]+([\w\\, \t]+)[ \t]*[;{]", code[body:j], re.M):
                    bases += [t.strip().split("\\")[-1] for t in tm.group(1).split(",") if t.strip()]
        elif ext in _CPP_EXTS:
            continue  # `class QMouseEvent;`: a forward declaration names a type defined elsewhere
        kind = "ext" if kw in ("extension", "impl") or re.search(r"\bpartial\b", code[max(0, m.start() - 40):m.end()]) else "class"
        if kw in ("interface", "protocol", "trait"):
            kind = "interface"
        am = re.match(r"\s*<([^>{]*)>", hdr) if ext == ".cs" else None  # C#'s `class Builder<TResult>`
        out.append({"name": name, "kw": kw, "bases": bases, "hdr": hdr, "pos": m.start(), "span": span, "kind": kind,
                    "arity": (am.group(1).count(",") + 1) if am else 0})
    return out


# fields: Rust `pub name: Type,`, Swift `var name: Type` / `let name = Type(`,
# PHP `private ?Type $name;` (and promoted constructor parameters)
_RS_FIELD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_]\w*)\s*:\s*([^,<]+?(?:<.*>)?[^,<]*?),?\s*$")
_SW_FIELD = re.compile(r"^\s*(?:(?:private|public|internal|fileprivate|open|static|class|lazy|weak|unowned|final|override|nonisolated)"
                       r"(?:\([^)]*\))?\s+)*(?:let|var)\s+([A-Za-z_]\w*)\s*(?::\s*\(?\s*(?:any\s+|some\s+)?(\[[^\]=\n]*\][?!]?|[A-Z][\w.]*)|=\s*([A-Z]\w*)\s*\()")
_PHP_FIELD = re.compile(r"(?:^\s*|[(,]\s*)(?:(?:public|private|protected|static|readonly|var)\s+)+\??([\\\w]+)\s+(\$\w+)")


# a function's declared return type, from its signature
_RET_AFTER = {
    "js": re.compile(r"\)\s*:\s*([^={;]+?)\s*(?:=>|\{|;|$)", re.S),
    "kotlin": re.compile(r"\)\s*:\s*([^={\n]+?)\s*(?:=|\{|\bwhere\b|\n|$)", re.S),
    "rust": re.compile(r"->\s*([^{;]+?)\s*(?:\bwhere\b|\{|;|$)", re.S),
    "swift": re.compile(r"->\s*([^{]+?)\s*(?:\bwhere\b|\{|$)", re.S),
    "php": re.compile(r"\)\s*:\s*\??\s*([\\\w|?]+)", re.S),
}
_RET_MODS = {"public", "private", "protected", "internal", "static", "final", "abstract", "virtual", "override",
             "async", "sealed", "extern", "unsafe", "new", "readonly", "synchronized", "native", "default", "inline",
             "constexpr", "explicit", "friend", "volatile", "Q_INVOKABLE", "transient", "strictfp", "partial", "const",
             "fn", "func", "fun", "def", "function", "operator", "infix", "suspend", "tailrec", "open", "export"}


def _after_params(sig: str) -> int:
    """Where the parameter list ends (its `)`): a parameter's own function type
    (`handler: () -> Void`) is no return type."""
    k = sig.find("(")
    if k < 0 or not re.fullmatch(r"[\s\w<>,:?]*", sig[:k]):
        return 0
    close = lambdas.matching(sig, k)
    return close if close > 0 else 0


def _returns(tk: str, prefix: str, sig: str, name: str, cls: str) -> str:
    """The return type's class: `Foo bar(` (Java, C#, C++: the words before
    the name), `): Foo` (TS, Kotlin, PHP), `-> Foo` (Rust, Swift), Go's result
    list; a constructor returns its class."""
    if name in (cls, "constructor", "__construct", "init") and cls:
        return "Self"
    if tk == "go":
        m = re.match(r"\s*(?:\[[^\]]*\])?\s*\((?:[^()]|\([^()]*\))*\)\s*(.*)$", sig, re.S)
        res = (m.group(1) if m else "").strip().rstrip("{").strip()
        if res.startswith("("):
            res = res[1:].split(",")[0].strip()
            res = res.split()[-1] if len(res.split()) == 2 else res  # `(n int, err error)`
        return type_name(res.lstrip("*"))
    pat = _RET_AFTER.get(tk)
    if pat:
        m = pat.search(sig, _after_params(sig))
        return type_name(m.group(1)) if m else ""
    if tk in ("jvm", "cpp"):
        p = re.sub(r"\[[^\]]*\]|@\w+(?:\([^)]*\))?", " ", prefix)  # attributes / annotations
        p = re.sub(r"\b\w+::\s*$", "", p.rstrip()).rstrip()  # `Foo* Class::` -> `Foo*`
        if re.search(r"\)[?\[\]]*$", p):
            return ""  # a C# tuple `(bool, object?) TryGet(`: no one class
        toks = re.findall(r"[\w.:]+(?:<[^;{}()]*>)?[?*&\[\]]*", p)
        toks = [t for t in toks if t not in _RET_MODS]
        return (generic_name(toks[-1]) if tk == "jvm" else type_name(toks[-1])) if toks else ""
    return ""


def _return_text(tk: str, prefix: str, sig: str) -> str:
    """The declared return type as written (`Vec<Match>`), for its element."""
    if tk == "go":
        m = re.match(r"\s*(?:\[[^\]]*\])?\s*\((?:[^()]|\([^()]*\))*\)\s*(.*)$", sig, re.S)
        res = (m.group(1) if m else "").strip().rstrip("{").strip()
        return res[1:].split(",")[0].strip().split()[-1] if res.startswith("(") and res[1:].strip() else res
    pat = _RET_AFTER.get(tk)
    if pat:
        m = pat.search(sig, _after_params(sig))
        return m.group(1).strip() if m else ""
    if tk in ("jvm", "cpp"):
        p = re.sub(r"\[[^\]]*\]|@\w+(?:\([^)]*\))?", " ", prefix) if tk == "jvm" else prefix
        p = re.sub(r"\b\w+::\s*$", "", p.rstrip()).rstrip()
        toks = re.findall(r"[\w.:]+(?:<[^;{}()]*>)?[?*&\[\]]*", p)
        toks = [t for t in toks if t not in _RET_MODS]
        return toks[-1] if toks else ""
    return ""


_RS_WRAPPED = re.compile(r"^\s*(?:&\s*)?(?:(?:std::)?(?:option|result|io|fmt)::)?(?:Option|Result)\s*<")
_RS_ARM = re.compile(r"(?:\b([A-Z]\w*)::)?\b([A-Z]\w*)\s*\(\s*(?:ref\s+)?(?:mut\s+)?([a-z_]\w*)\s*\)\s*(=>|=(?![=>]))")


def _rs_arms(body: str, first_line: int):
    """(name, 1-based line, "variant:<Enum::>Variant") for each line a
    `Variant(x) =>` arm or an `if let Variant(x) = ...` block binds x on."""
    out = []
    starts = [0]
    for ln in body.split("\n")[:-1]:
        starts.append(starts[-1] + len(ln) + 1)

    def line_at(pos):
        lo, hi = 0, len(starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if starts[mid] <= pos:
                lo = mid
            else:
                hi = mid - 1
        return lo
    for m in _RS_ARM.finditer(body):
        if m.group(2) in ("Some", "Ok", "Err", "Box", "Rc", "Arc", "Cow", "Vec"):
            continue
        k = m.end()
        while k < len(body) and body[k] in " \t\n":
            k += 1
        if m.group(4) == "=":  # `if let V(x) = expr {`: the block after
            k = body.find("{", k)
        if k < 0 or k >= len(body):
            continue
        if body[k] == "{":
            end = lambdas.matching(body, k)
        else:  # `V(x) => x.ino(),`: to the arm's `,` (or the match's `}`)
            d, end = 0, k
            while end < len(body) and end - k < 2000:
                ch = body[end]
                if ch in "([{":
                    d += 1
                elif ch in ")]}":
                    if d == 0:
                        break
                    d -= 1
                elif ch == "," and d == 0:
                    break
                end += 1
        if end < 0:
            continue
        spec = "variant:" + (f"{m.group(1)}::" if m.group(1) else "") + m.group(2)
        for ln_ in range(line_at(m.start()), line_at(end) + 1):
            out.append((m.group(3), first_line + ln_ + 1, spec))
    return out


# `for x in xs` / `for (const x of xs)` / `foreach (var x in xs)`: x is an element of xs
_LOOPS = {
    "rust": re.compile(r"\bfor\s+(?:&\s*)?(?:mut\s+)?([a-z_]\w*)\s+in\s+(?:&\s*(?:mut\s+)?)?"),
    "js": re.compile(r"\bfor\s*\(\s*(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s+of\s+"),
    "kotlin": re.compile(r"\bfor\s*\(\s*([a-z_]\w*)\s+in\s+"),
    "swift": re.compile(r"\bfor\s+(?:case\s+let\s+|var\s+)?([a-z_]\w*)\s+in\s+"),
    "go": re.compile(r"\bfor\s+[a-z_]\w*\s*,\s*([a-z_]\w*)\s*:=\s*range\s+"),
    "jvm": re.compile(r"\bfor(?:each)?\s*\(\s*(?:final\s+)?var\s+([a-z_]\w*)\s*(?::|\bin\b)\s*"),
    "cpp": re.compile(r"\bfor\s*\(\s*(?:const\s+)?auto\s*[&*]*\s*([a-z_]\w*)\s*:\s*"),
}
_ITER_ADAPTERS = {"iter()", "iter_mut()", "into_iter()", "drain()", "asSequence()", "asIterable()", "toList()",
                  "toArray()", "entries()", "values()", "lazy"}


def _loop_bindings(tk: str, text: str) -> list[tuple[str, str]]:
    """(loop variable, "elem:<chain>") for each for-loop over a chain."""
    pat = _LOOPS.get(tk)
    if not pat:
        return []
    out = []
    for m in pat.finditer(text):
        if tk in ("rust", "swift", "go"):  # `for m in xs.iter() {`: the chain ends at the body's brace
            brace = text.find("{", m.end())
            ch = expression(text[:brace] + "\n", m.end()) if brace > 0 else ()
        else:
            ch = expression(text, m.end())
        if not ch and tk in ("jvm", "cpp", "kotlin", "js"):  # (`for (x in xs)`: the chain ends at the `)`)
            close = text.find(")", m.end())
            ch = expression(text[:close] + "\n", m.end()) if close > 0 else ()
        while ch and ch[-1] in _ITER_ADAPTERS:
            ch = ch[:-1]
        if ch:
            out.append((m.group(1), "elem:" + "\x1f".join(ch)))
    return out


# `x = <expr>`: the local takes the expression's type
_ASSIGN = {
    "js": re.compile(r"\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=;\n]+)?=(?!=)"),
    "kotlin": re.compile(r"\b(?:val|var)\s+([A-Za-z_]\w*)\s*(?::[^=\n]+)?=(?!=)"),
    "swift": re.compile(r"\b(?:let|var)\s+([A-Za-z_]\w*)\s*(?::[^=\n]+)?=(?!=)"),
    "rust": re.compile(r"\blet\s+(?:mut\s+)?([A-Za-z_]\w*)\s*(?::[^=\n]+)?=(?!=)"),
    "go": re.compile(r"(?:^|[;{]|\bvar\b)\s*([A-Za-z_]\w*)\s*(?:,\s*[A-Za-z_]\w*\s*)*(?::=|=(?!=))", re.M),
    "jvm": re.compile(r"\bvar\s+([A-Za-z_]\w*)\s*=(?!=)"),
    "cpp": re.compile(r"\bauto\s*[&*]*\s*([A-Za-z_]\w*)\s*=(?!=)\s*[*&]?"),  # (`auto &x = *it.value().get();`)
    "php": re.compile(r"(\$[A-Za-z_]\w*)\s*=(?![=>])"),
    "py": re.compile(r"^[ \t]*([A-Za-z_]\w*)\s*(?::[^=\n]+)?=(?!=)", re.M),
}


# a class-level property set from an expression (no declared type)
_FIELD_ASSIGN = {
    "kotlin": re.compile(r"\b(?:val|var)\s+([A-Za-z_]\w*)\s*=(?!=)"),
    "swift": re.compile(r"\b(?:let|var)\s+([A-Za-z_]\w*)\s*=(?!=)"),
    "js": re.compile(r"^\s*(?:(?:private|public|protected|readonly|static|declare|override|export|const|let|var)\s+)*#?([A-Za-z_$][\w$]*)\s*[?!]?\s*(?::[^=;\n]+)?=(?![=>])"),
    "php": re.compile(r"(?:private|public|protected|var)\s+(?:readonly\s+)?(?:\??[\\\w]+\s+)?(\$\w+)\s*=(?!=)"),
    "jvm": re.compile(r"\bvar\s+([A-Za-z_]\w*)\s*=(?!=)"),
}


def _assignments(tk: str, text: str) -> list[tuple[str, str]]:
    """(local, "expr:<chain>") for `x = <chain>`, and (local, "call:<chain>")
    for a closure `x = |a| <chain>` / `x = { a -> <chain> }`: calling it gives that."""
    pat = _ASSIGN.get(tk)
    if not pat:
        return []
    out = []
    for m in pat.finditer(text):
        rest = text[m.end():m.end() + 300].lstrip()
        cm = re.match(r"(?:move\s*)?\|[^|]*\|\s*", rest) if tk == "rust" else \
            re.match(r"\{\s*(?:[\w, ]+->)?\s*", rest) if tk == "kotlin" else None
        if cm:
            body_at = cm.end()
            if rest[body_at:].startswith("{") and tk == "rust":  # `|p| {\n  one.expr(p)\n}`: a block of one expression
                body_at += 1
            if not rest[body_at:].startswith("{"):
                ch = expression(rest, body_at)
                if ch:
                    out.append((m.group(1), "call:" + "\x1f".join(ch)))
            continue
        lit = _LITERAL.get(tk, {}).get(rest[:1])
        if lit == "Vec" and not rest.startswith("vec!"):
            lit = None  # (only `vec![...]`)
        if lit and not re.match(r"[\[{][^\n]*?[\]}]\s*(?:\.|\[|\?\.)", rest):  # (`[a, b].map(...)` is no array)
            out.append((m.group(1), "type:" + lit))  # `x = []`, `x = ""`: the language's own value
            continue
        ch = expression(text, m.end())
        if ch and ch[0] in ("null", "nil", "None", "undefined", "NULL", "nullptr") and len(ch) == 1:
            continue  # `$stats = null;`: no type yet; a later assignment says it
        if ch:
            out.append((m.group(1), "expr:" + "\x1f".join(ch)))
    if tk == "js":  # `const template = <p>Hello</p>`: an element of the JSX runtime
        for m in re.finditer(r"\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=;\n]+)?=\s*\(?\s*<[A-Za-z>]", text):
            out.append((m.group(1), "jsx:"))
    if tk == "js":  # `var blog = express(), app = express()`: the names after the first
        for m in re.finditer(r"\b(?:var|let|const)\s+", text):
            k, d, n = m.end(), 0, len(text)
            while k < n and k - m.end() < 3000:  # the statement's end: `;`, or a line break at depth 0 not continued
                ch = text[k]
                if ch in "([{":
                    d += 1
                elif ch in ")]}":
                    d -= 1
                    if d < 0:
                        break
                elif d == 0 and ch == ";":
                    break
                elif d == 0 and ch == ",":
                    am = re.match(r"\s*([A-Za-z_$][\w$]*)\s*=(?![=>])", text[k + 1:k + 200])
                    if am:
                        ch_ = expression(text, k + 1 + am.end())
                        if ch_:
                            out.append((am.group(1), "expr:" + "\x1f".join(ch_)))
                elif d == 0 and ch == "\n" and not text[k + 1:k + 200].lstrip().startswith(",") \
                        and not text[:k].rstrip().endswith((",", "=")):
                    break
                k += 1
    if tk == "js":  # `const { h, app: a } = window`: each name is that member of the value
        for m in _JS_DESTRUCTURE.finditer(text):
            ch = expression(text, m.end())
            if not ch or not re.match(r"^[\w$]+$", ch[-1] if ch[-1][-2:] != "()" else "x"):
                continue
            for part in lambdas.split_top(m.group(1)):
                pm = re.match(r"^\s*([A-Za-z_$][\w$]*)\s*(?::\s*([A-Za-z_$][\w$]*))?\s*(?:=.*)?$", part, re.S)
                if pm:
                    out.append((pm.group(2) or pm.group(1), "expr:" + "\x1f".join(ch + (pm.group(1),))))
    return out


_JS_DESTRUCTURE = re.compile(r"\b(?:const|let|var)\s*\{([^{}=;]*)\}\s*(?::[^=;]+)?=(?!=)")


# what a literal on the right of `x =` builds
_LITERAL = {"js": {"[": "Array", '"': "String", "'": "String", "`": "String"},
            "py": {"[": "list", "{": "dict", '"': "str", "'": "str"},
            "swift": {"[": "Array", '"': "String"},
            "rust": {"[": "Array", '"': "str", "v": "Vec"}}


def _own_return(code: str, bpos: int) -> str:
    """With no declared type, what a function's own last `return <chain>`
    (at its body's top level, not a callback's) gives: "expr:<chain>"."""
    depth, k, n, last = 0, bpos, len(code), ""
    while k < n:
        ch = code[k]
        if ch in "{([":
            depth += 1
        elif ch in "})]":
            depth -= 1
            if depth == 0:
                break
        elif depth == 1 and code.startswith("return", k) and not code[k - 1:k].isalnum() and code[k + 6:k + 7] in " \t":
            ch_ = expression(code, k + 6)
            last = ("expr:" + "\x1f".join(ch_)) if ch_ else ""
        k += 1
    return last


def _stem_key(s: str) -> str:
    return re.sub(r"[-_.]", "", s).lower()


_QML_ID = re.compile(r"(?:^|[;{])[ \t]*id[ \t]*:[ \t]*([A-Za-z_]\w*)", re.M)
_QML_BASIC_TYPES = {"url": "QUrl", "string": "QString", "date": "QDate", "color": "QColor", "point": "QPointF",
                    "rect": "QRectF", "size": "QSizeF", "font": "QFont", "real": "double", "int": "int", "bool": "bool"}
_QML_BASIC = re.compile(r"\bproperty[ \t]+(" + "|".join(_QML_BASIC_TYPES) + r")[ \t]+([a-z_]\w*)")
_QML_PROP = re.compile(r"\b(?:required[ \t]+|readonly[ \t]+|default[ \t]+)*property[ \t]+([A-Z][\w.]*)[ \t]+([a-z_]\w*)")
_QML_ROOT = re.compile(r"^[ \t]*([A-Z][\w.]*)\s*\{", re.M)


def _qml_types(mod: Module, code: str, stem: str) -> None:
    """A QML file is a component type named by its file, whose base is its
    root object's type. `Type { id: x }` and `property Type x` say what x
    is; the root object's id is the component itself."""
    mod.kind = "class"
    name = next((d for d in [os.path.basename(mod.file)[:-4]] if d[:1].isupper()), "")
    if name and name not in mod.decls:
        mod.decls.append(name)
    root = _QML_ROOT.search(code)
    if root:
        mod.bases = [root.group(1)]
    for m in _QML_ID.finditer(code):
        depth, k = 0, m.start(1)
        while k > 0:  # back to the `{` of the object this id names
            k -= 1
            if code[k] == "}":
                depth += 1
            elif code[k] == "{":
                if depth == 0:
                    break
                depth -= 1
        tm = re.search(r"([A-Z][\w.]*)\s*$", code[max(0, k - 120):k])
        if not tm:
            continue
        if root and k == code.find("{", root.start()):
            mod.bound[m.group(1)] = (("type:" + name) if name else "mod:" + mod.name)  # the root: this component
        else:
            mod.bound.setdefault(m.group(1), "type:" + tm.group(1))
    generic = set()
    for m in _QML_PROP.finditer(code):
        if m.group(1) not in ("QtObject", "QObject", "Item", "Component"):  # (any object: unknown, not a library one)
            mod.bound.setdefault(m.group(2), "type:" + m.group(1))
        else:
            generic.add(m.group(2))
    for m in _QML_BASIC.finditer(code):  # `property url source`: a value type, its methods QML's own
        mod.bound.setdefault(m.group(2), "type:" + _QML_BASIC_TYPES[m.group(1)])
    for m in re.finditer(r"\b(?:property[ \t]+[\w.<>]+|signal)[ \t]+([a-z_]\w*)", code):
        mod.bound.setdefault(m.group(1), "qmlvar:")  # declared here, of a type QML doesn't say
    generic |= {m.group(1) for m in re.finditer(r"\bproperty[ \t]+(?:var|variant)[ \t]+([a-z_]\w*)", code)}
    for m in re.finditer(r"\bproperty[ \t]+(?:var|variant|QtObject|QObject)[ \t]+([a-z_]\w*)[ \t]*:", code):
        ch = expression(code, m.end())  # `property var iface: Factory.create(id)`: what it is bound to
        if ch and mod.bound.get(m.group(1), "qmlvar:") == "qmlvar:":
            mod.bound[m.group(1)] = "expr:" + "\x1f".join(ch)
    for m in re.finditer(r"(?:^|[;{])[ \t]*([a-z_]\w*)[ \t]*=(?!=)", code, re.M):
        if m.group(1) in generic and mod.bound.get(m.group(1), "qmlvar:") == "qmlvar:":  # `clipboard = Factory.create(id)`: what it holds
            ch = expression(code, m.end())
            if ch:
                mod.bound[m.group(1)] = "expr:" + "\x1f".join(ch)
    for m in re.finditer(r"^[ \t]*import[ \t]+[\w.]+(?:[ \t]+[\d.]+)?[ \t]+as[ \t]+([A-Z]\w*)", mod.raw and "\n".join(mod.raw) or code, re.M):
        mod.bound[m.group(1)] = "qmlns:"  # `KDEConnect.Foo`: a type of that QML module


_GO_RECV = re.compile(r"^func\s*\(\s*(?:\w+\s+)?\*?\s*([A-Za-z_]\w*)")
_GO_TEST = re.compile(r"(?:Test|Benchmark|Example|Fuzz)(?:[A-Z_0-9]|$)")
_GO_DECL = re.compile(r"\btype\s+([A-Za-z_]\w*)(?:\[[^\]]*\])?\s+(?!=)")
_GO_PACKAGE = re.compile(r"^package\s+(\w+)", re.M)
_GO_IMPORT_ONE = re.compile(r'^import\s+(?:([\w.]+)\s+)?"([^"]+)"', re.M)
_GO_IMPORT_BLOCK = re.compile(r"^import\s*\(([^)]*)\)", re.M)
_GO_IMPORT_LINE = re.compile(r'^\s*(?:([\w.]+)\s+)?"([^"]+)"', re.M)


def _go_imports(mod: Module, rel: str, text: str, code: str) -> None:
    """Go: the files of one directory and package see each other (the package
    is "<dir>#<name>"); an import binds its package name ("go:<path>", or
    "go?:<path>" when the name is the path's own, which core corrects to the
    package's real name)."""
    d = rel.rsplit("/", 1)[0] if "/" in rel else ""
    pm = _GO_PACKAGE.search(code)
    if pm:
        mod.package = f"{d}#{pm.group(1)}"
    specs = [(m.group(1), m.group(2)) for m in _GO_IMPORT_ONE.finditer(text)]
    for blk in _GO_IMPORT_BLOCK.finditer(text):
        specs += [(m.group(1), m.group(2)) for m in _GO_IMPORT_LINE.finditer(blk.group(1))]
    for alias, path in specs:
        if alias == "_":
            continue
        if alias == ".":
            mod.uses.append(f"go:{path}")
            continue
        last = path.rstrip("/").rsplit("/", 1)[-1]
        if not alias and re.match(r"v\d+$", last) and "/" in path:  # module/v2: the package is the one before
            last = path.rstrip("/").rsplit("/", 2)[-2]
        name = alias or re.sub(r"[^\w]", "_", last.removeprefix("go-").removesuffix("-go"))
        mod.aliases[name] = f"go:{path}" if alias else f"go?:{path}"


def _bind_lambdas(f: Func, fam: str, body: str, open_: int, name: str, chain: tuple, first_line: int, targs: int) -> None:
    """`run(x, b => b.go())`: `b` is whatever run's parameter at that place
    hands its callback ("lam:<arg>/<param>/<params>/<type args>\x1e<name>\x1e<chain>")."""
    if open_ >= len(body) or body[open_] != "(" or "\x1e" in name:
        return
    close = lambdas.matching(body, open_)
    if close < 0:
        return
    args = lambdas.split_top(body[open_ + 1:close])
    found = lambdas.lambda_args(fam, args)
    spans, at = [], open_ + 1
    for a_ in args:  # each argument's extent in the body
        spans.append((at, at + len(a_)))
        at += len(a_) + 1
    if fam in ("kotlin", "swift"):  # a trailing lambda: `run(x) { b -> ... }`
        k = close + 1
        while k < len(body) and body[k] in " \t":
            k += 1
        if body[k:k + 1] == "{":
            found.append((len(args), lambdas.brace_lambda(fam, body[k:k + 200])))
            spans.append((k, lambdas.matching(body, k)))
    for argi, names in found:
        a_, b_ = spans[argi] if argi < len(spans) else (0, -1)
        for pi, nm_ in enumerate(names):
            if not nm_ or nm_ == "_":
                continue
            spec = f"lam:{argi}/{pi}/{len(names)}/{targs}\x1e{name}\x1e" + "\x1f".join(chain)
            _bind_param(f, nm_, spec, body, a_, b_, first_line)  # (one name, two lambdas: neither says)


_TRAILING_SKIP = {"get", "set", "init", "apply", "run", "with", "use", "synchronized", "async", "launch",
                  # keywords before a block
                  "if", "else", "for", "while", "guard", "switch", "do", "catch", "defer", "repeat", "when", "try",
                  "func", "fun", "willSet", "didSet", "return", "in", "class", "struct", "enum", "extension",
                  "protocol", "object", "companion", "interface", "finally", "lazy", "inline", "where", "throws",
                  "rethrows", "async", "await", "some", "any", "private", "public", "internal", "open", "val", "var",
                  "let", "is", "as", "init", "deinit", "subscript", "operator", "actor", "mutating", "nonmutating",
                  "override", "static", "final", "unowned", "weak", "convenience", "required", "dynamic", "indirect"}


def _bind_closure(f: Func, fam: str, body: str, brace: int, name: str, chain: tuple, first_line: int = -1) -> None:
    """`xs.map { x -> ... }`: the closure's parameters, as the callee's sole
    (last) argument hands them."""
    names = lambdas.brace_lambda(fam, body[brace:brace + 400])
    end = lambdas.matching(body, brace)
    for pi, nm_ in enumerate(names):
        if not nm_ or nm_ == "_":
            continue
        spec = f"lam:0/{pi}/{len(names)}/-1\x1e{name}\x1e" + "\x1f".join(chain)
        _bind_param(f, nm_, spec, body, brace, end, first_line)


def _bind_param(f: Func, nm_: str, spec: str, body: str, a: int, b: int, first_line: int) -> None:
    """A lambda parameter: for the whole function when no other lambda uses
    the name, and (`$0`, `it`, `e` in many closures) on the lambda's own lines."""
    held = f.bound.get(nm_)
    if held is None:
        f.bound[nm_] = spec
    elif held.startswith("lam:") and held != spec:
        f.bound[nm_] = "lam:?"
    elif not held.startswith("lam:"):
        return  # (a framework's `req` / a declared type says more)
    if first_line < 0 or b < a:
        return
    l0 = first_line + body.count("\n", 0, a) + 1
    l1 = l0 + body.count("\n", a, b)
    for ln_ in range(l0, l1 + 1):
        key = f"{nm_}@{ln_}"
        old = f.bound.get(key)
        if old is None or (old.startswith("lam:") and b - a < _LAM_SIZE.get((id(f), key), 1 << 30)):
            f.bound[key] = spec  # (the innermost lambda over a line wins)
            _LAM_SIZE[(id(f), key)] = b - a


_LAM_SIZE: dict = {}  # (function, "name@line") -> the extent of the lambda bound there


# TypeScript blocks that only declare types: their `name(...)` lines are signatures, not calls
_TS_TYPE_BLOCK = re.compile(r"^[ \t]*(?:export\s+)?(?:declare\s+(?:global|module|namespace)\b[^{\n]*|(?:declare\s+)?interface\s+[^{\n]*|"
                            r"(?:export\s+)?(?:declare\s+)?type\s+\w+[^=\n]*=\s*)\{", re.M)
_TS_SIGNATURE = re.compile(r"^\s*(?:(?:public|private|protected|static|readonly|abstract|declare|export|async|get|set)\s+)*"
                           r"[\w$]+\??\s*(?:<[^>]*>)?\s*\([^)]*\)\s*:\s*[^=;{]+;\s*$")


def _top_calls(top: Func, mods_: list, code: str, lines: list[str], tk: str, fam: str) -> None:
    """JS / TS: the calls a module makes outside its functions: `const app = express()`,
    `app.use(router)`, and a test file's `describe` / `before` callbacks (not
    functions of their own), so who calls `createApp` counts them."""
    covered = set()
    for g in (g for m_ in mods_ for g in m_.funcs):
        covered.update(range(g.start - 1, g.end))
    for tm in _TS_TYPE_BLOCK.finditer(code):
        a = code.count("\n", 0, tm.start())
        close = lambdas.matching(code, tm.end() - 1)
        covered.update(range(a, (code.count("\n", 0, close) if close > 0 else a) + 1))
    keep = [i not in covered for i in range(len(lines))]
    text = "\n".join(ln if keep[i] else "" for i, ln in enumerate(lines))
    for nm_, held in _assignments(tk, text):
        top.bound.setdefault(nm_, held)
    off = 0
    for i, ln in enumerate(lines):
        if keep[i] and not _TS_SIGNATURE.match(ln):
            for m in _CALL.finditer(ln):
                nm = m.group(2)
                if re.search(r"\bfunction\s*\*?\s*$", ln[:m.start(2)]):
                    continue  # `function name(`: a declaration
                chain = receiver(text, off + m.start(2))
                before = ln[:m.start(2)].rstrip()
                q = m.group(1) or ("." if before.endswith((".", "->")) and not before.endswith("...") else "")
                if chain and q and q != ".":
                    q = chain[-1] if re.match(r"^[\w$]+$", chain[-1]) else "."
                if nm in _CALL_SKIP and (not q or (nm in _NOT_FUNC and nm != "new")):
                    continue
                top.calls.append(Call(nm, q, i + 1, chain if q else (), m.start(2)))
        off += (len(ln) if keep[i] else 0) + 1


def parse_brace(path: str, rel: str, text: str, res_of=None) -> list[Module]:
    from . import chains
    _LAM_SIZE.clear()
    chains.CALL_TARGS = rel.endswith(".cs")
    chains.TRAILING_CLOSURES = rel.endswith((".swift", ".kt", ".kts"))
    try:
        return _parse_brace(path, rel, text, res_of)
    finally:
        chains.CALL_TARGS = chains.TRAILING_CLOSURES = False


def _parse_brace(path: str, rel: str, text: str, res_of=None) -> list[Module]:
    ext = "." + rel.rsplit(".", 1)[-1]
    fam, pats = LANGS[ext]
    code, strings = blank(text, fam)
    if ext == ".cs":  # preprocessor lines (`#pragma warning disable` between a signature and its `=>`)
        code = re.sub(r"(?m)^[ \t]*#[ \t]*(?:pragma|region|endregion|nullable|if|elif|else|endif|define|undef|warning|error|line)\b[^\n]*",
                      lambda m: " " * len(m.group()), code)
    if ext in (".swift", ".kt", ".kts"):  # `default`, `is`: a quoted identifier is the identifier
        code = re.sub(r"`([A-Za-z_]\w*)`", r" \1 ", code)
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

    # (C++: only definitions declare a type; `class QMouseEvent;` names one defined elsewhere)
    mod.decls = [] if ext in _CPP_EXTS else sorted({d.group(1) for d in (_GO_DECL if ext == ".go" else _SW_DECL if ext == ".swift" else
                                                                         _DECL).finditer(code)})
    if fam == "js":  # `type X` declares only as `type X = ...` (not `import { type X }`), and an alias of a
        # runtime type (`type ElementsMap = Map<...> & Brand`) is that library type, no project class
        aliases = {m.group(1): m.group(2) for m in _TS_ALIAS.finditer(code)}
        others = {m.group(1) for m in re.finditer(r"\b(?:class|interface|enum)\s+([A-Z]\w*)", code)}
        mod.decls = [d for d in mod.decls if d in others or (d in aliases and aliases[d] not in _TS_BUILTIN_TYPES)]
        _js_imports(mod, rel, text, lines)
        for em in re.finditer(r"^[ \t]*(?:module\.)?exports\.([A-Za-z_$][\w$]*)\s*=\s*([A-Za-z_$][\w$]*)(?:\.([A-Za-z_$][\w$]*))?\s*;?[ \t]*$",
                              code, re.M):
            if not em.group(3):  # `exports.Router = Router`: whatever that name is here (`require('router')`: the package's)
                mod.bound.setdefault(em.group(1), "reexport:" + em.group(2))
        _js_reexports(mod, rel, text, lines)
        if "require(" in text or "module.exports" in text:
            _cjs(mod, rel, text)
    elif fam == "kotlin" or ext == ".java":
        _kt_imports(mod, text, lines)
    elif ext == ".cs":
        _cs_imports(mod, code)
    elif ext == ".go":
        _go_imports(mod, rel, text, code)
    elif ext == ".rs":
        _rs_imports(mod, rel, code)
        # `mod convert { pub fn string(...) }` inline: `convert::string(v)` is this file's
        for im_ in re.finditer(r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z_]\w*)\s*\{", code, re.M):
            if im_.group(1) not in ("tests", "test"):
                mod.bound.setdefault(im_.group(1), "mod:" + mod.name)
        # `pub static PROJECT_DIRS: Lazy<BatProjectDirs> = ...`: a module-level value of that type
        for sm in re.finditer(r"^(?:pub(?:\([^)]*\))?\s+)?(?:static|const)\s+(?:mut\s+)?([A-Z_][A-Z0-9_]*)\s*:\s*([^=;]+?)\s*=", code, re.M):
            t_ = type_name(sm.group(2))
            if t_:
                mod.bound.setdefault(sm.group(1), "type:" + t_)
        # `type Range = Match;`: another name for Match (top level, not an impl's associated type)
        for am in re.finditer(r"^(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Z]\w*)\s*(?:<[^=;]*>)?\s*=\s*([^;]+);", code, re.M):
            t_ = type_name(am.group(2))
            if t_ and t_ != am.group(1):
                mod.bound.setdefault(am.group(1), "type:" + t_)
    elif ext == ".swift":
        _swift_imports(mod, rel, code)
    elif ext == ".php":
        _php_imports(mod, text, code)
    elif ext in _CPP_EXTS:
        for gm in re.finditer(r"\bQ_GLOBAL_STATIC(?:_WITH_ARGS)?\s*\(\s*([\w:<>]+)\s*,\s*(\w+)", code):
            mod.bound.setdefault(gm.group(2), "type:" + type_name(gm.group(1)))  # Qt's global object `s_session`
        # `#include "x.h"` (core resolves it): its free functions are in scope
        d = rel.rsplit("/", 1)[0] if "/" in rel else ""
        for im in re.finditer(r'^[ \t]*#[ \t]*include[ \t]*"([^"]+)"', text, re.M):
            mod.uses.append(f"inc:{d}\0{im.group(1)}")
    seen: set[tuple[int, str]] = set()
    found = []
    kotlin = fam == "kotlin"
    cpp = ext in (".cpp", ".cc", ".cxx", ".hpp", ".hh", ".h", ".c")
    qml = ext == ".qml"
    for pat in pats:
        ts_start = "tsstart" in pat.groupindex
        for m in pat.finditer(code):
            name = m.group(2) if ts_start else m.group(1)
            if name in _NOT_FUNC and not (fam == "rust" and name == "new"):  # (`fn new` is every constructor)
                continue
            npos_ = m.start(2) if ts_start else m.start(1)
            start_line = line_of(npos_)
            if (start_line, name) in seen:
                continue
            if not ts_start:
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
                    if fam == "js" and not qml and code[:k].rstrip()[-1:] in (":", "&", "|", ",", "<") and ")" in code[m.end() - 1:k]:
                        e_ = lambdas.matching(code, k)  # a type literal (`): R & { get(): R } {`), not the body
                        if e_ > 0:
                            k = e_ + 1
                            continue
                    body_start = k
                    break
                elif depth <= 0 and ch == ";":
                    body_end = k  # a declaration without a body (trait/interface) or an expression body
                    break
                elif depth <= 0 and ch == "\n" and "=>" in code[m.start():k] and not code[max(m.start(), k - 8):k].rstrip().endswith("=>"):
                    body_end = k
                    break
                elif depth <= 0 and ch == "\n" and ext == ".swift" and \
                        not code[k + 1:k + 80].lstrip().startswith(("{", "->", "throws", "rethrows", "async", "where")) and \
                        not code[m.start():k].rstrip().endswith(("->", ",", "throws", "async", "where", "&", ":")):
                    body_end = k  # a protocol requirement: no body
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
            if cpp:
                ls = code.rfind("\n", 0, m.start(1)) + 1
                prev = code[:ls].rstrip()
                if code[ls:m.start(1)].rstrip().endswith(",") or prev.endswith((",", "(", "&&", "||", "=")) \
                        and not prev.endswith(("==", "!=", "<=", ">=")):
                    continue  # `    channel, sdslen(channel))`: an argument line of a call, not a definition
            if qml and name.startswith("on") and body_start is None:
                body_end = code.find("\n", m.end())  # `onX: expr` on one line
                body_end = n - 1 if body_end < 0 else body_end
            if ts_start:
                close_ = lambdas.matching(code, m.end() - 1)
                after_ = code[close_ + 1:body_start].strip() if close_ > 0 and body_start is not None else None
                if after_ is None or (after_ and not after_.startswith(":")) or code[m.end():body_start].count("\n") > 40:
                    continue  # a call statement (`foo(\n  a,\n)`), not a method: only `{` or `: Type {` follows
            seen.add((start_line, name))  # (a start-only match claims its line only with a body)
            if body_end is None:
                body_end = min(n - 1, m.end() + 200)
            found.append((start_line, line_of(body_end), name, npos_, body_start if body_start is not None else -1))
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
            found.append((start_line, line_of(k), name, m.start(), -1))
            tests.add((start_line, name))
    jsx = ext in (".tsx", ".jsx")
    go = ext == ".go"
    swift, php = ext == ".swift", ext == ".php"
    tk = ("cpp" if cpp else "jvm" if ext in (".java", ".cs") else "go" if go else "swift" if swift
          else "php" if php else fam)
    # --- which class each function belongs to --------------------------------
    # A function directly inside a class body is that class's method; a class
    # is a module of its own, `<file>.<Class>` (the file's module when the
    # class is named like the file: Java / Kotlin / C# / Swift convention).
    # Go methods go by their receiver, C++ `Foo::bar` bodies by their class.
    classes = [] if go or qml else _classes(code, ext, fam)
    plain: set = set()
    if ext == ".cs":  # a generic class goes by `Name`N`, not by the plain name another class may hold
        plain = {c["name"] for c in classes if not c.get("arity")}
        gen = {c["name"] for c in classes if c.get("arity")} - plain
        mod.decls = [d for d in mod.decls if d not in gen]
    for c in classes:
        if c["span"]:
            c["a"], c["b"] = line_of(c["span"][0]), line_of(c["span"][1])
        else:
            c["a"] = c["b"] = line_of(c["pos"])
    fspans = sorted(((a_, b_, nm, npos) for a_, b_, nm, npos, _ in found), key=lambda x: (x[0], -x[1]))
    stem = _stem_key(os.path.splitext(os.path.basename(rel))[0])
    mods: dict[str, Module] = {}

    def enclosing_classes(pos: int) -> list[dict]:
        return sorted((c for c in classes if c["span"] and c["span"][0] < pos <= c["span"][1]), key=lambda c: c["span"][0])

    def ckey(c: dict) -> str:  # C#: `PolicyBuilder` and `PolicyBuilder<T>` are two classes
        return f"{c['name']}`{c['arity']}" if ext == ".cs" and c.get("arity") else c["name"]

    def class_module(chain: list[dict]) -> Module:
        names = [ckey(c) for c in chain if c["name"]]
        if not names:
            return mod
        # (C#: `Foo<T>` in Foo.cs is the file's class unless a plain `Foo` is there too)
        file_cls = _stem_key(names[0].split("`")[0] if names[0].split("`")[0] not in plain else names[0]) == stem
        if len(names) == 1 and file_cls:
            home = mod
        else:
            # (a class nested in the file's own class: `OkHttpClient.Builder` is `<file>.Builder`)
            path = names[1:] if file_cls else names
            mname = f"{mod.name}.{'.'.join(path)}"
            home = mods.get(mname)
            if home is None:
                arity = chain[-1].get("arity", 0) if chain[-1]["name"] else 0
                last_ = names[-1].split("`")[0]
                home = mods[mname] = Module(mname, rel, len(raw), raw, lines, owner=mod.name,
                                            decls=[f"{last_}`{arity}" if arity else last_],
                                            aliases=mod.aliases, uses=mod.uses, imported=mod.imported)
        last = chain[-1] if chain[-1]["name"] else (chain[-2] if len(chain) > 1 else chain[-1])
        if last["kw"] == "companion":
            last = chain[-2] if len(chain) > 1 else last
        for b in last["bases"]:
            if b not in home.bases and b != names[-1]:
                home.bases.append(b)
        if home is not mod and last["kind"] == "ext" and not any(ckey(c) == names[-1] and c["kind"] != "ext" for c in classes):
            home.kind = "ext"
        elif home is not mod:
            home.kind = home.kind or ("interface" if last["kind"] == "interface" else "class")
        else:
            mod.kind = mod.kind or "class"
            arity = chain[-1].get("arity", 0) if chain[-1]["name"] else 0
            if arity:
                last_ = names[-1].split("`")[0]
                mod.decls = [d for d in mod.decls if d != last_ or last_ in plain] + [f"{last_}`{arity}"]
            elif names[-1] not in mod.decls:
                mod.decls.append(names[-1])
        return home

    def home_of(npos: int, a_: int) -> tuple[Module, dict | None]:
        """The module a function at `npos` belongs to: its innermost class,
        unless a function between them holds it (a lambda in a method)."""
        chain = enclosing_classes(npos)
        if not chain:
            return mod, None
        inner = chain[-1]
        member = code[max(0, npos - 5):npos] == "this."  # `this.use = (...) =>` in the constructor: the class's
        for fa, fb, _, fp in fspans:
            if fp < npos and fa <= a_ <= fb and fp > inner["span"][0] and (fa, fp) != (a_, npos) and not member:
                return mod, None  # inside a method: a local function
        return class_module(chain), inner
    for c in classes:  # every class gets its module, methods or not (fields, an interface)
        if c["name"] and (c["kw"] != "companion"):
            outer = enclosing_classes(c["pos"])
            home_ = class_module(outer + [c])
            gp_ = re.match(r"\s*<([^<>]*(?:<[^<>]*>[^<>]*)*)>", code[c["pos"]:c["pos"] + 300].split(c["name"], 1)[-1]) \
                if c["kw"] in ("class", "struct", "interface", "record", "protocol", "trait") else None
            if gp_:  # `final class Protected<Value>`: its type parameters, in order (for what a closure is handed)
                names_ = [re.split(r"[\s:=]", g.strip())[0] for g in lambdas.split_top(gp_.group(1))]
                if names_ and all(re.match(r"^[A-Z]\w*$", n) for n in names_):
                    home_.bound.setdefault("<>", ",".join(names_))
    if ext == ".cs":  # a class with a module of its own is not the file's (`partial class Policy` in WrapExtensions.cs)
        own_ = {d for m_ in mods.values() for d in m_.decls}
        mod.decls = [d for d in mod.decls if d not in own_ or d.split("`")[0] in {c["name"] for c in classes if _stem_key(c["name"]) == stem}]
    # Go: methods are keyed by their receiver type (two types' String() are
    # two functions), each type's in a module of its own, `<file>.<Type>`
    spans: dict[tuple[str, str], list[tuple[int, int, int, int]]] = {}
    owners: dict[tuple[str, str], tuple[Module, dict | None]] = {}
    for a_, b_, name, npos, bpos in sorted(found):
        recv = ""
        home, cls = mod, None
        if go:
            rm = _GO_RECV.match(lines[a_])
            recv = rm.group(1) if rm else ""
        elif cpp:
            cm = re.search(r"\b([A-Za-z_]\w*)\s*(?:<[^<>]*>)?\s*::\s*~?" + re.escape(name) + r"\s*\(", lines[a_])
            if cm and not enclosing_classes(npos):
                recv = cm.group(1)  # `Foo::bar(` outside a class body
        if recv and cpp and _stem_key(recv) == stem:
            home = mod  # `Foo::bar` in Foo.cpp: the file is the class
            if recv not in mod.decls:
                mod.decls.append(recv)
            mod.kind = mod.kind or "class"
        elif recv:
            home = mods.get(f"{mod.name}.{recv}")
            if home is None:
                home = mods[f"{mod.name}.{recv}"] = Module(f"{mod.name}.{recv}", rel, len(raw), raw, lines, owner=mod.name,
                                                           decls=[recv], aliases=mod.aliases, uses=mod.uses,
                                                           imported=mod.imported, kind="ext" if cpp else "")
        elif (a_, name) not in tests:
            home, cls = home_of(npos, a_)
            if fam == "js" and home.kind == "interface" and bpos < 0:
                continue  # an interface's member `applyOp: (op: Op) => void;` is a type, not a function
        key = (home.name, name)
        spans.setdefault(key, []).append((a_, b_, npos, bpos))
        owners[key] = (home, cls)
    # --- declared types: per line (a function takes the ones on its lines),
    # and fields from the lines outside every function (+ TS constructor
    # parameter properties), each in the class whose body holds them
    typed_by_line: dict[int, list[tuple[str, str]]] = {}
    if tk in ("kotlin", "js", "cpp", "jvm", "go", "rust", "swift", "php"):
        for i, ln in enumerate(lines):
            if ":" in ln or tk != fam or "=" in ln or php:  # `x: T`, or `val x = T(...)` / `new T(...)`
                got = _typed(tk, ln) + _elements(tk, ln)
                if got:
                    typed_by_line[i] = got
        inside = [False] * len(lines)
        for a_, b_, _, _, _ in found:
            for i in range(a_, min(b_ + 1, len(lines))):
                inside[i] = True
        fields: dict[str, dict[str, set[str]]] = {}
        enum_lines = {i for c in classes if c.get("kw") == "enum" and c["span"] for i in range(c["a"] + 1, c["b"] + 1)}

        def inside_any(i: int) -> bool:
            return inside[i]

        line_class: dict[int, Module] = {}
        for c in classes:
            if c["name"] and c["span"]:
                home = class_module(enclosing_classes(c["pos"]) + [c])
                for i in range(c["a"], c["b"] + 1):
                    line_class[i] = home  # (nested classes come later and win)
        if fam == "js":  # an interface member typed by an object literal (`h: {\n  app: App;\n}`): a type of its own
            for i, ln in enumerate(lines):
                om = re.match(r"^\s*(?:readonly\s+)?([A-Za-z_$][\w$]*)\s*\??\s*:\s*\{\s*$", ln)
                owner_ = line_class.get(i)
                if not om or owner_ is None or owner_.kind != "interface" or inside_any(i):
                    continue
                e_ = lambdas.matching(code, line_start[i] + ln.index("{", om.end(1)))
                if e_ < 0:
                    continue
                sub = f"{owner_.name}.{om.group(1)}"
                if sub not in mods:
                    mods[sub] = Module(sub, rel, len(raw), raw, lines, owner=owner_.owner or mod.name, decls=[],
                                       aliases=mod.aliases, uses=mod.uses, imported=mod.imported)
                    mods[sub].kind = "interface"
                fields.setdefault(owner_.name, {}).setdefault(om.group(1), set()).add("\0mod:" + sub)
                for j in range(i + 1, line_of(e_) + 1):
                    line_class[j] = mods[sub]
        for c in classes:  # Kotlin primary constructors: `class Foo(private val repo: Repo)`
            if c["name"] and fam == "kotlin":
                home = class_module(enclosing_classes(c["pos"]) + [c])
                for m in _KT_FIELD.finditer(c["hdr"]):
                    fields.setdefault(home.name, {}).setdefault(m.group(1), set()).add(m.group(2))
        expr_fields: dict[str, dict[str, tuple]] = {}
        fpat = _FIELD_ASSIGN.get(tk)
        for i, ln in enumerate(lines):
            home = line_class.get(i, mod)
            got_fields = fields.setdefault(home.name, {})
            if fpat and not inside[i]:  # `private val client = rule.newClient()`: a property's value says its type
                for m in fpat.finditer(ln):
                    ch = expression(code, line_start[i] + m.end())
                    if ch and fam == "js" and ch[0] in ("require()", "import()"):
                        continue  # `var express = require('..')`: the import handling knows what that is
                    if ch and fam == "js" and re.match(r"\s*(?:new\s+)?[\w$.<>]+\s*\((?:[^()]|\([^()]*\))*?(?:=>|\bfunction\b|\{)",
                                                         code[line_start[i] + m.end():line_start[i] + m.end() + 400]):
                        continue  # `create((set) => ({ applyOp }))`: a library object holding project callbacks
                    if ch:  # (a PHP property `$x` is read as `->x`)
                        expr_fields.setdefault(home.name, {}).setdefault(m.group(1).lstrip("$"), ch)
                    elif (lit := _LITERAL.get(tk, {}).get(code[line_start[i] + m.end():line_start[i] + m.end() + 40].lstrip()[:1])):
                        got_fields.setdefault(m.group(1).lstrip("$"), set()).add(lit)  # `let sample = """...`: a String
            if fam == "js" and not inside[i] and "{" in ln:  # `const { h } = window;` at the top of a file
                for nm_, held in _assignments("js", ln if ln.rstrip().endswith(";") else ln + ";"):
                    if held.startswith("expr:") and len(held.split("\x1f")) > 1:
                        expr_fields.setdefault(home.name, {}).setdefault(nm_, tuple(held[5:].split("\x1f")))
            pats_ = []
            if fam == "kotlin":
                pats_ = [] if inside[i] else [_KT_FIELD, _KT_NEW]
            elif fam == "js":
                pats_ = [_TS_PARAM_FIELD] + ([] if inside[i] else [_TS_FIELD, _TS_NEW, _TS_CONST, _TS_INSTANCE_TYPE])
            if not inside[i] and tk in ("cpp", "jvm", "kotlin", "js", "swift"):
                for nm_, ty in _elements(tk, ln):  # `QHash<QString, Player*> m_players;`: its elements
                    got_fields.setdefault(nm_, set()).add(ty)
            if fam == "js":
                if not inside[i] and home is not mod and home.kind == "interface" and \
                        re.match(r"^\s*(?:<[^()]*?>)?\s*\(", ln):  # a call signature: `<T>(data: T): Response`
                    o_ = line_start[i] + ln.index("(")
                    e_ = lambdas.matching(code, o_)
                    rm_ = re.match(r"\s*:\s*([^;{}\n]+)", code[e_ + 1:e_ + 300]) if e_ > 0 else None
                    if rm_ and (t_ := type_name(rm_.group(1))):
                        got_fields.setdefault("()", set()).add(t_)
            elif ext == ".cs" and not inside[i]:
                for m in _CS_MEMBER.finditer(ln):
                    got_fields.setdefault(m.group(2), set()).add(generic_name(m.group(1).rstrip("?")))
            elif tk == "jvm" and not inside[i]:
                for m in _JV_FIELD.finditer(ln):
                    got_fields.setdefault(m.group(2), set()).add(m.group(1))
            elif swift and not inside[i]:
                for m in re.finditer(r"\b(?:let|var)\s+([A-Za-z_]\w*)\s*:\s*\(?(?:any\s+)?[A-Z][\w.]*\s*<(.+)>\s*[?!]?\)?\s*(?:[={]|$)", ln):
                    got_fields.setdefault(m.group(1) + "<>", set()).add("\0gen:" + m.group(2))  # `Protected<MutableState>`
                for m in _SW_FIELD.finditer(ln):
                    t = type_name(m.group(2) or m.group(3) or "")
                    if t:
                        got_fields.setdefault(m.group(1), set()).add(t)
            elif php:
                for m in _PHP_FIELD.finditer(ln):
                    t = type_name(m.group(1).replace("\\", "."))
                    if t and (not inside[i] or "__construct" in ln):
                        got_fields.setdefault(m.group(2).lstrip("$"), set()).add(t)
                if inside[i] and "$this->" in ln:  # `$this->repo = $repo;`: the parameter's declared type
                    for m in re.finditer(r"\$this->(\w+)\s*=\s*(\$\w+)\s*;", ln):
                        fn = next((f_ for f_ in found if f_[0] <= i <= f_[1]), None)
                        if fn:
                            ts = {t for j in range(fn[0], i + 1) for v, t in typed_by_line.get(j, ()) if v == m.group(2)}
                            if len(ts) == 1:
                                got_fields.setdefault(m.group(1), set()).update(ts)
            elif tk == "go" and not inside[i]:
                if home is mod:  # `var echoCmd = &cobra.Command{`: a package-level value
                    for m in _GO_NEW.finditer(ln):
                        if m.group(1) not in _GO_KEYWORDS:
                            got_fields.setdefault(m.group(1), set()).add(m.group(2) or m.group(3))
                for m in _GO_FIELD.finditer(ln):
                    if m.group(1) not in _GO_KEYWORDS and m.group(2) not in _GO_KEYWORDS:
                        got_fields.setdefault(m.group(1), set()).add(m.group(2))
            elif tk == "rust" and not inside[i] and (home is not mod or i in line_class):  # (`struct Config` in config.rs too)
                im = re.match(r"^\s*type\s+Item\s*=\s*([^;]+);", ln)
                if im and (t := type_name(im.group(1))):  # `impl Iterator for X { type Item = Y;`: what iterating X gives
                    got_fields.setdefault("type Item", set()).add(t)
                if i in enum_lines:  # `Walkdir(walkdir::DirEntry),`: what that variant holds
                    vm = re.match(r"^\s*([A-Z]\w*)\s*\(\s*([^()]+?)\s*,?\s*\)\s*,?\s*$", ln)
                    if vm and len(lambdas.split_top(vm.group(2))) == 1 and (t := type_name(vm.group(2))):
                        got_fields.setdefault("::" + vm.group(1), set()).add(t)
                for m in _RS_FIELD.finditer(ln):
                    t = type_name(m.group(2))
                    if t:
                        got_fields.setdefault(m.group(1), set()).add(("opt:" if _RS_WRAPPED.match(m.group(2)) else "") + t)
            elif cpp and not inside[i]:
                qm = _Q_PROPERTY.match(ln)
                if qm and (t := type_name(qm.group(1))) and not _ALLCAPS.match(t):
                    got_fields.setdefault(qm.group(2), set()).add(t)  # what QML reads as `obj.name`
                for m in _CPP_FIELD.finditer(ln):
                    t = type_name(m.group(1))  # (`QPointer<CaptureTool>` holds a CaptureTool)
                    if t and not _ALLCAPS.match(t) and t not in ("Q_OBJECT", "Q_PROPERTY"):
                        got_fields.setdefault(m.group(2), set()).add(t)
            for pat_ in pats_:
                for m in pat_.finditer(ln):
                    t_ = type_name(m.group(2)) if "<" in m.group(2) else m.group(2)
                    if t_:
                        got_fields.setdefault(m.group(1), set()).add(t_)
        for mname, fl in fields.items():
            target = mod if mname == mod.name else mods.get(mname)
            if target is not None:
                target.bound = {**target.bound, **{k: (next(iter(v))[1:] if next(iter(v)).startswith("\0") else "type:" + next(iter(v)))
                                                   for k, v in fl.items() if len(v) == 1}}
        for mname, fl in expr_fields.items():
            target = mod if mname == mod.name else mods.get(mname)
            if target is not None:
                target.bound = {**{k: "expr:" + "\x1f".join(ch) for k, ch in fl.items()}, **target.bound}
        # Go: a type's methods see the fields of its struct
        if go:
            for m_ in mods.values():
                m_.bound = mod.bound
    go_test = go and rel.endswith("_test.go")
    decl_pos = None
    for (hname, name), ss in sorted(spans.items(), key=lambda kv: kv[1][0]):
        a_, b_, npos, bpos = max(ss, key=lambda s_: s_[1] - s_[0])
        home, cls = owners[(hname, name)]
        recv = home.decls[0] if (go or (cpp and home.kind == "ext")) and home is not mod else ""
        # JUnit / kotlin.test `@Test`, xUnit / NUnit / MSTest `[Fact]`, Rust `#[test]`
        annotated = fam in ("kotlin", "c", "rust") and any(_TEST_ATTR.search(raw[j]) for j in range(max(0, a_ - 3), a_ + 1))
        if go_test and not recv and _GO_TEST.match(name):
            annotated = True  # func TestX(t *testing.T), BenchmarkX, ExampleX, FuzzX
        if (swift or php) and (name.startswith("test") and (cls is not None or home.kind == "class") and re.search(r"(?:^|/)[Tt]ests?/", rel)
                               or any(re.search(r"@Test\b|@test\b|#\[Test\]", raw[j]) for j in range(max(0, a_ - 3), a_ + 1))):
            annotated = True  # XCTest / PHPUnit `testX` methods, swift-testing `@Test`, PHPUnit `#[Test]` / `@test`
        f = Func(home.name, name, rel, a_ + 1, b_ + 1, "\n".join(lines[a_:b_ + 1]),
                 kind="test" if (a_, name) in tests or annotated else "func", recv=recv)
        if fam == "js":
            _pack_params(f)
        sig_end = bpos if bpos >= 0 else min(len(code), npos + 400)
        sig = code[npos + len(name):sig_end]
        if tk in ("swift", "php", "kotlin", "jvm", "rust", "go", "js") and "\n" in sig:
            for nm_, ty in _typed(tk, sig.replace("\n", " ")):  # parameters on lines of their own
                f.bound.setdefault(nm_, "type:" + ty)
        if kotlin:
            rm = re.search(_KT_RECV % re.escape(name), lines[a_])
            if rm:
                f.recv = rm.group(1)
        elif ext == ".cs":  # overloads share one function: each one's type-parameter count, receiver, return type
            recvs, by_ov, targs = [], [], set()
            for _sa, _sb, np_, bp_ in ss:
                after = code[np_ + len(name):np_ + len(name) + 300]
                gm_ = re.match(r"\s*<([^<>()]*(?:<[^<>()]*>[^<>()]*)*)>\s*\(", after)
                t_ = 1 + gm_.group(1).count(",") if gm_ else 0  # (`NoOp<TResult>()`: 1)
                targs.add(t_)
                sig_ = code[np_ + len(name):bp_ if bp_ >= 0 else np_ + 400]
                rm = re.match(r"\s*(?:<[^>]*>)?\s*\(\s*this\s+([\w.<>\[\]?]+)\s+\w+", after)
                r = ""
                if rm:  # an extension method: `static bool IsPrimitive(this Type t)`
                    r = generic_name(rm.group(1)).rsplit(".", 1)[-1]  # (`this PolicyBuilder<T> b`: PolicyBuilder`1)
                    wm = re.search(rf"\bwhere\s+{re.escape(r)}\s*:\s*([\w.<>]+)", sig_)
                    if wm:  # `this TBuilder b ... where TBuilder : ResiliencePipelineBuilderBase`
                        r = generic_name(wm.group(1)).rsplit(".", 1)[-1]
                    if r not in recvs:
                        recvs.append(r)
                rt = _returns(tk, code[code.rfind("\n", 0, np_) + 1:np_], sig_, name, cls["name"] if cls else "")
                if f"{r}/{t_}={rt}" not in by_ov:
                    by_ov.append(f"{r}/{t_}={rt}")
            f.targs = ",".join(map(str, sorted(targs)))
            if recvs:
                f.recv = "|".join(recvs)
        if tk in ("jvm", "kotlin", "js", "swift", "rust"):  # each overload's parameter types (a callback's type for its lambda)
            ovs = []
            for _sa, _sb, np_, _bp in ss:
                k = code.find("(", np_ + len(name), np_ + len(name) + 300)
                e = lambdas.matching(code, k) if k >= 0 else -1
                gm_ = re.match(r"\s*<([^<>()]*(?:<[^<>()]*>[^<>()]*)*)>\s*\(", code[np_ + len(name):k + 1]) if k >= 0 else None
                pts = lambdas.param_types(tk, code[k + 1:e]) if e > 0 else []
                if tk == "rust" and e > 0:  # `F` -> its bound: `<F: FnMut(Match) -> bool>` / `where F: FnMut(...)`
                    bounds = dict(re.findall(r"\b([A-Z]\w*)\s*:\s*((?:Fn|FnMut|FnOnce)\s*\([^)]*\)(?:\s*->\s*[^,{>]+)?)",
                                             code[np_:np_ + len(name) + (k - np_ - len(name))] + code[e:e + 400].split("{")[0]))
                    pts = [bounds.get(re.sub(r"^&\s*(?:mut\s+)?", "", t_).strip(), t_) for t_ in pts]
                ov = (f"{1 + gm_.group(1).count(',') if gm_ else 0}{lambdas.TARGS}"  # (its type-parameter count first)
                      + lambdas.PARAM.join(pts)) if e > 0 else ""
                if e > 0 and ov not in ovs:
                    ovs.append(ov)
            f.ptypes = lambdas.OV.join(ovs)
        # the declared return type
        line_start_pos = line_start[a_]
        cls_name = (cls["name"] if cls else "") or (home.decls[-1] if home is not mod or recv else "")
        if (a_, name) not in tests:
            f.returns = _returns(tk, code[line_start_pos:npos], sig, name, cls_name)
            rtext_ = _return_text(tk, code[line_start_pos:npos], sig)
            f.relem = elem_type(rtext_)
            if fam == "js" and not f.returns and len(ss) > 1:  # `get res(): Response` beside `set res(r)`: the getter's
                for _sa, _sb, np_, _bp in ss:
                    gm_ = re.match(rf"\s*\(\s*\)\s*:\s*([^{{;=]+?)\s*\{{", code[np_ + len(name):np_ + len(name) + 200])
                    if gm_ and re.search(r"\bget\s+$", code[max(0, np_ - 12):np_]):
                        f.returns = type_name(gm_.group(1))
                        break
            crm = re.match(r"^ReturnType\s*<\s*([A-Z][\w$]*)\s*>$", rtext_.strip()) if fam == "js" else None
            if crm:  # `): ReturnType<BodyRespond> =>`: what calling a BodyRespond gives
                f.returns = "callret:" + crm.group(1)
            if tk == "rust" and _RS_WRAPPED.match(rtext_):  # `-> Option<Foo>`: a method on it is Option's until unwrapped
                f.returns = "opt:" + f.returns
            elif tk in ("rust", "swift") and rtext_.startswith("(") and lambdas.matching(rtext_, 0) == len(rtext_) - 1:
                parts_ = lambdas.split_top(rtext_[1:-1])  # `-> (Ignore, Option<Error>)`: each element's type
                if len(parts_) > 1:
                    f.returns = "tuple:" + "|".join(("opt:" if _RS_WRAPPED.match(p_) else "") + type_name(p_.split(":")[-1])
                                                    for p_ in parts_)
            if ext == ".cs" and len({x.split("=", 1)[1] for x in by_ov}) > 1:
                f.returns = "byov:" + "|".join(by_ov)  # overloads that return different types
            if not f.returns and bpos >= 0 and re.search(r"\breturn\s+(?:this|self|\$this)\s*[;\n}]", f.code):
                f.returns = "Self"  # a builder step: `return this`
            if not f.returns and bpos >= 0:
                f.returns = _own_return(code, bpos)
            if not f.returns and tk == "kotlin" and bpos < 0:
                em = re.search(r"\)\s*=\s*", sig)
                if em and re.match(r"\s*(?:apply|also)\s*\{|\s*this\b(?!\s*\.)", sig[em.end():]):
                    f.returns = "Self"  # `fun header(...) = apply { ... }`: a builder step returns its receiver
                else:
                    ch = expression(sig, em.end()) if em else ()
                    if ch:
                        f.returns = "expr:" + "\x1f".join(ch)
        done: set[int] = set()
        if decl_pos is None:  # where each function's name is declared: `exec(s) {` in a class is no call
            decl_pos = {npos_ for _, _, _, npos_, _ in found}
        for sa, sb, _, _ in ss:
            name_col = max(0, lines[sa].find(name))  # the return type before it is no call (`Task<int> Run(`)
            body = "\n".join(lines[sa:sb + 1])
            for nm_, held in _assignments(tk, body):
                f.bound.setdefault(nm_, held)
            for nm_, held in _loop_bindings(tk, body):
                f.bound.setdefault(nm_, held)
            if tk in ("rust", "swift"):  # `let (ig, err) = b.add_child(p);`: each name is that element of the tuple
                for tm_ in re.finditer(r"\b(?:let|var)\s+\(\s*([^()=]*?)\s*\)\s*(?::[^=]+)?=(?!=)", body):
                    ch_ = expression(body, tm_.end())
                    if ch_ and ch_[-1].endswith("()"):
                        for k_, nm_ in enumerate(lambdas.split_top(tm_.group(1))):
                            nm_ = re.sub(r"^(?:mut\s+|ref\s+)", "", nm_.strip())
                            if re.match(r"^[a-z_]\w*$", nm_) and nm_ != "_":
                                f.bound.setdefault(nm_, f"tuple:{k_}\x1e" + "\x1f".join(ch_))
            if tk == "rust":  # `Raw(ref x) => x.ino()`: x, on that arm's lines, is what the variant holds
                for nm_, ln_, held in _rs_arms(body, sa):
                    f.bound.setdefault(f"{nm_}@{ln_}", held)
            if fam == "js":  # `mixin(app, proto)` / `Object.assign(app, proto)`: app has proto's members
                for mm in re.finditer(r"\b(?:Object\.assign|mixin|merge|extend|_\.extend|_\.assign)\s*\(\s*([A-Za-z_$][\w$]*)\s*,\s*([A-Za-z_$][\w$]*)\s*[,)]", body):
                    f.bound[mm.group(1)] = "expr:" + mm.group(2)
            off = 0
            for i in range(sa, sb + 1):
                ln = lines[i]
                if i in done:  # (a merged span's lines count once)
                    off += len(ln) + 1
                    continue
                done.add(i)
                for nm_, ty in typed_by_line.get(i, ()):
                    held_ = f.bound.get(nm_, "")
                    segs_ = held_[5:].split("\x1f") if held_.startswith("expr:") else []
                    if len(segs_) > 2 and segs_[0] == ty.split(".")[-1] and segs_[1] in ("new()", "builder()", "default()"):
                        continue  # `Foo::new()\n  .a()\n  .build()`: the chain's end, not a Foo
                    if len(segs_) > 1 and segs_[0] == f"new {ty}()":
                        continue  # `var x = new Builder().Add().Build()`: the chain's end, not a Builder
                    f.bound[nm_] = "type:" + ty
                for m in _CALL.finditer(ln):
                    nm = m.group(2)
                    if i == sa and (nm == name or m.start(2) < name_col) or line_start[i] + m.start(2) in decl_pos:
                        continue
                    chain = receiver(body, off + m.start(2))
                    before = ln[:m.start(2)].rstrip()
                    q = m.group(1) or ("." if before.endswith((".", "->")) and not before.endswith("...") else "")
                    if chain and q and q != ".":
                        last = chain[-1]
                        q = last if re.match(r"^[\w$]+$", last) else "."
                    if nm in _CALL_SKIP and (not q or (nm in _NOT_FUNC and nm != "new")):
                        continue  # (a keyword, or `String(`; `c.String()` and `Foo::new()` are calls)
                    f.calls.append(Call(nm, q, i + 1, chain if q else (), m.start(2)))
                    if tk in ("jvm", "kotlin", "js", "swift", "rust"):
                        ta_ = m.group(0)[m.end(2) - m.start(0):].strip()
                        _bind_lambdas(f, fam, body, off + m.end() - 1, nm, chain if q else (), sa,
                                      1 + _top_commas(ta_[1:ta_.rindex(">")]) if ta_.startswith("<") and ">" in ta_ else -1)
                    if ext == ".cs":
                        ta = m.group(0)[m.end(2) - m.start(0):].strip()
                        f.calls[-1].targs = 1 + _top_commas(ta[1:ta.rindex(">")]) if ta.startswith("<") else 0
                if tk in ("kotlin", "swift"):  # `xs.map { $0.name }`, `x.let { it.go() }`: a trailing closure, no parentheses
                    for tm in re.finditer(r"(\.)?\b([A-Za-z_]\w*)[ \t]*\{", ln):
                        if tm.group(2) in _TRAILING_SKIP or tm.group(2)[:1].isupper():
                            continue
                        if tm.group(1):
                            tch = receiver(body, off + tm.start(2))
                            if tch:
                                _bind_closure(f, fam, body, off + tm.end() - 1, tm.group(2), tch, sa)
                        elif not re.search(r"[\w)\]>?]\s*$", re.sub(r"\b(?:await|try[?!]?|return|throw)\s*$", "", ln[:tm.start(2)])):
                            # `performEvent { $0... }`, `await withCheckedContinuation { c in }`: a bare call
                            _bind_closure(f, fam, body, off + tm.end() - 1, tm.group(2), (), sa)
                if jsx:  # `<Widget ...>` renders (calls) the component Widget
                    for m in _JSX.finditer(ln):
                        f.calls.append(Call(m.group(2), m.group(1) or "", i + 1))
                off += len(ln) + 1
        home.funcs.append(f)
    # a nested function (a Swift / Kotlin / Go local function, a JS closure) sees what
    # its enclosing function bound: `usedOrigins` is the outer function's parameter
    every = sorted((g for m_ in [mod, *mods.values()] for g in m_.funcs), key=lambda g: (g.start, -g.end))
    for g in every:
        outer = [o for o in every if o is not g and o.start <= g.start and g.end <= o.end and (o.start, o.end) != (g.start, g.end)]
        if outer:
            inner = max(outer, key=lambda o: o.start)
            g.bound = {**{k: v for k, v in inner.bound.items() if not v.startswith("fixture:")}, **g.bound}
    if qml:
        _qml_types(mod, code, stem)
    if go:  # `type Window interface { Top() int ... }`: a module listing the methods
        for im in re.finditer(r"^type\s+([A-Za-z_]\w*)(?:\[[^\]]*\])?\s+interface\s*\{", code, re.M):
            k, d = im.end(), 1
            while k < len(code) and d:
                d += code[k] == "{"
                d -= code[k] == "}"
                k += 1
            names = re.findall(r"^\s*([A-Za-z_]\w*)\s*\(", code[im.end():k], re.M)
            iname = f"{mod.name}.{im.group(1)}"
            imod = mods.get(iname) or Module(iname, rel, len(raw), raw, lines, owner=mod.name, decls=[im.group(1)],
                                             aliases=mod.aliases, uses=mod.uses, imported=mod.imported)
            imod.kind, imod.methods = "interface", names
            mods[iname] = imod
    top = Func(mod.name, "<top>", rel, 1, len(lines), "", kind="top")
    if fam == "js" and ext != ".qml" and not rel.endswith(".d.ts"):
        _top_calls(top, [mod, *mods.values()], code, lines, tk, fam)
    if fam == "js":  # module-level callbacks (`app.get('/', function (req, res) {` at the top of a file)
        top.code = code
        _pack_params(top)
        top.code = ""
        packed = {k: v for k, v in top.bound.items() if v.startswith("pack:")}
        for g in (g for m_ in [mod, *mods.values()] for g in m_.funcs) if packed else ():
            for k, v in packed.items():  # (`text: function(){ res.send() }` inside such a callback)
                g.bound.setdefault(k, v)
    if ext == ".qml":
        # `Button {` instantiates Button.qml: a use of that component
        for i, ln in enumerate(lines):
            cm = re.match(r"[ \t]*([A-Z]\w*)[ \t]*\{", ln)
            if cm:
                top.calls.append(Call(cm.group(1), cm.group(1), i + 1))
                top.refs.add(cm.group(1))
    mod.funcs.append(top)
    mod.decls = sorted(set(mod.decls) - {d for m_ in mods.values() for d in m_.decls if m_.kind != "ext"}) if not go else mod.decls
    return [mod, *mods.values()]
