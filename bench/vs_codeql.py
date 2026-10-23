"""seamcheck's call edges against CodeQL's call graph, on one repo.

    python3 bench/vs_codeql.py REPO_DIR --lang python --codeql PATH/TO/codeql [--show N] [--work DIR]
    python3 bench/vs_codeql.py REPO_DIR --csv CODEQL_CSV [--show N]

With --codeql it builds the database (`--build-mode=none` where the language
has one), runs bench/codeql/<lang>/calls.ql and times both; --csv takes rows
already decoded: call file, call line, callee name, callee file, callee
line. CodeQL builds a full database and
runs a points-to / type-tracking analysis; it is the closest thing to ground
truth that runs offline on the same checkout.

A call site is (file, line, callee name). Reported:
  agree      seamcheck's exact edges at a site CodeQL resolved: all among CodeQL's targets
  wider      CodeQL's target and more: overrides the receiver's declared type allows, where
             CodeQL's data flow saw the one runtime class
  wrong      no target in common with CodeQL
  covered    CodeQL's sites where seamcheck has an exact edge to a CodeQL target
  sc-only    sites seamcheck calls exact that CodeQL left unresolved (sampled: who is right?)
  by-name    what a name-only tool (grep, ast-grep, Semgrep) offers at CodeQL's sites:
             how often the name has more than one project definition, and how many
"""
import collections
import csv
import os
import random
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402


HERE = os.path.dirname(os.path.abspath(__file__))
# CodeQL's own scopes for a comprehension or a lambda: no call in the source
_SYNTHETIC = {"genexpr", "listcomp", "dictcomp", "setcomp", "lambda", "<lambda>"}
_BUILD_NONE = {"java", "csharp", "cpp"}  # (no build needed: Python, JS/TS, Ruby; Go builds itself)


def _codeql(path: str) -> dict:
    sites = collections.defaultdict(set)
    with open(path, newline="") as fh:
        rows = csv.reader(fh)
        next(rows, None)
        for f, ln, name, tf, tl in rows:
            if name not in _SYNTHETIC:
                sites[(f, int(ln), name)].add((tf, int(tl)))
    return sites


def continues(raw, ln: int) -> bool:
    """Line `ln` goes on with the expression above it: `.create()` under `new GsonBuilder()`."""
    if raw is None or not (1 < ln <= len(raw)):
        return False
    here, above = raw[ln - 1].lstrip(), raw[ln - 2].rstrip()
    return here.startswith((".", ")", "?.")) or above.endswith((".", "(", ",", "->", "=>"))


def _align(sites: dict, cq: dict, raw_of, reach: int = 20) -> dict:
    """seamcheck places a call on the line of its name (`.create(` in a builder
    chain), CodeQL on the line its whole expression starts (`new GsonBuilder()`):
    a seamcheck site with no CodeQL site of its own, on a line continuing the
    expression above, moves to the nearest same-named CodeQL site at most
    `reach` lines above it."""
    starts = collections.defaultdict(list)
    for f, ln, n in cq:
        starts[(f, n)].append(ln)
    out = collections.defaultdict(set)
    for (f, ln, n), v in sites.items():
        if (f, ln, n) not in cq and continues(raw_of(f), ln):
            above = [l0 for l0 in starts.get((f, n), ()) if ln - reach <= l0 < ln and (f, l0, n) not in sites]
            if above:
                ln = max(above)
        out[(f, ln, n)] |= set(v)
    return out


def _run_codeql(root: str, lang: str, codeql: str, work: str) -> tuple[str, float, float]:
    """(csv path, seconds to build the database, seconds to run the query)."""
    os.makedirs(work, exist_ok=True)
    db = os.path.join(work, os.path.basename(root.rstrip("/")) + "-" + lang)
    t0 = time.time()
    cmd = [codeql, "database", "create", db, f"--language={lang}", f"--source-root={root}", "--overwrite", "-q"]
    if lang in _BUILD_NONE:
        cmd.append("--build-mode=none")
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    t1 = time.time()
    qdir = os.path.join(HERE, "codeql", lang)
    subprocess.run([codeql, "pack", "install", qdir], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    bqrs = db + ".bqrs"
    subprocess.run([codeql, "query", "run", "-d", db, "-o", bqrs, os.path.join(qdir, "calls.ql")], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    t2 = time.time()
    out = db + ".csv"
    with open(out, "w") as fh:
        subprocess.run([codeql, "bqrs", "decode", "--format=csv", bqrs], check=True, stdout=fh, stderr=subprocess.DEVNULL)
    return out, t1 - t0, t2 - t1


def _same(g, target, raw=None) -> bool:
    """A seamcheck function and a CodeQL callee location are one function
    (CodeQL places a decorated Python def at its `def`, seamcheck may start at
    the decorator). seamcheck gives a method's overloads one id (Java's three
    `toJson`): CodeQL's overload counts as that function when its line
    declares the same name in the same file."""
    tf, tl = target
    if g.file != tf:
        return False
    if g.start <= tl <= max(g.end, g.start) or abs(g.start - tl) <= 3:
        return True
    return raw is not None and 0 < tl <= len(raw) and re.search(rf"\b{re.escape(g.name)}\s*[(<]", raw[tl - 1]) is not None


def main() -> None:
    args = sys.argv[1:]

    def opt(name, default=None):
        return args[args.index(name) + 1] if name in args else default
    root = os.path.abspath(args[0])
    show = int(opt("--show", 0))
    timing = ""
    if opt("--csv"):
        path = opt("--csv")
    else:
        work = opt("--work", os.path.join(os.environ.get("TMPDIR", "/tmp"), "seamcheck-vs-codeql"))
        path, t_db, t_q = _run_codeql(root, opt("--lang"), opt("--codeql", "codeql"), work)
        timing = f"CodeQL database {t_db:.0f}s + query {t_q:.0f}s"
    cq = _codeql(path)
    t0 = time.time()
    p = core.load(root, use_cache=False)
    timing = (timing + "; " if timing else "") + f"seamcheck {time.time() - t0:.1f}s (cold, no cache)"
    exact, guess = collections.defaultdict(set), collections.defaultdict(set)
    for src, es in p.edges.items():
        f = p.funcs.get(src)
        if f is None:
            continue
        for dst, ln, ex in es:
            g = p.funcs.get(dst)
            if g is None:
                continue
            (exact if ex else guess)[(f.file, ln, g.name)].add(g.id)
    def raw_of(f):
        mods = p.by_file.get(f) or []
        return mods[0].raw if mods else None
    exact = _align(exact, cq, raw_of)
    guess = _align(guess, cq, raw_of)
    files = {k[0] for k in cq}
    raws = {}

    def same(g, t):
        if t[0] not in raws:
            mods = p.by_file.get(t[0]) or []
            raws[t[0]] = mods[0].raw if mods else None
        return _same(g, t, raws[t[0]])
    agree = disagree = covered = wider = 0
    bad, only = [], []
    for site, targets in cq.items():
        got = [p.funcs[i] for i in exact.get(site, ())]
        if got:
            if all(any(same(g, t) for t in targets) for g in got):
                agree += 1
            elif any(same(g, t) for g in got for t in targets):
                wider += 1  # CodeQL's callee and overrides of it: the receiver's declared type allows them
            else:
                disagree += 1
                bad.append((site, sorted(g.id for g in got), sorted(targets)))
            if any(same(g, t) for g in got for t in targets):
                covered += 1
    for site, got in exact.items():
        if site[0] in files and site not in cq:
            only.append((site, sorted(got)))
    names = collections.Counter(g.name for g in p.funcs.values() if g.kind != "top")
    ambiguous = sum(1 for (_f, _l, n) in cq if names[n] > 1)
    mean = sum(names[n] for (_f, _l, n) in cq) / max(len(cq), 1)
    both = agree + disagree + wider
    print(f"{os.path.basename(root.rstrip('/'))}: CodeQL resolved {len(cq)} call sites; seamcheck exact at {len(exact)} sites")
    print(f"  time       {timing}")
    print(f"  agree      {agree}/{both} = {100 * agree / max(both, 1):.1f}% of the sites both resolved")
    print(f"  wider      {wider}/{both} = {100 * wider / max(both, 1):.1f}%: CodeQL's callee plus overrides the declared type allows")
    print(f"  wrong      {disagree}/{both} = {100 * disagree / max(both, 1):.1f}%: no callee in common")
    print(f"  covered    {covered}/{len(cq)} = {100 * covered / max(len(cq), 1):.1f}% of CodeQL's sites")
    print(f"  sc-only    {len(only)} exact sites CodeQL left unresolved")
    print(f"  by-name    {ambiguous}/{len(cq)} = {100 * ambiguous / max(len(cq), 1):.1f}% of CodeQL's sites name a function "
          f"defined more than once (mean {mean:.1f} definitions per name)")
    missed = [(site, sorted(guess.get(site, ()))) for site, targets in cq.items()
              if not any(same(p.funcs[i], t) for i in exact.get(site, ()) for t in targets)]
    kinds = collections.Counter("guess" if g else "none" for _s, g in missed)
    print(f"  missed     {len(missed)} of CodeQL's sites: seamcheck guessed at {kinds['guess']}, "
          f"had no edge at {kinds['none']}")
    if show:
        random.seed(1)
        for row in random.sample(missed, min(show, len(missed))):
            print("  MISSED  ", row, sorted(cq[row[0]]))
        for row in random.sample(bad, min(show, len(bad))):
            print("  DISAGREE", row)
        for row in random.sample(only, min(show, len(only))):
            print("  SC-ONLY ", row)


if __name__ == "__main__":
    main()
