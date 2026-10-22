"""Receiver chains and type names, read off blanked code.

A call's receiver is what it is called on: `self.store.get().save(x)` calls
`save` on the chain ("self", "store", "get()"). Core walks a chain one step
at a time (a variable's declared or assigned type, a field's type, a method's
return type) to know which class the call lands in. These helpers only read
text; they never guess a type.
"""
from __future__ import annotations

import re

_IDENT_CH = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_$")
# segments that unwrap an Option / Result / nullable: the type inside
UNWRAP = {"unwrap()", "expect()", "unwrap_or_default()", "unwrap_or()", "unwrap_or_else()", "?", "!!", "!",
          "get()", "orElseThrow()", "requireNotNull()", "checkNotNull()", "value()", "Value", "Result", "await"}


def _top_commas(text: str) -> int:
    d, n = 0, 0
    for ch in text:
        d += ch in "<([{"
        d -= ch in ">)]}"
        n += ch == "," and d == 0
    return n


def _skip_back_ws(code: str, i: int) -> int:
    while i > 0 and code[i - 1] in " \t\r\n":
        i -= 1
    return i


def _match_back(code: str, i: int, close: str, open_: str) -> int:
    """code[i-1] is `close`: the index of its matching `open_` (or -1). All
    bracket kinds nest (a callback's `{ ...; }` inside the arguments); a `;`
    outside them means the statement ended first."""
    pairs = {")": "(", "]": "[", "}": "{", ">": "<"}
    stack, k = [], i - 1
    while k >= 0:
        ch = code[k]
        if ch in ")]}" or (ch == ">" and close == ">"):
            stack.append(pairs[ch])
        elif ch in "([{" or (ch == "<" and close == ">"):
            if not stack or stack[-1] != ch:
                return -1
            stack.pop()
            if not stack:
                return k if ch == open_ else -1
        elif ch == ";" and len(stack) <= 1:
            return -1  # left the statement
        k -= 1
    return -1


def _ident_back(code: str, i: int) -> int:
    k = i
    while k > 0 and code[k - 1] in _IDENT_CH:
        k -= 1
    return k


def _anonymous_class(code: str, i: int) -> str:
    """code[i-1] is the `}` of `new Foo<T>(args) { ... }`: "new Foo()", else ""."""
    k = _match_back(code, i, "}", "{")
    if k < 0:
        return ""
    j = _skip_back_ws(code, k)
    if j == 0 or code[j - 1] != ")":
        return ""
    k2 = _match_back(code, j, ")", "(")
    if k2 < 0:
        return ""
    j2 = _skip_back_ws(code, k2)
    if j2 > 0 and code[j2 - 1] == ">":
        g = _match_back(code, j2, ">", "<")
        if g < 0:
            return ""
        j2 = _skip_back_ws(code, g)
    s = _ident_back(code, j2)
    if s == j2 or not code[max(0, s - 4):s].endswith("new "):
        return ""
    return f"new {code[s:j2]}()"


def receiver(code: str, pos: int, max_segs: int = 40) -> tuple[str, ...]:
    """The chain a call at `pos` (its name's first character) is made on, or
    () for a bare call. Segments: `name`, `name()`, `[]`, `?`, `!!`; `new X()`
    is "new X()"; a parenthesized expression is "(...)". Reads back across
    newlines (`builder\\n  .a()\\n  .b()`)."""
    segs: list[str] = []
    i = _skip_back_ws(code, pos)
    while len(segs) < max_segs:
        op = ""
        for o in ("?.", "!.", "->", "::", "."):
            if code[max(0, i - len(o)):i] == o:
                op = o
                break
        if not op:
            break
        if op == "." and i >= 2 and code[i - 2] == ".":
            break  # a range `..` or a spread `...`
        i = _skip_back_ws(code, i - len(op))
        if op == "." and not segs and (i == 0 or code[i - 1] in "(,[:={" or
                                       re.search(r"(?:^|[^\w$])(?:case|return|in|else|try|await|let|var)$", code[max(0, i - 7):i])):
            segs.append("<implicit>")  # Swift's `.option(x)`, `case .value(let v)`: a member of a type the context gives
            break
        # postfix unwraps: `x!!.`, `x!.`, `x?.` (Kotlin, TS, Swift), Rust `x?.`
        while i > 0 and code[i - 1] in "!?":
            segs.append("!!" if code[i - 1] == "!" else "?")
            i = _skip_back_ws(code, i - 1)
        if i == 0:
            break
        ch = code[i - 1]
        if ch == ")":
            k = _match_back(code, i, ")", "(")
            if k < 0:
                return ()
            j = _skip_back_ws(code, k)
            arity = ""
            if j > 0 and code[j - 1] == ">":  # `foo<T>(`: a generic call
                g = _match_back(code, j, ">", "<")
                if g > 0:
                    arity = "`" + str(1 + _top_commas(code[g + 1:j - 1]))
                    j = _skip_back_ws(code, g)
            s = _ident_back(code, j)
            if s == j:  # `(expr).x`: a parenthesized expression
                cast = re.search(r"\bas[!?]?\s+([A-Z][\w.]*)\s*[?]?\s*$", code[k + 1:i - 1]) or \
                    re.match(r"\s*\(\s*([A-Z][\w.]*)(?:<[^()]*>)?\s*\)\s*[\w$]", code[k + 1:i - 1])  # Java / C# `((Foo) x).y()`
                segs.append(f"<as:{cast.group(1)}>" if cast else "(...)")  # `($Timer as Timer).start()`: a Timer
                i = s
                break
            name = code[s:j]
            before = code[max(0, s - 4):s]
            if before.endswith("new ") or before.endswith("new\t"):
                segs.append(f"new {name}{arity}()")
                i = s - 4
                break
            segs.append(name + (arity if CALL_TARGS else "") + "()")
            i = s
        elif ch == "]":
            k = _match_back(code, i, "]", "[")
            if k < 0:
                return ()
            j = _skip_back_ws(code, k)
            if j > 0 and (code[j - 1] in _IDENT_CH or code[j - 1] in ")]"):
                segs.append("[]")
                i = j
                if code[j - 1] in _IDENT_CH:
                    s = _ident_back(code, j)
                    segs.append(code[s:j])
                    i = s
                else:
                    segs.append("(...)")
                    break
            else:
                segs.append("<literal>")  # `[1, 2].map(...)`: an array literal
                break
        elif ch == "}" and (anon := _anonymous_class(code, i)):
            segs.append(anon)  # `new TypeToken<List<X>>() {}.getType()`: an instance of TypeToken
            break
        elif ch in "\"'`":
            segs.append("<literal>")  # `"".join(...)`, `b"".join(...)`: a string's method
            break
        elif ch in _IDENT_CH:
            s = _ident_back(code, i)
            name = code[s:i]
            if not name or name[0].isdigit():
                return ()
            if name in ("await", "return", "new", "throw", "yield", "in", "of", "case", "else", "do"):
                break
            segs.append(name)
            i = s
        elif ch == ">" and code[i - 2:i] != "->":  # `Vec::<T>::new`, `Foo<T>::bar`, `Policy<T>.Handle()`
            g = _match_back(code, i, ">", "<")
            if g < 0:
                return ()
            j = _skip_back_ws(code, g)
            if code[j - 2:j] == "::":  # Rust's turbofish `Vec::<u8>::new`
                j = _skip_back_ws(code, j - 2)
            s = _ident_back(code, j)
            if s == j or code[s].isdigit():
                return ()  # `a > b.c()`: a comparison, not a generic type
            segs.append(code[s:j])
            i = s
        else:
            segs.append("(...)")
            break
    segs.reverse()
    return _bare_first(segs)


def expression(code: str, pos: int, max_segs: int = 40) -> tuple[str, ...]:
    """The chain an expression starting at `pos` builds (the right side of
    `x = <expr>`): `repo.find(id).first()` -> ("repo", "find()", "first()");
    `new Foo(a)` -> ("new Foo()",); `await svc.load()` -> ("svc", "load()").
    () when the expression is not a plain chain (arithmetic, a literal...)."""
    n = len(code)
    i = pos
    while i < n and code[i] in " \t\r\n":  # (the value may start on the next line: `val x =\n  a.b()`)
        i += 1
    for kw in ("await ", "try ", "try! ", "try? ", "yield "):
        if code.startswith(kw, i):
            i += len(kw)
    while i < n and code[i] in " \t":
        i += 1
    segs: list[str] = []
    new = False
    if code.startswith("new ", i):
        new = True
        i += 4
    while len(segs) < max_segs:
        while i < n and code[i] in " \t":
            i += 1
        s = i
        while i < n and code[i] in _IDENT_CH:
            i += 1
        if i == s or code[s].isdigit():
            return ()
        name = code[s:i]
        j = i
        while j < n and code[j] in " \t":
            j += 1
        arity = ""
        if j < n and code[j] == "<":  # generic arguments: `make<T>(` / `List<Foo>()`
            depth, k = 0, j
            while k < n and k - j < 200:
                if code[k] == "<":
                    depth += 1
                elif code[k] == ">":
                    depth -= 1
                    if depth == 0:
                        break
                elif code[k] in ";{}\n":
                    break
                k += 1
            if depth == 0 and k < n and code[k] == ">":
                arity = "`" + str(1 + _top_commas(code[j + 1:k]))
                j = k + 1
        if j < n and code[j] in "({" and not (code[j] == "{" and not new and not name[:1].isupper()):
            close = ")" if code[j] == "(" else "}"
            depth, k = 0, j
            while k < n:
                if code[k] in "([{":
                    depth += 1
                elif code[k] in ")]}":
                    depth -= 1
                    if depth == 0:
                        break
                k += 1
            if code[j] == "{":  # a struct / object literal `Foo { a: 1 }`: builds a Foo
                segs.append(f"new {name}()")
            elif new:
                segs.append(f"new {name}{arity}()")
            else:
                segs.append(name + (arity if CALL_TARGS else "") + "()")
            new = False
            i = k + 1
        else:
            if new:
                return ()
            segs.append(name)
            i = j
        # what follows: another link, an unwrap, or the end
        while True:
            while i < n and code[i] in " \t":
                i += 1
            if code.startswith("!!", i) or (i < n and code[i] == "!" and code[i + 1:i + 2] in (".", "")):
                segs.append("!!")
                i += 2 if code.startswith("!!", i) else 1
            elif i < n and code[i] == "?" and code[i + 1:i + 2] in (".", ";", "", "\n", ")", ","):
                segs.append("?")
                i += 1
            else:
                break
        nl = i
        while nl < n and code[nl] in " \t\r\n":
            nl += 1
        for o in ("?.", "->", "::", "."):
            if code.startswith(o, nl) and not code.startswith("..", nl):
                i = nl + len(o)
                break
        else:
            end = code[i:i + 1]
            if end and end not in ";,)\n\r}]" and not code[i:].lstrip(" \t").startswith(("//", "#", "as ", "?:", "??", "||", "or ")):
                return ()  # more expression follows: `a.b() + 1`, a ternary...
            return _bare_first(segs)
    return _bare_first(segs)


# C#: a method call's segment carries its type-argument count (`Handle<E>()`
# -> "Handle`1()"), as `Policy.NoOp()` and `Policy.NoOp<T>()` are different
# methods; set while a C# file is parsed
CALL_TARGS = False
_TARGS_SEG = re.compile(r"^([\w$]+)`\d+\(\)$")


def _bare_first(segs) -> tuple:
    """The chain, with a leading bare call's type-argument count dropped."""
    if segs and CALL_TARGS:
        m = _TARGS_SEG.match(segs[0])
        if m:
            segs[0] = m.group(1) + "()"
    return tuple(segs)


_WRAPPERS = {"Promise", "Task", "ValueTask", "Future", "Box", "Rc", "Arc", "Option", "Optional", "Result",
             "Deferred", "Lazy", "LazyLock", "OnceCell", "OnceLock", "Ref", "RefCell", "Cell", "Mutex", "RwLock", "Pin", "Cow", "Awaitable",
             "Coroutine", "Observable", "Single", "Maybe", "NotRequired", "Required", "Final", "ClassVar",
             "Annotated", "ReadOnly", "Readonly", "WeakReference", "Nullable", "QPointer", "QSharedPointer",
             "QScopedPointer", "QWeakPointer", "unique_ptr", "shared_ptr", "weak_ptr", "optional", "Ptr",
             "Partial", "NonNullable", "Awaited", "DeepReadonly", "Mutable",
             # what a `with` gives
             "ContextManager", "AbstractContextManager", "AsyncContextManager", "AbstractAsyncContextManager"}


# containers whose iteration gives their (first) type argument; a map's gives pairs
_SEQUENCES = {"Vec", "VecDeque", "HashSet", "BTreeSet", "LinkedList", "BinaryHeap", "List", "ArrayList", "Set", "TreeSet",
              "SortedSet", "Array", "ReadonlyArray", "Iterable", "Iterator", "IEnumerable", "ICollection", "IList",
              "IReadOnlyList", "IReadOnlyCollection", "ISet", "HashSet", "Sequence", "MutableList", "MutableSet",
              "Collection", "Stream", "QList", "QVector", "QSet", "QStringList", "vector", "list", "deque", "set",
              "span", "Span", "ReadOnlySpan", "Slice", "IntoIter", "Iter", "ArraySlice", "ContiguousArray", "Deque"}


def elem_type(text: str) -> str:
    """What iterating a declared type gives: `Vec<Match>`, `&[Match]`,
    `List<Foo>`, `Foo[]`, `[Foo]`, `impl Iterator<Item = &Foo>` -> the element's
    class; "" when it is no sequence (or a map: its pairs are no one class)."""
    t = text.strip()
    for _ in range(4):
        t = re.sub(r"^(?:&\s*(?:'\w+\s+)?(?:mut\s+)?|impl\s+|dyn\s+|const\s+|readonly\s+)", "", t).strip()
        m = re.match(r"^(?:Box|Rc|Arc|Option|Optional)\s*<(.*)>\s*[?]?$", t, re.S)
        if not m:
            break
        t = m.group(1).strip()
    t = t.rstrip("?!").strip()
    m = re.match(r"^\[([^\[\]:;]+)(?:;[^\]]*)?\]$", t)  # Rust `[Match]` / `[u8; 4]`, Swift `[Foo]`
    if m:
        return type_name(m.group(1))
    m = re.match(r"^(.+?)\s*\[\s*\]$", t)  # `Foo[]`
    if m:
        return type_name(m.group(1))
    m = re.match(r"^(?:[\w.:]+::|[\w.]+\.)?(\w+)\s*<(.*)>$", t, re.S)
    if m:
        it = re.match(r"^\s*Item\s*=\s*(.+)$", m.group(2), re.S)
        if it:  # `Iterator<Item = &Foo>`
            return type_name(it.group(1).lstrip("&").strip())
        if m.group(1) in _SEQUENCES:
            first = m.group(2)
            depth = 0
            for k, ch in enumerate(first):
                depth += ch in "<([{"
                depth -= ch in ">)]}"
                if ch == "," and depth == 0:
                    first = first[:k]
                    break
            return type_name(first.lstrip("&").strip())
    return ""


_TS_COMPUTED = {"ReturnType", "Parameters", "ConstructorParameters", "Pick", "Omit", "Exclude", "Extract",
                "ThisParameterType", "OmitThisParameter", "ValueOf"}


def generic_name(text: str) -> str:
    """type_name, plus the generic arity when the type itself is generic
    (`Builder<TResult>` -> "Builder`1"): C# has `Builder` and `Builder<T>` side
    by side. Lookups fall back to the plain name, so other languages lose nothing."""
    t = type_name(text)
    m = re.match(r"^\s*(?:const\s+|readonly\s+)?([\w.:]+)\s*<(.*)>\s*[?*&]*\s*$", text.strip(), re.S)
    if not t or not m or m.group(1).replace("::", ".").split(".")[-1] != t.split(".")[-1]:
        return t
    depth, n = 0, 1
    for ch in m.group(2):
        depth += ch in "<([{"
        depth -= ch in ">)]}"
        n += ch == "," and depth == 0
    return f"{t}`{n}"


def type_name(text: str) -> str:
    """The class a declared type names: `Promise<Foo>` -> Foo, `Foo?` -> Foo,
    `&mut Foo<'a>` -> Foo, `Optional[Foo]` -> Foo, `Foo | None` -> Foo,
    `pkg.Foo` -> pkg.Foo, `List<Foo>` -> List (a collection is not its
    elements). "" when it isn't one class (a tuple, a function type, void)."""
    t = text.strip().strip("\"'")
    for _ in range(6):
        prev = None
        while prev != t:
            prev = t
            t = re.sub(r"^(?:const|mut|readonly|final|static|volatile|unowned|weak|inout|ref|out|in|dyn|impl|some|any)\s+", "", t)
            t = t.lstrip("&*^").strip()
            t = re.sub(r"^'\w+\s+", "", t)  # a Rust lifetime
        inter = [p.strip() for p in re.split(r"\s&\s(?![^<\[(]*[>\])])", t) if p.strip()]
        if len(inter) > 1:  # TS `Response & TypedResponse<T>`: a Response (with more)
            t = inter[0]
        # `Foo | None`, `Foo | null | undefined`
        parts = [p.strip() for p in re.split(r"\|(?![^<\[(]*[>\])])", t) if p.strip()]
        parts = [p for p in parts if p not in ("None", "null", "undefined", "nil", "void")]
        if len(parts) > 1:  # `Response | Promise<Response>`: one type, sync or not
            each = {type_name(p) for p in parts}
            return each.pop() if len(each) == 1 else ""
        if len(parts) != 1:
            return ""
        t = parts[0]
        if re.match(r"^\[.*\][?!]?$", t, re.S):
            return "Array"  # Swift's `[Element]` / `[Key: Value]`: the language's collection
        if re.search(r"\[\s*\]\s*[?!]?$", t):
            return "Array"  # `Foo[]`: an array (the language's), not a Foo
        m = re.match(r"^([\w.:$\\]+)\s*[<\[](.*)[>\]]\s*[?!*&]*$", t, re.S)
        if m:
            head = m.group(1).split("::")[-1].split(".")[-1]
            if head in _TS_COMPUTED:
                return ""  # `ReturnType<typeof f>`, `Pick<T, K>`: a type computed from another, no class
            if head == "InstanceType":  # `InstanceType<typeof App>`: an App
                t = re.sub(r"^\s*typeof\s+", "", m.group(2)).strip()
                continue
            if head in _WRAPPERS:
                inner = m.group(2)
                depth, cut = 0, len(inner)
                for k, ch in enumerate(inner):
                    if ch in "<[(":
                        depth += 1
                    elif ch in ">])":
                        depth -= 1
                    elif ch == "," and depth == 0:
                        cut = k
                        break
                t = inner[:cut]
                continue
            t = m.group(1)
        break
    t = t.rstrip("?!*&").strip()
    t = t.replace("::", ".").replace("\\", ".").lstrip(".")
    if not re.match(r"^[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*$", t):
        return ""
    if t in ("void", "Unit", "None", "Nothing", "never", "any", "unknown", "object", "Any", "AnyObject", "auto",
             "var", "dynamic", "Self", "self", "this", "static", "mixed", "int", "bool", "string", "float", "double",
             "char", "long", "short", "byte", "usize", "isize", "u8", "u16", "u32", "u64", "i8", "i16", "i32",
             "i64", "f32", "f64", "str", "number", "boolean", "symbol", "bigint", "error", "rune", "uint"):
        return "Self" if t in ("Self", "self", "this", "static") else ""
    return t
