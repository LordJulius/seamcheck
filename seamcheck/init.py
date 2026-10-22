"""`seamcheck init`: a first seamcheck.toml for a repo that has none.

It writes only what it can see: the languages present, the folders their code
lives in (as `include` and as impact `areas`), the test layout when it is
not one of the usual ones, and the rule packs of the frameworks it finds. Rules are the project's to write; the file shows
one of each common kind, commented out, to start from. `--hook` also installs
the pre-commit gate.
"""
from __future__ import annotations

import collections
import json
import os
import re
import stat

from . import core

_LANG = {".ex": "Elixir", ".exs": "Elixir", ".kt": "Kotlin", ".kts": "Kotlin", ".java": "Java", ".ts": "TypeScript",
         ".tsx": "TypeScript", ".js": "JavaScript", ".jsx": "JavaScript", ".mjs": "JavaScript", ".py": "Python",
         ".rs": "Rust", ".go": "Go", ".cs": "C#", ".cpp": "C++", ".cc": "C++", ".h": "C++", ".hpp": "C++",
         ".qml": "QML", ".gd": "GDScript", ".swift": "Swift", ".php": "PHP"}
# folders that hold code no one maintains by hand
_SKIP_TOP = {"vendor", "third_party", "thirdparty", "external", "dist", "build", "out", "bin", "obj", "node_modules",
             "deps", "_build", "target", "generated", ".github", "docs", "doc"}

_EXAMPLES = '''
# Rules are yours: each one a contract no single file shows. Uncomment and
# adapt; `seamcheck check` runs them, `seamcheck gate` blocks a change on them.
#
# [[rule]]
# id = "writes-go-through-the-store"        # a pattern allowed only in one place
# kind = "forbid"
# pattern = 'db\\.execute\\('
# allow = ["src/store/**"]
# why = "the store invalidates the cache; a direct write leaves it stale"
#
# [[rule]]
# id = "handlers-reach-auth"                 # selected functions must reach a check
# kind = "reach"
# from = ["src/handlers/**:*"]
# to = ["require_user"]
# depth = 3
#
# [[rule]]
# id = "every-event-has-a-handler"           # ids defined in one place, used in another
# kind = "coverage"
# defined = { in = ["src/events.*"], pattern = 'EVENT_(\\w+)' }
# used = { in = ["src/**"], pattern = 'on_(\\w+)' }
# check = ["undefined", "unused"]

[gate]
# new code may not name these (a second path beside the old one)
forbid_added = ["legacy", "compat", "shim"]
'''


def _top_dirs(files: list[str]) -> dict[str, collections.Counter]:
    """top-level folder ("" = the root) -> extension counts of its code files."""
    out: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    for rel in files:
        ext = os.path.splitext(rel)[1]
        if ext not in _LANG:
            continue
        top = rel.split("/", 1)[0] if "/" in rel else ""
        if top.lower() in _SKIP_TOP or top.startswith("."):
            continue
        out[top][ext] += 1
    return out


# a framework is in use when its manifest says so: pack -> (manifest names, what one holds)
_FRAMEWORKS = {
    "phoenix": (("mix.exs",), re.compile(r"\{:phoenix\b")),
    "compose": (("build.gradle", "build.gradle.kts", "libs.versions.toml"),
                re.compile(r"\bcompose\s*=\s*true|androidx\.compose|org\.jetbrains\.compose")),
    "qml": (("CMakeLists.txt",), re.compile(r"Qt6?::Quick\b|\bqt_add_qml_module\b|\bQuick\b")),
}


def _uses(text: str, dep: str) -> bool:
    """A package.json depends on `dep` (or is it)."""
    try:
        pkg = json.loads(text)
    except ValueError:
        return False
    return isinstance(pkg, dict) and (pkg.get("name") == dep or any(
        dep in (pkg.get(k) or {}) for k in ("dependencies", "devDependencies", "peerDependencies")))


def detect_packs(root: str, files: list[str]) -> list[str]:
    """The rule packs (seamcheck/packs) whose framework a manifest declares.
    QML needs its files too: a Qt Widgets app links Qt Quick for one view."""
    manifests = {"package.json"} | {n for names, _ in _FRAMEWORKS.values() for n in names}
    found = set()
    for rel in files:
        base = os.path.basename(rel)
        if base not in manifests:
            continue
        try:
            with open(os.path.join(root, rel), encoding="utf-8", errors="replace") as fh:
                text = fh.read()
        except OSError:
            continue
        if base == "package.json":
            found |= {p for p in ("react", "express") if _uses(text, p)}
        for pack, (names, rx) in _FRAMEWORKS.items():
            if base in names and rx.search(text):
                found.add(pack)
    if not any(rel.endswith(".qml") for rel in files):
        found.discard("qml")
    return sorted(found)


def render(root: str) -> tuple[str, list[str]]:
    """The config text, and a summary of what was found (one line each)."""
    files = core.list_files(root, ["**/*"], core.DEFAULT_EXCLUDE)
    tops = _top_dirs(files)
    langs = collections.Counter()
    for c in tops.values():
        for ext, n in c.items():
            langs[_LANG[ext]] += n
    code_dirs = sorted(d for d in tops if d)
    root_files = sum(tops.get("", collections.Counter()).values())
    include = [f"{d}/**" for d in code_dirs]
    if root_files:
        include += sorted({f"*{ext}" for ext in tops[""]})
    from . import tidy
    tests = [d for d in code_dirs if d.lower() in ("test", "tests", "spec", "specs", "__tests__", "e2e", "testing")]
    areas = [d for d in code_dirs if d not in tests]
    name = os.path.basename(os.path.abspath(root))
    lines = ["# seamcheck config (https://github.com/LordJulius/seamcheck). Written by `seamcheck init`;",
             "# everything here is yours to change.", "", "[project]", f'name = "{name}"']
    packs = detect_packs(root, files)
    if packs:
        lines.append("# rules true for any project on these frameworks (seamcheck/packs; override one by its id)")
        lines.append("packs = [" + ", ".join(f'"{p}"' for p in packs) + "]")
    if include:
        lines.append("include = [" + ", ".join(f'"{g}"' for g in include) + "]")
    lines.append('exclude = ["**/generated/**", "**/*.min.js"]')
    if len(areas) > 1:
        lines.append("# how `seamcheck impact` groups what a change reaches")
        lines.append("areas = { " + ", ".join(f'{_key(d)} = ["{d}/**"]' for d in areas) + " }")
    unusual = [d for d in tests if not core.match_any(f"{d}/x.py", tidy.DEFAULT_TESTS)]
    if unusual:
        lines += ["", "[tidy]", "tests = [" + ", ".join(f'"{g}"' for g in tidy.DEFAULT_TESTS + [f"{d}/**" for d in unusual]) + "]"]
    text = "\n".join(lines) + "\n" + _EXAMPLES
    summary = [f"{n:>6} files  {lang}" for lang, n in langs.most_common()]
    if packs:
        summary.append(f"rule packs: {', '.join(packs)}")
    if not langs:
        summary.append("no source files seamcheck reads (it reads " + ", ".join(sorted(set(_LANG.values()))) + ")")
    return text, summary


def _key(d: str) -> str:
    k = "".join(ch if ch.isalnum() or ch == "_" else "_" for ch in d)
    return k if k and not k[0].isdigit() else f"_{k}"


HOOK = """#!/bin/sh
# seamcheck: may this commit land? (installed by `seamcheck init --hook`)
seamcheck gate --staged || { echo "fix the above, or waive a line with 'seamcheck: allow <rule>'"; exit 1; }
"""


def install_hook(root: str) -> str:
    """Write .git/hooks/pre-commit (never over an existing hook)."""
    git = os.path.join(root, ".git")
    if os.path.isfile(git):  # a worktree: `gitdir: ...`
        with open(git, encoding="utf-8") as fh:
            line = fh.read().strip()
        if line.startswith("gitdir:"):
            git = os.path.normpath(os.path.join(root, line.split(":", 1)[1].strip()))
    if not os.path.isdir(git):
        return "not a git checkout: no hook installed"
    hooks = os.path.join(git, "hooks")
    os.makedirs(hooks, exist_ok=True)
    path = os.path.join(hooks, "pre-commit")
    if os.path.exists(path):
        return f"{path} exists: add `seamcheck gate --staged` to it yourself"
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(HOOK)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return f"installed {path}"


def main(root: str, hook: bool, force: bool) -> int:
    path = os.path.join(root, "seamcheck.toml")
    text, summary = render(root)
    if os.path.exists(path) and not force:
        print(f"{path} exists (--force overwrites it)")
    else:
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(text)
        print(f"wrote {path}")
        for s in summary:
            print("  " + s)
    if hook:
        print(install_hook(root))
    print("next: `seamcheck stats` (how much of the graph is exact), `seamcheck tidy` (dead code, copies, "
          "tests that test nothing), then write the first rule")
    return 0
