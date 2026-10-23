"""Architecture rules over the file dependency graph: what ArchUnit checks in
Java and dependency-cruiser in JS / TS, for every language seamcheck reads.

A file depends on another when it imports it or (exactly) calls into it
(`via = "import" | "call" | "any"`, default "any").

  [[rule]]
  id = "ui-not-db"
  kind = "depend"                 # nothing in `from` may depend on `to`
  from = ["src/ui/**"]
  to = ["src/db/**"]
  allow = ["src/ui/admin/**"]     # from-side files exempt
  via = "any"

  [[rule]]
  id = "layers"
  kind = "layers"                 # each layer depends only on the layers after it
  layers = [{ name = "web", in = ["lib/web/**"] },
            { name = "domain", in = ["lib/domain/**"] },
            { name = "repo", in = ["lib/repo/**"] }]

  [[rule]]
  id = "no-cycles"
  kind = "cycles"                 # no cycle among files (or folders: level = "dir")
  in = ["src/**"]
  level = "file"
  via = "import"
"""
from __future__ import annotations

import os
import re




def file_deps(project, via: str = "any") -> dict[str, dict[str, tuple[int, str]]]:
    """src file -> {dst file: (line, "import" | "call")}: the first line that makes the dependency."""
    key = ("\0filedeps", via)
    cache = project.__dict__.setdefault("_arch_cache", {})
    if key in cache:
        return cache[key]
    out: dict[str, dict[str, tuple[int, str]]] = {}
    if via in ("import", "any"):
        for m in project.modules.values():
            if m.owner:
                continue
            typed = m.__dict__.get("type_imports", set())
            targets = set(m.aliases.values()) | {_in_package(project, w, n) for k, (w, n) in m.imported.items()
                                                 if k not in typed} | set(m.uses) | set(m.reexports)
            for t in targets:
                tm = project.modules.get(t) if t else None
                if tm is None or tm.file == m.file:
                    continue
                out.setdefault(m.file, {}).setdefault(tm.file, (_import_line(m, tm), "import"))
    if via in ("call", "any"):
        from .infer import Types
        types = Types(project)
        dispatched: dict[str, bool] = {}

        def through_dispatch(g) -> bool:
            """g overrides / implements a method its supertype declares: a call reaches it
            through that type (NodeTraversor's `visitor.head()` names NodeVisitor, not
            every visitor), so the caller depends on the supertype, not on g's file."""
            if g.id not in dispatched:
                from .infer import _arities
                dispatched[g.id] = any((f"{a}.{g.name}" in project.funcs or _arities(project.modules[a], g.name))
                                       for a in types.ancestors(g.module)[1:] if project.modules[a].file != g.file)
            return dispatched[g.id]
        for src, es in project.edges.items():
            f = project.funcs.get(src)
            if f is None:
                continue
            for dst, ln, exact in es:
                g = project.funcs.get(dst)
                if not exact or g is None or g.file == f.file or through_dispatch(g):
                    continue
                d = out.setdefault(f.file, {})
                if g.file not in d or d[g.file][1] == "call" and ln < d[g.file][0]:
                    d.setdefault(g.file, (ln, "call"))
    cache[key] = out
    return out


def _in_package(project, where: str, name: str) -> str:
    """`import org.jsoup.nodes.Document` names package org.jsoup.nodes: the module of its Document."""
    if not where.startswith("pkg:"):
        return where
    cache = project.__dict__.setdefault("_arch_pkg", {})
    key = (where, name)
    if key not in cache:
        cands = project.packages.get(where[4:], [])
        hit = [mn for mn in cands if os.path.splitext(os.path.basename(project.modules[mn].file))[0] == name] or \
              [mn for mn in cands if name in project.modules[mn].decls]
        cache[key] = hit[0] if hit else ""
    return cache[key]


def _import_line(m, tm) -> int:
    """The line of m that imports tm: the first import-like line naming its file stem."""
    stem = os.path.splitext(os.path.basename(tm.file))[0]
    if stem in ("index", "__init__", "mod", "lib"):
        stem = os.path.basename(os.path.dirname(tm.file)) or stem
    pat = re.compile(rf"\b{re.escape(stem)}\b")
    for i, ln in enumerate(m.raw[:400]):
        s = ln.lstrip()
        if s.startswith(("import", "from", "use ", "using", "#include", "require", "alias", "include", "const", "let", "var")) \
                and pat.search(ln):
            return i + 1
    return 1


def depend(project, r: dict) -> list:
    from .core import Finding, match_any
    deps = file_deps(project, r.get("via", "any"))
    out = []
    for src, ds in sorted(deps.items()):
        if not match_any(src, r["from"]) or match_any(src, r.get("allow", [])):
            continue
        for dst, (ln, how) in sorted(ds.items()):
            if match_any(dst, r["to"]) and not match_any(dst, r.get("allow_to", [])):
                out.append(Finding(r["id"], r.get("severity", "error"), src, ln, "",
                                   r.get("message", f"depends on {dst} ({how})")))
    return out


def layers(project, r: dict) -> list:
    from .core import Finding, match_any
    ls = r["layers"]
    deps = file_deps(project, r.get("via", "any"))

    def layer_of(path):
        for k, layer in enumerate(ls):
            if match_any(path, layer["in"]):
                return k
        return None
    out = []
    for src, ds in sorted(deps.items()):
        a = layer_of(src)
        if a is None or match_any(src, r.get("allow", [])):
            continue
        for dst, (ln, how) in sorted(ds.items()):
            b = layer_of(dst)
            if b is not None and b < a:
                out.append(Finding(r["id"], r.get("severity", "error"), src, ln, "",
                                   r.get("message", f"layer {ls[a]['name']} depends on the layer above it, "
                                                    f"{ls[b]['name']}: {dst} ({how})")))
    return out


def cycles(project, r: dict) -> list:
    """Strongly connected components of the dependency graph within `in`: one
    finding per cycle, at the first file of a shortest loop through it."""
    from .core import Finding, match_any
    level = r.get("level", "file")
    deps = file_deps(project, r.get("via", "import"))
    scope = r.get("in", ["**/*"])
    skip = r.get("skip", [])

    def node(path):
        return os.path.dirname(path) or "." if level == "dir" else path
    graph: dict[str, dict[str, tuple[str, int]]] = {}
    for src, ds in deps.items():
        if not match_any(src, scope) or match_any(src, skip):
            continue
        for dst, (ln, _how) in ds.items():
            if not match_any(dst, scope) or match_any(dst, skip):
                continue
            a, b = node(src), node(dst)
            if a != b:
                graph.setdefault(a, {}).setdefault(b, (src, ln))
    out = []
    for comp in _sccs(graph):
        if len(comp) < 2:
            continue
        loop = _shortest_loop(graph, comp)
        first = graph[loop[0]][loop[1]]
        out.append(Finding(r["id"], r.get("severity", "error"), first[0], first[1], "",
                           r.get("message", f"dependency cycle ({len(comp)} {'folders' if level == 'dir' else 'files'}): "
                                 + " -> ".join(loop + [loop[0]]))))
    return out


def _sccs(graph: dict) -> list[list[str]]:
    """Tarjan, iterative."""
    index, low, on, stack, out = {}, {}, set(), [], []
    counter = [0]
    nodes = sorted(set(graph) | {b for ds in graph.values() for b in ds})
    for root in nodes:
        if root in index:
            continue
        work = [(root, iter(sorted(graph.get(root, {}))))]
        index[root] = low[root] = counter[0]
        counter[0] += 1
        stack.append(root)
        on.add(root)
        while work:
            v, it = work[-1]
            nxt = next(it, None)
            if nxt is not None:
                if nxt not in index:
                    index[nxt] = low[nxt] = counter[0]
                    counter[0] += 1
                    stack.append(nxt)
                    on.add(nxt)
                    work.append((nxt, iter(sorted(graph.get(nxt, {})))))
                elif nxt in on:
                    low[v] = min(low[v], index[nxt])
                continue
            work.pop()
            if work:
                low[work[-1][0]] = min(low[work[-1][0]], low[v])
            if low[v] == index[v]:
                comp = []
                while True:
                    w = stack.pop()
                    on.discard(w)
                    comp.append(w)
                    if w == v:
                        break
                out.append(sorted(comp))
    return out


def _shortest_loop(graph: dict, comp: list[str]) -> list[str]:
    """A shortest cycle through the component's first node (BFS back to it)."""
    members, start = set(comp), comp[0]
    prev = {start: None}
    queue = [start]
    while queue:
        v = queue.pop(0)
        for w in sorted(graph.get(v, {})):
            if w not in members:
                continue
            if w == start:
                path = [v]
                while prev[path[-1]] is not None:
                    path.append(prev[path[-1]])
                return list(reversed(path))
            if w not in prev:
                prev[w] = v
                queue.append(w)
    return comp


def graph(project, level: str = "file", via: str = "import", only: list[str] | None = None) -> dict[str, dict[str, int]]:
    """The dependency graph at `level` ("file" | "dir"), edges weighted by how many file pairs make them."""
    from .core import match_any
    out: dict[str, dict[str, int]] = {}
    for src, ds in file_deps(project, via).items():
        if only and not match_any(src, only):
            continue
        for dst in ds:
            if only and not match_any(dst, only):
                continue
            a, b = (os.path.dirname(src) or ".", os.path.dirname(dst) or ".") if level == "dir" else (src, dst)
            if a != b:
                out.setdefault(a, {})
                out[a][b] = out[a].get(b, 0) + 1
    return out


def render(g: dict[str, dict[str, int]], fmt: str) -> str:
    """dot (Graphviz), mermaid, or json; nodes in a dependency cycle drawn red."""
    import json
    cyc = {n for comp in _sccs(g) if len(comp) > 1 for n in comp}
    nodes = sorted(set(g) | {b for ds in g.values() for b in ds})
    if fmt == "json":
        return json.dumps({"nodes": [{"id": n, "cycle": n in cyc} for n in nodes],
                           "edges": [{"from": a, "to": b, "weight": w} for a in sorted(g) for b, w in sorted(g[a].items())]},
                          indent=1)
    ids = {n: f"n{k}" for k, n in enumerate(nodes)}
    if fmt == "mermaid":
        lines = ["graph LR"] + [f'  {ids[n]}["{n}"]' for n in nodes]
        lines += [f"  {ids[a]} --> {ids[b]}" for a in sorted(g) for b in sorted(g[a])]
        lines += [f"  style {ids[n]} stroke:#c00" for n in nodes if n in cyc]
        return "\n".join(lines)
    lines = ["digraph deps {", "  rankdir=LR; node [shape=box, fontsize=10];"]
    lines += [f'  "{n}"' + (" [color=red]" if n in cyc else "") + ";" for n in nodes]
    lines += [f'  "{a}" -> "{b}"' + (" [color=red]" if a in cyc and b in cyc else "") + ";"
              for a in sorted(g) for b in sorted(g[a])]
    return "\n".join(lines + ["}"])


def path(project, r: dict) -> list:
    """[[rule]] kind = "path": no call chain from `from` (function specs) may reach a
    call matching `to` (regexes over `qual.name` as written: `os\\.system`, `\\.execute$`,
    `^eval$`) within `depth` exact calls, unless it passes a function named in `unless`
    (a sanitizer, an auth check). One finding per entry point and sink, with the chain.

    With `source` (a regex: `request\\.args`), it is a taint check: the entry points are
    the functions that read the source (tests aside), and a sink counts only when its
    arguments carry the value: the source itself, a name assigned from it (or from such
    a name), or, below the entry, a parameter the caller passed a tainted argument to.
    A value through a call named like an `unless` entry is clean, and so is a constant
    first argument (a parameterized query, a fixed redirect), unless it names a shell.
    Each sink is reported once, by its shortest chain."""
    from .core import Finding
    sinks = [re.compile(p) for p in (r["to"] if isinstance(r["to"], list) else [r["to"]])]
    source = re.compile(r["source"]) if r.get("source") else None
    unless = set(r.get("unless", []))
    # a call whose name holds an `unless` entry (`mysqli_real_escape_string(`, `_sanitize_username(`;
    # a short one, `int`, only as the whole name), or a method of that name (`params[:id].to_i`)
    names = "|".join(rf"\w*{re.escape(u)}\w*" if len(u) > 3 else rf"(?<![\w.]){re.escape(u)}" for u in unless)
    clean = (re.compile(rf"(?:{names})\s*\("), re.compile(rf"\.(?:{'|'.join(map(re.escape, unless))})\b")) if unless else None
    depth = int(r.get("depth", 6))
    fuzzy = bool(r.get("fuzzy", False))
    starts = []
    for spec in (r["from"] if isinstance(r["from"], list) else [r["from"]]):
        starts.extend(project.find(spec))
    bound, readers = {}, None  # entry id -> the parameters the framework fills from the request
    if source is not None:  # taint starts where the source is read
        from . import tidy
        # a handler the framework passes request values to: `@GetMapping` + `String cmd`, `[HttpPost]` + `[FromBody] T x`
        entry = re.compile(r["source_entry"]) if r.get("source_entry") else None
        param = re.compile(r["source_param"]) if r.get("source_param") else None
        if entry is not None and param is not None:
            for f in starts:
                if f.kind != "top" and entry.search(_annotations(project, f)):
                    got = frozenset(n for n, d in _param_decls(project, f) if param.search(d))
                    if got:
                        bound[f.id] = got
        # a helper that reads the request and returns it: `getCookie(request, "x")`, `WebUtils.getRequestBody(request)`
        rd = sorted({f.name for f in project.funcs.values() if f.kind != "top" and re.match(r"[A-Za-z_]\w*$", f.name)
                     and not tidy.is_test(project, f.file) and re.search(r"\breturn\b", _own_code(project, f))
                     and source.search(_own_code(project, f, raw=True))})
        readers = re.compile(rf"(?<![\w$])((?:[A-Za-z_]\w*\.)?)(?:{'|'.join(rd)})\s*\(") if rd else None
        # (raw text: `"#{params[:id]}"` reads the source inside a string)
        starts = [f for f in starts if not tidy.is_test(project, f.file)
                  and (f.id in bound or source.search(_own_code(project, f, raw=True)) or _reads(readers, _own_code(project, f)))]
    out, best = [], {}
    for s in sorted({f.id: f for f in starts}.values(), key=lambda f: f.id):
        if s.name in unless:
            continue
        prev = {s.id: None}
        given = {s.id: bound.get(s.id, frozenset())}  # (taint) the parameters each function was passed a tainted value in
        frontier, hits = [s.id], {}
        for _ in range(depth + 1):
            nxt = []
            for fid in frontier:
                f = project.funcs[fid]
                taint = _Taint(project, f, source, clean, given[fid], readers) if source is not None else None
                for c in f.calls:
                    # (Ruby's bare `system`; a chained call on its own line, `.FromSql(q)`, keeps its dot)
                    text = (f"{c.qual}." if c.qual and c.qual not in (".", "self") else "." if c.qual == "." else "") + c.name
                    if any(p.search(text) for p in sinks) and (f.file, c.line) not in hits \
                            and (source is None or _carries(project, f, c, source, clean, taint.at(c.line), sink=True)) \
                            and not (source is not None and taint.downstream(project, f, c, {ln for (fl, ln) in hits if fl == f.file})):
                        hits[(f.file, c.line)] = (fid, text)
                for dst, ln, exact in project.edges.get(fid, []):
                    if not (exact or fuzzy) or dst in prev or dst not in project.funcs or project.funcs[dst].name in unless:
                        continue
                    if source is not None:
                        g = project.funcs[dst]
                        params = _params(project, g)
                        got = set()
                        for c in (c for c in f.calls if c.line == ln and c.name == g.name):
                            got |= _passed(project, f, c, g, params, source, clean, taint.at(c.line))
                        if not got:
                            continue  # nothing tainted goes in
                        given[dst] = frozenset(got)
                    prev[dst] = fid
                    nxt.append(dst)
            frontier = nxt
        for (file, line), (fid, text) in sorted(hits.items()):
            chain, k = [], fid
            while k is not None:
                chain.append(project.funcs[k].name)
                k = prev[k]
            f = Finding(r["id"], r.get("severity", "error"), s.file, s.start, s.name,
                        r.get("message", f"reaches {text}() at {file}:{line}: ") + " -> ".join(reversed(chain)))
            if source is None:
                out.append(f)
            elif (file, line) not in best or len(chain) < best[(file, line)][0]:
                best[(file, line)] = (len(chain), f)
    return out + [f for _, f in sorted(best.values(), key=lambda x: (x[1].file, x[1].line))]


def _close(code: str, k: int) -> int:
    """Where the bracket open before offset k closes."""
    d = 1
    while k < len(code) and d:
        d += code[k] in "([{"
        d -= code[k] in ")]}"
        k += 1
    return k - 1


def _reads(readers, text: str) -> bool:
    """text calls a request-reading helper bare or on a type (`WebUtils.getRequestBody(req)`), not as
    any value's method (`d.get(k)` is not a helper `get`)."""
    return bool(readers) and any(not q or q[0].isupper() for q in readers.findall(text))


def _own_code(project, f, raw: bool = False) -> str:
    """f's code (comments and strings blanked, or `raw`, at the same offsets); a file's
    `<top>`: its lines outside functions."""
    m = project.modules.get(f.module)
    if m is None:
        return ""
    lines = (m.raw if raw else m.code)[f.start - 1:f.end]
    if f.kind == "top":  # (blanked, not dropped: line numbers hold)
        inner = {n for mod in project.by_file.get(f.file, []) for g in mod.funcs if g.kind != "top"
                 for n in range(g.start, g.end + 1)}
        lines = ["" if i in inner else ln for i, ln in enumerate(lines, f.start)]
    return "\n".join(lines)


class _Taint:
    """Which names in a function hold the source's value at a line: the parameters
    it was passed a tainted value in, then each assignment in source order (`x =
    request.args[...]` taints x, `x = escape(x)` cleans it again: the last assignment
    before the line decides; branches and loops are read top to bottom)."""

    def __init__(self, project, f, source, clean, given: frozenset, readers=None):
        code, raw = _own_code(project, f), _own_code(project, f, raw=True)  # (f"dig {domain}": the raw text has the name)
        # (no interpolation: `"... where username = ?"` holds no value named username)
        no_interp = f.file.endswith((".java", ".go"))
        self.steps = []  # (line, name, dirty, indent)
        self.given = set(given)
        self.source = source
        self.start, self.lines = f.start, code.split("\n")
        found = []  # (offset, indent, names, the value's offset)
        # `x = `, `$x = ` (PHP), `@x = ` (Ruby), `const x = ` / `String x = ` (declared), `x := ` (Go)
        for am in re.finditer(r"^([ \t]*)(?:(?:let|const|var|val|my|final|auto|[\w<>\[\],.?]+)[ \t]+)*([$@]?[A-Za-z_]\w*)"
                              r"[ \t]*(?::[^=\n]+)?:?=(?!=)\s*(.+)$", code, re.M):
            found.append((am.start(), am.group(1), [am.group(2)], am.start(3)))
        # destructuring: `const { a, b: c } = req.body`, `[x, y] = ...`, Python's `a, b = ...`
        for am in re.finditer(r"^([ \t]*)(?:(?:const|let|var)[ \t]+)?(\{[^{}=;]*\}|\[[^\[\]=;]*\]|[A-Za-z_]\w*(?:[ \t]*,[ \t]*[A-Za-z_]\w*)+)"
                              r"\s*=(?!=)\s*(.+)$", code, re.M):
            names = [re.findall(r"[$\w]+", part.split(":")[-1].split("=")[0])[-1:] for part in am.group(2).strip("{}[]").split(",")]
            found.append((am.start(), am.group(1), [n[0] for n in names if n], am.start(3)))
        # in a condition: `if ((cmd = req.getParameter("c")) != null)`, `while ((line = r.readLine()) != null)`
        conds = {}  # value offset -> where it ends, when not at the statement's end
        for am in re.finditer(r"\(\s*([A-Za-z_]\w*)[ \t]*=(?!=)\s*", code):
            bol = code.rfind("\n", 0, am.start()) + 1
            indent = re.match(r"[ \t]*", code[bol:]).group()
            found.append((am.start(), indent, [am.group(1)], am.end()))
            conds[am.end()] = _close(code, am.end())
        # only ever taint: a value put into an object (`sb.append(id)`, `doc.Load(Request.Body)`, `xs.push(q)`)
        # and a loop variable over a tainted collection (`for (T x : xs)`, `foreach (var x in xs)`, `for x in xs:`)
        adds = set()
        for am in re.finditer(r"^([ \t]*)([$@]?[A-Za-z_]\w*)\.(?:append|add|addAll|put|push|extend|insert|concat|write|"
                              r"Append|AppendFormat|AppendLine|Add|AddRange|Insert|Load|LoadXml|Write)[ \t]*\(", code, re.M):
            found.append((am.start(), am.group(1), [am.group(2)], am.end()))
            adds.add(am.end())
            conds[am.end()] = _close(code, am.end())
        for am in re.finditer(r"^([ \t]*)(?:[\w}]+[ \t]+)?for(?:each)?[ \t]*\([ \t]*(?:(?:const|let|var|final)[ \t]+)?(?:[\w<>\[\],.?]+[ \t]+)?"
                              r"([A-Za-z_]\w*)[ \t]+(?:in|of|:)[ \t]+", code, re.M):
            found.append((am.start(), am.group(1), [am.group(2)], am.end()))
            adds.add(am.end())
            conds[am.end()] = _close(code, am.end())
        for am in re.finditer(r"^([ \t]*)(?:async[ \t]+)?for[ \t]+([A-Za-z_]\w*)(?:[ \t]*,[ \t]*[A-Za-z_]\w*)*[ \t]+in[ \t]+(.+?):?[ \t]*$", code, re.M):
            found.append((am.start(), am.group(1), [am.group(2)], am.start(3)))
            adds.add(am.start(3))
            conds[am.start(3)] = am.end(3)
        for start, indent, names, val in sorted(found):
            end = conds.get(val) or _statement_end(code, val, len(indent.expandtabs(4)))
            rhs = (code if no_interp else raw)[val:end]
            ln = f.start + code.count("\n", 0, start)
            dirty = _dirty(rhs, source, clean, self.at(ln)) or (_reads(readers, rhs) and not
                                                                     (clean and any(c.search(rhs) for c in clean)))
            if val in adds and not dirty:
                continue  # (adding a clean value cleans nothing)
            self.steps += [(ln, name, dirty, len(indent.expandtabs(4))) for name in names]

    def _inside(self, a: int, b: int, indent: int) -> bool:
        """Line b is still in the block line a opened at `indent` (no line between dedents below it)."""
        for t in self.lines[a - self.start + 1:b - self.start + 1]:
            if t.strip() and len(t[:len(t) - len(t.lstrip())].expandtabs(4)) < indent and not t.lstrip()[:1] in ")]}":
                return False
        return True

    def downstream(self, project, f, c, sunk: set) -> bool:
        """Every tainted name call c uses was assigned from a sink already reported (`st =
        con.prepareStatement(sql)`, then `st.executeQuery()`): the same injection, reported once."""
        m = project.modules.get(f.module)
        line = m.code[c.line - 1] if m is not None and c.line <= len(m.code) else ""
        used = [t for t in self.at(c.line) if _names(t, line)]
        if not used or not sunk:
            return False
        for t in used:
            ln = max((ln for ln, name, _, _ in self.steps if name == t and ln < c.line), default=None)
            if ln not in sunk:
                return False
            # (`file = params[:type].constantize.new(path)` brings in a second value: path still counts)
            rhs = m.code[ln - 1].split("=", 1)[-1] if m is not None else ""
            if sum(_names(u, rhs) for u in self.at(ln)) + bool(self.source and self.source.search(m.raw[ln - 1])) > 1:
                return False
        return True

    def at(self, line: int) -> set:
        state = {n: 0 for n in self.given}  # name -> the indentation it was tainted at
        for ln, name, dirty, indent in self.steps:
            if ln >= line:
                break
            if dirty:
                state.setdefault(name, indent)
            elif name in state and (indent <= state[name] or self._inside(ln, line, indent)):
                # a clean value: from here on, or (deeper, in one branch) for the rest of that branch
                del state[name]
        return set(state)


def _statement_end(code: str, val: int, width: int) -> int:
    """Where the statement whose value starts at `val` ends: before the next line that
    starts at its indentation or less. A continuation is deeper, closes a bracket or a
    string, or is inside one (blanked to spaces)."""
    k = code.find("\n", val)
    while k >= 0:
        line = code[k + 1:code.find("\n", k + 1) if code.find("\n", k + 1) >= 0 else len(code)]
        body = line.lstrip()
        if body and len(line[:len(line) - len(body)].expandtabs(4)) <= width and body[0] not in ")]}'\"`":
            return k + 1
        k = code.find("\n", k + 1)
    return len(code)


_SHELLS = re.compile(r"""^\s*[fbru]*["'`](?:/bin/|/usr/bin/)?(?:sh|bash|zsh|cmd(?:\.exe)?|powershell|pwsh)["'`]""")
# a constant string (not Ruby's "#{x}", PHP's "$x", a JS `${x}` or a Python f-string)
_LITERAL = re.compile(r"""^\s*(?:[bru]*"(?:[^"\\$#]|\\.|#(?!\{)|\$(?![A-Za-z_{]))*"|[bru]*'(?:[^'\\]|\\.)*'|`[^`$]*`)\s*$""")


def _call_args(project, f, c) -> list[str]:
    """The call's arguments as written (raw text, split at top-level commas)."""
    m = project.modules.get(f.module)
    if m is None or not (0 < c.line <= len(m.raw)):
        return []
    # bounds from the blanked code (a `,` or `)` in a string is no boundary), text from the raw lines
    text = "\n".join(m.code[c.line - 1:c.line + 60])  # (a callback argument spans lines)
    raw = "\n".join(m.raw[c.line - 1:c.line + 60])
    # (the occurrence that is called: `ProcessBuilder pb = new ProcessBuilder(cmd)` declares the type first)
    eol = text.find("\n") % (len(text) + 1)
    nm = re.compile(rf"(?<![\w$]){re.escape(c.name)}(?!\w)\s*(?=\()").search(text, 0, eol) \
        or re.search(rf"(?<![\w$]){re.escape(c.name)}(?!\w)\s*", text)
    if nm is None:
        return []
    k = nm.end()
    if text[k:k + 1] != "(":
        if not f.file.endswith(".rb"):
            return []
        # Ruby without parentheses: `redirect_to :login`, `system "cp #{x}" if y`
        end = text.find("\n", k)
        rest = text[k:end if end >= 0 else len(text)]
        rest = re.split(r"\s+(?:if|unless|while|until|do)\b|\s*\{\s*\|", rest)[0]
        text = text[:k - 1] + "(" + rest + ")"
        raw = raw[:k - 1] + "(" + raw[k:k + len(rest)] + ")"
        k -= 1
    depth_, j = 0, k
    while 0 <= j < len(text):
        depth_ += text[j] in "([{"
        depth_ -= text[j] in ")]}"
        if depth_ == 0:
            break
        j += 1
    if k < 0 or j >= len(text):
        return []
    args, start, d = [], k + 1, 0
    for i in range(k + 1, j):
        d += text[i] in "([{"
        d -= text[i] in ")]}"
        if text[i] == "," and d == 0:
            args.append(raw[start:i])
            start = i + 1
    return [a for a in args + [raw[start:j]] if a.strip()]


_FUNCTION_LITERAL = re.compile(r"\s*(?:async\s+)?(?:function\b|(?:\([^()]*\)|[\w$]+)\s*=>|lambda\b|func\s*\(|proc\b|->)")


def _dirty(arg: str, source, clean, tainted: set) -> bool:
    """The text carries the value: the source or a tainted name, outside the sanitizer
    calls in it (`request.referrer or url_for("index")` is still the referrer); a
    sanitizing method (`params[:id].to_i`) cleans the whole expression. A callback
    (`(err, rows) => {...}`) is code, not the value, whatever names its body uses."""
    if _FUNCTION_LITERAL.match(arg):
        return False
    if clean:
        call, method = clean
        if re.search(rf"{method.pattern}\s*(?:\([^()]*\))?\s*$", arg.strip()):
            return False  # `params[:id].to_i`: the whole value
        # a sanitized part (`#{params[:image].to_i}`) is clean; the rest still counts
        # (`"cp #{file} #{Time.now.to_i}"` carries file)
        arg = re.sub(rf"[\w@$:.\[\]'\"]+?{method.pattern}(?:\([^()]*\))?", " ", arg)
        for m in reversed(list(call.finditer(arg))):
            d, j = 0, m.end() - 1
            while j < len(arg):
                d += arg[j] in "([{"
                d -= arg[j] in ")]}"
                if d == 0:
                    break
                j += 1
            arg = arg[:m.start()] + arg[j + 1:]
    return bool(source.search(arg)) or any(_names(t, arg) for t in tainted)


def _carries(project, f, c, source, clean, tainted: set, sink: bool = False) -> bool:
    """A sink call's arguments carry the value. A constant first argument (a
    parameterized query, a fixed or same-site redirect) is clean, unless it names a shell."""
    args = _call_args(project, f, c)
    m = project.modules.get(f.module)
    line = m.raw[c.line - 1] if m is not None and 0 < c.line <= len(m.raw) else ""
    # the receiver: `params[:class].classify.constantize`, `user_input.to_sym.send`
    recv = re.search(rf"([\w@$:.\[\]'\"()]+?)\s*\.\s*{re.escape(c.name)}\b", line)
    if recv and sink and _dirty(recv.group(1), source, clean, tainted):
        return True
    if not args:
        return False
    first = args[0].strip()
    cm_ = re.fullmatch(r"([A-Za-z_$][\w$]*)\(\s*\)", first)
    if sink and cm_ and m is not None:
        # `find(searchCriteria())`: a closure of this function's, built from its tainted names
        # (`const searchCriteria = () => ({ $where: `... ${threshold}` })`)
        for g in project.by_name.get(cm_.group(1), ()):
            if g.file == f.file and f.start < g.start <= f.end and g.id != f.id:
                body = "\n".join(m.raw[g.start:g.end])  # (after its head: the value it builds)
                if any(_names(t, body) for t in tainted) or source.search(body):
                    return True
    if sink and f.file.endswith(".rb") and re.match(r":?\w+\s*(?::(?!:)|=>)", first):
        return False  # Rails: `where(id: params[:id])`, a bound value
    if sink and not _SHELLS.match(first) and (_LITERAL.match(first) or re.match(r"""^[fbru]*["'`]/(?!/)""", first)
                                              and "redirect" in c.name.lower()):
        return False
    if sink and re.fullmatch(r"find\w*|update\w+|delete\w+|remove|count\w*|aggregate", c.name) \
            and f.file.endswith((".js", ".mjs", ".cjs", ".ts", ".mts", ".jsx", ".tsx")) and not first.startswith(("{", "[")):
        return False  # a MongoDB query is an object (`{ user: id }`); `rows.find(r => ...)` is an array's
    if sink and c.name in ("sendFile", "download") and any(re.search(r"\broot\s*:", a) for a in args[1:]):
        return False  # Express: a path under `{ root }` can't leave it
    return any(_dirty(a, source, clean, tainted) for a in args)


def _params(project, g) -> list[str]:
    """g's parameter names, in order (`self` / `cls` / `this` left out)."""
    return [n for n, _ in _param_decls(project, g)]


def _annotations(project, g) -> str:
    """The annotation / attribute lines just above g (`@GetMapping("/x")`, `[HttpPost]`)."""
    m = project.modules.get(g.module)
    out = []
    for ln in range(g.start - 2, max(g.start - 9, -1), -1):
        t = m.raw[ln].strip() if m is not None and ln < len(m.raw) else ""
        if not t.startswith(("@", "[")):
            break
        out.append(t)
    return "\n".join(reversed(out))


def _param_decls(project, g) -> list[tuple[str, str]]:
    """g's parameters as (name, its declaration as written), in order."""
    m = project.modules.get(g.module)
    if m is None:
        return []
    head = "\n".join(m.code[g.start - 1:min(g.end, g.start + 12)])
    k = head.find("(", head.find(g.name) if g.name in head else 0)
    if k < 0:
        return []
    d, j = 0, k
    while j < len(head):
        d += head[j] in "([{<"
        d -= head[j] in ")]}>"
        if d == 0:
            break
        j += 1
    ext = os.path.splitext(g.file)[1]
    out = []
    parts, cur, d = [], "", 0
    for ch in head[k + 1:j]:
        d += ch in "([{<"
        d -= ch in ")]}>"
        if ch == "," and d == 0:
            parts.append(cur)
            cur = ""
        else:
            cur += ch
    for p in parts + [cur]:
        decl = p.strip()
        p = re.sub(r"@[\w.]+(?:\([^()]*\))?|\[\w+(?:\([^()]*\))?\]", " ", p.split("=")[0])  # (`@RequestParam`, `[FromBody]`)
        if ext not in (".go",) and ":" in p.replace("::", ""):
            p = p.replace("::", "").split(":")[0]
        names = re.findall(r"[$@]?[A-Za-z_]\w*", p)
        names = [n for n in names[:-1] if n not in ("params",)] + names[-1:]  # (C#'s `params T[] xs`; a name `params` stays)
        names = [n for n in names if n not in ("self", "cls", "this", "final", "const", "mut", "ref", "out", "in",
                                               "readonly", "public", "private", "protected", "val", "var")]
        if not names:
            continue
        dollar = [n for n in names if n.startswith("$")]
        out.append((dollar[0] if dollar else names[0] if ext in (".go", ".py", ".js", ".ts", ".tsx", ".jsx", ".mjs", ".rb",
                                                                      ".kt", ".rs", ".swift", ".ex", ".exs") else names[-1], decl))
    return [(n.lstrip("*&"), d) for n, d in out]


def _passed(project, f, c, g, params: list[str], source, clean, tainted: set) -> set:
    """The parameters of g that call c (in f) passes a tainted value to."""
    got = set()
    pos = 0
    for a in _call_args(project, f, c):
        kw = re.match(r"\s*([A-Za-z_]\w*)\s*[=:](?!=)", a)
        dirty = _dirty(a, source, clean, tainted)
        if kw and kw.group(1) in params:
            if dirty:
                got.add(kw.group(1))
            continue
        if dirty and pos < len(params):
            got.add(params[pos])
        pos += 1
    return got


def _names(name: str, text: str) -> bool:
    """`name` (`$x`, `@x` or `x`) occurs in text as a whole name."""
    return re.search(rf"(?<![\w$@]){re.escape(name)}(?!\w)", text) is not None
