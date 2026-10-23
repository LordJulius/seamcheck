"""Config: the target project's own files, merged.

seamcheck never holds a project's rules. It reads them from the project:
`seamcheck.toml` at the root, every `seamcheck.d/*.toml` beside it, and any
`--config FILE` (repeatable) given on the command line (a config kept outside
the repo). All are merged in that order:

- `[project]` / `[runtime]` tables: keys merge; include / exclude lists add up.
- `[[rule]]`, `[[invariant]]`, `[[contract]]` lists: concatenated. Each entry
  remembers the file it came from (`_file`), and an id used twice is an error.
- `[project] packs = ["phoenix"]`: the rules of `seamcheck/packs/phoenix.toml`
  join as `phoenix/<id>`. With no config file at all, DEFAULT_PACKS (security) run. A project rule with a pack rule's id overrides that
  rule's keys (`severity = "warn"`, `gate = "off"`, a narrower `in`).
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
             "lint", "options", "gate", "bar", "controls", "menus", "repo", "files", "prefix", "fix", "tighten", "via", "layers", "level", "allow_to", "fuzzy"},
    "invariant": {"id", "each", "where", "when", "assert", "for", "key", "why", "severity"},
    "contract": {"id", "act", "when", "assert", "why", "severity"},
    "bridge": {"id", "from", "to", "match", "sep", "wildcard", "normalize", "why"},
}
SEVERITIES = {"error", "warn", "info"}
TABLES = ("project", "runtime", "deps", "tidy", "gate", "precise")
# keys whose lists add up across files instead of the last file winning
ADDITIVE = ("include", "exclude", "packs")
DEFAULT_PACKS = ("security",)  # what `check` runs in a repo with no config at all
PACKS_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "packs")


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
    listed: dict[str, str] = {}  # pack name -> the config file that listed it
    paths = files_for(root, extra)
    if not paths:  # a repo with no config yet: the rules true for any project
        listed = {name: "(no seamcheck.toml: built-in default)" for name in DEFAULT_PACKS}
    for path in paths:
        rel = os.path.relpath(path, root) if path.startswith(root) else path
        try:
            with open(path, "rb") as fh:
                cfg = tomllib.load(fh)
        except (OSError, tomllib.TOMLDecodeError) as e:
            problems.append(f"{rel}: {e}")
            continue
        for t in TABLES:
            for k, v in cfg.get(t, {}).items():
                if k in ADDITIVE and k in merged[t]:
                    merged[t][k] = merged[t][k] + list(v)
                else:
                    merged[t][k] = v
        for kind in LISTS:
            for entry in cfg.get(kind, []):
                rid = entry.get("id", "")
                _check(kind, entry, rel, problems)
                entry = dict(entry, _file=rel)
                if (kind, rid) in seen:
                    problems.append(f"{rel}: {kind} id {rid!r} already defined in {seen[(kind, rid)]}")
                    continue
                seen[(kind, rid)] = rel
                merged[kind].append(entry)
        packs = cfg.get("project", {}).get("packs", [])
        for name in packs if isinstance(packs, list) else []:
            listed.setdefault(name, rel)
    if not isinstance(merged["project"].get("packs", []), list):
        problems.append('[project] packs: a list of pack names, e.g. packs = ["phoenix"]')
    elif listed:
        _apply_packs(merged, listed, problems)
    return merged, problems


def _check(kind: str, entry: dict, rel: str, problems: list[str]) -> None:
    """Unknown keys, severities and gate modes of one entry."""
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


def available_packs() -> list[str]:
    return sorted(n[:-5] for n in os.listdir(PACKS_DIR) if n.endswith(".toml"))


def pack_params(names) -> dict[str, dict]:
    """The `[[param]]` entries of these packs, as {"<pack>/<id>": entry}: what
    a framework hands the functions it calls (Express's `req`, `res`)."""
    out = {}
    for name in names:
        path = os.path.join(PACKS_DIR, f"{name}.toml")
        if not os.path.exists(path):
            continue
        with open(path, "rb") as fh:
            pack = tomllib.load(fh)
        for entry in pack.get("param", []):
            out[f"{name}/{entry.get('id', '')}"] = entry
        for entry in pack.get("returns", []):  # functions whose result is the library's (a runtime proxy)
            out[f"{name}/{entry.get('id', '')}"] = dict(entry, kind="returns")
    return out


def _apply_packs(merged: dict, listed: dict[str, str], problems: list[str]) -> None:
    """Pack rules go first, as `<pack>/<id>`, named after the config file that
    listed the pack (where a project goes to change them). A project rule with
    a pack rule's id is an override: its keys replace the pack rule's."""
    known = available_packs()
    base: dict[str, dict] = {}
    for name, rel in listed.items():
        if name not in known:
            near = difflib.get_close_matches(name, known, 1)
            problems.append(f"{rel}: unknown pack {name!r}" + (f" (did you mean {near[0]!r}?)" if near else "")
                            + f"; known: {', '.join(known)}")
            continue
        with open(os.path.join(PACKS_DIR, f"{name}.toml"), "rb") as fh:
            pack = tomllib.load(fh)
        for entry in pack.get("rule", []):
            _check("rule", entry, f"pack {name}", problems)
            rid = f"{name}/{entry.get('id', '')}"
            base[rid] = dict(entry, id=rid, _file=rel)
    own = []
    for entry in merged["rule"]:
        prefix = entry["id"].split("/", 1)[0] if "/" in entry["id"] else ""
        if entry["id"] in base:
            base[entry["id"]].update(entry)
        elif prefix in known:
            # an override that overrides nothing (a renamed pack rule) would silently do nothing
            problems.append(f"{entry['_file']}: rule {entry['id']!r}: " +
                            (f"pack {prefix} has no rule {entry['id'].split('/', 1)[1]!r}" if prefix in listed
                             else f"overrides pack {prefix}, which [project] packs doesn't list"))
        else:
            own.append(entry)
    merged["rule"] = list(base.values()) + own
