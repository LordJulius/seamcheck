"""Lambda parameters: what type a callback's parameter has, read from the
function it is passed to.

`services.AddPipeline("k", builder => builder.AddRetry())`: AddPipeline's
parameter at that position is `Action<PipelineBuilder>`, so `builder` is a
PipelineBuilder. The parser records, per function, each overload's parameter
types (`Func.ptypes`) and, per call, which lambda parameter names sit in which
argument (`lam:` bindings); infer.Types joins the two.
"""
from __future__ import annotations

import re

OV, PARAM, TARGS = "\x1e", "\x1f", "\x1d"  # Func.ptypes: overloads ("<type-parameter count>\x1d<params>"), then parameters


def split_top(text: str, sep: str = ",") -> list[str]:
    """`text` split at `sep` outside brackets (an arrow's `>` is no bracket)."""
    out, d, cur, i = [], 0, [], 0
    while i < len(text):
        ch = text[i]
        if ch in "=-" and text[i + 1:i + 2] == ">":  # `=>` / `->`
            cur.append(text[i:i + 2])
            i += 2
            continue
        if ch in "<([{":
            d += 1
        elif ch in ">)]}":
            d = max(0, d - 1)
        if ch == sep and d == 0:
            out.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
        i += 1
    out.append("".join(cur))
    return out if any(s.strip() for s in out) else []


def matching(code: str, i: int) -> int:
    """code[i] is an opening bracket: the index of its partner, or -1."""
    pairs = {"(": ")", "[": "]", "{": "}"}
    stack = []
    for k in range(i, min(len(code), i + 20000)):
        ch = code[k]
        if ch in pairs:
            stack.append(pairs[ch])
        elif ch in ")]}":
            if not stack or stack.pop() != ch:
                return -1
            if not stack:
                return k
    return -1


def _cut_default(p: str) -> str:
    parts = split_top(p, "=")  # (`=>` stays whole: an arrow type is no default)
    return parts[0] if parts else p


def param_types(tk: str, inner: str) -> list[str]:
    """The declared parameter types of one signature's parameter list."""
    out = []
    for p in split_top(inner):
        p = p.strip()
        if not p:
            continue
        if tk == "jvm":  # Java / C#: `[Attr] this Foo<T> name = default`, `final @Ann Foo name`
            p = re.sub(r"^(?:\s*\[[^\]]*\])+", "", p)
            p = re.sub(r"@\w+(?:\([^)]*\))?\s*", "", p)
            p = _cut_default(p).strip()
            m = re.match(r"^(?:(?:this|params|ref|out|in|final|scoped|readonly)\s+)*(.+?)\s*\b([A-Za-z_]\w*)$", p, re.S)
            out.append(m.group(1).strip() if m else "")
        elif tk == "rust":  # `name: Type`; a method's `self` takes no argument
            if re.match(r"^(?:&\s*(?:'\w+\s+)?)?(?:mut\s+)?self\b", p):
                continue
            parts = split_top(p, ":")
            out.append(":".join(parts[1:]).strip() if len(parts) > 1 else "")
        elif tk in ("kotlin", "js", "swift"):  # `name: Type = default` (TS `name?: Type`, Swift `label name: Type`)
            parts = split_top(p, ":")
            out.append(_cut_default(":".join(parts[1:])).strip() if len(parts) > 1 else "")
        else:
            out.append("")
    return out


# Java's functional interfaces: how many leading type arguments are parameters
_JAVA_FN = {"Consumer": 1, "Predicate": 1, "Function": 1, "UnaryOperator": 1, "ToIntFunction": 1,
            "ToLongFunction": 1, "ToDoubleFunction": 1, "BiConsumer": 2, "BiFunction": 2, "BiPredicate": 2,
            "BinaryOperator": 2, "ObjIntConsumer": 1, "IntFunction": 0, "Supplier": 0, "Callable": 0, "Runnable": 0}


def lambda_params(ty: str) -> list[str]:
    """A function type's parameter types: C# `Action<A, B>` / `Func<A, R>`,
    Java `Consumer<A>` / `BiFunction<A, B, R>`, Kotlin / Swift `(A, B) -> R`
    (and `A.() -> R`, whose receiver is no parameter), TS `(a: A) => R`."""
    t = ty.strip()
    t = re.sub(r"^(?:@escaping\s+|@Sendable\s+|suspend\s+|noinline\s+|crossinline\s+)+", "", t)
    while t.endswith("?") or (t.startswith("(") and matching(t, 0) == len(t) - 1 and ("->" in t or "=>" in t)):
        t = t[:-1].strip() if t.endswith("?") else t[1:-1].strip()  # `((A) -> R)?`: the function type inside
    m = re.match(r"^(?:&\s*(?:'\w+\s+)?(?:mut\s+)?)?(?:Box\s*<\s*)?(?:impl|dyn)?\s*(?:Fn|FnMut|FnOnce)\s*\((.*?)\)(?:\s*->.*)?\s*>?"
                 r"(?:\s*\+\s*[\w']+)*$", t, re.S)  # Rust `impl FnMut(Match) -> bool`, `&mut dyn Fn(&Path)`
    if m:
        return [a.strip() for a in split_top(m.group(1))]
    m = re.match(r"^(?:[\w.]+\.)?Expression\s*<(.*)>$", t, re.S)  # C# `Expression<Func<A, bool>>`
    if m:
        t = m.group(1).strip()
    m = re.match(r"^(?:System\.)?(Action|Func)\s*<(.*)>$", t, re.S)
    if m:
        args = [a.strip() for a in split_top(m.group(2))]
        return args if m.group(1) == "Action" else args[:-1]
    m = re.match(r"^(?:java\.util\.function\.)?(\w+)\s*<(.*)>$", t, re.S)
    if m and m.group(1) in _JAVA_FN:
        args = [re.sub(r"^\?\s*(?:super|extends)\s+", "", a.strip()) for a in split_top(m.group(2))]
        n = _JAVA_FN[m.group(1)]
        return args[:n] if n else []
    if t.startswith("("):  # `(A, B) -> R`, `(a: A) => R`
        k = matching(t, 0)
        if k > 0 and re.match(r"\s*[-=]>", t[k + 1:]):
            ps = []
            for p in split_top(t[1:k]):
                parts = split_top(p, ":")
                ps.append((":".join(parts[1:]) if len(parts) > 1 else p).strip().rstrip("?").strip())
            return ps
    return []


# a lambda argument's parameter names
_ARROW_ONE = re.compile(r"^(?:async\s+|static\s+)*([A-Za-z_$][\w$]*)\s*(?:=>|->)")
_ARROW_MANY = re.compile(r"^(?:async\s+|static\s+)*\(([^()]*)\)\s*(?::\s*[^=]+)?(?:=>|->)")
_JS_FUNCTION = re.compile(r"^(?:async\s+)?function\b\s*\*?\s*[\w$]*\s*\(([^()]*)\)")
_RS_CLOSURE = re.compile(r"^(?:move\s+)?\|([^|]*)\|")
_BRACE_LAMBDA = re.compile(r"^\{\s*(?:\(([^()]*)\)|([A-Za-z_]\w*(?:\s*,\s*[A-Za-z_]\w*)*))\s*(->|\bin\b)")


def _names(params: str, typed_ok: bool = False) -> list[str]:
    out = []
    for p in split_top(params):
        p = p.strip()
        m = re.match(r"^([A-Za-z_$][\w$]*)\s*$", p)
        if m:
            out.append(m.group(1))
        elif typed_ok and re.match(r"^[A-Za-z_$][\w$]*\s*[:?]", p):
            out.append("")  # typed in place (`(a: A) =>`): the parser's declared types have it
        else:
            out.append("")  # destructuring, a typed C# / Java parameter...
    return out


def lambda_args(fam: str, args: list[str]) -> list[tuple[int, list[str]]]:
    """For each argument that is a lambda: (its index, its parameter names, ""
    for one this can't bind)."""
    out = []
    if fam == "rust":  # `|a, b| ...` was split at its comma: put each closure back together
        merged, k = [], 0
        while k < len(args):
            a = args[k]
            while re.match(r"^\s*(?:move\s+)?\|[^|]*$", a) and k + 1 < len(args):
                k += 1
                a += "," + args[k]
            merged.append(a)
            k += 1
        args = merged
    for i, a in enumerate(args):
        a = a.strip()
        m = _ARROW_ONE.match(a)
        if m and m.group(1) not in ("async", "static"):
            out.append((i, [m.group(1)]))
            continue
        m = _ARROW_MANY.match(a)
        if m:
            out.append((i, _names(m.group(1), typed_ok=True)))
            continue
        if fam == "js":
            m = _JS_FUNCTION.match(a)
            if m:
                out.append((i, _names(m.group(1), typed_ok=True)))
                continue
        if fam in ("kotlin", "swift") and a.startswith("{"):
            out.append((i, brace_lambda(fam, a)))
        if fam == "rust":
            m = _RS_CLOSURE.match(a)
            if m:
                out.append((i, [re.sub(r"^(?:&\s*)?(?:mut\s+)?", "", n.strip()) if re.match(r"^(?:&\s*)?(?:mut\s+)?[a-z_]\w*$", n.strip())
                                else "" for n in split_top(m.group(1))] if m.group(1).strip() else []))
    return out


def brace_lambda(fam: str, text: str) -> list[str]:
    """A Kotlin / Swift `{ a, b -> ... }` / `{ a, b in ... }` lambda's
    parameter names; Kotlin's `{ ... }` has one, `it`."""
    m = _BRACE_LAMBDA.match(text)
    if m and ((m.group(3) == "->") == (fam == "kotlin")):
        return _names(m.group(1) or m.group(2))
    return ["it"] if fam == "kotlin" else []


def generic_param(ty: str) -> bool:
    """`T`, `TResult`, `K`: a type parameter, not a class."""
    return bool(re.match(r"^(?:[A-Z]|T[A-Z]\w*)$", ty.strip().rstrip("?")))
