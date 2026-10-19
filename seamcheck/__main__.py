"""seamcheck: see the whole codebase at once, and check the rules between its
parts. Deterministic, local, language-agnostic.

  seamcheck check [--json] [--html out.html] [--only id,id] [--errors-only]
  seamcheck map --html out.html          interactive map of modules + calls + findings
  seamcheck callers NAME [--depth N]     who calls it (transitively; exact edges, --fuzzy adds guesses)
  seamcheck callees SPEC [--depth N]     what it calls (same)
  seamcheck path FROM TO                 shortest call chain (exact; --fuzzy through guesses)
  seamcheck find NAME                    where a function is defined
  seamcheck dead                         functions nothing calls or references
  seamcheck stats                        size, parse time, graph density
  seamcheck matrix [RULE] [--all]        a fanout rule's token x view grid (all fanout rules if none;
                                         --all also shows what views handle that nothing produces)
Tidy (see tidy.py; `[tidy]` config):
  seamcheck tidy                         one compact report: hotspots, dups, test-only, unused, tests
  seamcheck tests [--limit N]            tests reaching no product code; tests grepping product source
  seamcheck test-only                    product functions only tests call (dead code tests keep alive)
  seamcheck unused                       product functions nothing calls or names anywhere
  seamcheck dups                         near-duplicate functions across files, by file pair
  seamcheck split FILE                   a big file's features and call clusters (where it splits)
  seamcheck hotspots                     biggest files: where splitting pays
  seamcheck remove ID... [--apply]       delete functions / test blocks with their docs (dry by default)
  seamcheck refs NAME...                 where a name is defined, called, and mentioned (scripts, config too)
  --since REF  tidy reports: only files changed since REF (check your own change: --since HEAD~1)
  --ids        test-only / unused / tests: print only ids (pipe into `remove`)
  seamcheck gate [--since REF] [--staged] may this change land? rule errors anywhere, plus dead code,
                                         copies and do-nothing tests in the lines it touched (exit 1)
  seamcheck impact TARGET... [--depth N]  what a change could break: callers + bridged views, by area
                                         (TARGET: a function spec or a file path; or --since REF:
                                         the functions your diff touched, e.g. --since HEAD)
  seamcheck deps [NAME] [--all]          declared dependencies vs use: unused, test-only,
                                         only-in-dead-code (--all: every dep; NAME: where it's used)

Runtime (the program reports itself as `SEAMCHECK {json}` lines, see runtime.py):
  seamcheck trace LOG [LOG...]           check finished runs' output ('-' = stdin)
  seamcheck run [--log F] -- CMD...      run a command and check what it reports
  seamcheck fuzz --seeds 1-8 -- CMD...   run CMD once per seed ({seed} in CMD, or $SEAMCHECK_SEED);
                                         lists the seeds that broke a rule, with the replay command

  -C DIR       project root (default: the nearest parent with seamcheck.toml, else .)
  --config F   more config (repeatable; a file or a folder of .toml). Always read too:
               seamcheck.toml and seamcheck.d/*.toml at the project root
SPEC is `name`, `Module.name`, or `path/glob:funcglob`.
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import sys
import time

from . import core


def _root(start: str) -> str:
    d = os.path.abspath(start)
    while True:
        if os.path.exists(os.path.join(d, "seamcheck.toml")):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return os.path.abspath(start)
        d = parent


def _fmt(p: core.Project, fid: str) -> str:
    f = p.funcs.get(fid)
    via = sorted({b for (a, d), b in p.bridged.items() if fid in (a, d)})
    return (f"{fid}  ({f.file}:{f.start})" if f else fid) + (f"  [via {', '.join(via)}]" if via else "")


def _tighten(root: str, extra: list[str]) -> int:
    """`seamcheck tighten`: run each command rule's `tighten` (its own allowlist
    rewrite, e.g. `node design/check_surface_kit.mjs --write-allow`). Those
    rewrites refuse to grow a list, so this only drops counts and gone rows:
    allowlists stop drifting until a lane trips over them."""
    import subprocess
    from . import config as config_mod
    config, problems = config_mod.load(root, extra)
    for pr in problems:
        print(f"config: {pr}")
    failed = 0
    for r in config.get("rule", []):
        cmd = r.get("tighten")
        if not cmd:
            continue
        res = subprocess.run(cmd, shell=True, cwd=root, capture_output=True, text=True, timeout=600)
        tail = (res.stdout + res.stderr).strip().splitlines()[-1:] or [""]
        print(f"{'ok ' if res.returncode == 0 else 'FAIL'} {r['id']}: {tail[0]}")
        failed += res.returncode != 0
    return 1 if failed or problems else 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="seamcheck", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", choices=["check", "map", "callers", "callees", "path", "find", "dead", "stats", "deps", "matrix", "tidy", "tests", "test-only", "unused", "dups", "split", "hotspots", "remove", "refs", "impact", "gate",
                                    "trace", "run", "fuzz", "tighten"])
    ap.add_argument("args", nargs="*")
    ap.add_argument("-C", dest="root", default=".")
    ap.add_argument("--config", action="append", default=[])
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--html")
    ap.add_argument("--only")
    ap.add_argument("--skip", help="check: leave out these rule ids")
    ap.add_argument("--staged", action="store_true", help="gate: only what is staged (pre-commit)")
    ap.add_argument("--depth", type=int, default=2)
    ap.add_argument("--errors-only", action="store_true")
    ap.add_argument("--no-cache", action="store_true")
    ap.add_argument("--exact", action="store_true", help="(the default now; kept so old commands still run)")
    ap.add_argument("--log", help="run: also save the command's full output here")
    ap.add_argument("--echo", action="store_true", help="run: pass the command's output through")
    ap.add_argument("--timeout", type=float, default=0.0, help="run / fuzz: kill the command after N seconds")
    ap.add_argument("--seeds", default="1-4", help="fuzz: seeds, e.g. 1-8 or 3,7,19")
    ap.add_argument("--jobs", type=int, default=4, help="fuzz: runs at once")
    ap.add_argument("--limit", type=int, default=20, help="tidy reports: rows shown per list")
    ap.add_argument("--fuzzy", action="store_true", help="callers / callees / path / impact: also follow name-guess edges")
    ap.add_argument("--since", help="tidy reports: only files changed since this git ref; impact: start from the functions changed since it")
    ap.add_argument("--ids", action="store_true", help="tidy reports: only ids, one per line")
    ap.add_argument("--apply", action="store_true", help="remove: write the files (default: show what would go)")
    ap.add_argument("--all", action="store_true", help="deps: every dependency, not only the actionable ones")
    ap.add_argument("--strict", action="store_true", help="runtime: also flag rules that never had anything to check")
    # `run -- CMD...`: the command after `--` is ours to split off (Python
    # 3.11's argparse refuses a `--` after a positional that already matched)
    argv = list(sys.argv[1:] if argv is None else argv)
    tail = argv[argv.index("--") + 1:] if "--" in argv else []
    a = ap.parse_args(argv[:len(argv) - len(tail) - 1] if "--" in argv else argv)
    a.args += tail
    root = _root(a.root) if not a.config else os.path.abspath(a.root)
    t0 = time.time()
    if a.cmd in ("trace", "run", "fuzz"):
        from . import runcmd
        return runcmd.main(a, root)
    if a.cmd == "tighten":
        return _tighten(root, a.config)
    p = core.load(root, a.config, use_cache=not a.no_cache)

    if a.cmd in ("check", "map"):
        findings = (core.run_rules(p, a.only.split(",") if a.only else None, a.skip.split(",") if a.skip else None)
                    if a.cmd == "check" or a.html else [])
        if a.errors_only:
            findings = [f for f in findings if f.severity == "error"]
        if a.html:
            from .report import write_html
            write_html(p, findings, a.html)
        if a.cmd == "map":
            print(f"map: {len(p.modules)} modules, {len(p.funcs)} functions -> {a.html or '(use --html out.html)'}")
            return 0
        if a.json:
            print(json.dumps([f.__dict__ for f in findings], indent=1))
        else:
            cur = None
            for f in findings:
                if f.rule != cur:
                    cur = f.rule
                    rule = next((r for r in p.config.get("rule", []) if r["id"] == cur), {})
                    print(f"\n[{f.severity}] {cur}" + (f"  - {rule.get('why')}" if rule.get("why") else ""))
                where = f"{f.file}:{f.line}" + (f" ({f.func})" if f.func else "")
                print(f"  {where}  {f.message}")
                if f.snippet:
                    print(f"      {f.snippet}")
            errs = sum(1 for f in findings if f.severity == "error")
            warns = len(findings) - errs
            print(f"\nseamcheck: {len(p.config.get('rule', []))} rules, {len(p.files)} files, {len(p.funcs)} functions; "
                  f"{errs} errors, {warns} warnings  ({int((time.time() - t0) * 1000)} ms)")
        return 1 if any(f.severity == "error" for f in findings) else 0

    if a.cmd == "impact":
        if not a.args and not a.since:
            ap.error("impact needs a function spec, a file, or --since REF")
        start = set()
        if a.since:
            # the functions the diff touched (committed since REF, staged, unstaged, untracked)
            from . import tidy
            for rel, lines in tidy.changed_lines(p, a.since, a.staged, deletions=True).items():
                for m in p.by_file.get(rel, []):
                    for ln in lines:
                        f = m.func_at(ln)
                        if f and f.kind != "top":
                            start.add(f.id)
            if not start:
                print(f"no functions changed since {a.since}")
                return 0
            if not a.json:
                print(f"since {a.since}: {len(start)} functions changed")
        for t in a.args:
            if t in p.by_file or os.path.exists(os.path.join(p.root, t)):
                start |= {f.id for m in p.by_file.get(t, []) for f in m.funcs}
            else:
                start |= {f.id for f in p.find(t)}
        if not start:
            print(f"nothing matches {' '.join(a.args)}")
            return 1
        dist = core.impact(p, start, max(a.depth, 4) if a.depth == 2 else a.depth, fuzzy=a.fuzzy)
        areas = p.config.get("project", {}).get("areas") or {}
        def area(rel):
            return next((k for k, g in areas.items() if core.match_any(rel, g)), rel.split("/")[0])
        by = collections.defaultdict(list)
        for fid, d in dist.items():
            if d and fid in p.funcs:
                by[area(p.funcs[fid].file)].append((d, fid))
        own = {area(p.funcs[s].file) for s in start if s in p.funcs}
        if a.json:
            print(json.dumps({k: sorted(v) for k, v in by.items()}, indent=1))
            return 0
        print(f"change to {len(start)} functions in {', '.join(sorted(own))} reaches:")
        for k, v in sorted(by.items(), key=lambda kv: (kv[0] in own, -len(kv[1]))):
            files = sorted({p.funcs[f].file for _, f in v})
            mark = "" if k in own else "   <- another view: check it"
            print(f"  {k}: {len(v)} functions in {len(files)} files{mark}")
            for d, fid in sorted(v)[:a.limit if k not in own else min(a.limit, 5)]:
                print(f"    {'  ' * (d - 1)}{_fmt(p, fid)}")
        return 0

    if a.cmd in ("callers", "callees"):
        if not a.args:
            ap.error(f"{a.cmd} needs a function")
        starts = p.find(a.args[0])
        if not starts:
            print(f"no function matches {a.args[0]!r}")
            return 1
        rev = a.cmd == "callers"
        word = "callers" if rev else "callees"
        for s in starts:
            ex, guess = core.edge_split(p, s.id, reverse=rev)
            print(f"{_fmt(p, s.id)}  [{ex} exact, {guess} name-guess {word}{'' if a.fuzzy else ', walking exact'}]")
            dist = core.reach(p, s.id, a.depth, reverse=rev, exact_only=not a.fuzzy)
            for k, d in sorted(dist.items(), key=lambda kv: (kv[1], kv[0])):
                if d:
                    print(f"  {'  ' * (d - 1)}{'<-' if rev else '->'} {_fmt(p, k)}")
            if ex == 0 and not a.fuzzy:
                # (a miss must not read as "nothing calls it")
                print(f"  0 exact {word}" + (f"; {guess} by name guess only (--fuzzy shows them)" if guess else "; none by name guess either"))
        return 0

    if a.cmd == "path":
        if len(a.args) < 2:
            ap.error("path needs FROM and TO")
        froms, tos = p.find(a.args[0]), {f.id for f in p.find(a.args[1])}
        def shortest(exact_only):
            best = []
            for f in froms:
                for t in tos:
                    c = core.path(p, f.id, t, exact_only=exact_only)
                    if c and (not best or len(c) < len(best)):
                        best = c
            return best
        best = shortest(not a.fuzzy)
        if not best:
            if not a.fuzzy and shortest(False):
                print("no exact call chain; one exists through name guesses (--fuzzy shows it)")
            else:
                print("no call chain found")
            return 1
        for i, fid in enumerate(best):
            print(f"{'  ' * i}{'-> ' if i else ''}{_fmt(p, fid)}")
        return 0

    if a.cmd == "find":
        for f in p.find(a.args[0] if a.args else "*"):
            ex, guess = core.edge_split(p, f.id)
            print(f"{f.id}  {f.file}:{f.start}-{f.end}  calls={len(f.calls)} callers: {ex} exact, {guess} name-guess")
        return 0

    if a.cmd == "dead":
        for f in sorted(core.dead(p), key=lambda f: (f.file, f.start)):
            print(f"{f.file}:{f.start}  {f.id}")
        return 0

    if a.cmd in ("tidy", "tests", "test-only", "unused", "dups", "split", "hotspots", "remove", "refs", "gate"):
        from . import tidy_cli
        return tidy_cli.main(a, p, t0)

    if a.cmd == "matrix":
        rules = [r for r in p.config.get("rule", []) if r.get("kind") == "fanout" and (not a.args or r["id"] in a.args)]
        if not rules:
            print("no fanout rules" + (f" named {a.args[0]!r}" if a.args else ""))
            return 1
        mark = {"ok": "+", "except": "~", "fallback": "generic", "missing": "MISSING", "extra": "extra", "": "."}
        bad = 0
        for r in rules:
            toks, views, cell = core.matrix(p, r)
            w = max([len(t) for t in toks] + [8])
            print(f"\n{r['id']}" + (f"  - {r['why']}" if r.get("why") else ""))
            print("  " + " " * w + "".join(f"{v:>10}" for v in views))
            show_extra = a.all or "extra" in r.get("check", ["missing"])
            for t in toks:
                row = [cell[(t, v)] for v in views]
                if not show_extra and all(c in ("extra", "") for c in row):
                    continue
                bad += row.count("missing")
                print(f"  {t:<{w}}" + "".join(f"{mark[c]:>10}" for c in row))
        print("\n+ handled   ~ intentional gap (except)   generic: the view's fallback host   "
              "MISSING not handled   extra: handled, never produced")
        return 1 if bad else 0

    if a.cmd == "deps":
        from . import deps
        found = deps.analyze(p)
        if a.json:
            print(json.dumps([dict(d.__dict__, files=d.files) for d in found], indent=1))
        else:
            print(deps.report(found, a.all, a.args[0] if a.args else None))
            if not a.args:
                n = sum(1 for d in found if d.status in deps.ACTIONABLE)
                print(f"\nseamcheck deps: {len(found)} dependencies, {n} to look at  ({int((time.time() - t0) * 1000)} ms)")
        return 1 if any(d.status in deps.ACTIONABLE for d in found) and not a.args else 0

    if a.cmd == "stats":
        edges = sum(len(v) for v in p.edges.values())
        exact = sum(1 for v in p.edges.values() for e in v if e[2])
        langs = {}
        for rel in p.by_file:
            ext = os.path.splitext(rel)[1]
            langs[ext] = langs.get(ext, 0) + 1
        print(f"root {p.root}\nfiles {len(p.files)} (parsed {len(p.by_file)}: {langs})\nmodules {len(p.modules)}  "
              f"functions {len(p.funcs)}\ncall edges {edges} (exact {exact}, fuzzy {edges - exact})\n"
              f"load {p.parse_ms} ms, total {int((time.time() - t0) * 1000)} ms")
        print("call sites resolved (exact / name guess only / nothing), and of the calls that reach project")
        print("code (exact + guess) how many are certain; \"nothing\" is mostly the language's own library:")
        for ext, (ex, fz, no) in sorted(p.resolution.items(), key=lambda kv: -sum(kv[1])):
            n = ex + fz + no
            own = f"project calls exact {100 * ex // (ex + fz):>3}%" if ex + fz else ""
            print(f"  {ext or '(none)':<6} {n:>7}  exact {100 * ex // n:>3}%  guess {100 * fz // n:>3}%  "
                  f"nothing {100 * no // n:>3}%   {own}")
        return 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
