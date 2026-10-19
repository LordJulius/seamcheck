"""Blank comments and string contents out of source text, keeping every
newline and column in place, so regexes only ever match real code.
Also returns the string literals (line, text) for "method-name" references.
"""
from __future__ import annotations

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

    cfam = lang in ("c", "rust", "kotlin", "js")
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
                if not triple and text[j] == "\n" and lang in ("gdscript", "python") and c != "`":
                    break  # unterminated: stop at end of line
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
