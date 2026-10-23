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
