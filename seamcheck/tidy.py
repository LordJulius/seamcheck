"""Keeping a big repo tidy: what's dead, what's duplicated, which tests test
nothing, where a god module splits. Each report is conservative: a function
counts as unused only if nothing calls it AND its name appears in no other
file (scripts, config and templates included), because frameworks, routers
and `mix run -e` call things no call graph sees.

Configured by the project's `[tidy]` table (all optional):

  tests    = ["test/**", "**/*.test.ts", ...]   test files (defaults cover the usual)
  entry    = 'regex'        names frameworks call (added to the built-in list)
  entry_in = ["lib/app_web/controllers/**"]      files whose functions are all entry points
  corpus   = ["scripts/**", "config/**", ...]    extra files searched for name references
  suites   = { hub = ["test/**"], web = ["web/**"] }   test suites, for `tests`
  domains  = { spotify = 'spotify|track', ... }  feature vocabulary, for `split`

Commands: tests, test-only, unused, dups, split FILE, hotspots, tidy, remove.
"""
from __future__ import annotations

import collections
import hashlib
import itertools
import os
import pickle
import re

from . import core
from .model import Func

DEFAULT_TESTS = ["test/**", "tests/**", "**/test/**", "**/tests/**", "**/__tests__/**", "**/src/test/**",
                 "**/src/androidTest/**", "**/*.test.*", "**/*.spec.*", "**/*_test.*", "**/test_*.py",
                 "**/*Test.kt", "**/*Test.java", "**/*_spec.rb", "**/spec/**/*.rb",
                 "**/test_*.rb"]
DEFAULT_CORPUS = ["scripts/**", "config/**", "deploy/**", "priv/**", "rel/**", "bin/**", "tools/**", "Makefile",
                  "justfile", "*.toml", "*.json", "*.yml", "*.yaml", ".github/**", "**/*.service", "**/*.sh",
                  "**/*.tscn", "**/*.tres", "**/*.heex", "**/*.eex", "**/*.html", "**/AndroidManifest.xml",
                  "**/res/layout/**", "**/*.qml", "**/*.ui"]
DEFAULT_CORPUS_SKIP = ["**/node_modules/**", "**/_build/**", "**/deps/**", "**/build/**", "**/dist/**",
                       "priv/static/**", "**/.git/**", "**/target/**", "**/.venv/**", "**/venv/**",
                       "**/site-packages/**", "**/__pycache__/**", "**/.gradle/**", "**/media/**"]
# names a framework or runtime calls: never "unused" because nothing in the graph calls them
ENTRY = re.compile(
    r"^(main|init|start|stop|start_link|child_spec|terminate|code_change|format_status|handle_\w+|mount|render|"
    r"call|run|perform|execute|update|changeset|index|show|create|delete|new|edit|join|connect|id|fetch|"
    r"get_and_update|pop|cast|dump|load|embed_as|equal\?|type|spec|definition|schema|describe|validate\w*|"
    r"tool_definition|primitive_specs?|setup\w*|teardown\w*|before\w*|after\w*|on[A-Z]\w*|_\w+|__\w+__|"
    r"test\w*|constructor|toString|equals|hashCode|onCreate|onStart|onResume|onPause|onStop|onDestroy|"
    r"invoke|apply|default|render\w*|use[A-Z]\w*|get[A-Z]\w*Props)$")
_IDENT = re.compile(r"[A-Za-z_$][\w$]*[?!]?")
_TOK = re.compile(r"[A-Za-z_]\w*|\d+|[^\s\w]")


def cfg(project) -> dict:
    return project.config.get("tidy", {})


def is_test(project, rel: str) -> bool:
    return core.match_any(rel, cfg(project).get("tests", DEFAULT_TESTS))


def is_entry(project, f: Func) -> bool:
    c = cfg(project)
    if ENTRY.match(f.name) or (c.get("entry") and re.match(c["entry"], f.name)):
        return True
    if core.match_any(f.file, c.get("entry_in", [])):
        return True
    mod = project.modules.get(f.module)
    raw = mod.raw if mod else []
    # annotations right above: @impl / @Override / @Test / @Composable preview / decorators
    return any(re.match(r"\s*@(impl|Override|Test|Preview|app\.|router\.|route|get|post|put|delete|patch)\b", raw[i])
               for i in range(max(0, f.start - 4), f.start - 1))


class Corpus:
    """Every identifier in every file (project files + `corpus` globs), indexed
    once: 'is this name mentioned anywhere else?' is a dict lookup."""

    def __init__(self, project):
        self.where: dict[str, set[str]] = collections.defaultdict(set)
        self.counts: dict[str, collections.Counter] = {}
        files = set(project.files)
        extra = cfg(project).get("corpus", DEFAULT_CORPUS)
        exclude = project.config.get("project", {}).get("exclude", []) + DEFAULT_CORPUS_SKIP
        files |= set(core.list_files(project.root, extra, exclude))
        # an on-disk index keyed by each file's content, shared by every checkout
        # of the repo: a lane's worktree reads only what it changed
        from . import fingerprint as fp
        hashes = fp.Hashes(project.root)
        name = fp.repo_key(project.root) + "-corpus.pkl"
        old = fp.load(name, {})
        cached = old.get("files", {}) if isinstance(old, dict) and old.get("v") == 2 else {}
        fresh = {}
        for rel in files:
            if rel.endswith((".lock", "-lock.json", ".min.js", ".map", ".svg", ".png", ".jpg", ".webp", ".pdf")):
                continue
            full = os.path.join(project.root, rel)
            stamp = hashes.of(rel)
            if stamp is None:
                continue
            hit = cached.get(rel)
            if hit and hit[0] == stamp:
                c = hit[1]
            else:
                try:
                    with open(full, encoding="utf-8", errors="replace") as fh:
                        text = fh.read(1_000_000)
                except OSError:
                    continue
                # a bundle / generated blob (very long lines) names everything: skip it
                if text and max(len(l) for l in text[:200_000].split("\n")) > 5000:
                    c = collections.Counter()
                else:
                    c = collections.Counter(_IDENT.findall(text))
            fresh[rel] = (stamp, c)
            self.counts[rel] = c
            for w in c:
                self.where[w].add(rel)
                if w[-1] in "?!":  # `seed!` is also a mention of `seed`
                    self.where[w[:-1]].add(rel)
        if any(cached.get(k, (None,))[0] != v[0] for k, v in fresh.items()):
            fp.save(name, {"v": 2, "files": {**cached, **fresh}})
        hashes.save()

    def elsewhere(self, name: str, own: str, skip=lambda rel: False) -> list[str]:
        return sorted(r for r in self.where.get(name, ()) if r != own and not skip(r))

    def in_own(self, name: str, own: str) -> int:
        c = self.counts.get(own)
        return c[name] if c else 0


def changed_files(project, ref: str) -> set[str]:
    """Files changed since `ref` (committed, staged, unstaged) plus untracked."""
    import subprocess
    out = set()
    for cmd in (["git", "diff", "--name-only", ref], ["git", "ls-files", "--others", "--exclude-standard"]):
        try:
            r = subprocess.run(cmd, cwd=project.root, capture_output=True, text=True, timeout=60)
            out |= {l.strip() for l in r.stdout.splitlines() if l.strip()}
        except (OSError, subprocess.TimeoutExpired):
            pass
    return out


def refs(project, name: str, corpus: "Corpus") -> dict:
    """Everywhere `name` is called (graph) or mentioned (any indexed file)."""
    fs = [f for f in project.funcs.values() if f.name == name or f.id == name or f.id.endswith("." + name)]
    mods = [m for m in project.modules.values()
            if m.name == name or m.name.endswith("." + name) or m.name.endswith("/" + name)]
    if mods and not fs:
        # a module / file: where it's defined, who calls into it, who names it
        ids = {f.id for m in mods for f in m.funcs}
        callers = sorted({(c, ex) for i in ids for c, _, ex in project.redges.get(i, []) if c not in ids})
        short = name.rsplit(".", 1)[-1].rsplit("/", 1)[-1]
        mentions = sorted((r, corpus.counts[r][short]) for r in corpus.where.get(short, ()))
        return {"defined": [f"{m.name}  {m.file}" for m in mods],
                "callers": [f"{c}{'' if ex else ' (name guess)'}" for c, ex in callers],
                "mentions": [f"{r} x{n}" for r, n in mentions]}
    short = fs[0].name if fs else name.rsplit(".", 1)[-1]
    callers = sorted({(c, ex) for f in fs for c, _, ex in project.redges.get(f.id, [])})
    mentions = sorted((r, corpus.counts[r][short] + corpus.counts[r][short + "!"] + corpus.counts[r][short + "?"])
                      for r in corpus.where.get(short, ()))
    return {"defined": [f"{f.id}  {f.file}:{f.start}" for f in fs],
            "callers": [f"{c}{'' if ex else ' (name guess)'}" for c, ex in callers],
            "mentions": [f"{r} x{n}" for r, n in mentions]}


# --- which tests test something ----------------------------------------------------

def test_audit(project, depth: int = 5, only: set[str] | None = None) -> list[dict]:
    """Per test function (or just the ids in `only`): how many product functions
    it reaches (exact edges), whether it reads a product source file as text (a
    grep test), its suite."""
    product_files = {r for r in project.files if not is_test(project, r)}
    suites = cfg(project).get("suites")
    # product types by short name (a Kotlin enum, a struct): a test that names
    # one exercises it even with no call edge (an enum's entries aren't functions)
    types = {k for k, mods in project.short.items()
             if any((m2 := project.modules.get(n)) and m2.file in product_files for n in mods)}
    rows = []
    for t in project.funcs.values():
        if t.kind != "test" or (only is not None and t.id not in only):
            continue
        if t.file.endswith((".ex", ".exs")) and not t.name.startswith(("test ", "property ")):
            continue  # setup / setup_all / describe: fixtures and grouping, not tests
        reach = core.reach(project, t.id, depth, exact_only=True)
        prod = sum(1 for fid in reach if fid in project.funcs and
                   project.funcs[fid].file in product_files)
        mod = project.modules.get(t.module)
        raw = "\n".join(mod.raw[t.start - 1:t.end]) if mod else ""
        if prod == 0 and types:
            prod = len({w for w in re.findall(r"\b[A-Z]\w+\b", raw) if w in types})
        if prod == 0:
            routed = _routed(project, t, reach, mod)
            prod = len({fid for r in routed for fid in core.reach(project, r, depth, exact_only=True)
                        if fid in project.funcs and project.funcs[fid].file in product_files} | set(routed))
        named = _repo_paths(project, t.file, re.findall(r"""["'`]([\w./-]+\.\w+)["'`]""", raw))
        reads = sorted(p for p in named if p in product_files)
        # a test that reads a repo file no language parses (compose.yml, a
        # firewall table, a unit) checks that file: it is the product there
        configs = sorted(p for p in named if p not in project.files and not is_test(project, p))
        suite = next((s for s, g in (suites or {}).items() if core.match_any(t.file, g)), t.file.split("/")[0])
        runs = _runs_scripts(project, t, reach, mod) if prod == 0 else []
        reads = [r for r in reads if r not in runs]  # running it isn't grepping it
        configs = [c for c in configs if c not in runs]
        rows.append({"id": t.id, "file": t.file, "line": t.start, "suite": suite,
                     "reach": prod or len(runs) or len(configs), "reads_source": reads, "runs": runs})
    return rows


_SPAWN = re.compile(r"System\.cmd|System\.shell|Port\.open|:os\.cmd|\bsubprocess\.|\bexecFile(?:Sync)?\b|\bspawn(?:Sync)?\s*\("
                    r"|ProcessBuilder|Runtime\.getRuntime\(\)\.exec|\bexecv?p?\(")
_SCRIPT_PATH = re.compile(r"""["'`]((?:\./)?[\w.-]+(?:/[\w.-]+)+)["'`]""")


def _runs_scripts(project, t, reach, mod) -> list[str]:
    """Repo scripts a test runs as a subprocess (System.cmd("python3",
    ["scripts/lib/x.py", ...]), Port.open, subprocess.run, spawn): the call
    graph can't follow a process boundary, but the test does exercise them.
    The spawn may sit in a helper of the test file; the path in a module
    attribute (@script "scripts/x.py")."""
    bodies = []
    for fid in reach:
        f = project.funcs.get(fid)
        if f and f.file == t.file:
            m = project.modules.get(f.module)
            if m:
                bodies.append("\n".join(m.raw[f.start - 1:f.end]))
    if not any(_SPAWN.search(b) for b in bodies):
        return []
    text = "\n".join(bodies) + ("\n" + "\n".join(mod.raw) if mod else "")
    return sorted(p for p in _repo_paths(project, t.file, _SCRIPT_PATH.findall(text)) if not is_test(project, p))


_ROUTE = re.compile(r"""^\s*(get|post|put|patch|delete|head|options|live|resources)\s+"([^"]*)",\s*([A-Z][\w.]*)(?:,\s*:(\w+))?""")
_SCOPE = re.compile(r"""^\s*scope\s+"([^"]*)"(?:\s*,\s*([A-Z][\w.]*))?""")
_OPENS = re.compile(r"\bdo\s*$|\bfn\b.*->\s*$")
_REQUEST = re.compile(r'\b(get|post|put|patch|delete|head|options|live)\(\s*(?:[\w.()]+\s*,\s*)?~?p?"([^"]*)"')
_RESOURCE = {"index": ("get", ""), "show": ("get", "/:id"), "create": ("post", ""), "update": ("put", "/:id"),
             "delete": ("delete", "/:id"), "new": ("get", "/new"), "edit": ("get", "/:id/edit")}


def _routes(project) -> list[tuple[str, re.Pattern, str]]:
    """Phoenix routes as (verb, path pattern, Module.action), from every module
    that `use`s a router: a scope's path and alias prefix the routes inside it."""
    if hasattr(project, "_phoenix_routes"):
        return project._phoenix_routes
    out = []
    for m in project.modules.values():
        if not m.file.endswith(".ex") or not any(re.search(r"use\s+(Phoenix\.Router|\w+,\s*:router)", l) for l in m.raw):
            continue
        stack: list[tuple[str, str]] = []
        for line, code in zip(m.raw, m.code):
            if (sc := _SCOPE.match(line)) and _OPENS.search(code):
                stack.append((sc.group(1), sc.group(2) or ""))
                continue
            if (r := _ROUTE.match(line)):
                verb, path, mod, action = r.groups()
                prefix = "".join(p for p, _ in stack)
                alias = ".".join(a for _, a in stack if a)
                full = f"{alias}.{mod}" if alias else mod
                acts = [(verb, "", action or "")]
                if verb == "live":
                    acts = [("get", "", "mount")]
                elif verb == "resources":
                    acts = [(v, suffix, a) for a, (v, suffix) in _RESOURCE.items()]
                for v, suffix, a in acts:
                    out.append((v, _route_pattern(prefix + path + suffix), f"{full}.{a}"))
                if not _OPENS.search(code):
                    continue
            if _OPENS.search(code):
                stack.append(("", ""))
            elif re.match(r"^\s*end\b", code) and stack:
                stack.pop()
    project._phoenix_routes = out
    return out


def _route_pattern(path: str) -> re.Pattern:
    """"/api/car/:id" -> a regex: `:param` is one segment, `*glob` the rest."""
    segs = [s for s in path.split("/") if s]
    parts = ["[^/]+" if s.startswith(":") else ".*" if s.startswith("*") else re.escape(s) for s in segs]
    return re.compile("^/" + "/".join(parts) + "/?$")


def _routed(project, t, reach, mod) -> list[str]:
    """Controller actions a test's requests reach through the router
    (`get(conn, ~p"/api/car")`, `conn |> post("/api/x", body)`): the call graph
    stops at the HTTP boundary, the test doesn't."""
    if not t.file.endswith(".exs"):
        return []
    routes = _routes(project)
    if not routes:
        return []
    bodies = [mod.raw[t.start - 1:t.end]] if mod else []
    for fid in reach:
        f = project.funcs.get(fid)
        if f and f.file == t.file and f.id != t.id and (m := project.modules.get(f.module)):
            bodies.append(m.raw[f.start - 1:f.end])
    hit = set()
    for verb, path in _REQUEST.findall("\n".join(l for b in bodies for l in b)):
        path = re.sub(r"#\{[^}]*\}", "x", path.split("?", 1)[0])
        for v, pat, fid in routes:
            if (v == verb or verb == "live") and pat.match(path) and fid in project.funcs:
                hit.add(fid)
    return sorted(hit)


def _repo_paths(project, test_file: str, cands) -> set[str]:
    """The repo files these path strings name: from the root ("scripts/x.py",
    "./x"), or from the test's own folder (Path.expand("../../scripts/x.py",
    __DIR__), path.resolve(__dirname, "../x.css"))."""
    here = os.path.dirname(test_file)
    out = set()
    for cand in cands:
        for rel in (os.path.normpath(cand), os.path.normpath(os.path.join(here, cand))):
            if not rel.startswith("..") and not os.path.isabs(rel) \
                    and (rel in project.files or os.path.isfile(os.path.join(project.root, rel))):
                out.add(rel)
                break
    return out


# --- dead and test-only ------------------------------------------------------------

def _callers(project, fid):
    return [(c, ex) for c, _, ex in project.redges.get(fid, []) if c in project.funcs]


def test_only(project, corpus: Corpus | None = None) -> list[dict]:
    """Product functions whose every caller is a test (and at least one exact),
    whose name no product/script/config file mentions: dead code its tests
    keep alive. Delete both, or wire it up."""
    corpus = corpus or Corpus(project)
    out = []
    for f in project.funcs.values():
        if f.kind != "func" or is_test(project, f.file) or is_entry(project, f):
            continue
        cs = _callers(project, f.id)
        if not cs or not any(ex for _, ex in cs):
            continue
        if any(not is_test(project, project.funcs[c].file) for c, _ in cs):
            continue
        if corpus.in_own(f.name, f.file) > 1 or corpus.elsewhere(f.name, f.file, lambda r: is_test(project, r)):
            continue
        tests = sorted({c for c, _ in cs})
        out.append({"id": f.id, "file": f.file, "line": f.start, "lines": f.end - f.start + 1, "tests": tests})
    return sorted(out, key=lambda o: -o["lines"])


def unused(project, corpus: Corpus | None = None) -> list[dict]:
    """Product functions nothing calls or references, whose name appears in no
    other file at all and only once in their own."""
    corpus = corpus or Corpus(project)
    out = []
    for f in core.dead(project):
        if f.kind != "func" or is_test(project, f.file) or is_entry(project, f):
            continue
        if corpus.in_own(f.name, f.file) > 1 or corpus.elsewhere(f.name, f.file):
            continue
        out.append({"id": f.id, "file": f.file, "line": f.start, "lines": f.end - f.start + 1})
    return sorted(out, key=lambda o: -o["lines"])


# --- duplication --------------------------------------------------------------------

_LITERAL_START = re.compile(r"^\s*(?:return\s+|=\s*)?(?:\[|\{|%\w*(?:\.\w+)*\{|(?:listOf|mapOf|setOf|arrayOf|"
                            r"mutableListOf|mutableMapOf|List\.of|Map\.of|dict|frozenset)\s*\()")


def _data_only(f) -> bool:
    """A function whose whole body is one literal table (a list of skill
    structs, a map of labels): two such tables share their shape by design,
    and folding them into a helper would only hide the entries."""
    text = "\n".join(f.code.split("\n")[1:-1])
    m = _LITERAL_START.match(text)
    if not m:
        return False
    depth = 0
    for i in range(m.end() - 1, len(text)):
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return not text[i + 1:].strip()
    return False


def dups(project, min_lines: int = 12, threshold: float = 0.45, only: set[str] | None = None) -> list[dict]:
    """Near-duplicate functions in different files (6-token shingles, Jaccard),
    grouped by file pair: copies to fold into one shared helper. With `only`,
    just the pairs that include one of those ids."""
    def eligible(f):
        return f.kind != "top" and f.end - f.start + 1 >= min_lines and not is_test(project, f.file) \
            and not _data_only(f)

    sh = {}
    want = None
    if only is not None:
        # shingle only functions sharing most of their words with a changed one
        want = {}
        for fid in only:
            f = project.funcs.get(fid)
            if f is not None and eligible(f):
                toks = _TOK.findall(f.code)
                if len(toks) >= 60:
                    want[fid] = set(toks)
        if not want:
            return []
    for f in project.funcs.values():
        if f.end - f.start + 1 < min_lines or not eligible(f):
            continue
        toks = _TOK.findall(f.code)
        if len(toks) < 60:
            continue
        if want is not None and f.id not in want:
            words = set(toks)
            if not any(len(words & w) >= threshold * min(len(words), len(w)) for w in want.values()):
                continue
        sh[f.id] = {tuple(toks[i:i + 6]) for i in range(len(toks) - 5)}
    idx = collections.defaultdict(list)
    for fid, s in sh.items():
        for x in s:
            idx[x].append(fid)
    shared = collections.Counter()
    for fids in idx.values():
        if 2 <= len(fids) <= 12 and (only is None or any(f in only for f in fids)):
            for a, b in itertools.combinations(sorted(fids), 2):
                if only is None or a in only or b in only:
                    shared[(a, b)] += 1
    groups: dict[tuple[str, str], dict] = {}
    for (a, b), n in shared.items():
        fa, fb = project.funcs[a], project.funcs[b]
        if n < 15 or fa.file == fb.file:
            continue
        j = len(sh[a] & sh[b]) / len(sh[a] | sh[b])
        if j < threshold:
            continue
        key = tuple(sorted((fa.file, fb.file)))
        g = groups.setdefault(key, {"files": list(key), "pairs": [], "lines": 0})
        g["pairs"].append({"a": a, "b": b, "similarity": round(j, 2)})
        g["lines"] += min(fa.end - fa.start, fb.end - fb.start) + 1
    return sorted(groups.values(), key=lambda g: -g["lines"])


# --- god modules ---------------------------------------------------------------------

_STOP = set("get set is has on to from with for and the of a in do make build run update load open close handle "
            "apply by fetch default new init render view state data item items value values list map text label id "
            "ok maybe to_ put".split())


def _words(name: str) -> list[str]:
    parts = re.findall(r"[A-Z]?[a-z]+|[A-Z]+(?![a-z])", name.replace("_", " "))
    return [w.lower() for w in parts if w.lower() not in _STOP and len(w) > 2]


def split(project, rel: str) -> dict:
    """How a big file could split: its features (configured `domains`, else
    the leading words of function names) with line counts, and its call
    clusters once widely shared helpers are set aside. One giant cluster
    means the file is knotted: split by feature, moving helpers out first."""
    fs = [f for m in project.by_file.get(rel, []) for f in m.funcs if f.kind != "top"]
    size = lambda f: f.end - f.start + 1
    domains = cfg(project).get("domains") or {}
    feats = collections.Counter()
    feat_fns = collections.defaultdict(list)
    for f in fs:
        n = f.name.lower()
        d = next((k for k, pat in domains.items() if re.search(pat, n)), None)
        if d is None:
            ws = _words(f.name)
            d = ws[0] if ws and not domains else "other"
        feats[d] += size(f)
        feat_fns[d].append(f.name)
    ids = {f.id for f in fs}
    adj, indeg = collections.defaultdict(set), collections.Counter()
    for f in fs:
        for t, _, _ in project.edges.get(f.id, []):
            if t in ids and t != f.id:
                adj[f.id].add(t)
                adj[t].add(f.id)
                indeg[t] += 1
    hubs = {t for t, c in indeg.items() if c > max(6, len(fs) * 0.04)}
    seen, comps = set(), []
    fmap = {f.id: f for f in fs}
    for f in fs:
        if f.id in seen or f.id in hubs:
            continue
        stack, comp = [f.id], []
        seen.add(f.id)
        while stack:
            u = stack.pop()
            comp.append(u)
            for v in adj[u] - hubs - seen:
                seen.add(v)
                stack.append(v)
        comps.append(comp)
    comps.sort(key=lambda c: -sum(size(fmap[i]) for i in c))
    total = sum(size(f) for f in fs) or 1
    return {
        "file": rel, "functions": len(fs), "lines": total,
        "shared_helpers": sorted(fmap[h].name for h in hubs),
        "features": [{"name": k, "lines": v, "examples": feat_fns[k][:5]} for k, v in feats.most_common()],
        "clusters": [{"functions": len(c), "lines": sum(size(fmap[i]) for i in c),
                      "examples": [fmap[i].name for i in c[:5]]} for c in comps if len(c) > 1][:10],
        "knotted": bool(comps) and sum(size(fmap[i]) for i in comps[0]) > total * 0.6,
    }


def hotspots(project, limit: int = 15) -> list[dict]:
    """The biggest files (lines in functions), with function count and how
    many other files call into them: where splits pay off most."""
    per = collections.defaultdict(lambda: {"lines": set(), "functions": 0, "callers": set()})
    for f in project.funcs.values():
        if f.kind == "top" or is_test(project, f.file):
            continue
        p = per[f.file]
        p["lines"].update(range(f.start, f.end + 1))  # (a nested function's lines count once)
        p["functions"] += 1
        for c, _, ex in project.redges.get(f.id, []):
            cf = project.funcs.get(c)
            if cf and ex and cf.file != f.file:
                p["callers"].add(cf.file)
    rows = [{"file": k, "lines": len(v["lines"]), "functions": v["functions"], "caller_files": len(v["callers"])}
            for k, v in per.items()]
    return sorted(rows, key=lambda r: -r["lines"])[:limit]


# --- removing code -------------------------------------------------------------------

_ATTACHED = re.compile(r"\s*(@\w+|#|//|/\*|\*|--|\"\"\"|'''|\"\"\"\s*$)")


def _is_private(project, f: Func) -> bool:
    """Visible only inside its file: Elixir defp, JS/TS not exported, Kotlin /
    Java / C# `private`, Python `_name`."""
    if f.kind == "private":
        return True
    mod = project.modules.get(f.module)
    line = mod.raw[f.start - 1] if mod and f.start - 1 < len(mod.raw) else ""
    ext = os.path.splitext(f.file)[1]
    if ext in (".ts", ".tsx", ".js", ".jsx", ".mjs", ".mts"):
        return "export" not in line
    if ext in (".kt", ".kts", ".java", ".cs"):
        return bool(re.search(r"\bprivate\b", line))
    if ext == ".py":
        return f.name.startswith("_") and not f.name.startswith("__")
    return False


def _orphans(project, gone: set[str]) -> list[str]:
    """Functions left with no caller once `gone` is removed (helpers of what
    was cut), repeatedly: every graph caller is gone, it isn't an entry
    point, and no other file names it; within its own file, every mention
    other than its definition sits inside something removed."""
    corpus = Corpus(project)
    gone = set(gone)
    out: list[str] = []
    changed = True
    while changed:
        changed = False
        spans = collections.defaultdict(list)
        for g in gone:
            f = project.funcs[g]
            spans[f.file].append((f.start, f.end))
        for f in project.funcs.values():
            if f.id in gone or f.kind not in ("func", "private") or is_entry(project, f):
                continue
            cs = [c for c, _, _ in project.redges.get(f.id, []) if c in project.funcs]
            # a file-private helper can't be used from another file, whatever
            # other files happen to name things; a public one can
            if any(c not in gone for c in cs) or (not _is_private(project, f) and corpus.elsewhere(f.name, f.file)):
                continue
            mod = project.modules.get(f.module)
            raw = mod.raw if mod else []
            pat = re.compile(r"(?<![\w])" + re.escape(f.name) + r"(?![\w?!])")
            mentions = [i + 1 for i, ln in enumerate(raw) if pat.search(ln) and not (f.start <= i + 1 <= f.end)]
            inside = [i for i in mentions if any(a <= i <= b for a, b in spans[f.file])]
            # used only from removed code (a call or a `&name(&1)` capture the
            # graph may not see); never-used code is `unused`'s job, not this
            if not inside or len(inside) != len(mentions):
                continue
            gone.add(f.id)
            out.append(f.id)
            changed = True
    return out


_BUILTIN_ATTRS = {"doc", "moduledoc", "spec", "impl", "type", "typep", "opaque", "callback", "behaviour",
                  "derive", "enforce_keys", "primary_key", "foreign_key_type", "schema_prefix", "timestamps_opts",
                  "before_compile", "after_compile", "on_load", "compile", "dialyzer", "deprecated", "external_resource",
                  "optional_callbacks", "vsn", "tag", "moduletag", "describetag", "macrocallback", "since"}


# File-level declarations whose every use was in removed code. Per language:
# (declaration regex with the name in group 1, how a use is spelled).
_DECLS = {
    (".ex", ".exs"): (r"\s*@(\w+)\s+\S", lambda n: r"@" + n + r"\b"),
    (".ts", ".tsx", ".js", ".jsx", ".mjs", ".mts"): (r"(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*[:=]",
                                                     lambda n: r"(?<![\w$.])" + re.escape(n) + r"(?![\w$])"),
    (".kt", ".kts"): (r"\s*private\s+(?:const\s+)?val\s+(\w+)\b", lambda n: r"(?<![\w.])" + n + r"\b"),
    (".py",): (r"(_[A-Za-z]\w*)\s*=", lambda n: r"(?<![\w.])" + n + r"\b"),
}


def _drop_orphan_decls(rel: str, lines: list[str], drop: set[int]) -> list[str]:
    """Declarations (Elixir @attributes, top-level non-exported JS/TS consts,
    Kotlin private vals, Python _names) whose every use sat in removed code
    (in place: adds their lines to `drop`). Repeats: one may feed another."""
    spec = next((v for k, v in _DECLS.items() if rel.endswith(k)), None)
    if not spec:
        return []
    decl, use = spec
    report = []
    removed_text = "\n".join(lines[k] for k in sorted(drop))
    while True:
        kept = [(k, l) for k, l in enumerate(lines) if k not in drop]
        text = "\n".join(l for _, l in kept)
        found = False
        for k, l in kept:
            m = re.match(decl, l)
            if not m or (rel.endswith((".ex", ".exs")) and m.group(1) in _BUILTIN_ATTRS):
                continue
            if rel.endswith((".ts", ".tsx", ".js", ".jsx", ".mjs", ".mts")) and (l[:1].isspace() or "export" in l[:m.start(1)]):
                continue  # only file-level, non-exported
            name = m.group(1)
            if len(re.findall(use(name), text)) > 1 or not re.search(use(name), removed_text):
                continue  # still used, or never used by the removed code (not ours to judge)
            depth, j = 0, k
            while j < len(lines):
                # a heredoc / multi-line string opened here runs to its closer
                # (`@prompt """` ... `"""`); its brackets are text, not structure
                q = next((d for d in ('"""', "'''", "`") if lines[j].count(d) % 2 == 1), None)
                if q:
                    j += 1
                    while j < len(lines) and q not in lines[j]:
                        j += 1
                    if j >= len(lines):
                        break
                    tail = lines[j].split(q, 1)[1]
                    depth += sum(tail.count(c) for c in "[{(") - sum(tail.count(c) for c in "]})")
                else:
                    depth += sum(lines[j].count(c) for c in "[{(") - sum(lines[j].count(c) for c in "]})")
                if depth <= 0 and not lines[j].rstrip().endswith((",", "=", "\\", "(", "[", "{")):
                    break
                j += 1
            drop.update(range(k, j + 1))
            if j + 1 < len(lines) and not lines[j + 1].strip() and k > 0 and not lines[k - 1].strip():
                drop.add(j + 1)
            removed_text += "\n" + "\n".join(lines[k:j + 1])
            report.append(f"{rel}:{k + 1}-{j + 1}  {name}  (orphaned)")
            found = True
            break
        if not found:
            return report


def remove(project, ids: list[str], dry: bool = True, cascade: bool = True) -> list[str]:
    """Delete functions (or test blocks: they are functions too) by id, with
    the doc comments / @doc heredocs / annotations directly above each. Every
    clause of a multi-clause function goes. With `cascade`, helpers that only
    the removed code called go too. Returns what was (or would be) removed;
    nothing is written when dry."""
    by_file = collections.defaultdict(list)
    missing = [i for i in ids if i not in project.funcs]
    if missing:
        raise KeyError(f"no such function: {', '.join(missing)} (ids as `seamcheck find` prints them)")
    orphans = _orphans(project, set(ids)) if cascade else []
    for fid in list(ids) + orphans:
        by_file[project.funcs[fid].file].append(project.funcs[fid])
    orphan_set = set(orphans)
    report = []
    for rel, fs in by_file.items():
        path = os.path.join(project.root, rel)
        with open(path, encoding="utf-8") as fh:
            lines = fh.read().split("\n")
        drop: set[int] = set()
        for f in fs:
            a, b = f.start - 1, f.end - 1
            i = a - 1
            while i >= 0:
                s = lines[i].strip()
                if s in ('"""', "'''"):  # the end of a heredoc doc: find its opener
                    j = i - 1
                    while j >= 0 and not re.search(r'(@doc|""")\s*(~S)?"""\s*$|@doc\s+~?S?"""', lines[j]):
                        j -= 1
                    if j >= 0 and "@doc" in lines[j]:
                        i = j - 1
                        continue
                    break
                if s and _ATTACHED.match(lines[i]) and not re.match(r"(@moduledoc|#!|#\s*(include|define|if|pragma|import)\b)", s):
                    i -= 1
                    continue
                break
            start = i + 1
            drop.update(range(start, b + 1))
            if b + 1 < len(lines) and not lines[b + 1].strip() and (start == 0 or not lines[start - 1].strip()):
                drop.add(b + 1)
            report.append(f"{rel}:{start + 1}-{b + 1}  {f.name}" + ("  (orphaned)" if f.id in orphan_set else ""))
        if cascade:
            report += _drop_orphan_decls(rel, lines, drop)
        if not dry:
            with open(path, "w", encoding="utf-8") as fh:
                fh.write("\n".join(l for k, l in enumerate(lines) if k not in drop))
    return report


# --- the land gate ---------------------------------------------------------------

_MANIFESTS = ("mix.exs", "mix.lock", "package.json", "package-lock.json", "build.gradle", "build.gradle.kts",
              "libs.versions.toml", "Cargo.toml", "Cargo.lock", "pyproject.toml", "go.mod")


def changed_lines(project, ref: str, staged: bool = False, deletions: bool = False) -> dict[str, set[int]]:
    """Lines added or changed since `ref` (committed, staged, unstaged), per
    file; an untracked file counts whole. `staged`: only what is about to be
    committed (a pre-commit hook: other work in the tree isn't judged).
    `deletions`: a pure deletion marks the line it happened after (impact: a
    function that lost lines changed too)."""
    import subprocess
    out: dict[str, set[int]] = collections.defaultdict(set)
    try:
        diff = subprocess.run(["git", "diff", "-U0", "--no-color", *(["--cached"] if staged else []), ref],
                              cwd=project.root, capture_output=True, text=True, timeout=120).stdout
        untracked = [] if staged else subprocess.run(
            ["git", "ls-files", "--others", "--exclude-standard"], cwd=project.root,
            capture_output=True, text=True, timeout=60).stdout.split()
    except (OSError, subprocess.TimeoutExpired):
        return out
    cur = None
    for line in diff.splitlines():
        if line.startswith("+++ "):
            cur = line[6:] if line.startswith("+++ b/") else None
        elif line.startswith("@@") and cur:
            m = re.search(r"\+(\d+)(?:,(\d+))?", line)
            if m:
                start, n = int(m.group(1)), int(m.group(2) or 1)
                out[cur].update(range(start, start + n))
                if n == 0 and deletions:
                    out[cur].update((max(1, start), start + 1))
            if cur not in out:
                out[cur] = set()  # a pure deletion still marks the file changed
    for rel in untracked:
        try:
            with open(os.path.join(project.root, rel), encoding="utf-8", errors="replace") as fh:
                out[rel].update(range(1, fh.read().count("\n") + 2))
        except OSError:
            pass
    return out


def moved_lines(project, ref: str, staged: bool, lines: dict[str, set[int]]) -> tuple[dict[str, set[int]], set[str]]:
    """Added lines whose text the same change also removed somewhere (a file
    split, a function moved between files, a rename): old code in a new place,
    not new code. Matched by stripped text, each removed line pays for one
    added line. Also returns the files code moved OUT of into other files."""
    import subprocess
    try:
        diff = subprocess.run(["git", "diff", "-U0", "--no-color", "-M", *(["--cached"] if staged else []), ref],
                              cwd=project.root, capture_output=True, text=True, timeout=120).stdout
    except (OSError, subprocess.TimeoutExpired):
        return {}, set()
    removed: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    src = None
    for line in diff.splitlines():
        if line.startswith("--- "):
            src = line[6:] if line.startswith("--- a/") else None
        elif line.startswith("-") and src and line[1:].strip():
            removed[line[1:].strip()][src] += 1
    out: dict[str, set[int]] = collections.defaultdict(set)
    sources: set[str] = set()
    if not removed:
        return out, sources
    for rel in sorted(lines):
        try:
            with open(os.path.join(project.root, rel), encoding="utf-8", errors="replace") as fh:
                text = fh.read().split("\n")
        except OSError:
            continue
        for ln in sorted(lines[rel]):
            t = text[ln - 1].strip() if 0 < ln <= len(text) else ""
            pool = removed.get(t)
            if t and pool and sum(pool.values()) > 0:
                origin = next(f for f, n in pool.items() if n > 0)
                pool[origin] -= 1
                out[rel].add(ln)
                if origin != rel and len(t) > 12:  # a real line, not a brace, left `origin`
                    sources.add(origin)
    return out, sources


def moved_refs(project, sources: set[str], changed: set[str]) -> list[dict]:
    """Tests and scripts that name a file code just moved out of, by path. A
    check reading `src/ContinuityClient.cpp` kept passing after the split moved
    what it asserts on into 12 other files (lane M): it now checks less than it
    says. The referencing line must change (point at the new files or a glob)
    or say `seamcheck: allow moved-ref`."""
    if not sources:
        return []
    out = []
    names = {s: [s, s.split("/", 1)[1] if "/" in s else s] for s in sources}
    for rel in project.files:
        if rel in changed or not (is_test(project, rel) or rel.startswith(("scripts/", "test/", "tests/"))):
            continue
        try:
            with open(os.path.join(project.root, rel), encoding="utf-8", errors="replace") as fh:
                raw = fh.read().split("\n")
        except OSError:
            continue
        for i, text in enumerate(raw, 1):
            for s, forms in names.items():
                if any(f in text for f in forms) and not core._suppressed(raw, i, "moved-ref"):
                    out.append({"file": rel, "line": i, "moved_from": s})
    return out

_GENERATED = re.compile(r"@generated|\bGENERATED\b|DO NOT EDIT|generated by|auto-?generated", re.I)


def _is_generated(project, rel: str) -> bool:
    """A file a generator wrote (its banner says so in the first lines)."""
    try:
        with open(os.path.join(project.root, rel), encoding="utf-8", errors="replace") as fh:
            head = fh.read(600)
    except OSError:
        return False
    return bool(_GENERATED.search(head))


_VERSION_LINE = re.compile(r'^\s*(?:version\s*[:=]|"version"\s*:|versionCode\s*=|versionName\s*=|@version\b|@app_version\b)')


def _version_only(root: str, rel: str, changed: set[int]) -> bool:
    """A manifest edit that only moves version lines (bump_version.sh on every
    land) changes no dependency: the deps rules don't need to run for it."""
    if not changed or os.path.basename(rel).endswith((".lock", "-lock.json")):
        return False
    try:
        with open(os.path.join(root, rel), encoding="utf-8", errors="replace") as fh:
            text = fh.read().split("\n")
    except OSError:
        return False
    return all(0 < ln <= len(text) and _VERSION_LINE.match(text[ln - 1]) for ln in changed)


def _rule_scoped(project, lines) -> bool:
    """A changed file outside `[project] include` that a rule still reads: a
    repo-wide lint (deploy scripts, units, lockfiles), a forbid / require with
    repo = true or an explicit path, a command's `when`."""
    from . import core, lint
    for r in project.config.get("rule", []):
        kind = r.get("kind", "forbid")
        globs = []
        if kind == "lint" and (r.get("lint") in lint.REPO_WIDE or r.get("repo")):
            globs = r.get("in", lint.DEFAULT_IN.get(r.get("lint"), []))
        elif kind in ("forbid", "require") and (r.get("repo") or kind == "require"):
            globs = r.get("in", [])
        elif kind == "command":
            globs = r.get("when", ["**/*"])
        if globs and any(core.match_any(rel, globs) for rel in lines):
            return True
    return False


class _NamePart:
    """Finds identifiers one of whose parts (split at _ and camelCase) is a
    word, or the word with a plain ending: fallback, fallbacks, compatibility."""
    _IDENT = re.compile(r"\b[A-Za-z_]\w*\b")
    _PART = re.compile(r"[A-Z]+(?![a-z])|[A-Z]?[a-z0-9]+")

    def __init__(self, words):
        alt = "|".join(re.escape(w) for w in words)
        self.word = re.compile(rf"(?:{alt})(?:s|es|ed|ing|ity|ibility|ible|able)?", re.I)

    def finditer(self, text):
        return (m for m in self._IDENT.finditer(text)
                if any(self.word.fullmatch(p) for p in self._PART.findall(m.group(0))))

    def search(self, text):
        return next(self.finditer(text), None)


def gate(project, ref: str = "HEAD", staged: bool = False) -> dict:
    """What a change may not land with: rule errors anywhere (the repo is kept
    at zero), plus dead code, copies and do-nothing tests in the lines it
    touched. Old debt in a touched file does not block; new debt does."""
    from . import core
    lines = changed_lines(project, ref, staged)
    touched = {rel for rel in lines if rel in project.by_file or rel in project.files}
    # judged as new: changed lines minus code the change only moved
    moved, moved_from = moved_lines(project, ref, staged, lines)
    fresh = {rel: ln - moved.get(rel, set()) for rel, ln in lines.items()}
    manifest_changed = sorted(r for r in lines if os.path.basename(r) in _MANIFESTS
                              and not _version_only(project.root, r, lines[r]))
    report = {"changed_files": sorted(lines), "rules": [], "forbidden": [], "unused": [], "test_only": [],
              "dups": [], "tests": [], "moved_refs": []}
    if not touched and not manifest_changed and not _rule_scoped(project, lines):
        return report

    skip = [] if manifest_changed else [r["id"] for r in project.config.get("rule", []) if r.get("kind") == "deps"]
    # a rule's `gate`: "all" (default: its errors anywhere block), "changed"
    # (its findings, warnings too, block only on lines this change adds or
    # edits: a heuristic keeps old debt visible in `check` and stops new debt),
    # "off" (never blocks)
    mode = {r["id"]: r.get("gate", "all") for r in project.config.get("rule", [])}

    def blocks(f) -> bool:
        m = mode.get(f.rule, "all")
        if m == "off" or " not checked here: " in f.message:
            return False  # a check that couldn't run here vouches neither way
        if m == "changed":
            return f.severity in ("error", "warn") and f.line in fresh.get(f.file, ())
        if m == "touched":  # any finding in a file this change touches (or about the whole check)
            return f.severity in ("error", "warn") and (not f.file or f.file in lines)
        return f.severity == "error"
    project.gate_changed = set(lines)  # command rules run only when their `when` paths changed
    deps_ids = {r["id"] for r in project.config.get("rule", []) if r.get("kind") == "deps"}
    owners = {os.path.dirname(m) for m in manifest_changed}
    # a dependency finding blocks only in a manifest this change edited
    # (or whose lockfile it edited): another project's debt isn't this change's
    report["rules"] = [f for f in core.run_rules(project, skip=skip) if blocks(f)
                       and (f.rule not in deps_ids or os.path.dirname(f.file) in owners)]

    # [gate] forbid_added: words new code may not name (fallback, legacy, ...),
    # checked in identifiers on changed lines; comments and strings don't count
    words = project.config.get("gate", {}).get("forbid_added")
    report["forbidden"] = []
    if words:
        # a word is a whole part of a name (fallbackUrl, legacy_path,
        # CompatLayer), not letters inside another word ("shim" in StashImages)
        rx = _NamePart(words)
        # a name a generator emits (tokens.generated.ts) is the generator's
        # contract, not hand-written fallback code: using it is allowed
        generated_names = {m.group(0) for rel in project.files
                           if "generated" in rel.lower() and _is_generated(project, rel)
                           for m in rx.finditer("\n".join(core._text_for(project, rel, "code")))}
        for rel in sorted(touched):
            if is_test(project, rel) or _is_generated(project, rel):
                continue
            code = core._text_for(project, rel, "code")
            raw = core._text_for(project, rel, "raw")
            for ln in sorted(fresh[rel]):
                if ln <= len(code) and (m := rx.search(code[ln - 1])) and not core._suppressed(raw, ln, "gate") \
                        and m.group(0) not in generated_names:
                    report["forbidden"].append({"file": rel, "line": ln, "word": m.group(0)})

    def waived(f) -> bool:
        return core._suppressed(core._text_for(project, f.file, "raw"), f.start, "gate")

    funcs = [f for rel in touched for m in project.by_file.get(rel, []) for f in m.funcs
             if f.kind != "top" and not waived(f)]
    # new (or re-declared): judged for dead code and do-nothing tests; an edit
    # inside an old function doesn't make its old debt yours
    new = {f.id for f in funcs if f.start in fresh[f.file]}
    # touched anywhere (moved code aside): judged for copies. Generated files
    # repeat their generator's shapes by design (api/generate.mjs emits one
    # parser per schema def), so they are never a copy to remove.
    hit = {f.id for f in funcs if fresh[f.file] & set(range(f.start, f.end + 1))
           and not _is_generated(project, f.file)}

    corpus = Corpus(project)
    report["unused"] = [r for r in unused(project, corpus) if r["id"] in new]
    report["test_only"] = [r for r in test_only(project, corpus) if r["id"] in new]
    report["dups"] = [{"a": p["a"], "b": p["b"], "similarity": p["similarity"]}
                      for g in dups(project, only=hit) for p in g["pairs"]]
    report["tests"] = [r for r in test_audit(project, only=new) if r["reach"] == 0 or r["reads_source"]]
    report["moved_refs"] = moved_refs(project, moved_from, set(lines))
    return report


def gate_failed(report: dict) -> bool:
    return any(report.get(k) for k in ("rules", "forbidden", "unused", "test_only", "dups", "tests", "moved_refs"))
