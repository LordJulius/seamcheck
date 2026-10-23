"""'Who calls X?' answered by name (ast-grep, Semgrep) and by seamcheck,
scored against CodeQL's call graph, on one repo.

    python3 bench/vs_grep.py REPO_DIR CODEQL_CSV --lang python [--sg PATH] [--semgrep PATH] [--n 10] [--semgrep-n 3]
        [--scip INDEX.scip [--scip-prefix DIR]]

(--scip: a SCIP index, as `scip-typescript index` / `scip-python index` write it, which
is what Sourcegraph's precise "find references" serves: a reference written as a call counts.)

(Semgrep scans the whole repo for each question, minutes on a big one:
--semgrep-n limits it to the first few targets; its matches are ast-grep's.)

CODEQL_CSV is what bench/vs_codeql.py left (<work>/<repo>-<lang>.csv). The
targets are the project functions CodeQL sees called most whose name more
than one project function has (`send`, `get`, `copy`): where a name says
least about the callee. For each, a tool's call sites are scored against
CodeQL's callers of that one function:
  precision  of the sites the tool reports, how many CodeQL also has
  recall     of CodeQL's sites, how many the tool reports
CodeQL leaves some calls unresolved that seamcheck resolves (vs_codeql.py's
sc-only), so seamcheck's precision here is a lower bound.
"""
import collections
import csv
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from seamcheck import core, queries  # noqa: E402

_SG_LANG = {"python": "python", "javascript": "javascript", "ruby": "ruby", "java": "java", "go": "go",
            "csharp": "csharp", "typescript": "typescript"}
_SEMGREP_LANG = {"python": "python", "javascript": "js", "ruby": "ruby", "java": "java", "go": "go",
                 "csharp": "csharp", "typescript": "ts"}
_SKIP_DIRS = ("node_modules/", ".git/", "vendor/")


def _cq(path: str):
    callers = collections.defaultdict(set)
    with open(path, newline="") as fh:
        rows = csv.reader(fh)
        next(rows, None)
        for f, ln, name, tf, tl in rows:
            callers[(tf, int(tl), name)].add((f, int(ln)))
    return callers


def _sg(sg: str, root: str, lang: str, name: str) -> set:
    hits = set()
    for pat in (f"$R.{name}($$$A)", f"{name}($$$A)"):
        out = subprocess.run([sg, "run", "-p", pat, "-l", _SG_LANG[lang], "--json=compact", "."],
                             capture_output=True, text=True, cwd=root).stdout or "[]"
        for m in json.loads(out):
            rel = os.path.normpath(m["file"])
            if not rel.startswith(_SKIP_DIRS):
                hits.add((rel, m["range"]["start"]["line"] + 1))
    return hits


def _semgrep(semgrep: str, root: str, lang: str, name: str) -> set:
    """(Semgrep skips test folders unless the repo has its own .semgrepignore: an empty one keeps them.)"""
    hits = set()
    ignore = os.path.join(root, ".semgrepignore")
    made = not os.path.exists(ignore)
    if made:
        open(ignore, "w").close()
    rule = os.path.join(os.environ.get("TMPDIR", "/tmp"), f"seamcheck-vs-grep-{os.getpid()}.yml")
    with open(rule, "w") as fh:  # (both patterns in one rule: one scan)
        fh.write(f"rules:\n- id: calls\n  languages: [{_SEMGREP_LANG[lang]}]\n  severity: INFO\n  message: call\n"
                 f"  pattern-either:\n  - pattern: $R.{name}(...)\n  - pattern: {name}(...)\n")
    try:
        outs = [subprocess.run([semgrep, "--metrics=off", "--quiet", "--json", "--no-git-ignore", "--config", rule, "."],
                               capture_output=True, text=True, cwd=root).stdout or "{}"]
    finally:
        os.remove(rule)
        if made:
            os.remove(ignore)
    for out in outs:
        for r in json.loads(out).get("results", []):
            rel = os.path.normpath(r["path"])
            if not rel.startswith(_SKIP_DIRS):
                hits.add((rel, r["start"]["line"]))
    return hits


def _pb(buf: bytes):
    """(field, wire type, value) of one protobuf message (value: int or bytes)."""
    i, n = 0, len(buf)

    def varint():
        nonlocal i
        shift = out = 0
        while True:
            b = buf[i]
            i += 1
            out |= (b & 0x7F) << shift
            if b < 0x80:
                return out
            shift += 7
    while i < n:
        key = varint()
        field, wt = key >> 3, key & 7
        if wt == 0:
            yield field, wt, varint()
        elif wt == 2:
            ln = varint()
            yield field, wt, buf[i:i + ln]
            i += ln
        elif wt == 5:
            i += 4
        elif wt == 1:
            i += 8
        else:
            return


def _packed(buf: bytes) -> list[int]:
    out, shift, cur = [], 0, 0
    for b in buf:
        cur |= (b & 0x7F) << shift
        shift += 7
        if b < 0x80:
            out.append(cur)
            cur = shift = 0
    return out


def _scip(path: str, prefix: str = "") -> dict:
    """A SCIP index (Sourcegraph's precise navigation): {file: [(line0, col0, end_col, symbol, roles)]}."""
    docs = {}
    with open(path, "rb") as fh:
        data = fh.read()
    for field, _wt, doc in _pb(data):
        if field != 2:
            continue
        rel, occs = "", []
        for f2, _w2, v2 in _pb(doc):
            if f2 == 1:
                rel = v2.decode()
            elif f2 == 2:
                rng, sym, roles = [], "", 0
                for f3, w3, v3 in _pb(v2):
                    if f3 == 1:
                        rng = _packed(v3) if w3 == 2 else rng + [v3]
                    elif f3 == 2:
                        sym = v3.decode()
                    elif f3 == 3:
                        roles = v3
                if len(rng) >= 3:
                    occs.append((rng[0], rng[1], rng[-1], sym, roles))
        docs[os.path.normpath(os.path.join(prefix, rel))] = occs
    return docs


def _scip_callers(docs: dict, root: str, g) -> set:
    """The call sites SCIP's references give for the function g: the symbol defined at g's
    name, each non-definition occurrence of it written as a call (`name(` / `name<T>(`)."""
    texts = {}

    def lines(rel):
        if rel not in texts:
            try:
                with open(os.path.join(root, rel), errors="replace") as fh:
                    texts[rel] = fh.read().split("\n")
            except OSError:
                texts[rel] = []
        return texts[rel]
    syms = {sym for l0, c0, c1, sym, roles in docs.get(g.file, ())
            if roles & 1 and g.start - 2 <= l0 <= g.start + 1 and l0 < len(lines(g.file))
            and lines(g.file)[l0][c0:c1] == g.name}  # (the name at its definition: a `local N` symbol too)
    if not syms:
        return set()
    out = set()
    for rel, occs in docs.items():
        for l0, _c0, c1, sym, roles in occs:
            if sym not in syms or roles & 1 or (sym.startswith("local ") and rel != g.file):
                continue
            if rel not in texts:
                try:
                    with open(os.path.join(root, rel), errors="replace") as fh:
                        texts[rel] = fh.read().split("\n")
                except OSError:
                    texts[rel] = []
            ln = texts[rel][l0] if l0 < len(texts[rel]) else ""
            if re.match(r"\s*(?:\?\.)?\s*(?:<[^()]*>)?\s*\(", ln[c1:]):
                out.add((rel, l0 + 1))
    return out


def _align_lines(got: set, truth: set, p, reach: int = 20) -> set:
    """A site on the line of the call's name moves to the line CodeQL gives the
    whole call expression (a builder chain's start), when that is the nearest
    above and the line continues it (see vs_codeql.continues)."""
    from vs_codeql import continues
    out = set()
    for f, ln in got:
        mods = p.by_file.get(f) or []
        if (f, ln) not in truth and continues(mods[0].raw if mods else None, ln):
            above = [l0 for f0, l0 in truth if f0 == f and ln - reach <= l0 < ln and (f, l0) not in got]
            if above:
                ln = max(above)
        out.add((f, ln))
    return out


def _score(got: set, truth: set) -> tuple[int, int, int]:
    return len(got & truth), len(got), len(truth)


def main() -> None:
    args = sys.argv[1:]

    def opt(name, default=None):
        return args[args.index(name) + 1] if name in args else default
    root, path, lang = os.path.abspath(args[0]), args[1], opt("--lang")
    n = int(opt("--n", 10))
    callers = _cq(path)
    # only files CodeQL analysed count, for every tool (its Go database is `go build`'s: no _test.go)
    seen = {f for sites in callers.values() for f, _ln in sites}
    t0 = time.time()
    p = core.load(root, use_cache=False)
    t_sc = time.time() - t0
    names = collections.Counter(g.name for g in p.funcs.values() if g.kind != "top")

    def func_at(tf, tl, name):
        for g in p.funcs.values():
            if g.file == tf and g.name == name and (g.start <= tl <= max(g.end, g.start) or abs(g.start - tl) <= 3):
                return g
        return None
    picks = []
    for (tf, tl, name), sites in sorted(callers.items(), key=lambda kv: -len(kv[1])):
        if name in ("exports", "constructor", "<init>", "__init__", "default"):
            continue  # (called as `new X` / by the importer's name: no tool asks by this name)
        if names[name] > 1 and not name.startswith("__") and (g := func_at(tf, tl, name)) is not None:
            if any(g is g0 for g0, _ in picks):  # (an overload CodeQL lists apart: the same function here)
                picks = [(g0, t0 | sites if g0 is g else t0) for g0, t0 in picks]
                continue
            picks.append((g, set(sites)))
        if len(picks) == n:
            break
    # a function's overloads are one function to seamcheck (and one name to a search): CodeQL's
    # callers of each overload in that file count for it
    picks = [(g, set().union(t, *(sites for (tf, tl, name), sites in callers.items()
                                  if tf == g.file and name == g.name and func_at(tf, tl, name) in (None, g))))
             for g, t in picks]
    tools = {"seamcheck": [0, 0, 0], "seamcheck --wide": [0, 0, 0], "ast-grep": [0, 0, 0], "semgrep": [0, 0, 0], "scip": [0, 0, 0]}
    docs = _scip(opt("--scip"), opt("--scip-prefix", "")) if opt("--scip") else None
    if docs is not None:  # (every tool scored on the files both CodeQL and the SCIP indexer read)
        seen &= set(docs)
        picks = [(g, {x for x in truth if x[0] in seen}) for g, truth in picks]
        picks = [(g, truth) for g, truth in picks if truth]
    times = collections.Counter()
    print(f"{os.path.basename(root)}: {len(picks)} functions with shared names, scored against CodeQL's callers")
    for g, truth in picks:
        mine = {(p.funcs[s].file, ln) for s, es in p.edges.items() if s in p.funcs
                for d, ln, ex in es if ex and d == g.id}
        mine = _align_lines(mine, truth, p)
        row = {"seamcheck": _score({x for x in mine if x[0] in seen}, truth)}
        # `callers --wide`: exact, then name guesses, then calls by that name on a library-typed value
        wide = mine | {(p.funcs[s].file, ln) for s, es in p.edges.items() if s in p.funcs
                       for d, ln, ex in es if not ex and d == g.id}
        wide |= {(p.funcs[fid].file, ln) for fid, ln in queries.possible_callers(p, g)}
        row["seamcheck --wide"] = _score(_align_lines({x for x in wide if x[0] in seen}, truth, p), truth)
        if opt("--sg"):
            t = time.time()
            row["ast-grep"] = _score({x for x in _sg(opt("--sg"), root, lang, g.name) if x[0] in seen}, truth)
            times["ast-grep"] += time.time() - t
        if docs is not None:
            row["scip"] = _score(_align_lines({x for x in _scip_callers(docs, root, g) if x[0] in seen}, truth, p), truth)
        if opt("--semgrep") and picks.index((g, truth)) < int(opt("--semgrep-n", 3)):
            t = time.time()
            row["semgrep"] = _score({x for x in _semgrep(opt("--semgrep"), root, lang, g.name) if x[0] in seen}, truth)
            times["semgrep"] += time.time() - t
        cells = []
        for tool, (hit, got, tr) in row.items():
            tools[tool] = [a + b for a, b in zip(tools[tool], (hit, got, tr))]
            cells.append(f"{tool} {hit}/{got}")
        print(f"  {g.id:60.60s} CodeQL {len(truth):3d}  " + "  ".join(cells))
    print(f"  (seamcheck's graph: {t_sc:.1f}s for every question; ast-grep {times['ast-grep']:.1f}s, "
          f"semgrep {times['semgrep']:.1f}s for these {len(picks)})")
    for tool, (hit, got, tr) in tools.items():
        if got or tool == "seamcheck":
            print(f"  {tool:16s} precision {100 * hit / max(got, 1):5.1f}%  recall {100 * hit / max(tr, 1):5.1f}%")


if __name__ == "__main__":
    main()
