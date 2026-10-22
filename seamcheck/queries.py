"""The commands' answers as data. The CLI prints them; `seamcheck serve`
returns them as JSON. One implementation, so an agent asking over MCP gets
the answer the terminal gets."""
from __future__ import annotations

import collections
import os

from . import core, exactness


def where(p: core.Project, fid: str) -> dict:
    """A function's id, place and the bridges it sits on."""
    f = p.funcs.get(fid)
    via = sorted({b for (a, d), b in p.bridged.items() if fid in (a, d)})
    return {"id": fid, "file": f.file if f else "", "line": f.start if f else 0, "via": via}


def check(p: core.Project, only: list[str] | None = None, skip: list[str] | None = None,
          errors_only: bool = False) -> list[core.Finding]:
    found = core.run_rules(p, only, skip)
    exactness.record(p)  # the gate's exactness guard compares a change against these
    return [f for f in found if f.severity == "error"] if errors_only else found


def flat(p: core.Project, findings: list[core.Finding]) -> list[dict]:
    """Findings as `check --json` prints them: each with its rule's why and fix."""
    rules = {r["id"]: r for r in p.config.get("rule", [])}
    return [dict(f.__dict__, why=rules.get(f.rule, {}).get("why", ""), fix=rules.get(f.rule, {}).get("fix", ""))
            for f in findings]


def walk(p: core.Project, spec: str, depth: int, reverse: bool, fuzzy: bool = False) -> list[dict]:
    """`callers` (reverse) / `callees`: per matching function, its exact and
    name-guess edge counts and what it reaches within `depth` calls (exact
    edges unless `fuzzy`)."""
    out = []
    for s in p.find(spec):
        ex, guess = core.edge_split(p, s.id, reverse=reverse)
        dist = core.reach(p, s.id, depth, reverse=reverse, exact_only=not fuzzy)
        out.append(dict(where(p, s.id), exact=ex, guess=guess,
                        reach=[dict(where(p, k), depth=d) for k, d in sorted(dist.items(), key=lambda kv: (kv[1], kv[0])) if d]))
    return out


def shortest(p: core.Project, a: str, b: str, fuzzy: bool = False) -> tuple[list[str], bool]:
    """The shortest call chain from `a` to `b`, and (when there is no exact
    one) whether a chain through name guesses exists: a miss must not read as
    "nothing connects them"."""
    froms, tos = p.find(a), {f.id for f in p.find(b)}

    def best(exact_only: bool) -> list[str]:
        out: list[str] = []
        for f in froms:
            for t in tos:
                c = core.path(p, f.id, t, exact_only=exact_only)
                if c and (not out or len(c) < len(out)):
                    out = c
        return out
    chain = best(not fuzzy)
    return chain, bool(not chain and not fuzzy and best(False))


def find(p: core.Project, spec: str) -> list[dict]:
    out = []
    for f in p.find(spec):
        ex, guess = core.edge_split(p, f.id)
        out.append({"id": f.id, "file": f.file, "start": f.start, "end": f.end, "calls": len(f.calls),
                    "exact_callers": ex, "guess_callers": guess})
    return out


def changed_functions(p: core.Project, since: str, staged: bool = False) -> set[str]:
    """The functions a diff touched (committed since REF, staged, unstaged, untracked)."""
    from . import tidy
    start = set()
    for rel, lines in tidy.changed_lines(p, since, staged, deletions=True).items():
        for m in p.by_file.get(rel, []):
            for ln in lines:
                f = m.func_at(ln)
                if f and f.kind != "top":
                    start.add(f.id)
    return start


def target_functions(p: core.Project, targets: list[str]) -> set[str]:
    """Each target a file (all its functions) or a function spec."""
    start = set()
    for t in targets:
        if t in p.by_file or os.path.exists(os.path.join(p.root, t)):
            start |= {f.id for m in p.by_file.get(t, []) for f in m.funcs}
        else:
            start |= {f.id for f in p.find(t)}
    return start


def impact(p: core.Project, start: set[str], depth: int = 4, fuzzy: bool = False) -> tuple[dict[str, list], set[str]]:
    """What a change to `start` reaches, by area ([project] areas, else the
    top folder): {area: [(distance, function id)]}, and the start's own areas."""
    dist = core.impact(p, start, depth, fuzzy=fuzzy)
    areas = p.config.get("project", {}).get("areas") or {}

    def area(rel):
        return next((k for k, g in areas.items() if core.match_any(rel, g)), rel.split("/")[0])
    by = collections.defaultdict(list)
    for fid, d in dist.items():
        if d and fid in p.funcs:
            by[area(p.funcs[fid].file)].append((d, fid))
    return {k: sorted(v) for k, v in by.items()}, {area(p.funcs[s].file) for s in start if s in p.funcs}


def stats(p: core.Project) -> dict:
    """Size, timing and, per language, how calls resolve. `project_exact`:
    of the calls that reach project code, the share that is exact."""
    exactness.record(p)
    langs: dict[str, int] = {}
    for rel in p.by_file:
        ext = os.path.splitext(rel)[1]
        langs[ext] = langs.get(ext, 0) + 1
    res = {}
    for ext, r in sorted(p.resolution.items(), key=lambda kv: -sum(kv[1][:3])):
        ex, fz, no = r[:3]
        res[ext] = {"calls": ex + fz + no, "exact": ex, "guess": fz, "nothing": no,
                    "project_exact": 100 * ex // (ex + fz) if ex + fz else None}
    return {"root": p.root, "files": len(p.files), "parsed": len(p.by_file), "languages": langs,
            "modules": len(p.modules), "functions": len(p.funcs),
            "edges": sum(len(v) for v in p.edges.values()),
            "exact_edges": sum(1 for v in p.edges.values() for e in v if e[2]),
            "load_ms": p.parse_ms, "resolution": res}
