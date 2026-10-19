"""Content fingerprints and the shared on-disk cache.

Caches used to be keyed by the checkout's path and each file's mtime: a lane's
fresh git worktree (same files, new path, new mtimes) reparsed everything,
re-ran `mix phx.routes` and rebuilt the corpus, so its gate took 90-130 s, and
every worktree left ~150 MB behind (10 GB in ~/.cache/seamcheck by 2026-10-07).

Now a file is known by its content hash. The parse, corpus and command caches
are shared by every checkout of one repository (keyed by git's common dir), so
a worktree reuses what the main checkout already did. A per-checkout stamp map
(mtime + size -> hash) keeps the warm path to a stat per file.
"""
from __future__ import annotations

import hashlib
import os
import pickle
import subprocess
import time

CACHE_DIR = os.path.expanduser("~/.cache/seamcheck")
# total size the cache may grow to before the oldest entries go
BUDGET_BYTES = 3 * 1024 ** 3


def _key(text: str) -> str:
    return hashlib.sha1(text.encode()).hexdigest()[:16]


def repo_key(root: str) -> str:
    """One key for every checkout of a repository: git's common dir (a worktree
    shares it with the main checkout); the path itself outside git."""
    try:
        r = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], cwd=root,
                           capture_output=True, text=True, timeout=10)
        if r.returncode == 0 and r.stdout.strip():
            return _key("repo|" + r.stdout.strip())
    except (OSError, subprocess.TimeoutExpired):
        pass
    return _key("repo|" + os.path.abspath(root))


def path(name: str) -> str:
    return os.path.join(CACHE_DIR, name)


def load(name: str, default):
    try:
        with open(path(name), "rb") as fh:
            return pickle.load(fh)
    except (OSError, pickle.PickleError, EOFError, AttributeError, ImportError, IndexError, TypeError):
        return default


def save(name: str, obj) -> None:
    """Atomic: two lanes saving at once leave one complete file, never half of each."""
    try:
        os.makedirs(CACHE_DIR, exist_ok=True)
        tmp = f"{path(name)}.{os.getpid()}.tmp"
        with open(tmp, "wb") as fh:
            pickle.dump(obj, fh, protocol=pickle.HIGHEST_PROTOCOL)
        os.replace(tmp, path(name))
    except (OSError, pickle.PicklingError, TypeError, AttributeError):
        pass


def digest_bytes(data: bytes) -> str:
    return hashlib.blake2b(data, digest_size=16).hexdigest()


class Hashes:
    """Content hash per file of one checkout; a file whose mtime + size match
    the last run isn't read again."""

    def __init__(self, root: str):
        self.root = root
        self.name = _key("stamps|" + root) + "-stamps.pkl"
        old = load(self.name, {})
        self.old: dict = old.get("files", {}) if isinstance(old, dict) and old.get("v") == 1 else {}
        self.new: dict = {}

    def of(self, rel: str) -> str | None:
        full = os.path.join(self.root, rel)
        try:
            st = os.stat(full)
        except OSError:
            return None
        stamp = (st.st_mtime_ns, st.st_size)
        hit = self.old.get(rel)
        if hit and hit[0] == stamp:
            h = hit[1]
        else:
            try:
                with open(full, "rb") as fh:
                    h = digest_bytes(fh.read())
            except OSError:
                return None
        self.new[rel] = (stamp, h)
        return h

    def save(self) -> None:
        merged = {**self.old, **self.new}
        if merged != self.old:
            save(self.name, {"v": 1, "files": merged})


def digest_files(root: str, rels: list[str]) -> list[tuple[str, str | None]]:
    out = []
    for rel in sorted(rels):
        try:
            with open(os.path.join(root, rel), "rb") as fh:
                out.append((rel, digest_bytes(fh.read())))
        except OSError:
            out.append((rel, None))
    return out


def prune(keep: set[str] = frozenset(), budget: int = BUDGET_BYTES) -> None:
    """Drop the least recently used cache files until the cache fits its budget.
    Files this run uses (`keep`) stay."""
    try:
        entries = []
        for n in os.listdir(CACHE_DIR):
            p = os.path.join(CACHE_DIR, n)
            try:
                st = os.stat(p)
            except OSError:
                continue
            entries.append((max(st.st_atime, st.st_mtime), st.st_size, n, p))
    except OSError:
        return
    total = sum(e[1] for e in entries)
    now = time.time()
    for atime, size, n, p in sorted(entries):
        if total <= budget:
            break
        if n in keep or n.endswith(".tmp") and now - atime < 3600:
            continue
        try:
            os.remove(p)
            total -= size
        except OSError:
            pass
