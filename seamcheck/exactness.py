"""The gate's exactness guard: a change that quietly breaks import resolution
shows up.

When a change removes or breaks an import seamcheck follows, the calls through
it fall back to name guesses (or to nothing). Every rule and query over that
code then sees less, and nothing said so: `stats` would show it, but nobody
runs `stats` on every change. So each check / gate / stats on a tree that is
HEAD's records, per language, how many calls into project code are exact
(`Project.resolution`), and the gate compares the languages a change touches
against that record. A drop of more than DROP points is a warning, never a
block: deleting well-resolved code lowers the share honestly too.

Records live in the shared cache (one small file per repository), keyed by
commit, seamcheck's own code and the config: a seamcheck upgrade or an
`include` change measures differently, and must not read as a regression.
"""
from __future__ import annotations

import hashlib
import io
import json
import os
import subprocess
import tarfile
import tempfile

from . import config as config_mod
from . import core
from . import fingerprint as fp

DROP = 2.0  # percentage points of "project calls exact" a change may lose quietly
# Measuring a commit no run recorded loads its files from scratch; only a
# project this small does that within a gate's budget.
CHEAP_FILES = 300
KEEP = 64  # records per repository


def shares(project) -> dict[str, tuple[int, int]]:
    """extension -> (exact, name guess) call sites into project code."""
    return {ext: (r[0], r[1]) for ext, r in project.resolution.items() if r[0] + r[1]}


def _pct(c: tuple[int, int]) -> float:
    return 100 * c[0] / (c[0] + c[1])


def _commit(root: str, ref: str) -> str:
    try:
        r = subprocess.run(["git", "rev-parse", "--verify", "-q", ref + "^{commit}"], cwd=root,
                           capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return r.stdout.strip() if r.returncode == 0 else ""


def _key(project, sha: str) -> str:
    cfg = json.dumps(project.config, sort_keys=True, default=str)
    return f"{sha}|{core._code_version()}|{hashlib.sha1(cfg.encode()).hexdigest()[:12]}"


def _book_name(root: str) -> str:
    return f"{fp.repo_key(root)}-exactness.pkl"


def _save(project, sha: str, value: dict) -> None:
    name = _book_name(project.root)
    book = fp.load(name, {})
    book = book if isinstance(book, dict) else {}
    book.pop(_key(project, sha), None)
    book[_key(project, sha)] = value  # (last in, newest: the oldest go first)
    fp.save(name, dict(list(book.items())[-KEEP:]))


def _in_graph(project, rel: str) -> bool:
    """Would a change to this file change the graph? A code file the project
    parses (or parsed: a deletion), or its seamcheck config."""
    if rel == "seamcheck.toml" or rel.startswith("seamcheck.d/"):
        return True
    pc = project.config.get("project", {})
    return (rel in project.by_file or core.adapter_for(rel) is not None
            and core.match_any(rel, pc.get("include", ["**/*"]))
            and not core.match_any(rel, core.DEFAULT_EXCLUDE + pc.get("exclude", [])))


def record(project, changed: set[str] | None = None) -> None:
    """Remember this tree's shares as HEAD's, when the graph is HEAD's (no
    parsed file or config differs from it)."""
    from . import tidy
    if changed is None:
        changed = tidy.changed_files(project, "HEAD")
    if any(_in_graph(project, rel) for rel in changed):
        return
    sha = _commit(project.root, "HEAD")
    if sha:
        _save(project, sha, shares(project))


def _cheap(project) -> bool:
    # a bridge's `run` (mix phx.routes) would run again in the copy
    runs = any("run" in b.get(side, {}) for b in project.config.get("bridge", []) for side in ("from", "to"))
    return len(project.by_file) <= CHEAP_FILES and not runs


def _measure(project, sha: str, extra: list[str]) -> dict | None:
    """Load the commit's files into a scratch dir under today's config: what
    the change did to resolution, not what a config edit did."""
    try:
        tar = subprocess.run(["git", "archive", "--format=tar", sha], cwd=project.root, capture_output=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if tar.returncode != 0:
        return None
    with tempfile.TemporaryDirectory(prefix="seamcheck-head-") as tmp:
        with tarfile.open(fileobj=io.BytesIO(tar.stdout)) as tf:
            if hasattr(tarfile, "data_filter"):
                tf.extractall(tmp, filter="data")
            else:  # Python 3.11.0-3.11.3
                tf.extractall(tmp)
        for name in ("seamcheck.toml", "seamcheck.d"):
            path = os.path.join(tmp, name)
            if os.path.isdir(path):
                for n in os.listdir(path):
                    os.remove(os.path.join(path, n))
            elif os.path.exists(path):
                os.remove(path)
        then = core.load(tmp, config_mod.files_for(project.root, extra), use_cache=False)
    return shares(then)


def baseline(project, ref: str, extra: list[str] | None = None) -> dict | None:
    """The shares recorded for `ref`'s commit, measured now when no run
    recorded them and that is cheap; None when unknown."""
    sha = _commit(project.root, ref)
    if not sha:
        return None
    book = fp.load(_book_name(project.root), {})
    got = book.get(_key(project, sha)) if isinstance(book, dict) else None
    if got is None and _cheap(project):
        got = _measure(project, sha, extra or [])
        if got is not None:
            _save(project, sha, got)
    return got


def drops(project, ref: str, changed: set[str], extra: list[str] | None = None) -> list[dict]:
    """The languages this change touches whose exact share fell more than DROP
    points since `ref`. Records the tree first when it is HEAD's."""
    record(project)
    exts = {os.path.splitext(rel)[1] for rel in changed if _in_graph(project, rel) and core.adapter_for(rel)}
    if not exts:
        return []
    before = baseline(project, ref, extra)
    if not before:
        return []
    now = shares(project)
    from .init import _LANG
    out = []
    for ext in sorted(exts):
        if ext not in before or ext not in now:
            continue
        b, a = _pct(before[ext]), _pct(now[ext])
        if b - a > DROP:
            first = sorted(rel for rel in changed if rel.endswith(ext))[0]
            out.append({"ext": ext, "language": _LANG.get(ext, ext), "before": round(b, 1), "after": round(a, 1),
                        "calls_before": list(before[ext]), "calls_after": list(now[ext]), "file": first})
    return out


def message(d: dict, ref: str) -> str:
    return (f"{d['language']} ({d['ext']}) calls into project code exact: {d['before']}% at {ref}, "
            f"{d['after']}% now ({d['calls_before'][0]} of {sum(d['calls_before'])} -> "
            f"{d['calls_after'][0]} of {sum(d['calls_after'])})")


WHY = "calls that lost their import fall back to name guesses: rules and queries over them see less"
FIX = "check the imports this change removed or moved (seamcheck stats shows the shares)"
