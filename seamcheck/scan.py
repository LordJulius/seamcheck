"""Blank comments and string contents out of source text, keeping every
newline and column in place, so regexes only ever match real code.
Also returns the string literals (line, text) for "method-name" references.
"""
from __future__ import annotations

import bisect
import re

# Elixir sigil delimiters: opener -> closer
_SIGIL = {"(": ")", "[": "]", "{": "}", "<": ">", "/": "/", "|": "|", '"': '"', "'": "'"}


def blank(text: str, lang: str, keep_strings: bool = False) -> tuple[str, list[tuple[int, str]]]:
    if lang == "ruby":
        return _blank_ruby(text, keep_strings)
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


# --- Ruby ---------------------------------------------------------------------
# Ruby's literals nest code (`"#{a("b")}"`), and a heredoc body, a %w() list,
# a regex or an `=begin` block can hold `end`, `def` or a quote: any of them
# left in the code would shift every block after it. Interpolated code stays
# (its calls are real calls); the literal text around it is blanked.

_RB_HEREDOC = re.compile(r"<<([~-]?)(['\"`]?)([A-Za-z_]\w*)\2")
_RB_PAIR = {"(": ")", "[": "]", "{": "}", "<": ">"}
# a word after which `/` or `%` starts a literal, not a division / modulo
_RB_VALUE_KW = {"if", "unless", "while", "until", "and", "or", "not", "return", "when", "in", "then", "else",
                "elsif", "do", "case", "yield", "puts", "print", "p", "raise", "next", "break"}
_RB_OPS = set("([{,=;!&|?:<>+-*/%~^")


def _blank_ruby(text: str, keep_strings: bool) -> tuple[str, list[tuple[int, str]]]:
    out = list(text)
    strings: list[tuple[int, str]] = []
    n = len(text)
    nls = [k for k, ch in enumerate(text) if ch == "\n"]
    pending: list[tuple[str, bool, str]] = []  # heredocs opened on the current line

    def line_of(k: int) -> int:
        return bisect.bisect_left(nls, k) + 1

    def wipe(a: int, b: int, literal: bool = True) -> None:
        if literal and keep_strings:
            return
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    def is_value(k: int) -> bool:
        """text[k] (a `/`, `%` or `<<`) starts a literal: it stands where a
        value goes (after an operator, `(`, a keyword, or `foo /re/`)."""
        j = k - 1
        while j >= 0 and text[j] in " \t":
            j -= 1
        if j < 0 or text[j] == "\n":
            return True
        if text[j] in _RB_OPS:
            return True
        if text[j].isalnum() or text[j] in "_?!":
            s = j
            while s > 0 and (text[s - 1].isalnum() or text[s - 1] == "_"):
                s -= 1
            if text[s:j + 1] in _RB_VALUE_KW and (s == 0 or text[s - 1] not in ".@$:"):
                return True
            # `split /,/`, `puts %w(a)`: a method's argument (`a / b`, `a /= 2` divide)
            return j < k - 1 and text[k + 1:k + 2] not in (" ", "\t", "=", "\n", "")
        return False

    def literal(k: int, close: str, open_: str, interp: bool, regex: bool = False) -> int:
        """Scan a literal whose text starts at k; return the index after its close."""
        start, seg, depth, cls = k, k, 0, 0
        while k < n:
            ch = text[k]
            if ch == "\\":
                k += 2
                continue
            if regex and ch == "[":
                cls += 1
            elif regex and ch == "]" and cls:
                cls -= 1
            elif interp and ch == "#" and text[k + 1:k + 2] == "{":
                wipe(seg, k)
                k = code(k + 2, True)
                seg = k + 1
            elif ch == close and not cls and depth == 0:
                break
            elif open_ != close and ch == open_:
                depth += 1
            elif open_ != close and ch == close:
                depth -= 1
            k += 1
        k = min(k, n)
        wipe(seg, k)
        strings.append((line_of(start), text[start:k]))
        return k + 1

    def heredocs(k: int) -> int:
        """text[k] is the newline ending a line that opened heredocs: blank
        their bodies (and terminators); return the newline after the last."""
        pos = k + 1
        for flag, interp, ident in list(pending):
            j = pos
            while j < n:
                e = text.find("\n", j)
                e = n if e < 0 else e
                ln = text[j:e].rstrip("\r")
                if (ln.strip() if flag else ln) == ident:
                    break
                j = e + 1
            body_end = min(j, n)
            seg, q = pos, pos
            while interp and q < body_end:
                if text[q] == "\\":
                    q += 2
                    continue
                if text[q] == "#" and text[q + 1:q + 2] == "{":
                    wipe(seg, q)
                    q = code(q + 2, True)
                    seg = q + 1
                q += 1
            wipe(seg, body_end)
            strings.append((line_of(pos), text[pos:body_end]))
            e = text.find("\n", body_end)
            e = n if e < 0 else e
            wipe(body_end, e, literal=False)  # the terminator: not a constant
            pos = e + 1
        pending.clear()
        return min(pos - 1, n)

    def code(k: int, interp: bool) -> int:
        """Scan code from k; inside `#{...}` return the index of its `}`."""
        depth = 0
        while k < n:
            ch = text[k]
            bol = k == 0 or text[k - 1] == "\n"
            if ch == "\n":
                if pending and not interp:
                    k = heredocs(k)
                k += 1
                continue
            if interp and ch == "{":
                depth += 1
            elif interp and ch == "}":
                if depth == 0:
                    return k
                depth -= 1
            elif ch == "#":
                e = text.find("\n", k)
                e = n if e < 0 else e
                wipe(k, e, literal=False)
                k = e
                continue
            elif bol and text.startswith("=begin", k) and text[k + 6:k + 7] in (" ", "\t", "\r", "\n", ""):
                e = text.find("\n=end", k)
                e = n if e < 0 else text.find("\n", e + 1)
                e = n if e < 0 else e
                wipe(k, e, literal=False)
                k = e
                continue
            elif bol and text.startswith("__END__", k) and text[k + 7:k + 8] in ("\n", "\r", ""):
                wipe(k, n, literal=False)  # what follows is data
                return n
            elif ch in "\"`":
                k = literal(k + 1, ch, ch, True)
                continue
            elif ch == "'":
                k = literal(k + 1, ch, ch, False)
                continue
            elif ch == "?" and k + 1 < n and text[k + 1] not in " \t\n" and \
                    (k == 0 or not (text[k - 1].isalnum() or text[k - 1] in "_)]}")):
                if text[k + 1] == "\\":
                    k += 3  # a character literal: ?\\ ?\n
                    continue
                if not (text[k + 2:k + 3].isalnum() or text[k + 2:k + 3] == "_"):
                    k += 2  # ?" ?# ?/
                    continue
            elif ch == "$" and k + 1 < n and text[k + 1] in "\"'`/\\;,.<>!@&~=*$?:":
                k += 2  # $" $' $/ special globals
                continue
            elif ch == "<" and text.startswith("<<", k) and (m := _RB_HEREDOC.match(text, k)) and \
                    (m.group(1) or m.group(2) or (m.group(3)[0].isupper() and is_value(k))):
                pending.append((m.group(1), m.group(2) != "'", m.group(3)))
                k = m.end()
                continue
            elif ch == "%" and is_value(k):
                kind = text[k + 1:k + 2]
                typed = kind != "" and kind in "qQwWiIrsx" and text[k + 2:k + 3] not in ("", " ", "\t", "\n") \
                    and not text[k + 2].isalnum()
                d = k + 2 if typed else k + 1
                op = text[d:d + 1]
                if typed or (op and op in "([{<|!/^"):
                    lit = kind if typed else ""
                    k = literal(d + 1, _RB_PAIR.get(op, op), op, lit not in ("q", "w", "i", "s"), regex=(lit == "r"))
                    continue
            elif ch == "/" and is_value(k):
                k = literal(k + 1, "/", "/", True, regex=True)
                while k < n and text[k] in "imxounse":
                    k += 1
                continue
            elif ch.isalnum() or ch == "_":
                while k < n and (text[k].isalnum() or text[k] == "_"):
                    k += 1
                continue
            k += 1
        return n

    code(0, False)
    return "".join(out), strings
