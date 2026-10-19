"""Output for the tidy commands: compact by default (a report should cost an
agent a screenful, not a context window), --json for tools, --ids for piping
into `seamcheck remove`."""
from __future__ import annotations

import collections
import json
import time

from . import tidy


def _loc(r: dict) -> str:
    return f"{r['file']}:{r['line']}"


def _short(fid: str) -> str:
    return fid.rsplit("/", 1)[-1]


def _tests_summary(rows: list[dict], limit: int) -> list[str]:
    out = []
    by = collections.defaultdict(list)
    for r in rows:
        by[r["suite"]].append(r)
    for s, rs in sorted(by.items()):
        zero = [r for r in rs if r["reach"] == 0]
        grep = [r for r in rs if r["reads_source"]]
        out.append(f"  {s}: {len(rs)} tests, {len(zero)} reach no product code, {len(grep)} read product source")
    files = collections.Counter(r["file"] for r in rows if r["reach"] == 0)
    if files:
        out.append("  files with the most tests reaching nothing: " +
                   ", ".join(f"{f.rsplit('/', 1)[-1]} {n}" for f, n in files.most_common(limit // 2 or 1)))
    return out


def main(a, p, t0) -> int:
    lim = a.limit
    only = tidy.changed_files(p, a.since) if a.since else None
    keep = (lambda rel: rel in only) if only is not None else (lambda rel: True)

    if a.cmd == "gate":
        rep = tidy.gate(p, a.since or "HEAD", a.staged)
        whys = {r["id"]: r.get("why", "") for r in p.config.get("rule", [])}
        if a.json:
            # one flat list an agent can act on: where, what, why it matters, how to fix
            fix = {"forbidden": "retire the old path instead of naming a second one",
                   "unused": "delete it, or call it from product code", "test_only": "delete it with its tests, or wire it up",
                   "dups": "keep one copy and call it", "tests": "make the test call product code (or delete it)",
                   "moved_refs": "point it at where the code went (or a glob), or `seamcheck: allow moved-ref`"}
            flat = [{"kind": "rule", "rule": f.rule, "severity": f.severity, "file": f.file, "line": f.line,
                     "message": f.message, "why": whys.get(f.rule, ""), "fix": ""} for f in rep["rules"]]
            for k in ("forbidden", "unused", "test_only", "tests"):
                for r in rep[k]:
                    flat.append({"kind": k, "rule": k, "severity": "error", "file": r["file"], "line": r["line"],
                                 "message": r.get("id") or r.get("word", ""), "why": "", "fix": fix[k]})
            for r in rep.get("moved_refs", []):
                flat.append({"kind": "moved_refs", "rule": "moved-ref", "severity": "error", "file": r["file"],
                             "line": r["line"], "message": f"names {r['moved_from']}, which this change split up",
                             "why": "a check pinned to one file checks less after a split", "fix": fix["moved_refs"]})
            for d in rep["dups"]:
                flat.append({"kind": "dups", "rule": "dups", "severity": "error", "file": d["a"], "line": 0,
                             "message": f"copy of {d['b']} ({d['similarity']})", "why": "", "fix": fix["dups"]})
            print(json.dumps({"blocked": tidy.gate_failed(rep), "changed_files": rep["changed_files"], "findings": flat,
                              "ms": int((time.time() - t0) * 1000)}, indent=1, default=list))
            return 1 if tidy.gate_failed(rep) else 0
        shown = set()
        for f in rep["rules"]:
            print(f"  rule {f.rule}: {f.file}:{f.line}  {f.message}")
            if whys.get(f.rule) and f.rule not in shown:
                shown.add(f.rule)
                print(f"      why: {whys[f.rule]}")
        for r in rep["forbidden"]:
            print(f"  new code names '{r['word']}': {r['file']}:{r['line']}  (retire the old path instead)")
        for r in rep["unused"]:
            print(f"  unused (nothing calls or names it): {r['id']}  {r['file']}:{r['line']}")
        for r in rep["test_only"]:
            print(f"  only tests call it: {r['id']}  {r['file']}:{r['line']}")
        for d in rep["dups"]:
            print(f"  copy ({d['similarity']}): {d['a']}  ~  {d['b']}")
        for r in rep["tests"]:
            why = f"reads {', '.join(r['reads_source'])}" if r["reads_source"] else "reaches no product code"
            print(f"  test {why}: {r['id'].rsplit('.test ', 1)[-1]}  {r['file']}:{r['line']}")
        for r in rep.get("moved_refs", []):
            print(f"  names {r['moved_from']}, which this change split up: {r['file']}:{r['line']}"
                  "  (point it at the new files or a glob)")
        bad = tidy.gate_failed(rep)
        n = len(rep["changed_files"])
        print(f"seamcheck gate: {n} changed files; {'BLOCKED' if bad else 'clear'}"
              f"  ({int((time.time() - t0) * 1000)} ms)")
        return 1 if bad else 0

    if a.cmd == "refs":
        if not a.args:
            print("refs needs a name")
            return 2
        corpus = tidy.Corpus(p)
        for name in a.args:
            r = tidy.refs(p, name, corpus)
            if a.json:
                print(json.dumps(r, indent=1))
                continue
            print(f"{name}:")
            for k in ("defined", "callers", "mentions"):
                vals = r[k]
                print(f"  {k} ({len(vals)}): " + ("; ".join(vals[:lim]) if vals else "-") + (" ..." if len(vals) > lim else ""))
        return 0
    if a.cmd == "remove":
        if not a.args:
            print("remove needs function ids (as `seamcheck find` / `--ids` print them)")
            return 2
        try:
            done = tidy.remove(p, a.args, dry=not a.apply)
        except KeyError as e:
            print(e.args[0])
            return 1
        for line in done:
            print(("removed " if a.apply else "would remove ") + line)
        if not a.apply:
            print("(dry run: --apply writes the files)")
        return 0

    if a.cmd == "split":
        if not a.args:
            print("split needs a file")
            return 2
        for rel in a.args:
            r = tidy.split(p, rel)
            if a.json:
                print(json.dumps(r, indent=1))
                continue
            print(f"{rel}: {r['functions']} functions, {r['lines']} lines"
                  + ("  (knotted: one call cluster holds most of it; split by feature)" if r["knotted"] else ""))
            print("  features: " + ", ".join(f"{x['name']} {x['lines']}" for x in r["features"][:lim]))
            if r["shared_helpers"]:
                print("  shared helpers (move out first): " + ", ".join(r["shared_helpers"][:lim]))
            for c in r["clusters"][:lim]:
                print(f"  cluster {c['functions']} fns / {c['lines']} lines: {', '.join(c['examples'])}")
        return 0

    if a.cmd == "hotspots":
        rows = tidy.hotspots(p, lim)
        if a.json:
            print(json.dumps(rows, indent=1))
        else:
            for r in rows:
                print(f"  {r['lines']:6} lines {r['functions']:4} fns  called from {r['caller_files']:3} files  {r['file']}")
        return 0

    if a.cmd == "dups":
        groups = [g for g in tidy.dups(p) if keep(g["files"][0]) or keep(g["files"][1])]
        if a.json:
            print(json.dumps(groups, indent=1))
        else:
            for g in groups[:lim]:
                names = ", ".join(sorted({x["a"].rsplit(".", 1)[-1] for x in g["pairs"]})[:5])
                print(f"  {g['lines']:5} lines in {len(g['pairs']):3} fns  {g['files'][0]}  ~  {g['files'][1]}  [{names}]")
            print(f"seamcheck dups: {len(groups)} file pairs, {sum(g['lines'] for g in groups)} duplicated lines")
        return 0

    corpus = tidy.Corpus(p) if a.cmd in ("tidy", "test-only", "unused") else None

    if a.cmd in ("test-only", "unused"):
        rows = tidy.test_only(p, corpus) if a.cmd == "test-only" else tidy.unused(p, corpus)
        rows = [r for r in rows if keep(r["file"])]
        if a.ids:
            print("\n".join(r["id"] for r in rows))
        elif a.json:
            print(json.dumps(rows, indent=1))
        else:
            for r in rows[:lim]:
                extra = f"  <- {', '.join(sorted({t.split('.test ')[0].rsplit('.', 1)[0] for t in r['tests']})[:3])}" if "tests" in r else ""
                print(f"  {r['lines']:4} {r['id']}  {_loc(r)}{extra}")
            print(f"seamcheck {a.cmd}: {len(rows)} functions, {sum(r['lines'] for r in rows)} lines")
        return 0

    if a.cmd == "tests":
        rows = [r for r in tidy.test_audit(p) if keep(r["file"])]
        if a.ids:
            print("\n".join(r["id"] for r in rows if r["reach"] == 0 or r["reads_source"]))
        elif a.json:
            print(json.dumps(rows, indent=1))
        else:
            print("\n".join(_tests_summary(rows, lim)))
            for r in [r for r in rows if r["reads_source"]][:lim]:
                print(f"  reads {', '.join(r['reads_source'][:2])}: {_short(r['id'])}  {_loc(r)}")
        return 0

    # tidy: everything, briefly
    rep = {
        "hotspots": [r for r in tidy.hotspots(p, 400) if keep(r["file"])][:8],
        "dups": [g for g in tidy.dups(p) if keep(g["files"][0]) or keep(g["files"][1])],
        "test_only": [r for r in tidy.test_only(p, corpus) if keep(r["file"])],
        "unused": [r for r in tidy.unused(p, corpus) if keep(r["file"])],
        "tests": [r for r in tidy.test_audit(p) if keep(r["file"])],
    }
    if only is not None:
        print(f"(only files changed since {a.since}: {len(only)})")
    if a.json:
        print(json.dumps(rep, indent=1, default=list))
        return 0
    print("biggest files:")
    for r in rep["hotspots"]:
        print(f"  {r['lines']:6} lines {r['functions']:4} fns  {r['file']}")
    d = rep["dups"]
    print(f"duplication: {len(d)} file pairs, {sum(g['lines'] for g in d)} lines")
    for g in d[:min(lim, 6)]:
        print(f"  {g['lines']:5} lines  {g['files'][0]}  ~  {g['files'][1]}")
    for key, label in (("test_only", "only tests call"), ("unused", "nothing calls or names")):
        rows = rep[key]
        print(f"{label}: {len(rows)} functions, {sum(r['lines'] for r in rows)} lines")
        for r in rows[:min(lim, 6)]:
            print(f"  {r['lines']:4} {r['id']}")
    print("tests:")
    print("\n".join(_tests_summary(rep["tests"], lim)))
    print(f"(seamcheck tidy: {int((time.time() - t0) * 1000)} ms; each list in full: seamcheck test-only | unused | dups | tests)")
    return 0
