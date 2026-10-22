"""Config: the target project's own files, merged.

seamcheck never holds a project's rules. It reads them from the project:
`seamcheck.toml` at the root, every `seamcheck.d/*.toml` beside it, and any
`--config FILE` (repeatable) given on the command line (a config kept outside
the repo). All are merged in that order:

- `[project]` / `[runtime]` tables: keys merge; include / exclude lists add up.
- `[[rule]]`, `[[invariant]]`, `[[contract]]` lists: concatenated. Each entry
  remembers the file it came from (`_file`), and an id used twice is an error.
"""
from __future__ import annotations

import glob
import os
import tomllib

import difflib

LISTS = ("rule", "invariant", "contract", "bridge")
# The keys each entry may have. A typo'd key (`wehre`) used to be ignored
# silently, quietly changing what a rule means; now it's a config error.
KEYS = {
    "rule": {"id", "kind", "pattern", "in", "allow", "why", "severity", "if", "then", "scope", "window", "text",
             "defined", "used", "check", "from", "when", "to", "depth", "unless", "message", "mode", "report", "skip",
             "strict", "ignore", "package", "path", "func", "ordered", "source", "views", "extra_severity", "match", "fallback", "normalize", "rewrite", "run", "timeout", "sep", "wildcard",
             "lint", "options", "gate", "bar", "controls", "menus", "repo", "files", "prefix", "fix", "tighten"},
    "invariant": {"id", "each", "where", "when", "assert", "for", "key", "why", "severity"},
    "contract": {"id", "act", "when", "assert", "why", "severity"},
    "bridge": {"id", "from", "to", "match", "sep", "wildcard", "normalize", "why"},
}
SEVERITIES = {"error", "warn", "info"}
TABLES = ("project", "runtime", "deps", "tidy", "gate", "precise")


def files_for(root: str, extra: list[str] | None = None) -> list[str]:
    out = []
    main = os.path.join(root, "seamcheck.toml")
    if os.path.exists(main):
        out.append(main)
    out += sorted(glob.glob(os.path.join(root, "seamcheck.d", "*.toml")))
    for p in extra or []:
        p = os.path.abspath(p)
        if os.path.isdir(p):
            out += sorted(glob.glob(os.path.join(p, "*.toml")))
        elif p not in out:
            out.append(p)
    return out


def load(root: str, extra: list[str] | None = None) -> tuple[dict, list[str]]:
    """The merged config and a list of problems (bad files, duplicate ids)."""
    merged: dict = {k: [] for k in LISTS}
    for t in TABLES:
        merged[t] = {}
    problems = []
    seen: dict[tuple[str, str], str] = {}
    for path in files_for(root, extra):
        rel = os.path.relpath(path, root) if path.startswith(root) else path
        try:
            with open(path, "rb") as fh:
                cfg = tomllib.load(fh)
        except (OSError, tomllib.TOMLDecodeError) as e:
            problems.append(f"{rel}: {e}")
            continue
        for t in TABLES:
            for k, v in cfg.get(t, {}).items():
                if k in ("include", "exclude") and k in merged[t]:
                    merged[t][k] = merged[t][k] + list(v)
                else:
                    merged[t][k] = v
        for kind in LISTS:
            for entry in cfg.get(kind, []):
                rid = entry.get("id", "")
                for k in entry:
                    if k not in KEYS[kind]:
                        near = difflib.get_close_matches(k, KEYS[kind], 1)
                        problems.append(f"{rel}: {kind} {rid!r}: unknown key {k!r}"
                                        + (f" (did you mean {near[0]!r}?)" if near else ""))
                sev = entry.get("severity", "error")
                if sev not in SEVERITIES:
                    problems.append(f"{rel}: {kind} {rid!r}: severity {sev!r} (use {', '.join(sorted(SEVERITIES))})")
                if kind == "rule" and entry.get("gate", "all") not in ("all", "changed", "touched", "off"):
                    problems.append(f"{rel}: rule {rid!r}: gate {entry.get('gate')!r} (use all, changed, touched or off)")
                if not rid:
                    problems.append(f"{rel}: a {kind} without an id")
                entry = dict(entry, _file=rel)
                if (kind, rid) in seen:
                    problems.append(f"{rel}: {kind} id {rid!r} already defined in {seen[(kind, rid)]}")
                    continue
                seen[(kind, rid)] = rel
                merged[kind].append(entry)
    return merged, problems
