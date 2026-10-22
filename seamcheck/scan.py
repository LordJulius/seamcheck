"""Blank comments and string contents out of source text, keeping every
newline and column in place, so regexes only ever match real code.
Also returns the string literals (line, text) for "method-name" references.
"""
from __future__ import annotations

import re as _re

# Elixir sigil delimiters: opener -> closer
_SIGIL = {"(": ")", "[": "]", "{": "}", "<": ">", "/": "/", "|": "|", '"': '"', "'": "'"}


def blank(text: str, lang: str, keep_strings: bool = False) -> tuple[str, list[tuple[int, str]]]:
    out = list(text)
    strings: list[tuple[int, str]] = []
    i, n, line = 0, len(text), 1

    def wipe(a: int, b: int) -> None:
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    cfam = lang in ("c", "rust", "kotlin", "js", "swift", "php")
    while i < n:
        c = text[i]
        if c == "\n":
            line += 1
            i += 1
            continue
        if cfam and c == "/" and i + 1 < n and text[i + 1] in "/*":
            if text[i + 1] == "/":
                j = text.find("\n", i)
                j = n if j < 0 else j
            else:
                j = text.find("*/", i + 2)
                j = n if j < 0 else j + 2
            wipe(i, j)
            line += text.count("\n", i, j)
            i = j
            continue
        if lang == "rust" and c == "r" and text[i + 1:i + 2] in ('"', "#") and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            k = i + 1
            while k < n and text[k] == "#":
                k += 1
            if k < n and text[k] == '"':
                close = '"' + "#" * (k - i - 1)
                j = text.find(close, k + 1)
                j = n if j < 0 else j
                strings.append((line, text[k + 1:j]))
                if not keep_strings:
                    wipe(k + 1, j)
                line += text.count("\n", i, j)
                i = min(n, j + len(close))
                continue
        if lang == "rust" and c == "'" and not (i + 2 < n and text[i + 2] == "'") and not (i + 3 < n and text[i + 1] == "\\"):
            i += 1  # a lifetime ('a), not a char literal
            continue
        if lang == "php" and (c == "#" and text[i + 1:i + 2] != "[" or c == "?" and text[i + 1:i + 2] == ">"):
            if c == "?":  # `?>`: HTML until the next `<?php`
                j = text.find("<?php", i)
                j = n if j < 0 else j + 5
                wipe(i, j)
                line += text.count("\n", i, j)
                i = j
                continue
            j = text.find("\n", i)  # a `#` comment (`#[Attr]` is code)
            j = n if j < 0 else j
            wipe(i, j)
            i = j
            continue
        if lang == "php" and text.startswith("<<<", i):  # heredoc / nowdoc
            hm = _HEREDOC.match(text, i)
            if hm:
                end = _re.compile(r"^[ \t]*" + hm.group(1) + r"\b", _re.M).search(text, hm.end())
                j = n if end is None else end.start()
                strings.append((line, text[hm.end():j]))
                if not keep_strings:
                    wipe(hm.end(), j)
                line += text.count("\n", i, j)
                i = j if end is None else end.end()
                continue
        if c == "#" and not cfam:
            j = text.find("\n", i)
            j = n if j < 0 else j
            wipe(i, j)
            i = j
            continue
        if lang == "elixir" and c == "?" and i + 1 < n and not (text[i - 1].isalnum() if i else False):
            i += 2  # ?# ?" char literals
            continue
        if lang == "elixir" and c == "~" and i + 2 < n and text[i + 1].isalpha() and text[i + 2] in _SIGIL:
            op = text[i + 2]
            cl = _SIGIL[op]
            if text[i + 2:i + 5] in ('"""', "'''"):
                end = text.find(text[i + 2:i + 5], i + 5)
                end = n if end < 0 else end + 3
            else:
                j, depth = i + 3, 1
                while j < n:
                    if text[j] == "\\":
                        j += 2
                        continue
                    if text[j] == cl and (op == cl or depth == 1):
                        break
                    if op != cl and text[j] == op:
                        depth += 1
                    elif op != cl and text[j] == cl:
                        depth -= 1
                    j += 1
                end = min(n, j + 1)
            body = text[i + 3:end - 1]
            strings.append((line, body))
            if not keep_strings:
                wipe(i + 3, end - 1)
            line += text.count("\n", i, end)
            i = end
            continue
        if lang == "c" and c == "'" and 0 < i < n - 1 and text[i - 1].isalnum() and text[i + 1].isalnum():
            i += 1  # a C++14 digit separator (25'000), not a char literal
            continue
        if lang == "js" and c == "/" and _regex_start(text, i):
            # a regex literal (`/^(?:W\/)?"[^"]+"$/`): its quotes open no string
            j, in_class = i + 1, False
            while j < n and text[j] != "\n":
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == "[":
                    in_class = True
                elif text[j] == "]":
                    in_class = False
                elif text[j] == "/" and not in_class:
                    break
                j += 1
            if j < n and text[j] == "/":
                if not keep_strings:
                    wipe(i + 1, j)
                i = j + 1
                continue
        if c in "\"'" or (lang == "js" and c == "`"):
            triple = text[i:i + 3] == c * 3
            q = c * 3 if triple else c
            j = i + len(q)
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text.startswith(q, j):
                    break
                if not triple and text[j] == "\n" and lang in ("gdscript", "python", "js", "kotlin", "swift") and c != "`":
                    break  # unterminated (or not a string at all): stop at end of line
                j += 1
            end = min(n, j + len(q))
            strings.append((line, text[i + len(q):j]))
            if not keep_strings:
                wipe(i + len(q), j)
            line += text.count("\n", i, end)
            i = end
            continue
        i += 1
    return "".join(out), strings


_HEREDOC = _re.compile(r"<<<[ \t]*[\"']?([A-Za-z_]\w*)[\"']?[ \t]*\n")
_REGEX_AFTER = set("(,=:[!&|?{};+-*%<>~^")
_REGEX_WORDS = ("return", "typeof", "case", "in", "of", "delete", "void", "throw", "new", "yield", "await", "else", "do")


def _regex_start(text: str, i: int) -> bool:
    """A `/` that starts a JS regex literal, not a division: what comes before
    it is an operator, an opening bracket, a keyword or nothing."""
    if text[i + 1:i + 2] in ("/", "*", ""):
        return False
    k = i - 1
    while k >= 0 and text[k] in " \t\r\n":
        k -= 1
    if k < 0 or text[k] in _REGEX_AFTER:
        return True
    w = k
    while w >= 0 and (text[w].isalnum() or text[w] in "_$"):
        w -= 1
    return text[w + 1:k + 1] in _REGEX_WORDS
