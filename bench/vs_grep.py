"""'Who calls X?' answered by name (ast-grep, Semgrep) and by seamcheck,
scored against CodeQL's call graph, on one repo.

    python3 bench/vs_grep.py REPO_DIR CODEQL_CSV --lang python [--sg PATH] [--semgrep PATH] [--n 10] [--semgrep-n 3]

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
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

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


def _score(got: set, truth: set) -> tuple[int, int, int]:
    return len(got & truth), len(got), len(truth)


def main() -> None:
    args = sys.argv[1:]

    def opt(name, default=None):
        return args[args.index(name) + 1] if name in args else default
    root, path, lang = os.path.abspath(args[0]), args[1], opt("--lang")
    n = int(opt("--n", 10))
    callers = _cq(path)
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
        if names[name] > 1 and not name.startswith("__") and (g := func_at(tf, tl, name)) is not None:
            picks.append((g, sites))
        if len(picks) == n:
            break
    tools = {"seamcheck": [0, 0, 0], "ast-grep": [0, 0, 0], "semgrep": [0, 0, 0]}
    times = collections.Counter()
    print(f"{os.path.basename(root)}: {len(picks)} functions with shared names, scored against CodeQL's callers")
    for g, truth in picks:
        mine = {(p.funcs[s].file, ln) for s, es in p.edges.items() if s in p.funcs
                for d, ln, ex in es if ex and d == g.id}
        row = {"seamcheck": _score(mine, truth)}
        if opt("--sg"):
            t = time.time()
            row["ast-grep"] = _score(_sg(opt("--sg"), root, lang, g.name), truth)
            times["ast-grep"] += time.time() - t
        if opt("--semgrep") and picks.index((g, truth)) < int(opt("--semgrep-n", 3)):
            t = time.time()
            row["semgrep"] = _score(_semgrep(opt("--semgrep"), root, lang, g.name), truth)
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
            print(f"  {tool:10s} precision {100 * hit / max(got, 1):5.1f}%  recall {100 * hit / max(tr, 1):5.1f}%")


if __name__ == "__main__":
    main()
