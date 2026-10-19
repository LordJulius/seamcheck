"""Nesting, not lines: which element or block a position sits in.

Two trees, both over a file's text with positions kept:

- `jsx_tree`: JSX/TSX elements and `{...}` expression containers. A control
  inside `{busy && <button/>}` has an expression ancestor (it renders only
  sometimes); one inside `<div className="v-menu">` has a menu ancestor.
- `brace_tree`: `{ }` blocks and `( )` groups with the text that opens them
  (Kotlin/Compose, QML, C-family). `if (x) {`, `DropdownMenu(...) {` and
  `Column {` are all blocks; the header says which.

Both are heuristics over source, not parsers, and say so by being forgiving:
an unbalanced file gives a shallower tree, never an exception.
"""
from __future__ import annotations

import bisect
import re
import sys
from dataclasses import dataclass, field

sys.setrecursionlimit(max(sys.getrecursionlimit(), 10000))


@dataclass
class Node:
    kind: str  # "el" (JSX element) | "expr" (JSX {...}) | "{" | "("
    start: int
    end: int = -1  # exclusive
    name: str = ""  # JSX tag name ("" = fragment); brace header (the text that opens it)
    head_end: int = -1  # JSX: just past the opening tag's ">"
    hstart: int = -1  # braces: where the header (`name`) starts
    parent: int = -1
    children: list[int] = field(default_factory=list)


def ancestors(nodes: list[Node], i: int) -> list[int]:
    out = []
    p = nodes[i].parent
    while p >= 0:
        out.append(p)
        p = nodes[p].parent
    return out


class Tree(list):
    """Nodes in document (pre-)order, with their starts for bisecting."""
    _starts: list[int] | None = None

    def starts(self) -> list[int]:
        if self._starts is None or len(self._starts) != len(self):
            self._starts = [n.start for n in self]
        return self._starts


def innermost(nodes: list[Node], pos: int, kinds: tuple[str, ...]) -> int:
    """The deepest node of one of `kinds` containing `pos` (-1: none). Nodes
    are in pre-order, so it is an ancestor-or-self of the last node that
    starts at or before pos."""
    starts = nodes.starts() if isinstance(nodes, Tree) else [n.start for n in nodes]
    k = bisect.bisect_right(starts, pos) - 1
    while k >= 0:
        n = nodes[k]
        if n.kind in kinds and n.start <= pos < n.end:
            return k
        k = n.parent
    return -1


# --- JSX ----------------------------------------------------------------------------

_TAG_START = re.compile(r"<([A-Za-z_$][\w$.:-]*|>)")
_JS_BEFORE_TAG = set("(,=:?&|!{[;>}") | {""}
_REGEX_BEFORE = set("(,=:[!&|?{};")


class _Jsx:
    def __init__(self, text: str):
        self.t = text
        self.n = len(text)
        self.nodes: Tree = Tree()

    def add(self, node: Node, parent: int) -> int:
        node.parent = parent
        self.nodes.append(node)
        k = len(self.nodes) - 1
        if parent >= 0:
            self.nodes[parent].children.append(k)
        return k

    def prev_sig(self, i: int) -> str:
        j = i - 1
        while j >= 0 and self.t[j] in " \t\r\n":
            j -= 1
        if j < 0:
            return ""
        if self.t[j].isalpha():
            k = j
            while k >= 0 and (self.t[k].isalnum() or self.t[k] == "_"):
                k -= 1
            word = self.t[k + 1:j + 1]
            return "kw" if word in ("return", "yield", "default", "case", "await") else "id"
        return self.t[j]

    def string(self, i: int) -> int:
        q = self.t[i]
        j = i + 1
        while j < self.n:
            c = self.t[j]
            if c == "\\":
                j += 2
                continue
            if c == q:
                return j + 1
            if c == "\n" and q != "`":
                return j
            if q == "`" and c == "$" and self.t[j + 1:j + 2] == "{":
                j = self.js(j + 2, "}", -1) + 1
                continue
            j += 1
        return self.n

    def js(self, i: int, stop: str, parent: int) -> int:
        """Scan JS from i to the unmatched `stop`; returns its index (or n)."""
        t, n = self.t, self.n
        closers = {"(": ")", "[": "]", "{": "}"}
        want: list[str] = []
        while i < n:
            c = t[i]
            if c in "\"'`":
                i = self.string(i)
                continue
            if c == "/" and t[i + 1:i + 2] == "/":
                j = t.find("\n", i)
                i = n if j < 0 else j
                continue
            if c == "/" and t[i + 1:i + 2] == "*":
                j = t.find("*/", i + 2)
                i = n if j < 0 else j + 2
                continue
            if c == "/" and self.prev_sig(i) in _REGEX_BEFORE:
                j, cls = i + 1, False
                while j < n and t[j] != "\n":
                    if t[j] == "\\":
                        j += 2
                        continue
                    if t[j] == "[":
                        cls = True
                    elif t[j] == "]":
                        cls = False
                    elif t[j] == "/" and not cls:
                        break
                    j += 1
                i = j + 1
                continue
            if c == "<" and (m := _TAG_START.match(t, i)) and self.prev_sig(i) in (_JS_BEFORE_TAG | {"kw"}):
                i = self.element(i, parent)
                continue
            if c in closers:
                want.append(closers[c])
            elif want and c == want[-1]:
                want.pop()
            elif not want and c == stop:
                return i
            i += 1
        return n

    def element(self, i: int, parent: int) -> int:
        """At '<' of an element or fragment; returns the index just past it."""
        t, n = self.t, self.n
        m = _TAG_START.match(t, i)
        name = "" if m.group(1) == ">" else m.group(1)
        k = self.add(Node("el", i, name=name), parent)
        j = i + 1 + len(name)
        if name:
            while j < n:
                c = t[j]
                if c in "\"'":
                    j = self.string(j)
                    continue
                if c == "{":
                    j = self.js(j + 1, "}", k) + 1
                    continue
                if c == "/" and t[j + 1:j + 2] == ">":
                    self.nodes[k].head_end = self.nodes[k].end = j + 2
                    return j + 2
                if c == ">":
                    break
                j += 1
        self.nodes[k].head_end = j + 1
        j += 1
        while j < n:  # children
            c = t[j]
            if c == "<" and t[j + 1:j + 2] == "/":
                e = t.find(">", j)
                e = n if e < 0 else e + 1
                self.nodes[k].end = e
                return e
            if c == "<" and _TAG_START.match(t, j):
                j = self.element(j, k)
                continue
            if c == "{":
                x = self.add(Node("expr", j), k)
                e = self.js(j + 1, "}", x)
                self.nodes[x].end = e + 1
                j = e + 1
                continue
            j += 1
        self.nodes[k].end = n
        return n


def jsx_tree(text: str) -> list[Node]:
    """JSX elements and expression containers of a JS/TS file."""
    s = _Jsx(text)
    i = 0
    while i < s.n:
        e = s.js(i, "\0", -1)  # top level: never stops early
        i = max(e + 1, i + 1)
    return s.nodes


def jsx_attrs(text: str, node: Node) -> str:
    """The opening tag's text (`<button type="button" onClick={...}>`)."""
    return text[node.start:node.head_end]


# --- braces -------------------------------------------------------------------------

_BRACKETS = re.compile(r"[{}();]")


def brace_tree(code: str) -> Tree:
    """`{}` blocks and `()` groups of comment- and string-blanked code, each
    with its header: the text before it back to the previous `;`, `{` or `}`
    of the same block (`if (x)`, `} else`, `DropdownMenu(expanded = open)`).
    One forward pass over the brackets only."""
    nodes: Tree = Tree()
    stack: list[int] = []
    bound = [0]  # per open `{` (and the file): where the current statement began
    for m in _BRACKETS.finditer(code):
        i, c = m.start(), m.group()
        if c == ";":
            if not stack or nodes[stack[-1]].kind == "{":
                bound[-1] = i + 1
            continue
        if c in "{(":
            j = bound[-1]
            if stack and nodes[stack[-1]].kind == "(":
                j = max(j, nodes[stack[-1]].start + 1)
                # inside a group, a header starts after the last `,` at this level
                k = code.rfind(",", j, i)
                j = k + 1 if k >= 0 else j
            node = Node(c, i, name=code[j:i].strip(), parent=stack[-1] if stack else -1, hstart=j)
            nodes.append(node)
            if stack:
                nodes[stack[-1]].children.append(len(nodes) - 1)
            stack.append(len(nodes) - 1)
            if c == "{":
                bound.append(i + 1)
            continue
        if not stack:
            continue
        want = "{" if c == "}" else "("
        # tolerate a stray closer: pop to the nearest matching opener
        for back in range(len(stack) - 1, -1, -1):
            if nodes[stack[back]].kind == want:
                for k in stack[back:]:
                    nodes[k].end = i + 1
                    if nodes[k].kind == "{":
                        bound.pop()
                del stack[back:]
                if c == "}":
                    bound[-1] = i + 1
                break
    for k in stack:
        nodes[k].end = len(code)
    return nodes


def strip_groups(header: str) -> str:
    """A header without its parenthesised groups: `Box(M.background(if (a) x else y))`
    -> `Box()`, so an argument's `if` doesn't read as a branch."""
    out, depth = [], 0
    for c in header:
        if c == "(":
            if depth == 0:
                out.append("(")
            depth += 1
        elif c == ")":
            depth = max(0, depth - 1)
            if depth == 0:
                out.append(")")
        elif depth == 0:
            out.append(c)
    return "".join(out)


def head_line(header: str) -> str:
    """The statement that opens a block: its header without argument groups,
    last line only (`Spacer()\\n  Box()` -> `Box()`; `} else` -> `else`)."""
    return strip_groups(header).rstrip().rsplit("\n", 1)[-1].strip()


def own_text(code: str, nodes: list[Node], k: int) -> str:
    """A block's body without its nested blocks (QML: the object's own properties)."""
    n = nodes[k]
    parts, pos = [], n.start + 1
    for c in n.children:
        ch = nodes[c]
        if ch.kind != "{":
            continue
        parts.append(code[pos:ch.start])
        pos = ch.end
    parts.append(code[pos:n.end - 1])
    return "".join(parts)
