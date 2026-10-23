"""Load a project, parse it (in parallel, cached), build the call graph, run
the rules.

Config: seamcheck.toml at the project root (see README.md). Everything is
deterministic: same files + same config = same report.
"""
from __future__ import annotations

import bisect
import difflib
import fnmatch
import hashlib
import multiprocessing
import json
import os
import pickle
import re
import subprocess
from collections import defaultdict, deque
import signal
from concurrent.futures import ProcessPoolExecutor
from dataclasses import dataclass, field

from . import config as config_mod
from . import lang_elixir, lang_gdscript, lang_other, lang_ruby
from .model import Func, Module
from .scan import blank

CACHE_VERSION = 34


def _code_version() -> str:
    """CACHE_VERSION plus a digest of seamcheck's own source: a changed adapter
    or resolver never reuses a parse or graph the old code made (forgetting
    to bump CACHE_VERSION used to leave warm runs on the old graph)."""
    global _CODE
    if _CODE is None:
        h = hashlib.sha1(str(CACHE_VERSION).encode())
        here = os.path.dirname(os.path.abspath(__file__))
        for name in sorted(os.listdir(here)) + sorted("packs/" + n for n in os.listdir(os.path.join(here, "packs"))):
            if name.endswith((".py", ".txt", ".toml")):
                with open(os.path.join(here, name), "rb") as fh:
                    h.update(name.encode() + b"\0" + fh.read())
        _CODE = f"{CACHE_VERSION}-{h.hexdigest()[:12]}"
    return _CODE


_CODE: str | None = None
DEFAULT_EXCLUDE = ["**/.git/**", "**/node_modules/**", "**/deps/**", "**/_build/**", "**/target/**",
                   "**/.godot/**", "**/.claude/**", "**/dist/**", "**/build/**", "**/.venv/**", "**/__pycache__/**",
                   # third-party code kept in the tree (Go / Composer / Bundler vendoring, a vendored JS bundle):
                   # not the project's to check; `include` it explicitly to have it read
                   "**/vendor/**", "**/third_party/**", "**/*.min.js"]


def default_exclude(include: list[str]) -> list[str]:
    """DEFAULT_EXCLUDE, less what the project's `include` names (`vendor/**`
    listed in include: the vendored code is read)."""
    named = " ".join(include)
    return [d for d in DEFAULT_EXCLUDE if d.strip("*/").split("/")[0] not in named]


def adapter_for(rel: str):
    ext = os.path.splitext(rel)[1]
    if ext in lang_gdscript.EXT:
        return lang_gdscript.parse
    if ext in lang_elixir.EXT:
        return lang_elixir.parse
    if ext == ".py":
        return lang_other.parse_python
    if ext in lang_ruby.EXT:
        return lang_ruby.parse
    if ext in lang_other.LANGS:
        return lang_other.parse_brace
    return None


def scan_family(rel: str) -> str:
    ext = os.path.splitext(rel)[1]
    if ext in lang_gdscript.EXT:
        return "gdscript"
    if ext in lang_elixir.EXT:
        return "elixir"
    if ext == ".py":
        return "python"
    if ext in lang_ruby.EXT:
        return "ruby"
    if ext in lang_other.LANGS:
        return lang_other.LANGS[ext][0]
    return "python"  # '#' comments are the safest guess


# --- globs ------------------------------------------------------------------

_glob_cache: dict[str, re.Pattern] = {}


def glob_re(g: str) -> re.Pattern:
    """fnmatch with ** spanning directories."""
    if g not in _glob_cache:
        out, i = "", 0
        while i < len(g):
            if g.startswith("**/", i):
                out += "(?:.*/)?"
                i += 3
            elif g.startswith("**", i):
                out += ".*"
                i += 2
            elif g[i] == "*":
                out += "[^/]*"
                i += 1
            elif g[i] == "?":
                out += "[^/]"
                i += 1
            else:
                out += re.escape(g[i])
                i += 1
        _glob_cache[g] = re.compile(out + r"\Z")
    return _glob_cache[g]


def match_any(path: str, globs: list[str]) -> bool:
    return any(glob_re(g).match(path) for g in globs)


# --- the project ----------------------------------------------------------------

def _zero3() -> list[int]:  # [exact, guess only, nothing, nothing though a project function has the name]
    return [0, 0, 0, 0]


@dataclass
class Project:
    root: str
    config: dict
    modules: dict[str, Module] = field(default_factory=dict)  # name -> module
    by_file: dict[str, list[Module]] = field(default_factory=dict)
    funcs: dict[str, Func] = field(default_factory=dict)  # id -> func
    by_name: dict[str, list[Func]] = field(default_factory=lambda: defaultdict(list))
    edges: dict[str, list[tuple[str, int, bool]]] = field(default_factory=lambda: defaultdict(list))  # caller -> [(callee, line, exact)]
    redges: dict[str, list[tuple[str, int, bool]]] = field(default_factory=lambda: defaultdict(list))
    files: list[str] = field(default_factory=list)  # every included file (also ones with no adapter)
    children: dict[str, list[str]] = field(default_factory=lambda: defaultdict(list))
    # short name (file stem, declared type) -> modules, for path-named languages
    short: dict[str, list[str]] = field(default_factory=lambda: defaultdict(list))
    # cross-language edges added by [[bridge]] rules: (src, dst) -> bridge id
    bridged: dict = field(default_factory=dict)
    bridge_problems: list = field(default_factory=list)
    # Kotlin/Java package -> the modules (files) in it
    packages: dict[str, list[str]] = field(default_factory=lambda: defaultdict(list))
    # per file extension: [exact, fuzzy only, unresolved] call sites. A low exact
    # share is the tool missing imports, not the code having no calls.
    resolution: dict[str, list[int]] = field(default_factory=lambda: defaultdict(_zero3))
    text_cache: dict = field(default_factory=dict)
    parse_ms: int = 0
    config_problems: list[str] = field(default_factory=list)
    precise: dict = field(default_factory=dict)  # what the language servers settled (precise.py), when asked
    params: dict = field(default_factory=dict)  # the packs' [[param]] entries in force ("<pack>/<id>" -> entry)

    # ---- lookups
    def module_of_file(self, rel: str) -> Module | None:
        mods = self.by_file.get(rel)
        return mods[0] if mods else None

    def text(self, rel: str) -> str:
        with open(os.path.join(self.root, rel), encoding="utf-8", errors="replace") as fh:
            return fh.read()

    def find(self, spec: str) -> list[Func]:
        """An id as `find` prints it, 'name' (glob ok), 'Module.name', 'path/glob:funcglob',
        or 'path/file.ext:LINE' (the function holding that line)."""
        if spec in self.funcs:  # (an id copied from another answer: `lib/sinatra/base.Sinatra::Helpers.logger`)
            return [self.funcs[spec]]
        lm = re.match(r"^(.+?\.\w+):(\d+)$", spec)
        if lm and lm.group(1) in self.by_file:  # `src/requests/auth.py:150`
            ln = int(lm.group(2))
            held = [f for f in self.funcs.values() if f.file == lm.group(1) and f.start <= ln <= f.end and f.kind != "top"]
            return sorted(held, key=lambda f: f.end - f.start)[:1]
        if ":" in spec:
            pg, _, fg = spec.partition(":")
            return [f for f in self.funcs.values() if glob_re(pg).match(f.file) and fnmatch.fnmatchcase(f.name, fg or "*")]
        if "/" in spec:
            return [f for f in self.funcs.values() if glob_re(spec).match(f.file)]
        if "." in spec:
            mod, _, name = spec.rpartition(".")
            return [f for f in self.funcs.values()
                    if (f.module == mod or f.module.endswith("." + mod) or f.module.endswith("/" + mod)) and fnmatch.fnmatchcase(f.name, name)]
        return [f for f in self.funcs.values() if fnmatch.fnmatchcase(f.name, spec) and f.kind != "top"]


def _res_map(files: list[str]) -> dict[str, str]:
    """Godot: res://path -> module name (class_name or file stem)."""
    out = {}
    for rel in files:
        if rel.endswith(".gd"):
            out["res://" + rel] = os.path.splitext(os.path.basename(rel))[0]
    return out


def _worker_init() -> None:
    """A parse worker leaves Ctrl-C to the run, and ends when the run does.

    Ctrl-C used to reach the workers mid-task too; the broken pool then left
    the run waiting forever in its shutdown, and whatever had started it gave
    up and left the pool behind (17 idle processes, three days old).
    """
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    try:  # Linux: SIGTERM this worker when the process that forked it dies.
        import ctypes
        ctypes.CDLL("libc.so.6", use_errno=True).prctl(1, signal.SIGTERM)  # PR_SET_PDEATHSIG
    except (OSError, AttributeError):
        pass


def _parse_parallel(todo: list, ctx) -> dict:
    """Parse `todo` across worker processes. An interrupted run (Ctrl-C, an
    error) kills its workers instead of waiting on them."""
    ex = ProcessPoolExecutor(mp_context=ctx, initializer=_worker_init)
    try:
        out = dict(ex.map(_parse_one, todo, chunksize=16))
    except BaseException:
        for proc in list((getattr(ex, "_processes", None) or {}).values()):
            proc.kill()
        ex.shutdown(wait=False, cancel_futures=True)
        raise
    ex.shutdown()
    return out


def _parse_one(args):
    root, rel, res = args
    fn = adapter_for(rel)
    if fn is None:
        return rel, []
    try:
        with open(os.path.join(root, rel), encoding="utf-8", errors="replace") as fh:
            text = fh.read()
    except OSError:
        return rel, []
    try:
        return rel, fn(os.path.join(root, rel), rel, text, lambda p: res.get(p, ""))
    except Exception as e:  # a file the adapter can't read shouldn't sink the run
        return rel, [("error", str(e))]


def _git_files(root: str) -> list[str] | None:
    """Tracked plus untracked-but-not-ignored files under root, or None outside a repo."""
    try:
        r = subprocess.run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root,
                           capture_output=True, text=True, check=False, timeout=120)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if r.returncode != 0:
        return None
    return [p for p in r.stdout.split("\0") if p and os.path.isfile(os.path.join(root, p))]


def list_files(root: str, include: list[str], exclude: list[str]) -> list[str]:
    try:
        # ripgrep filters up front (and honours .gitignore): listing is most of
        # the cost on big repos otherwise.
        # (--follow: a workspace folder of symlinks to checkouts is one graph across them)
        args = ["rg", "--files", "--hidden", "--follow", "-g", "!.git"]
        for g in include:
            args += ["-g", g]
        for g in exclude:
            args += ["-g", "!" + g]
        out = subprocess.run(args, cwd=root, capture_output=True, text=True, check=False).stdout.splitlines()
    except FileNotFoundError:
        # no ripgrep (Pylon, 2026-10-06: a bare os.walk read node_modules, deps
        # and _build, 164 s instead of 14): git's own listing honours .gitignore
        out = _git_files(root)
        if out is None:
            out, seen = [], set()
            for dp, dn, fn in os.walk(root, followlinks=True):
                real = os.path.realpath(dp)
                if real in seen:  # (a symlink loop)
                    dn[:] = []
                    continue
                seen.add(real)
                dn[:] = [d for d in dn if d not in (".git", "node_modules", "_build", "deps", ".gradle", "build")]
                for f in fn:
                    out.append(os.path.relpath(os.path.join(dp, f), root))
    return _one_copy(root, sorted(p for p in out if match_any(p, include) and not match_any(p, exclude)))


def _one_copy(root: str, files: list[str]) -> list[str]:
    """Each real file once: a symlink inside the repo to a folder it already holds
    (swift-argument-parser's Plugins/*/GenerateCommon) would define everything twice.
    The path without a link wins; a link to code outside the root (a workspace) stays."""
    real_root = os.path.realpath(root)
    seen, out = {}, []
    for rel in files:
        real = os.path.realpath(os.path.join(root, rel))
        direct = real == os.path.join(real_root, rel)
        if real in seen:
            if direct and not seen[real][1]:
                out[seen[real][0]] = rel
                seen[real] = (seen[real][0], True)
            continue
        seen[real] = (len(out), direct)
        out.append(rel)
    return sorted(out)


def _stamp(root: str, rel: str):
    try:
        st = os.stat(os.path.join(root, rel))
        return (st.st_mtime_ns, st.st_size)
    except OSError:
        return None


def load(root: str, config_path: str | list[str] | None = None, use_cache: bool = True,
         precise: bool | None = None) -> Project:
    """precise: let language servers settle the calls the static resolver
    guesses (precise.py); None: as `[precise] enabled` says."""
    import time
    t0 = time.time()
    root = os.path.abspath(root)
    extra = [config_path] if isinstance(config_path, str) else list(config_path or [])
    config, problems = config_mod.load(root, extra)
    proj_cfg = config.get("project", {})
    include = proj_cfg.get("include", ["**/*"])
    exclude = default_exclude(include) + proj_cfg.get("exclude", [])
    files = list_files(root, include, exclude)
    if precise is None:
        precise = bool(config.get("precise", {}).get("enabled"))
    project = Project(root, config, files=files)
    project.config_problems = problems
    # what the frameworks in use hand the functions they call (packs' [[param]]):
    # type knowledge, so a detected framework's applies without being listed
    from . import init as init_mod
    project.params = config_mod.pack_params(sorted(set(proj_cfg.get("packs", []) or [])
                                                   | set(init_mod.detect_packs(root, files))))
    res = _res_map(files)
    # caches are keyed by content and shared by every checkout of the repo (a
    # lane's worktree reuses the main checkout's parse, graph and command runs)
    from . import fingerprint as fp
    repo = fp.repo_key(root)
    hashes = fp.Hashes(root)
    parse_name = f"{repo}-parse.pkl"
    cache = {}
    if use_cache:
        c = fp.load(parse_name, {})
        if isinstance(c, dict) and c.get("v") == _code_version():
            cache = c["files"]
    todo, parsed = [], {}
    digests = {}
    for rel in files:
        if adapter_for(rel) is None:
            continue
        h = hashes.of(rel)
        if h is None:
            continue
        digests[rel] = h
        hit = cache.get(rel, {}).get(h)
        if hit is not None:
            parsed[rel] = hit
        else:
            todo.append((root, rel, res))
    # The whole linked graph, when nothing changed: an agent runs seamcheck
    # many times in a row, and linking (edges) is most of a warm load.
    from . import precise as precise_mod
    graph_key = hashlib.sha1(json.dumps([_code_version(), extra, config, sorted(files), sorted(digests.items())]
                                        + ([precise_mod.key(config)] if precise else []),
                                        sort_keys=True, default=str).encode()).hexdigest()
    graph_name = f"{repo}-graph-{graph_key[:16]}.pkl"
    index_name = f"{repo}-graphs.pkl"
    recent = fp.load(index_name, []) if use_cache else []
    if not isinstance(recent, list):
        recent = []
    previous = None
    if use_cache:
        g = fp.load(graph_name, None)
        if isinstance(g, dict) and g.get("key") == graph_key:
            cached = g["project"]
            cached.root = root
            cached.parse_ms = int((time.time() - t0) * 1000)
            hashes.save()
            fp.save(index_name, [graph_name] + [n for n in recent if n != graph_name][:3])
            return cached
        for n in recent:  # the newest graph of this repo: its unchanged files' blanked text is still good
            g = fp.load(n, None)
            if isinstance(g, dict) and "project" in g:
                previous = g
                break
    if len(todo) > 64:
        # fork where there is one: a script importing seamcheck as a library
        # (no `if __name__ == "__main__"`) crashed under 3.14's forkserver
        ctx = multiprocessing.get_context("fork") if "fork" in multiprocessing.get_all_start_methods() else None
        parsed.update(_parse_parallel(todo, ctx))
    else:
        for t in todo:
            rel, mods = _parse_one(t)
            parsed[rel] = mods
    if use_cache and todo:
        ok = {r: v for r, v in parsed.items() if not any(isinstance(m, tuple) for m in v)}  # retry crashes next run
        files_cache = dict(cache)
        for r in ok:
            slot = dict(files_cache.get(r, {}))
            slot[digests[r]] = ok[r]
            # two versions per file: the main checkout's and a lane's
            files_cache[r] = dict(list(slot.items())[-2:])
        fp.save(parse_name, {"v": _code_version(), "files": files_cache})
    for rel in files:
        for m in parsed.get(rel, []):
            if isinstance(m, tuple):
                # an adapter crash: say so (it used to leave the file silently empty)
                project.config_problems.append(f"{rel}: parser error: {m[1]}")
                continue
            name = m.name
            if name in project.modules:  # two files, one name: keep both, disambiguate
                name = f"{m.name}@{rel}"
                m.name = name
                for f in m.funcs:
                    f.module = name
                    f.id = f"{name}.{f.name}"
            project.modules[name] = m
            project.by_file.setdefault(rel, []).append(m)
            for f in m.funcs:
                project.funcs[f.id] = f
                project.by_name[f.name].append(f)
    if previous is not None:
        # blanked text of files that didn't change is still good (blanking is
        # most of a harvest after a one-file edit)
        old_digests = previous.get("text_digests", {})
        for k, v in previous["project"].text_cache.items():
            if k[0] in old_digests and old_digests[k[0]] == hashes.of(k[0]):
                project.text_cache[k] = v
        previous = None
    _link(project, precise, use_cache)
    project.__dict__.pop("_counted", None)  # (only for counting while linking)
    _apply_bridges(project)
    project.config_problems += project.bridge_problems
    if use_cache:
        text_digests = {rel: hashes.of(rel) for rel in {k[0] for k in project.text_cache}}
        fp.save(graph_name, {"key": graph_key, "project": project, "text_digests": text_digests})
        fp.save(index_name, [graph_name] + [n for n in recent if n != graph_name][:3])
        hashes.save()
        fp.prune(keep={graph_name, parse_name, index_name, hashes.name})
    project.parse_ms = int((time.time() - t0) * 1000)
    return project


def _lookup(project: Project, mod_name: str, name: str) -> Func | None:
    seen = set()
    while mod_name and mod_name not in seen:
        seen.add(mod_name)
        mod = project.modules.get(mod_name)
        if mod is None:
            return None
        f = project.funcs.get(f"{mod_name}.{name}")
        if f:
            return f
        mod_name = mod.parent
    return None


_JS_EXT = ("", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".mts", ".cjs")


def _read_tsconfig(path: str, depth: int = 0) -> dict:
    """compilerOptions of a tsconfig, its `extends` chain merged in (the
    base's `paths` count unless this file sets its own)."""
    try:
        with open(path, encoding="utf-8") as fh:
            txt = fh.read()
        try:
            cfg = json.loads(txt)
        except ValueError:  # tsconfig allows comments and trailing commas
            txt = re.sub(r"(?m)^\s*//.*$", "", txt)
            cfg = json.loads(re.sub(r",(\s*[}\]])", r"\1", txt))
    except (OSError, ValueError):
        return {}
    if not isinstance(cfg, dict):
        return {}
    opts = dict(cfg.get("compilerOptions") or {})
    here = os.path.dirname(path)
    if "baseUrl" in opts:
        opts["baseUrl"] = os.path.normpath(os.path.join(here, opts["baseUrl"]))
    ext = cfg.get("extends")
    if isinstance(ext, str) and ext.startswith(".") and depth < 4:
        base_path = os.path.normpath(os.path.join(here, ext))
        if not base_path.endswith(".json"):
            base_path += ".json"
        base = _read_tsconfig(base_path, depth + 1)
        for k, v in base.items():
            opts.setdefault(k, v)
        opts.setdefault("\0paths_dir", base.get("\0paths_dir", os.path.dirname(base_path)))
    if "paths" in (cfg.get("compilerOptions") or {}):
        opts["\0paths_dir"] = here
    return opts


def _tsconfig_paths(project: Project, d: str, cache: dict) -> list[tuple[str, str, bool]]:
    """(key, target, wildcard) from the nearest tsconfig.json's `paths`:
    `"@/*": ["src/*"]` is a prefix, `"@app/common": ["packages/common/src/index.ts"]`
    one exact name."""
    if d in cache:
        return cache[d]
    found: list[tuple[str, str, bool]] = []
    p = os.path.join(project.root, d, "tsconfig.json")
    if os.path.isfile(p):
        try:
            opts = _read_tsconfig(p)
            root = os.path.abspath(project.root)
            base = opts.get("baseUrl") or opts.get("\0paths_dir") or os.path.dirname(p)
            for k, vs in (opts.get("paths") or {}).items():
                if not vs or not isinstance(vs, list):
                    continue
                target = os.path.relpath(os.path.normpath(os.path.join(base, vs[0].rstrip("*"))), root)
                if k.endswith("*") and vs[0].endswith("*"):
                    found.append((k[:-1], target.rstrip("/") + "/" if vs[0][:-1].endswith("/") else target, True))
                elif "*" not in k:
                    found.append((k, target, False))
        except (OSError, ValueError, AttributeError, TypeError):
            pass
    elif d:
        found = _tsconfig_paths(project, d.rsplit("/", 1)[0] if "/" in d else "", cache)
    cache[d] = found
    return found


EXTERNAL = "ext:"  # _resolve_where: an import from outside the project


def _resolve_where(project: Project, where: str, cache: dict) -> str:
    """A module name for an adapter's "js:<dir>\\0<spec>" / "py:<dir>\\0<spec>"
    (anything else unchanged). "" when it can't be found, EXTERNAL when it
    names a package from outside the project (node_modules, the stdlib)."""
    if where.startswith("py:"):
        return _resolve_py(project, where, cache)
    if where.startswith("rs:"):
        return _resolve_rs(project, where, cache)
    if where.startswith("inc:"):
        return _resolve_include(project, where, cache)
    if where.startswith(("go:", "go?:")):
        return _resolve_go(project, where.split(":", 1)[1], cache)
    if not where.startswith("js:"):
        return where
    d, _, spec = where[3:].partition("\0")
    if spec.startswith("."):
        base = os.path.normpath(os.path.join(d, spec))
    else:
        base = next((t + spec[len(pre):] if wild else t for pre, t, wild in _tsconfig_paths(project, d, cache)
                     if (spec.startswith(pre) if wild else spec == pre)), "")
        if not base:
            base = _workspace_entry(project, spec, cache)
        if not base:
            return EXTERNAL  # a package from node_modules
    base = re.sub(r"\.(?:[mc]?[jt]sx?)$", "", base.rstrip("/"))
    base = "" if base == "." else base
    for cand in ((base, base + "/index", base + "/src/index") if base else ("index", "src/index")):
        if cand in project.modules:
            return cand
    return ""


def _manifests(project: Project, name: str) -> list[str]:
    """Every `name` (package.json, go.mod, Cargo.toml) in the repo, whether or
    not `[project] include` takes it in: they say how imports resolve."""
    found = project.__dict__.setdefault("_manifests", {})
    if name not in found:
        exclude = default_exclude(project.config.get("project", {}).get("include", ["**/*"])) + \
            project.config.get("project", {}).get("exclude", [])
        found[name] = list_files(project.root, [name, f"**/{name}"], exclude)
    return found[name]


def _workspace_entry(project: Project, spec: str, cache: dict) -> str:
    """`@scope/pkg` or `@scope/pkg/sub` -> a path in the project, for monorepo
    packages named in their package.json (npm / yarn / pnpm workspaces)."""
    ws = cache.get("\0workspaces")
    if ws is None:
        ws = {}
        for rel in _manifests(project, "package.json"):
            if rel.endswith("package.json") and rel.rsplit("/", 1)[-1] == "package.json" and "/node_modules/" not in "/" + rel:
                try:
                    with open(os.path.join(project.root, rel), encoding="utf-8") as fh:
                        pkg = json.load(fh)
                except (OSError, ValueError):
                    continue
                if isinstance(pkg, dict) and isinstance(pkg.get("name"), str):
                    d = rel.rsplit("/", 1)[0] if "/" in rel else ""
                    main = pkg.get("source") or pkg.get("main") or pkg.get("module") or ""
                    ws[pkg["name"]] = (d, main if isinstance(main, str) else "")
        cache["\0workspaces"] = ws
    for name, (d, main) in ws.items():
        if spec == name:
            entry = os.path.normpath(os.path.join(d, main)) if main and not main.startswith(("dist", "./dist", "build", "./build", "lib/", "./lib/")) else ""
            for cand in ([entry] if entry else []) + [os.path.join(d, "src/index"), os.path.join(d, "index")]:
                if re.sub(r"\.(?:[mc]?[jt]sx?)$", "", cand) in project.modules:
                    return cand
            return os.path.join(d, "src/index")
        if spec.startswith(name + "/"):
            rest = spec[len(name) + 1:]
            for cand in (os.path.join(d, "src", rest), os.path.join(d, rest)):
                c2 = re.sub(r"\.(?:[mc]?[jt]sx?)$", "", cand)
                if c2 in project.modules or c2 + "/index" in project.modules:
                    return cand
            return os.path.join(d, rest)
    return ""


def _resolve_go(project: Project, path: str, cache: dict) -> str:
    """A Go import path -> "pkg:<dir>#<name>" through the go.mod files'
    module paths; EXTERNAL for the standard library and other modules."""
    mods = cache.get("\0gomod")
    if mods is None:
        mods = []
        for rel in _manifests(project, "go.mod"):
            if rel == "go.mod" or rel.endswith("/go.mod"):
                try:
                    with open(os.path.join(project.root, rel), encoding="utf-8") as fh:
                        mm = re.search(r"^module\s+(\S+)", fh.read(), re.M)
                except OSError:
                    continue
                if mm:
                    mods.append((mm.group(1).strip('"'), rel.rsplit("/", 1)[0] if "/" in rel else ""))
        mods.sort(key=lambda x: -len(x[0]))  # the most specific module first
        cache["\0gomod"] = mods
        by_dir: dict[str, list[str]] = defaultdict(list)
        for key in project.packages:
            if "#" in key:
                d, _, name = key.rpartition("#")
                by_dir[d].append(key)
        cache["\0godirs"] = by_dir
    for mpath, mdir in mods:
        if path == mpath or path.startswith(mpath + "/"):
            d = os.path.normpath(os.path.join(mdir, path[len(mpath):].lstrip("/"))) if path != mpath else mdir
            d = "" if d == "." else d
            keys = [k for k in cache["\0godirs"].get(d, []) if not k.endswith("_test")]
            return f"pkg:{keys[0]}" if len(keys) == 1 else ""
    return EXTERNAL


def _resolve_include(project: Project, where: str, cache: dict) -> str:
    """A C/C++ `#include "x.h"`: beside the file, else the one project file
    whose path ends with it (an include directory)."""
    d, _, spec = where[4:].partition("\0")
    base = os.path.normpath(os.path.join(d, spec))
    if base in project.by_file:
        return project.by_file[base][0].name
    idx = cache.get("\0inc")
    if idx is None:
        idx = cache["\0inc"] = defaultdict(list)
        for rel in project.by_file:
            if rel.endswith((".h", ".hpp", ".hh", ".hxx")):
                parts = rel.split("/")
                for k in range(len(parts)):
                    idx["/".join(parts[k:])].append(rel)
    hits = idx.get(spec.lstrip("./"), [])
    if len(hits) == 1:
        return project.by_file[hits[0]][0].name
    return "" if hits else EXTERNAL  # a system / library header


def _rs_crates(project: Project, cache: dict) -> tuple[list[tuple[str, str]], dict[str, str]]:
    """Rust crates: [(crate dir, src dir)] deepest first, and name -> src dir."""
    got = cache.get("\0rs")
    if got is None:
        dirs, names = [], {}
        for rel in _manifests(project, "Cargo.toml"):
            if rel != "Cargo.toml" and not rel.endswith("/Cargo.toml"):
                continue
            d = rel.rsplit("/", 1)[0] if "/" in rel else ""
            src = f"{d}/src" if d else "src"
            try:
                with open(os.path.join(project.root, rel), encoding="utf-8") as fh:
                    txt = fh.read()
            except OSError:
                txt = ""
            # `[lib] path = "x/lib.rs"` / `[[bin]] path = "crates/core/main.rs"`: the crate root is there
            roots = re.findall(r'(?ms)^\[\[?(?:lib|bin)\]\]?[^\[]*?^\s*path\s*=\s*"([^"]+)"', txt)
            for r in roots:
                rd = os.path.normpath(os.path.join(d, os.path.dirname(r)))
                dirs.append((rd, rd))
            dirs.append((d, src))
            for sect in ("package", "lib"):
                mm = re.search(rf"(?ms)^\[{sect}\][^\[]*?^\s*name\s*=\s*\"([^\"]+)\"", txt)
                if mm:
                    lib = re.search(r'(?ms)^\[lib\][^\[]*?^\s*path\s*=\s*"([^"]+)"', txt)
                    names[mm.group(1).replace("-", "_")] = os.path.normpath(os.path.join(d, os.path.dirname(lib.group(1)))) if lib else src
        dirs.sort(key=lambda x: -len(x[0]))
        got = cache["\0rs"] = (dirs, names)
    return got


def _rs_module(project: Project, src: str, parts: list[str]) -> str:
    """The file module for a module path in a crate ("" when there is none)."""
    if not parts:
        for root in ("lib", "main"):
            if f"{src}/{root}" in project.modules:
                return f"{src}/{root}"
        return ""
    base = f"{src}/{'/'.join(parts)}"
    for cand in (base, base + "/mod"):
        if cand in project.modules:
            return cand
    return ""


def _resolve_rs(project: Project, where: str, cache: dict) -> str:
    """A Rust `use` path -> the module it names, or the module holding the item
    it names. `crate::` / `self::` / `super::` / a workspace crate's name; any
    other first segment is a child module, else another crate (EXTERNAL)."""
    rel, _, path = where[3:].partition("\0")
    segs = path.split("::") if path else []
    dirs, names = _rs_crates(project, cache)
    src = next((s for d, s in dirs if rel.startswith(s + "/")), "")
    bm = re.match(r"(.*?/?src/bin)/([^/]+)/", rel)
    if bm and src and rel.startswith(src + "/bin/"):
        src = f"{bm.group(1)}/{bm.group(2)}"  # `src/bin/bat/main.rs`: a binary crate rooted there
    cur: list[str] = []
    if src:
        p = rel[len(src) + 1:-3]  # (.rs)
        cur = [] if p in ("lib", "main") else (p[:-4].split("/") if p.endswith("/mod") else p.split("/"))
        if p.endswith("/mod") and p[:-4] in ("", "lib"):
            cur = []
    if not segs:
        return ""
    if segs[0] == "crate":
        base, segs = [], segs[1:]
    elif segs[0] in ("self", "super"):
        base = list(cur)
        while segs and segs[0] in ("self", "super"):
            if segs[0] == "super":
                base = base[:-1]
            segs = segs[1:]
    elif segs[0] in names:
        src, base, segs = names[segs[0]], [], segs[1:]
    elif src and _rs_module(project, src, cur + [segs[0]]):
        base = list(cur)
    elif src and _rs_module(project, src, [segs[0]]):
        base = []  # (2015 edition: paths start at the crate root)
    else:
        return EXTERNAL  # std, core, alloc or a dependency
    if not src:
        return ""
    best = _rs_module(project, src, base)
    for k in range(len(segs)):
        m = _rs_module(project, src, base + segs[:k + 1])
        if not m:
            break
        best = m
    return best


def _py_index(project: Project, cache: dict) -> dict[str, list[str]]:
    """Dotted-path suffix -> Python file modules: `flask.helpers` is any
    module named `.../flask/helpers` (or a package `.../flask/helpers/__init__`)."""
    idx = cache.get("\0py")
    if idx is None:
        idx = defaultdict(list)
        for m in project.modules.values():
            if m.owner or not m.file.endswith(".py"):
                continue
            path = m.name[:-len("/__init__")] if m.name.endswith("/__init__") else m.name
            parts = path.split("/")
            for k in range(len(parts)):
                idx[".".join(parts[k:])].append(m.name)
        cache["\0py"] = idx
    return idx


def _resolve_py(project: Project, where: str, cache: dict) -> str:
    d, _, spec = where[3:].partition("\0")
    if spec.startswith("."):
        dots = len(spec) - len(spec.lstrip("."))
        base = d
        for _ in range(dots - 1):
            base = base.rsplit("/", 1)[0] if "/" in base else ""
        rest = spec[dots:].replace(".", "/")
        path = f"{base}/{rest}" if base and rest else base or rest
        for cand in (path, f"{path}/__init__" if path else "__init__"):
            if cand in project.modules:
                return cand
        return ""
    depth = spec.count(".") + 1

    def root(h):
        parts = (h[:-len("/__init__")] if h.endswith("/__init__") else h).split("/")
        return "/".join(parts[:len(parts) - depth])
    # an absolute import starts at a source root, never inside a package:
    # `import typing` in flask/ is the stdlib's, not flask/typing.py
    hits = [h for h in _py_index(project, cache).get(spec, []) if not root(h) or root(h) + "/__init__" not in project.modules]
    if not hits:
        return EXTERNAL  # the stdlib or an installed package
    if len(hits) == 1:
        return hits[0]
    # several (`utils` in two packages, a test fixture named like the real
    # package): what Python would do. A source root holding the importing
    # file wins (the longest such); else the shallowest root (`src/flask`
    # over `tests/apps/x/flask`).
    mine = [h for h in hits if root(h) == "" or d == root(h) or d.startswith(root(h) + "/")]
    pool = mine or hits
    key = (lambda h: -len(root(h).split("/"))) if mine else (lambda h: len(root(h).split("/")) if root(h) else 0)
    best = min(key(h) for h in pool)
    top = [h for h in pool if key(h) == best]
    return top[0] if len(top) == 1 else ""


_EX_IMPORT = re.compile(r"^\s*import\s+([A-Z]\w*(?:\.[A-Z]\w*)*|__MODULE__)")
_EX_USE = re.compile(r"^\s*use\s+([A-Z]\w*(?:\.[A-Z]\w*)*|__MODULE__)(?:\s*,\s*:(\w+))?")


def _injected_imports(project: Project, use: str, depth: int) -> list[str]:
    """The modules an Elixir `use X` (or `use X, :controller`) imports into
    the caller: the `import` lines in X's `__using__` / `using do` block, or in
    X's `def controller` (and the helpers it unquotes), and in the project
    modules those blocks `use` in turn. Without this every call to a
    DataCase / ConnCase helper or a component imported by `use App, :html`
    was a name guess."""
    name, _, which = use.partition(":")
    m = project.modules.get(name)
    if m is None or depth < 0:
        return []
    lines = m.code
    heads = [rf"^\s*def\s+{which}\b"] if which else [r"^\s*defmacro\s+__using__\b", r"^\s*using\b.*\bdo\s*$"]
    spans = []

    def span_of(i):
        ind = len(lines[i]) - len(lines[i].lstrip())
        for j in range(i + 1, len(lines)):
            t = lines[j]
            if t.strip() and len(t) - len(t.lstrip()) <= ind:
                return (i, j)
        return (i, len(lines) - 1)

    for i, ln in enumerate(lines):
        if any(re.match(h, ln) for h in heads):
            spans.append(span_of(i))
    # `unquote(html_helpers())` inside the block: that helper's imports too
    for a, b in list(spans):
        for helper in re.findall(r"unquote\((\w+)\(\)\)", "\n".join(lines[a:b + 1])):
            for i, ln in enumerate(lines):
                if re.match(rf"^\s*defp?\s+{helper}\b", ln):
                    spans.append(span_of(i))
    out = []
    for a, b in spans:
        for ln in lines[a:b + 1]:
            if (im := _EX_IMPORT.match(ln)):
                q = im.group(1)
                q = m.name if q == "__MODULE__" else m.aliases.get(q.split(".")[0], q.split(".")[0]) + q[len(q.split(".")[0]):]
                out.append(q)
            elif (us := _EX_USE.match(ln)):
                q = us.group(1)
                q = m.aliases.get(q.split(".")[0], q.split(".")[0]) + q[len(q.split(".")[0]):]
                out += _injected_imports(project, q + (f":{us.group(2)}" if us.group(2) else ""), depth - 1)
    return out


_CONTEXT_PROP = re.compile(r"""setContextProperty\(\s*(?:QStringLiteral\()?\s*"(\w+)"\s*\)?\s*,\s*&?\s*(\w+)""")


def _qml_context(project: Project) -> None:
    """Objects C++ hands to QML by name (`setContextProperty("continuity",
    &continuity)`), typed by the variable's declaration in that file
    (`ContinuityClient continuity;`): every QML file's `continuity.refresh()`
    is then a call on ContinuityClient, not a guess."""
    names: dict[str, set[str]] = {}
    for rel, mods in project.by_file.items():
        if rel.endswith((".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx", ".c")) and mods and "QML_NAMED_ELEMENT" in "\n".join(mods[0].raw):
            # `class FooForeign { QML_NAMED_ELEMENT(Foo) QML_FOREIGN(FooImpl) }`: QML's `Foo` is FooImpl
            raw = "\n".join(mods[0].raw)
            for body in re.split(r"\b(?:class|struct)\s+(?=\w+[^;]*\{)", raw)[1:]:
                cls = re.match(r"(?:\w+\s+)*?(\w+)\s*(?::|\{)", body)
                named = re.search(r"\bQML_NAMED_ELEMENT\s*\(\s*(\w+)\s*\)", body)
                if cls and named:
                    foreign = re.search(r"\bQML_FOREIGN\s*\(\s*([\w:]+)\s*\)", body)
                    project.__dict__.setdefault("_qml_names", {})[named.group(1)] = \
                        (foreign.group(1) if foreign else cls.group(1)).split("::")[-1]
        if not rel.endswith((".cpp", ".cc", ".cxx")) or not mods:
            continue
        raw = "\n".join(mods[0].raw)
        # `qmlRegisterType<ConversationModel>("uri", 1, 0, "ConversationModel")`: a C++ class under a QML name
        for cls, qname in re.findall(r"qmlRegister\w*Type<\s*([A-Za-z_][\w:]*)\s*>\s*\([^;]*?\"(\w+)\"\s*[,)]", raw):
            project.__dict__.setdefault("_qml_names", {})[qname] = cls.split("::")[-1]
        if "setContextProperty" not in raw:
            continue
        for name, var in _CONTEXT_PROP.findall(raw):
            if var == "new":  # `setContextProperty("X", new Foo(...))`
                nm = re.search(rf'"{name}"\s*\)?\s*,\s*new\s+([A-Z]\w*)', raw)
                if nm:
                    names.setdefault(name, set()).add(nm.group(1))
                continue
            decl = re.search(rf"(?:^|[;{{}}(\s])([A-Z]\w*)\s*[\*&]?\s*\b{var}\b\s*[;({{=]", raw, re.M) or \
                re.search(rf"\b{var}\s*=\s*(?:new\s+|std::make_unique<|std::make_shared<)([A-Z]\w*)", raw)
            if decl:
                names.setdefault(name, set()).add(decl.group(1))
    bound = {n: "type:" + next(iter(t)) for n, t in names.items() if len(t) == 1}
    # an id declared in one QML file only: a child component's `conversationModel`
    # (QML looks ids up through the components that instantiate it) is that one
    from .lang_other import _QML_ID
    ids: dict[str, set[str]] = {}
    for rel, mods in project.by_file.items():
        if rel.endswith(".qml") and mods:
            code = "\n".join(mods[0].code)
            for m in _QML_ID.finditer(code):
                held = mods[0].bound.get(m.group(1), "")
                ids.setdefault(m.group(1), set()).add(held if held.startswith(("type:", "mod:")) else "?" + rel)
            # the root object's own properties: what a child component sees by
            # QML's dynamic scope (`appSettings` of main.qml, in TerminalWindow.qml)
            for m in re.finditer(r"\bproperty[ \t]+[\w.<>]+[ \t]+([a-z_]\w*)", code):
                if code.count("{", 0, m.start()) - code.count("}", 0, m.start()) == 1:
                    held = mods[0].bound.get(m.group(1), "")
                    ids.setdefault(m.group(1), set()).add(held if held.startswith(("type:", "mod:")) else "?" + rel)
    for n, held in ids.items():
        if len(held) == 1 and next(iter(held)).startswith(("type:", "mod:")):
            bound.setdefault(n, next(iter(held)))
    if not bound:
        return
    for rel, mods in project.by_file.items():
        if rel.endswith((".qml", ".js")):
            for m in mods:
                for n, t in bound.items():
                    m.bound.setdefault(n, t)


def _is_test_file(project: Project, rel: str) -> bool:
    cache = project.__dict__.setdefault("_test_files", {})
    if rel not in cache:
        from . import tidy
        cache[rel] = tidy.is_test(project, rel)
    return cache[rel]


def _jvm_member_home(project: Project, path: str) -> str:
    """`okhttp3.TestUtil` / `okhttp3.CipherSuite.Companion`: the module of type
    TestUtil (CipherSuite) in package okhttp3, or ""."""
    pkg, _, typ = path.rpartition(".")
    if typ == "Companion":
        pkg, _, typ = pkg.rpartition(".")
    for mn in project.packages.get(pkg, []):
        m = project.modules[mn]
        if typ in m.decls:
            inner = f"{mn}.{typ}"
            return inner if inner in project.modules else mn
    return ""


def _resolve_imports(project: Project) -> None:
    cache: dict = {}
    lang_ruby.link(project)  # Ruby: requires, constants and supertypes, across files (lang_ruby.py)
    lang_other.qt_dbus_interfaces(project)  # Qt: a generated D-Bus proxy stands for the class it calls
    lang_other.qml_page_props(project)  # QML: what each page's untyped properties are handed

    def res(v):
        return _resolve_where(project, v, cache)

    for m in project.modules.values():
        if m.package:
            project.packages[m.package].append(m.name)
    for m in project.modules.values():
        ext: set[str] = set()
        aliases, imported = {}, {}
        for k, v in m.aliases.items():
            r = res(v)
            if r == EXTERNAL:
                ext.add(k)
            elif r:
                if v.startswith("go?:") and r.startswith("pkg:"):
                    k = r.rpartition("#")[2]  # imported without a name: the package's own name
                aliases[k] = r
        jvm = m.file.endswith((".kt", ".kts", ".java"))
        for k, (v, n) in m.imported.items():
            r = res(v)
            if jvm and r.startswith("pkg:") and r[4:] not in project.packages:
                # `import okhttp3.TestUtil.headerEntries`: a member of object TestUtil in package
                # okhttp3 (or of a companion); a package the project doesn't have: a library's
                r = _jvm_member_home(project, r[4:]) or EXTERNAL
            if r == EXTERNAL:
                ext.add(k)
            elif r:
                imported[k] = (r, n)
        m.aliases, m.imported = aliases, imported
        m.uses = [r for r in (res(u) for u in m.uses) if r and r != EXTERNAL]
        rx = [res(u) for u in m.reexports]
        if EXTERNAL in rx:
            m.__dict__["_ext_star"] = True  # `export * from "@testing-library/react"`
        m.reexports = [r for r in rx if r and r != EXTERNAL]
        m.external = ext - set(aliases) - set(imported)
    for m in project.modules.values():
        for k, (w, n) in list(m.imported.items()):  # `export { renderApp as render }` in the module imported from
            src = project.modules.get(w)
            alias = src.bound.get("\0export:" + n, "") if src is not None else ""
            if alias.startswith("export:") and alias[7:] != n:
                m.imported[k] = (w, alias[7:])
                m.external.discard(k)
    for m in project.modules.values():
        # a name imported from a module that re-exports a library wholesale, and that no
        # project module defines (`fireEvent` through test-utils): the library's
        for k, (w, n) in list(m.imported.items()):
            src = project.modules.get(w)
            if src is not None and src.__dict__.get("_ext_star") and not _in_scope(project, w, n) \
                    and not _defining_module(project, w, n) and not any(f"{w}.{n}" == mn for mn in project.modules):
                m.external.add(k)
                del m.imported[k]
    _python_names(project)
    _qml_context(project)
    for m in project.modules.values():
        for u in m.using:
            for w in _injected_imports(project, u, 2):
                if w not in m.uses:
                    m.uses.append(w)


def _py_class(project: Project, mod: Module, name: str, depth: int = 0) -> str:
    """The class module a Python name in `mod`'s file means (`Base`, `models.Base`, an
    imported or re-exported class), or ""."""
    file_mod = project.modules.get(mod.owner) if mod.owner else mod
    if file_mod is None or depth > 4:
        return ""
    if "." in name:
        q, _, rest = name.rpartition(".")
        target = file_mod.aliases.get(q.split(".")[-1])
        if target and target in project.modules:
            if f"{target}.{rest}" in project.modules:
                return f"{target}.{rest}"
            return _py_class(project, project.modules[target], rest, depth + 1)
        return ""
    if f"{file_mod.name}.{name}" in project.modules:
        return f"{file_mod.name}.{name}"
    if name in file_mod.aliases and project.modules.get(file_mod.aliases[name], mod).owner:
        return file_mod.aliases[name]
    if name in file_mod.imported:
        w, n = file_mod.imported[name]
        if w in project.modules:
            return _py_class(project, project.modules[w], n, depth + 1)
    for w in file_mod.reexports:
        if w in project.modules:
            hit = _py_class(project, project.modules[w], name, depth + 1)
            if hit:
                return hit
    return ""


def _python_names(project: Project) -> None:
    """Python, once every import is resolved: `from x import Foo` (a class) and
    `from pkg import sub` (a submodule) become aliases, so `Foo.m()` and
    `sub.f()` are exact; and each class's base names its parent class."""
    py = [m for m in project.modules.values() if m.file.endswith(".py")]
    for m in py:
        if m.owner:
            continue
        for local, (w, n) in list(m.imported.items()):
            sub = (w[:-len("/__init__")] if w.endswith("/__init__") else w.rsplit("/", 1)[0] if "/" in w else "") + "/" + n
            sub = sub.lstrip("/")
            if sub in project.modules or sub + "/__init__" in project.modules:
                m.aliases.setdefault(local, sub if sub in project.modules else sub + "/__init__")
                continue
            target = project.modules.get(w)
            cls = _py_class(project, target, n) if target else ""
            if cls:
                m.aliases.setdefault(local, cls)
    for m in py:
        if m.owner:
            fm = project.modules.get(m.owner)
            if fm:
                m.aliases, m.imported, m.uses, m.reexports, m.external = fm.aliases, fm.imported, fm.uses, fm.reexports, fm.external
        if m.parent.startswith("py:"):
            m.parent = _py_class(project, m, m.parent[3:])
        if m.bases and m.bases[0].startswith("py:"):
            # every base (`class FlaskProxy(ProxyMixin[Flask], Flask)`); one from a
            # library (`werkzeug.test.Client`) means an inherited method may be its
            fm = project.modules.get(m.owner, m)
            got = [c for b in m.bases if (c := _py_class(project, m, b[3:]))]
            if any(not _py_class(project, m, b[3:]) and (b[3:].split(".")[0] in fm.external or b[3:] in _PY_BUILTINS
                                                          or _py_lib_reexport(project, fm, b[3:].split(".")[0]))
                   for b in m.bases):  # (`class Config(dict)`, `class MyError(ValueError)`: Python's own)
                m.unresolved = True
            m.bases = got
            m.parent = m.parent or (got[0] if got else "")


def _py_lib_reexport(project: Project, fm: Module, name: str, depth: int = 0) -> bool:
    """`from .compat import MutableMapping`, where compat imports it from a
    library: the name is the library's."""
    if depth > 4 or name not in fm.imported:
        return False
    w, n = fm.imported[name]
    src = project.modules.get(w)
    if src is None:
        return False
    return n in src.external or _py_lib_reexport(project, src, n, depth + 1)


def _in_scope(project: Project, where: str, name: str, depth: int = 0) -> list[Func]:
    """`name` in a module, or in any file of a "pkg:" package. A module that
    only re-exports the name (an `index.ts` barrel, a package `__init__.py`)
    leads on to where it is defined."""
    if where.startswith("pkg:"):
        return [t for t in (_lookup(project, mn, name) for mn in project.packages.get(where[4:], [])) if t]
    if where.startswith("type:"):
        return []  # a type is only a scope for method names (see _scope)
    t = _lookup(project, where, name)
    if t:
        return [t]
    m = project.modules.get(where)
    if m is None or depth >= 4:
        return []
    if name in m.imported:
        w, n = m.imported[name]
        hits = _in_scope(project, w, n, depth + 1)
        if hits:
            return hits
    for w in m.reexports:
        hits = _in_scope(project, w, name, depth + 1)
        if hits:
            return hits
    return []


def _defining_module(project: Project, where: str, name: str, depth: int = 0) -> str:
    """The module that defines the object a named import refers to
    (`import { API } from "./helpers"`, through re-exports): `API.create()`
    is that module's `create`."""
    m = project.modules.get(where)
    if m is None or depth > 4:
        return ""
    if f"{where}.{name}" in project.modules:  # a class with a module of its own
        return f"{where}.{name}"
    if name in m.decls or f"{where}.{name}" in project.funcs:
        return where
    if name in m.imported:
        w, n = m.imported[name]
        return _defining_module(project, w, n, depth + 1)
    for w in m.reexports:
        hit = _defining_module(project, w, name, depth + 1)
        if hit:
            return hit
    if name == "default" and m.file.endswith((".js", ".cjs", ".mjs")) and not m.owner:
        return where  # `require('./context')` of `module.exports = { inspect() {...} }`: the module's own object
    return ""


def _scope(project: Project, mod: Module, cache: dict) -> set[str]:
    """The modules a file can see: itself, what it imports, its package."""
    if mod.name in cache:
        return cache[mod.name]
    fm = project.modules.get(mod.owner, mod) if mod.owner else mod
    out = {mod.name, fm.name, *fm.aliases.values(), *(w for w, _ in fm.imported.values())}
    for w in fm.uses + ([f"pkg:{fm.package}"] if fm.package else []):
        if w.startswith("pkg:"):
            out.update(project.packages.get(w[4:], []))
        elif w.startswith("type:"):
            out.update(project.short.get(w[5:], []))
        else:
            out.add(w)
    # a file seen is all of it: its classes' modules too
    out |= {m.name for w in list(out) if w in project.modules for m in project.by_file.get(project.modules[w].file, ())}
    cache[mod.name] = out
    return out


_LOCALS = re.compile(r"\b(?:val|var|let|const)\s+([A-Za-z_$][\w$]*)|[(,]\s*([A-Za-z_$][\w$]*)\s*:(?!:)")
_DESTRUCTURED = re.compile(r"\b(?:val|var|let|const)\s*[\[{(]([^\]})=;]*)[\]})]\s*(?::[^=;]+)?=")
_locals_cache: dict[str, set[str]] = {}


def _shadowed(f: Func, name: str) -> bool:
    """`name` is a parameter or local of `f` (Kotlin/Java/TS/Rust), so a bare
    `name()` calls that value, not a function in scope. (Ruby's `foo(key: v)`
    passes a keyword, it declares nothing.)"""
    loc = _locals_cache.get(f.id)
    if loc is None:
        rb = f.file.endswith((".rb", ".rake", ".ru"))
        loc = _locals_cache[f.id] = {a or ("" if rb else b) for a, b in _LOCALS.findall(f.code)} - {""}
        for dm in _DESTRUCTURED.finditer(f.code):  # `const [value, setValue] = useState()`, `const { a, b: c } =`
            for part in dm.group(1).split(","):
                nm = re.sub(r"^\s*\.\.\.", "", part).split("=")[0].split(":")[-1].strip()
                if re.match(r"^[A-Za-z_$][\w$]*$", nm):
                    loc.add(nm)
    return name in loc


def _groups(project: Project) -> dict[str, str]:
    """[project.groups]: group id -> file, as "group:<id>" -> module name. A
    file seamcheck doesn't know is a config problem (the binding would quietly
    fall back to name guesses)."""
    out = {}
    for gid, rel in (project.config.get("project", {}).get("groups", {}) or {}).items():
        mods = project.by_file.get(rel, [])
        if mods:
            out["group:" + gid] = mods[0].name
        else:
            project.config_problems.append(f"[project.groups] {gid} = {rel!r}: no such file in the project")
    return out


def _typed_targets(project: Project, mod: Module | None, ty: str, name: str, ext_cache: dict) -> tuple[list[Func], bool]:
    """The functions `x.name()` reaches when x is declared `ty`, and whether ty
    is a project type at all."""
    if mod and mod.file.endswith(".go"):
        return _go_typed(project, mod, ty, name)
    mods = []
    if mod and mod.file.endswith(".py") and (cls := _py_class(project, mod, ty)):
        mods.append(cls)
    if mod and ty in mod.aliases:
        mods.append(mod.aliases[ty])
    if ty in project.modules:
        mods.append(ty)
    mods += project.short.get(ty, [])
    hits = []
    for mn in dict.fromkeys(mods):
        t = _lookup(project, mn, name)
        if t and t not in hits:
            hits.append(t)
    if not hits:
        if not ext_cache:
            for g in project.funcs.values():
                if g.recv:
                    ext_cache.setdefault((g.recv, g.name), []).append(g)
            ext_cache.setdefault(("", ""), [])  # (built)
        hits = list(ext_cache.get((ty, name), []))
    return hits, bool(mods)


def _go_typed(project: Project, mod: Module, ty: str, name: str) -> tuple[list[Func], bool]:
    """Go `x.M()` with x declared a type of the caller's own package: that
    type's method M, from whichever file of the package defines it."""
    fm = project.modules.get(mod.owner, mod) if mod.owner else mod
    idx = project.__dict__.setdefault("_go_types", {})
    if not idx:
        for m in project.modules.values():
            if m.owner and m.file.endswith(".go"):
                o = project.modules.get(m.owner)
                if o and o.package:
                    idx.setdefault((o.package, m.decls[0]), []).append(m.name)
        idx.setdefault(("", ""), [])
    pkg = fm.package
    if "." in ty:  # `w tui.Window`: a type of an imported package
        q, _, ty = ty.partition(".")
        target = mod.aliases.get(q, "")
        if not target.startswith("pkg:"):
            return [], False  # another module's or the standard library's type
        pkg = target[4:]
    hits = [t for mn in idx.get((pkg, ty), []) if (t := project.funcs.get(f"{mn}.{name}"))]
    if hits:
        return hits, True
    declared = any(ty in project.modules[mn].decls for mn in project.packages.get(pkg, []) if mn in project.modules)
    return [], declared  # declared here and missed: an embedded type's method (a guess); else a builtin


# A qualifier that is a module's name: `Enum`, `Foo.Bar`, `Vector3`. Not a
# value: `main`, `self.x`, `Foo.get()`, and not an ALL_CAPS constant holding
# an object (`LOG`, `INSTANCE`).
_MODULE_Q = re.compile(r"(?![A-Z0-9_]+$)[A-Z]\w*(?:\.[A-Z]\w*)*$")


# Which files a call can land in: a Kotlin call never runs a TypeScript
# function. QML calls its JS libraries and C++ types; .js is shared by the web
# and QML families.
_FAMILIES = {
    ".ex": {"ex"}, ".exs": {"ex"},
    ".kt": {"jvm"}, ".kts": {"jvm"}, ".java": {"jvm"},
    ".ts": {"web"}, ".tsx": {"web"}, ".jsx": {"web"}, ".mjs": {"web"}, ".cjs": {"web"}, ".js": {"web", "qt"},
    ".qml": {"qt"}, ".c": {"qt"}, ".cpp": {"qt"}, ".cc": {"qt"}, ".cxx": {"qt"}, ".h": {"qt"}, ".hpp": {"qt"}, ".hh": {"qt"},
    ".py": {"py"}, ".gd": {"gd"}, ".rs": {"rs"}, ".go": {"go"}, ".cs": {"cs"},
    ".rb": {"rb"}, ".rake": {"rb"}, ".ru": {"rb"},
}


# what Swift's standard types conform to (an extension of the protocol is theirs too)
_SWIFT_SEQ = ("Sequence", "Collection", "BidirectionalCollection", "RandomAccessCollection", "Equatable", "Hashable")
_SWIFT_CONFORMS = {
    "String": ("StringProtocol", "RangeReplaceableCollection", "Comparable", "CustomStringConvertible") + _SWIFT_SEQ,
    "Substring": ("StringProtocol", "RangeReplaceableCollection", "Comparable") + _SWIFT_SEQ,
    "Array": ("MutableCollection", "RangeReplaceableCollection", "ExpressibleByArrayLiteral") + _SWIFT_SEQ,
    "ArraySlice": ("MutableCollection", "RangeReplaceableCollection") + _SWIFT_SEQ,
    "Range": _SWIFT_SEQ, "ClosedRange": _SWIFT_SEQ,
    "Dictionary": ("Sequence", "Collection", "Equatable", "Hashable"),
    "Set": ("Sequence", "Collection", "SetAlgebra", "Equatable", "Hashable"),
    "Int": ("BinaryInteger", "FixedWidthInteger", "SignedInteger", "Numeric", "Comparable", "Equatable", "Hashable"),
    "Double": ("BinaryFloatingPoint", "FloatingPoint", "Numeric", "Comparable", "Equatable", "Hashable"),
    "Character": ("Comparable", "Equatable", "Hashable"),
}

# browser globals a project augments (`declare global { interface Window { h: ... } }`): what the
# augmentation doesn't declare is the browser's
_DOM_GLOBALS = {"Window", "Document", "Navigator", "HTMLElement", "Element", "Node", "EventTarget", "Event",
                "GlobalThis", "WorkerGlobalScope", "ServiceWorkerGlobalScope", "Console", "Location", "History",
                "HTMLCanvasElement", "CanvasRenderingContext2D", "Performance", "Storage", "ImportMeta"}


def _family(rel: str) -> set[str]:
    ext = os.path.splitext(rel)[1]
    return _FAMILIES.get(ext, {ext})


_EXPLICIT_RECEIVER = (".go", ".rs", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".php")


def _argc(mod, c) -> int | None:
    """How many arguments a call passes, where overloads go by count (Java, C#,
    Kotlin, C++); None elsewhere or when it can't be read."""
    if mod is None or not mod.file.endswith(infer_mod._OVERLOADED) or not (0 < c.line <= len(mod.code)):
        return None
    lines = mod.code
    text = "\n".join(lines[c.line - 1:c.line + 30])
    k = text.find("(", c.col if c.col >= 0 else 0)
    if k < 0 or text[c.col:k].strip(" \t") not in ("",) and not re.fullmatch(r"\w*\s*(?:<[^()]*>)?\s*", text[c.col:k]):
        return None
    close = infer_mod._paren_close(text, k)
    if close < 0:
        return None
    inner = text[k + 1:close]
    if not inner.strip():
        return 0
    inner = re.sub(r"(?<=\w)\s*<[\w\s,.?<>\[\]]*>(?=\s*[(\[{>),]|\s*$)", "", inner)  # `new HashMap<A, B>()`
    return len(infer_mod._top_split(inner.replace("<", " ").replace(">", " ")))


_CPP_EXTS = (".cpp", ".cc", ".cxx", ".hpp", ".hh")
_C_DEFINE = re.compile(r"^[ \t]*#[ \t]*define[ \t]+([A-Za-z_]\w*)\(([^)]*)\)((?:[^\n]*\\\n)*[^\n]*)", re.M)
_C_KEYWORDS = {"if", "for", "while", "switch", "return", "sizeof", "do", "else", "case", "typeof", "__typeof__", "defined",
               "_Generic", "offsetof", "__builtin_expect", "__attribute__"}


_C_FAMILY = (".c", ".h", ".cpp", ".cc", ".cxx", ".hpp", ".hh")
_STD_NAMESPACES = {"std", "boost", "absl", "Qt", "QtConcurrent", "chrono", "filesystem", "ranges", "views", "this_thread"}


def _cpp_namespaces(project: Project) -> set:
    got = project.__dict__.get("_cpp_ns")
    if got is None:
        got = project.__dict__["_cpp_ns"] = {part for g in project.funcs.values() if g.ns for part in g.ns.split("::")}
    return got


def _cpp_namespace_call(project: Project, f: Func, c, targets: list) -> bool:
    """`detail::write(...)`, `details::os::localtime(...)`, `std::move(...)`: a call
    qualified by a namespace path. True when the path names namespaces (not a
    class or a variable): `targets` gets the free functions of that name in a
    namespace ending with that path (one overload set: exact), none for `std::`."""
    if not all(re.fullmatch(r"[A-Za-z_]\w*", s_) for s_ in c.chain) or c.chain[-1] in f.bound:
        return False
    own = _cpp_namespaces(project)
    if c.chain[0] in _STD_NAMESPACES and c.chain[0] not in own:
        return True  # the standard library's (or boost's): no project function
    if not all(s_ in own for s_ in c.chain):
        return False  # a class (`Foo::make()`) or a variable: the type layer decides
    path = "::".join(c.chain)
    hits = [g for g in project.by_name.get(c.name, []) if g.ns and (g.ns == path or g.ns.endswith("::" + path))]
    if not hits:  # `details::m_formatter<Padder>(...)`: a class's constructor, the class named in that namespace
        hits = [g for g in project.by_name.get(c.name, []) if g.module.rsplit(".", 1)[-1] == c.name and project.modules[g.module].owner]
        if len({g.module for g in hits}) == 1:
            targets.extend((g, True) for g in hits)
            return True
        return False
    sure = len({g.ns for g in hits}) == 1  # (overloads in one namespace, over several headers: one function)
    targets.extend((g, sure) for g in hits)
    return True


def _c_macros(project: Project) -> dict[str, list[str]]:
    """Function-like macros in the project's C files: name -> the names their body calls."""
    got = project.__dict__.get("_c_macros")
    if got is None:
        got = project.__dict__["_c_macros"] = {}
        for rel, mods in project.by_file.items():
            if not rel.endswith((".c", ".h")) or not mods:
                continue
            for m in _C_DEFINE.finditer("\n".join(mods[0].raw)):
                params = {p.strip() for p in m.group(2).split(",")}
                body = m.group(3).replace("\\\n", " ")
                names = [n for n in re.findall(r"\b([A-Za-z_]\w*)\s*\(", body) if n not in params and n not in _C_KEYWORDS]
                got.setdefault(m.group(1), names)
    return got


def _c_macro_targets(project: Project, caller_file: str, name: str, depth: int = 0) -> list:
    """The project functions a call to macro `name` runs (through macros it uses, 3 deep)."""
    out = []
    for n in _c_macros(project).get(name, []) if depth < 3 else []:
        cfuncs = [g for g in project.by_name.get(n, []) if g.kind != "top" and g.file.endswith((".c", ".h"))]
        own = [g for g in cfuncs if g.file == caller_file]
        glob_ = [g for g in cfuncs if not _c_static(project, g) or g.file.endswith(".h")]
        if own:
            out.append(own[0])
        elif len({g.file for g in glob_}) == 1 and glob_:
            out.append(glob_[0])
        elif not cfuncs:
            out += _c_macro_targets(project, caller_file, n, depth + 1)
    return list({g.id: g for g in out}.values())


def _needs_receiver(project: Project, g: Func) -> bool:
    """`g` is a method a call reaches only through a receiver (Go's `func (c *Command) X`,
    Rust's `fn x(&self)`, a JS / TS / PHP class's method), not a free function."""
    if g.file.endswith(".go"):
        return bool(g.recv)
    if g.file.endswith(".rs"):
        return re.search(r"\(\s*(?:&\s*)?(?:'\w+\s+)?(?:mut\s+)?self\b", g.code.split("{", 1)[0]) is not None \
            or bool(project.modules[g.module].owner)
    return bool(project.modules[g.module].owner)


def _scoped_language(rel: str, mod=None) -> bool:
    """A language where a bare name resolves only through the file's own
    definitions and imports. A plain .js script (a QML library, a browser
    script) sees globals from anywhere, so it is not; a CommonJS or ES
    module .js file (`require(`, `module.exports`, `import`) is."""
    ext = os.path.splitext(rel)[1]
    if ext in (".js", ".jsx", ".mjs", ".cjs") and mod is not None:
        fm = mod
        hit = _MODULE_JS.get(fm.file)
        if hit is None:
            raw = "\n".join(fm.raw)
            hit = _MODULE_JS[fm.file] = ext in (".mjs", ".cjs") or bool(re.search(
                r"\brequire\s*\(\s*['\"]|\bmodule\.exports\b|^[ \t]*exports\.\w+\s*=|^[ \t]*(?:import|export)\b", raw, re.M)) \
                and not re.search(r"^[ \t]*(?:global|globalThis|window)\.\w+\s*=", raw, re.M)
        return hit
    return ext in (".ex", ".exs", ".kt", ".kts", ".java", ".ts", ".tsx", ".mts", ".cts", ".py", ".go", ".rs",
                   ".swift", ".php", ".rb", ".rake", ".ru")


_MODULE_JS: dict[str, bool] = {}


def _implicit_member(project: Project, types, mod, f: Func, c: Call):
    """What a Swift implicit member `.name(...)` is, from its context:
    ("none", []) for a pattern, an enum case or a library type's member;
    ("exact", funcs) for the expected type's static function; ("?", []) when
    the context doesn't say the type."""
    if mod is None or not (0 < c.line <= len(mod.raw)):
        return "?", []
    line = mod.raw[c.line - 1]
    code_ = mod.code if mod.code and len(mod.code) == len(mod.raw) else mod.raw
    # the text before the member, from a few lines up (a call's arguments span lines)
    before = "\n".join(code_[max(0, c.line - 12):c.line - 1] + [code_[c.line - 1][:max(0, c.col - 1)]])
    if re.search(r"\bcase\s+(?:let\s+|var\s+)?$", before) or \
            re.match(r"\s*(?:case|if\s+case|guard\s+case|for\s+case)\b", line) and "=" not in before.split("case", 1)[-1]:
        return "none", []  # `case .value(let v):` matches, it builds nothing
    ty = ""
    elem = re.search(r"(?:\breturn\s*\[|=\s*\[|\b(\w+)\s*\.\s*(?:append|insert)\s*\(|\[\s*)\s*$", before)
    if elem:  # an element: of the returned / declared array, or of the one appended to
        src = "\n".join(mod.raw[max(0, f.start - 1):f.start + 4]) if "return" in elem.group(0) else ""
        if elem.group(1):
            src = "\n".join(mod.raw)
            dm = re.search(r"\b" + re.escape(elem.group(1)) + r"\s*:\s*\[([\w.]+)\]", src)
        elif "return" in elem.group(0):
            dm = re.search(r"->\s*\[([\w.]+)\]", src)
        else:
            dm = re.search(r"\b(?:let|var)\s+\w+\s*:\s*\[([\w.]+)\]\s*=\s*\[\s*$", before)
        if dm:
            ty = dm.group(1)
    if ty:
        pass
    elif re.search(r"\breturn\s*$", before):
        ty = f.returns if f.returns and not f.returns.startswith("expr:") else ""
    elif re.search(r"\bself\s*=\s*$", before):
        own = types.own_class(mod, f)
        ty = project.modules[own[0]].decls[0] if own and project.modules[own[0]].decls else ""
    elif (tm := re.search(r"\b(?:let|var)\s+\w+\s*:\s*([\w.]+)[?!]?\s*=\s*$", before)):
        ty = tm.group(1)
    elif (lm := re.search(r"(?:^|[(,]\s*)(\w+)\s*:\s*$", before)) or re.search(r",\s*$", before):
        # a labelled argument: the parameter of that label in the callee's signature
        # (start just after the `(` or `,` the label follows, so that paren is the call's own)
        depth, k, text = 0, len(before) - (len(lm.group(0)) - (1 if lm.group(0)[:1] in "(," else 0) if lm else 0), before
        while k > 0:
            k -= 1
            if text[k] == ")":
                depth += 1
            elif text[k] == "(":
                if depth == 0:
                    break
                depth -= 1
        ty = _param_type(project, types, mod, text, k, lm.group(1) if lm else "",
                         len(_top_commas(text[k + 1:])) if k >= 0 else 0, f, c.line)
    elif (am := re.search(r"\(\s*$", before)):
        ty = _param_type(project, types, mod, before, am.start(), "", 0, f, c.line)  # the first, unlabelled argument
    from .chains import type_name
    verdicts = []
    for t in (ty.split("|") if ty else []):  # overloads: the parameter type that has the member
        t = type_name(t)
        if not t or t == "Self":
            verdicts.append(("?", []))
            continue
        nested = None  # `Value` inside `Element`: the enclosing types' own nested `Value` first
        o = mod.name
        while o and nested is None:
            if f"{o}.{t}" in project.modules:
                nested = f"{o}.{t}"
            o = project.modules[o].owner if o in project.modules else ""
        tmods, ex, declared = ([nested], True, True) if nested else types.type_modules(mod, t)
        if not declared:
            verdicts.append(("lib", []))  # a library type's member
            continue
        if not ex:
            verdicts.append(("?", []))
            continue
        hits = [g for g in types.method(list(tmods), c.name)[0] if _is_static(project, g)]
        if hits:
            verdicts.append(("exact", hits))
        elif any(_enum_case(project.modules[m_], c.name) for m_ in tmods):
            verdicts.append(("none", []))  # an enum case: `case option(String)`
        else:
            verdicts.append(("miss", []))  # this overload's type has no such member
    found = list({(v[0], tuple(g.id for g in v[1])): v for v in verdicts if v[0] in ("exact", "none")}.values())
    if len(found) == 1 and all(v[0] in ("exact", "none", "miss") for v in verdicts):
        return found[0]
    if verdicts and all(v[0] == "lib" for v in verdicts):
        return "none", []
    return "?", []


def _top_commas(text: str) -> list[int]:
    """The positions of the commas at bracket depth 0."""
    out, d = [], 0
    for i, ch in enumerate(text):
        d += ch in "([{"
        d -= ch in ")]}"
        if ch == "," and d == 0:
            out.append(i)
    return out


def _params(sig: str) -> list[tuple[str, str]]:
    """A Swift signature's parameters: (external label, type text)."""
    a = sig.find("(")
    if a < 0:
        return []
    d, b = 0, a
    for b in range(a, len(sig)):
        d += sig[b] in "(["
        d -= sig[b] in ")]"
        if d == 0:
            break
    body, out, last = sig[a + 1:b], [], 0
    for cut in _top_commas(body) + [len(body)]:
        p = body[last:cut].strip()
        last = cut + 1
        pm = re.match(r"(\w+)(?:\s+\w+)?\s*:\s*(?:inout\s+|@\w+\s+)*([^=]+)", p)
        if pm:
            out.append((pm.group(1), pm.group(2).strip().removesuffix("...")))  # (variadic `T...`: each a T)
    return out


from . import infer as infer_mod  # noqa: E402


def _overload_starts(raw: list[str], g: Func) -> list[int]:
    """The lines of every `func name(` in the type block `g` is in: overloads
    share one Func (`add(name:value:)`, `add(_ header:)`)."""
    def owner(i):  # the top-level declaration line i sits under
        while i >= 0 and not re.match(r"(?:\w+\s+)*(?:struct|class|enum|extension|protocol|actor)\s", raw[i]):
            i -= 1
        return i
    top = owner(g.start - 1)
    pat = re.compile(r"\s*(?:@\w+\s+)*(?:\w+\s+)*func\s+" + re.escape(g.name) + r"\s*[(<]")
    starts = [g.start]
    if top < 0:
        return starts
    for i in range(top + 1, len(raw)):
        if re.match(r"(?:\w+\s+)*(?:struct|class|enum|extension|protocol|actor)\s", raw[i]):
            break
        if i + 1 != g.start and pat.match(raw[i]):
            starts.append(i + 1)
    return starts


def _param_type(project: Project, types, mod, text: str, k: int, label: str, index: int = 0,
                f: Func | None = None, line_no: int = 0) -> str:
    """The declared type of the parameter `label` (or the first, unlabelled
    one) of the call whose `(` is at text[k]: `Element(value: .option(x))`,
    `session.request(.status(401))`. "" when the callees disagree."""
    callee = re.search(r"([A-Za-z_]\w*)\s*$", text[:k]) if k > 0 else None
    if not callee:
        return ""
    name = callee.group(1)
    prefix = text[:callee.start(1)]  # where the callee is: `text` ends on line `line_no`
    callee_line = line_no - (text.count("\n") - prefix.count("\n"))
    callee_col = len(prefix) - (prefix.rfind("\n") + 1)
    if name == "init" and re.search(r"\b(?:self|Self|super)\s*\.\s*init\s*$", text[:k]):
        own = types.own_class(mod, f) if f else None  # `self.init(base: .message(x))`: this type's own init
        cands = [g for a in (types.ancestors(own[0]) if own else []) for g in project.modules[a].funcs if g.name == "init"]
    elif name[:1].isupper():
        tmods, ex, _ = types.type_modules(mod, name)
        cands = [g for m_ in (tmods if ex else []) for a in types.ancestors(m_)
                 for g in project.modules[a].funcs if g.name == "init"]
    else:
        cands = project.by_name.get(name, [])
        # the callee through its own receiver: `session.request(` is Session's request
        outer = next((c2 for c2 in (f.calls if f else []) if c2.line == callee_line and c2.name == name
                      and c2.col == callee_col), None) if line_no else None
        if outer is not None and outer.chain:
            v = types.chain(mod, f, outer.chain)
            if v is not None and not infer_mod.is_ext(v) and v[0] in ("inst", "static"):
                cands = types.method(list(v[1]), name)[0] or cands
        elif outer is not None and mod is not None:
            own = types.own_class(mod, f)
            hits = types.method(own, name)[0] if own else []
            cands = hits or cands
    found = set()
    for g in cands[:12]:
        gm = project.modules.get(g.module)
        if gm is None:
            continue
        for start in _overload_starts(gm.raw, g):
            sig = " ".join(gm.raw[start - 1:start + 6])
            ps = _params(sig[sig.find(g.name):] if g.name in sig else sig)
            hit = [t for lab, t in ps if lab == label] if label else ([ps[index][1]] if index < len(ps) and ps[index][0] == "_" else [])
            if hit:
                found.add(hit[0])
    if not found and label and (name == "init" or name[:1].isupper()) and mod is not None:
        # a struct's memberwise init (`Element(value: .option(x))`, `self.init(base: .message(t))`):
        # the parameter is the stored property of that name
        if name == "init":
            owners = types.own_class(mod, f) if f else []
        else:
            tmods, ex, _ = types.type_modules(mod, name)
            owners = tmods if ex else []
        for o in owners or []:
            for a in types.ancestors(o):
                held = project.modules[a].bound.get(label, "")
                if held.startswith("type:"):
                    found.add(held[5:])
                    break
    return "|".join(sorted(found))  # (overloads: each parameter type; the member settles which)


def _gd_declared(project: Project, types, mod, f: Func, chain: tuple) -> str:
    """The engine class a GDScript receiver is declared as (`var x: Camera3D`),
    or "" (untyped, or a project script's type)."""
    if len(chain) == 1:
        held = f.bound.get(chain[0]) or mod.bound.get(chain[0], "")
    else:
        v = types.chain(mod, f, chain[:-1])
        if v is None or infer_mod.is_ext(v) or v[0] != "inst" or len(v[1]) != 1:
            return ""
        held = project.modules[v[1][0]].bound.get(chain[-1], "")
    ty = held[5:] if held and held.startswith("type:") else ""
    return ty if ty and ty not in project.modules and not project.short.get(ty) else ""


def _gd_extends(project: Project, module: str, engine: str) -> bool:
    """A script's `extends` chain reaches the engine class `engine`."""
    seen = set()
    m = project.modules.get(module)
    while m is not None and m.name not in seen:
        seen.add(m.name)
        if m.parent == engine:
            return True
        m = project.modules.get(m.parent) if m.parent else None
    return False


def _swift_labels_match(project: Project, mod, c: Call, g: Func) -> bool:
    """A Swift call's first argument label against `g`'s first parameter:
    `append(contentsOf:)` is the library's, not the project's `append(optional:)`."""
    if mod is None or not (0 < c.line <= len(mod.raw)):
        return True
    am = re.match(r"\s*\(\s*(?:(\w+)\s*:(?!:))?", mod.raw[c.line - 1][c.col + len(c.name):])
    gm = project.modules.get(g.module)
    ps = _params(" ".join(gm.raw[g.start - 1:g.start + 3])[" ".join(gm.raw[g.start - 1:g.start + 3]).find(g.name):]) if gm else []
    if not am or not ps:
        return True
    want = am.group(1) or "_"
    return ps[0][0] == want


def _enum_case(m: Module, name: str) -> bool:
    """`enum <m's type> { case name(...) }`: name is one of its cases (in the
    enum's own body, not another type's in the same file)."""
    if not m.decls:
        return False
    text = "\n".join(m.raw)
    for em in re.finditer(r"\benum\s+" + re.escape(m.decls[0].split(".")[-1]) + r"\b[^{]*\{", text):
        d, k = 1, em.end()
        while k < len(text) and d:
            d += text[k] == "{"
            d -= text[k] == "}"
            k += 1
        body = text[em.end():k]
        if re.search(r"\bcase\s+(?:[^\n]*,\s*)?" + re.escape(name) + r"\b", body):
            return True
    return False


def _rs_crate_dir(project: Project, rel: str) -> str:
    """The directory of the crate root a Rust file belongs to: the nearest ancestor
    holding main.rs / lib.rs (an integration test binary's `tests/builder/main.rs`)."""
    d = os.path.dirname(rel)
    while d:
        if f"{d}/main.rs" in project.by_file or f"{d}/lib.rs" in project.by_file:
            return d + "/"
        d = os.path.dirname(d)
    return ""


def _c_included(project: Project, rel: str) -> set:
    """The .c files a C file `#include`s (a test that includes the unit it tests)."""
    got, todo, seen = set(), [rel], {rel}
    while todo:  # (through headers: a test's `common.h` includes `../cJSON.c`)
        m = project.by_file.get(todo.pop())
        for inc in (getattr(m[0], "c_includes", ()) if m else ()):
            if inc not in project.by_file:
                inc = next((r for r in project.by_file if r.endswith("/" + inc.rsplit("/", 1)[-1])), inc)
            if inc in seen:
                continue
            seen.add(inc)
            if inc.endswith(".c"):
                got.add(inc)
            todo.append(inc)
    return got


def _c_static(project: Project, g: Func) -> bool:
    """A C function declared `static` (file-local)."""
    m = project.modules.get(g.module)
    if not m:
        return False
    head = " ".join(m.raw[max(0, g.start - 2):g.start])
    return bool(re.search(r"\bstatic\b", head))


def _is_static(project: Project, g: Func) -> bool:
    """A function declared `static` / `class func` (callable as `.name(...)`)."""
    m = project.modules.get(g.module)
    line = m.raw[g.start - 1] if m and 0 < g.start <= len(m.raw) else ""
    return bool(re.search(r"\b(?:static|class)\s+(?:func|var|let)\b", line))


_PY_BUILTINS = set(dir(__import__("builtins")))
_SELF = {"self", "this", "Self", "cls"}
_CTOR_NAMES = {"init", "__init__", "constructor", "__construct", "initialize"}
_RS_STD_METHODS = {
    "unwrap", "expect", "unwrap_or", "unwrap_or_else", "unwrap_or_default", "unwrap_err", "is_none", "is_some",
    "is_ok", "is_err", "ok", "err", "map_err", "ok_or", "ok_or_else", "and_then", "as_ref", "as_mut", "as_deref",
    "clone", "cloned", "copied", "to_string", "to_owned", "into", "iter", "iter_mut", "into_iter", "collect",
    "push", "push_str", "pop", "len", "is_empty", "as_bytes", "as_str", "as_slice", "to_vec", "extend", "borrow",
    "borrow_mut", "lock", "take", "chars", "bytes", "trim", "split", "join", "starts_with", "ends_with",
    "filter", "filter_map", "enumerate", "zip", "rev", "any", "all", "fold", "sum", "count", "last", "first",
    "to_str", "to_path_buf", "display", "fmt", "eq", "cmp", "partial_cmp", "hash", "deref", "drop", "default",
    # Option / Result / Iterator combinators (`.map(|v| v.as_str())` after `.get_one()`)
    "map", "map_or", "map_or_else", "flat_map", "flatten", "skip", "skip_while", "take_while", "peekable",
    "windows", "chunks", "position", "rposition", "max_by", "min_by", "max_by_key", "min_by_key", "sorted",
    "unwrap_or_else", "is_some_and", "is_none_or", "inspect", "step_by", "cycle", "product", "partition",
    "as_deref_mut", "transpose", "zip_with", "chars", "char_indices", "lines", "split_whitespace", "trim_end",
    "trim_start", "to_lowercase", "to_uppercase", "to_ascii_lowercase", "repeat", "retain", "dedup", "truncate",
}


def _nothing(project: Project, f: Func, c) -> None:
    """A call that reaches no project function. Counted apart when a project
    function in the caller's language has that name: the share stats shows
    so a resolver can't look better by turning guesses into nothing."""
    if not _first_count(project, f, c):
        return
    res = project.resolution[os.path.splitext(f.file)[1]]
    res[2] += 1
    fam = _family(f.file)
    named = any(g.kind != "top" and fam & _family(g.file) for g in project.by_name.get(c.name, ()))
    if named:
        res[3] += 1
    if _TRACE is not None:
        _TRACE.append((f.file, c.line, c.name, c.chain, "named" if named else "nothing", ()))


def _first_count(project: Project, f: Func, c) -> bool:
    """Whether this call site is counted now: a call inside a nested function
    (a callback, a lambda) belongs to every enclosing function's calls, but
    the resolution counters count each site once."""
    seen = project.__dict__.setdefault("_counted", set())
    key = (f.file, c.line, c.col, c.name)
    if key in seen:
        return False
    seen.add(key)
    return True


# SEAMCHECK_TRACE=1: every call's outcome, for tuning a resolver (bench/trace.py)
_TRACE: list | None = [] if os.environ.get("SEAMCHECK_TRACE") else None


def _link(project: Project, precise: bool, use_cache: bool) -> None:
    """The static call graph; in precise mode, language servers then settle
    the calls it guessed, read from this build's trace (precise.py)."""
    global _TRACE
    if not precise:
        _build_edges(project)
        return
    outer, _TRACE = _TRACE, []
    try:
        _build_edges(project)
        trace = _TRACE
    finally:
        _TRACE = outer
    if outer is not None:
        outer.extend(trace)
    from . import precise as precise_mod
    precise_mod.refine(project, trace, use_cache)


def walk_order(project: Project) -> list[Func]:
    """The order calls are resolved in (and the trace written): innermost
    function first, so a call in a callback or a nested class's method is
    counted where it is made, with that function's `this` and locals."""
    return sorted(project.funcs.values(), key=lambda g: g.end - g.start)


def _build_edges(project: Project) -> None:
    _resolve_imports(project)
    _locals_cache.clear()
    _MODULE_JS.clear()
    groups = _groups(project)
    scope_cache: dict = {}
    ext_cache: dict = {}
    # Short names for qualifiers in languages whose modules are named by path
    # (Kotlin, Java, TS, Rust, Python): `MapPrefs.set` means the module in
    # MapPrefs.kt, or the one declaring `object MapPrefs`.
    for m in project.modules.values():
        stem = os.path.splitext(os.path.basename(m.file))[0]
        for nm in ({*m.decls} if m.owner else {stem, *m.decls}):  # a Python class is named by its class, not its file
            if m.name not in project.short[nm]:
                project.short[nm].append(m.name)
    # C++: a class's members are declared in its header; the .cpp's functions
    # use them (`m_board->indexOf()`): the header's fields count for the .cpp.
    for rel, mods in project.by_file.items():
        stem, ext = os.path.splitext(rel)
        if ext in (".cpp", ".cc", ".cxx", ".c") and mods:
            for hext in (".h", ".hpp", ".hh"):
                for hm in project.by_file.get(stem + hext, []):
                    for k, v in hm.bound.items():
                        mods[0].bound.setdefault(k, v)
    # supertypes as modules, then every module's descendants (for overrides
    # and for an interface's implementations)
    from . import infer
    infer.resolve_bases(project)
    types = infer.Types(project)
    pending: list = []
    for m in project.modules.values():
        for a in types.ancestors(m.name)[1:]:
            project.children[a].append(m.name)
    for f in walk_order(project):
        mod = project.modules.get(f.module)
        for c in f.calls:
            targets: list[tuple[Func, bool]] = []
            recv: tuple = ()  # the receiver's type, when known: overrides only in its subtypes run
            q = c.qual
            if c.chain == ("<implicit>",):
                # `.option(x)` / `case .value(let v)`: an enum case or a static member of the
                # type the context expects. A pattern runs nothing; with the expected type
                # known, that type's static function (or its enum case: nothing); else a
                # guess among the project's static functions of that name
                kind_, got = _implicit_member(project, types, mod, f, c)
                if kind_ == "none":
                    _nothing(project, f, c)
                    continue
                if kind_ == "exact":
                    targets = [(g, True) for g in got]
                else:
                    stat = [g for g in project.by_name.get(c.name, []) if _is_static(project, g)]
                    if not stat:
                        _nothing(project, f, c)
                        continue
                    targets = [(g, False) for g in stat]
            elif c.chain and mod and f.file.endswith(_C_FAMILY) and _cpp_namespace_call(project, f, c, targets):
                if not targets:
                    _nothing(project, f, c)  # `std::move(x)`: the standard library's
                    continue
            elif c.chain and mod:
                # the receiver's type, step by step (see infer.py)
                types.line = c.line
                v = types.chain(mod, f, c.chain)
                if v is not None and v[0] == "wrap":
                    v = infer.EXT  # `self.bump().map(...)`: Option's own method
                if infer.is_ext(v):
                    # a library value; a Kotlin / C# / Swift extension the project
                    # declares on that library type is the one exception
                    exts = (types.ext_fns.get((v[1][0], c.name), []) or
                            types.ext_fns.get((v[1][0].split("`")[0], c.name), [])) if v[1] else []
                    if not exts and v[1] and f.file.endswith(".swift"):
                        # `sample.wrapped(to:)` on a String: an extension of a protocol String conforms to
                        exts = next((types.ext_fns[(pr, c.name)] for pr in _SWIFT_CONFORMS.get(v[1][0], ())
                                     if (pr, c.name) in types.ext_fns), [])
                    if exts and f.file.endswith(".swift"):
                        exts = [g for g in exts if _swift_labels_match(project, mod, c, g)]
                    own = [g for g in exts if g.file == f.file]
                    exts = own if len(own) == 1 else exts
                    if not exts:
                        _nothing(project, f, c)
                        continue
                    targets = [(g, len(exts) == 1) for g in exts]
                    v = None
                if v is not None and v[0] == "static" and len(v[1]) > 1 and f.file.endswith(".rs"):
                    root_ = _rs_crate_dir(project, f.file)  # (each test binary's own `mod utils`)
                    own_ = tuple(mn for mn in v[1] if project.modules[mn].file.startswith(root_)) if root_ else ()
                    if len(own_) == 1:
                        v = ("static", own_, True)
                if v is not None and v[0] in ("static", "inst") and f.file.endswith(infer._JS_EXTS):
                    v = types.forward(mod, v, c.name) or v  # `ctx.redirect()`: delegated to ctx.response
                if v is not None:
                    kind, mods_, ex = v
                    if kind == "ns":
                        hits, on_type = [], True
                    elif kind == "pkg":
                        hits, on_type = _in_scope(project, "pkg:" + mods_[0], c.name), True
                    elif kind == "static":
                        hits = [h for mn in mods_ for h in _in_scope(project, mn, c.name)]
                        on_type = True
                        if not hits:
                            hits, on_type = types.method(list(mods_), c.name, _argc(mod, c))
                    else:
                        hits, on_type = types.method(list(mods_), c.name, _argc(mod, c))
                        if kind == "inst":  # (`super.x()` runs the parent's x itself: no subtype's)
                            recv = ("\0super",) if c.chain[-1] in ("super", "base", "parent") else tuple(mods_)
                    if len(hits) > 1 and c.targs >= 0:  # C#: `NoOp()` is not `NoOp<T>()`
                        hits = [h for h in hits if not h.targs or str(c.targs) in h.targs.split(",")] or hits
                    if hits:
                        # on the type: one definition (or one type's overloads); through an
                        # interface: each implementation runs. Several same-named types and
                        # only one has the method: the receiver's name settles it.
                        sure = (ex and (len(hits) == 1 or not on_type or types.one_type(hits))) or \
                            (on_type and types.one_type(hits))
                        targets = [(h, sure) for h in hits]
                    elif kind != "pkg" and any(types.external_base(mn) for mn in mods_):
                        _nothing(project, f, c)  # inherited from a library supertype
                        continue
                    elif kind == "inst" and mods_ and f.file.endswith(infer._JS_EXTS) and any(
                            project.modules[mn].kind == "interface" and c.name in project.modules[mn].methods for mn in mods_):
                        _nothing(project, f, c)  # a TS type's member no project class implements: an object's, unknown
                        continue
                    elif kind == "inst" and mods_ and all(project.modules[mn].kind == "interface" and (
                            project.modules[mn].file.endswith(".d.ts") and not types._es_module(project.modules[mn].file)
                            or (project.modules[mn].decls or [""])[0] in _DOM_GLOBALS) for mn in mods_):
                        _nothing(project, f, c)  # `interface CanvasRenderingContext2D {...}` in global.d.ts adds to the DOM's
                        continue
                    elif kind == "inst" and mods_ and all(project.modules[mn].kind == "ext" for mn in mods_) and \
                            f.file.endswith((".swift", ".kt", ".kts", ".cs")):
                        _nothing(project, f, c)  # `Data` the project only extends: a method not in an extension is the library's
                        continue
            if not q and mod and not targets and f.file.endswith(infer._JS_EXTS[:-1]) and c.name in f.bound \
                    and f.bound[c.name].startswith("lam:"):
                types.line = c.line
                if infer.is_ext(types.first(mod, f, c.name, 0)):
                    _nothing(project, f, c)  # `new Promise((resolve) => resolve(x))`: the runtime's function
                    continue
            if not q and mod and not f.file.endswith((".py", ".ex", ".exs")) and not targets:
                # a bare call in a class: its own (or an inherited) method first;
                # `Foo(...)` / `new Foo(...)` / `: Base(args)`: Foo's constructor
                own = types.own_class(mod, f) if (mod.owner or mod.kind) else None
                # (Go, Rust, JS/TS, PHP: a method needs its receiver, `c.Name()` / `self.x()` /
                # `this.x()` / `$this->x()`; a bare `MarkFlagRequired(...)` in a method is the package's)
                hits, on_type = types.method(own, c.name) if own else ([], False)
                if f.file.endswith(_EXPLICIT_RECEIVER):
                    hits = [g for g in hits if not _needs_receiver(project, g)]
                if hits and on_type and len(hits) == 1:
                    targets = [(hits[0], True)]
                elif c.name[:1].isupper() and not _in_scope(project, (project.modules.get(mod.owner, mod) if mod.owner else mod).name, c.name):
                    tmods, ex, declared = types.type_modules(mod, c.name)
                    if declared and not _is_test_file(project, f.file):
                        # product code never builds a test's type (a test target's `extension URL`)
                        tmods = [m for m in tmods if not _is_test_file(project, project.modules[m].file)]
                        declared = bool(tmods)
                    if declared:
                        ctor, _ = types.method([m for m in tmods if types.searchable(m) or project.modules[m].kind],
                                               c.name) if tmods else ([], False)
                        ctor = [g for g in ctor if g.name == c.name] or [
                            g for m in tmods for n in ("constructor", "init", "__construct", "new")
                            if (g := project.funcs.get(f"{m}.{n}"))][:1]
                        ctor = list({g.id: g for g in ctor}.values())
                        if ctor:
                            targets = [(g, ex and (len(ctor) == 1 or types.one_type(ctor))) for g in ctor]
                        else:
                            _nothing(project, f, c)  # a project type with no constructor of its own: no function runs
                            continue
                if not targets and own and not hits and any(types.external_base(m) for m in own) and \
                        not _in_scope(project, (project.modules.get(mod.owner, mod) if mod.owner else mod).name, c.name):
                    # inherited from a library supertype (Qt's `disconnect`, `data`)
                    _nothing(project, f, c)
                    continue
            if targets:
                pass
            elif q == "super" and mod and mod.parent:
                t = _lookup(project, mod.parent, c.name)
                if t:
                    targets = [(t, True)]
            elif q == "super" and mod and f.file.endswith((".rb", ".rake", ".ru")):
                # Ruby's `super`: the same method in the nearest ancestor (superclass, included modules)
                own = types.own_class(mod, f)
                for a in (types.ancestors(own[0])[1:] if own else []):
                    t = project.funcs.get(f"{a}.{c.name}")
                    if t is not None and t.id != f.id:
                        targets = [(t, True)]
                        break
                else:
                    if own and types.external_base(own[0]):
                        _nothing(project, f, c)  # a library superclass's
                        continue
            elif q and mod and q in _SELF and (t := _lookup(project, mod.name, c.name)) and (q != "cls" or mod.owner):
                # `self.x()` / `this.x()` / `Self::x()`: the caller's own class
                # (a Python class is its own module; elsewhere, the file)
                targets = [(t, True)]
            elif q and mod and q in mod.external and q not in f.bound:
                # `json.dumps()`, `fmt.Println()`, `path.join()`: a library the file imports
                _nothing(project, f, c)
                continue
            elif q:
                # Each way to name the qualifier, most certain first; a miss falls
                # through to the next (an elif chain sent a missed alias straight to
                # the name guess, which drops common names like `set`).
                # A local the project says what it is ([project.groups]): exact.
                held = f.bound.get(q) or (mod.bound.get(q, "") if mod else "")
                bound = groups.get(held, "")
                if bound:
                    t = _lookup(project, bound, c.name)
                    if t:
                        targets = [(t, True)]
                elif held.startswith("type:"):
                    # A declared type: its module's function, or a Kotlin extension
                    # on it. A type outside the project: nothing (a `String`'s
                    # `split` is not ours). A project type that misses (inherited
                    # from a library, an extension we can't see): the usual ways.
                    hits, in_project = _typed_targets(project, mod, held[5:], c.name, ext_cache)
                    own = [t for t in hits if t.file == f.file]
                    if len(own) == 1:
                        hits = own  # a file's own (private) extension shadows the others'
                    if hits:
                        targets = [(t, len(hits) == 1) for t in hits]
                    elif not in_project and not (f.file.endswith(infer._DYNAMIC) and held[5:] in types.extended):
                        _nothing(project, f, c)  # (`n: Node` in GDScript may hold a project script: not this)
                        continue
                if not targets and mod and q in mod.aliases:
                    hits = _in_scope(project, mod.aliases[q], c.name)  # (through a package's re-exports)
                    if len(hits) == 1:
                        targets = [(hits[0], True)]
                if not targets and q in project.modules:
                    t = _lookup(project, q, c.name)
                    if t:
                        targets = [(t, True)]
                if not targets and mod and q in mod.imported:
                    # a named import (`import { API } from "./helpers"`): its module's function
                    dm = _defining_module(project, *mod.imported[q])
                    t = _lookup(project, dm, c.name) if dm else None
                    if t:
                        targets = [(t, True)]
                if not targets:
                    names = [q] + ([mod.aliases[q].rsplit(".", 1)[-1]] if mod and q in mod.aliases else [])
                    for nm in names:
                        hits = [t for t in (_lookup(project, mn, c.name) for mn in project.short.get(nm, [])) if t]
                        if hits:
                            targets = [(t, len(hits) == 1) for t in hits]
                            break
            elif not q and mod and _shadowed(f, c.name):
                # a parameter or local holding a function: the helper the
                # caller itself defines (`const nudge = () => ...` inside a
                # component) when there is one, else a name guess at most
                inner = [g for g in project.by_name.get(c.name, []) if g.file == f.file
                         and f.start <= g.start <= f.end and g.id != f.id
                         and not (mod and re.match(rf"^\s*{re.escape(c.name)}\s*:", mod.raw[g.start - 1]))]  # (not `key: () =>`)
                if len(inner) == 1:
                    targets = [(inner[0], True)]
                elif not inner and _scoped_language(f.file, mod):
                    if f.file.endswith(_PARAM_FLOW_EXTS) and c.name in _param_names(f):
                        pending.append((f, c))  # a parameter: what the callers pass (after this pass)
                    _nothing(project, f, c)  # a closure or a parameter: no project function by that name
                    continue
            elif not q and mod:
                # own module, then a name imported by itself, then modules and
                # packages imported whole, then the rest of its own package.
                # (A bare name in a Python method means the file's, not the class's.)
                smod = project.modules.get(mod.owner, mod) if mod.owner else mod
                hits = _in_scope(project, smod.name, c.name)
                if not hits and c.name in smod.imported:
                    w, n = smod.imported[c.name]
                    hits = _in_scope(project, w, n)
                for w in ([] if hits else smod.uses):
                    hits = _in_scope(project, w, c.name)
                    if hits:
                        break
                if not hits and smod.package:
                    hits = _in_scope(project, "pkg:" + smod.package, c.name)
                if not hits and f.file.endswith(".py"):
                    # `Foo(...)` builds a Foo: its __init__
                    cls = _py_class(project, mod, c.name)
                    t = _lookup(project, cls, "__init__") if cls else None
                    if t:
                        hits = [t]
                    elif cls:
                        _nothing(project, f, c)  # a project class with no __init__ of its own: no function runs
                        continue
                # Go: two functions of one name in one package are build variants
                # (`_windows.go` / `_unix.go`): the call reaches whichever is built
                variants = f.file.endswith(".go") and len({t.module for t in hits}) == len(hits) and \
                    len({(project.modules[t.module].owner or t.module) and project.modules.get(project.modules[t.module].owner or t.module, project.modules[t.module]).package for t in hits}) == 1
                hits = list({t.id: t for t in hits}.values())  # (overloads share an id: one function)
                targets = [(t, len(hits) == 1 or variants or types.one_type(hits)) for t in hits]
                if not targets and c.name in mod.external:
                    _nothing(project, f, c)
                    continue
                if not targets and _scoped_language(f.file, mod) and c.name not in mod.imported:
                    # Elixir, Kotlin, Java, an ES module: a bare call reaches only
                    # what is local, imported or in its package. Nothing in scope
                    # means a library or the language's own (Enum, Compose's Row,
                    # vitest's expect, ConnTest's post): not a guess at every
                    # project function of that name.
                    _nothing(project, f, c)
                    continue
            if not targets and q and f.file.endswith(".py") and c.name[:1].isupper() and mod is not None \
                    and (cls := _py_class(project, mod, f"{q}.{c.name}")):
                # `sessions.Session()`: a class through its module builds one, so its __init__ runs
                t = _lookup(project, cls, "__init__")
                if t is None:
                    _nothing(project, f, c)  # (a class with no __init__ of its own: no project function runs)
                    continue
                targets = [(t, True)]
            if not targets and q and _MODULE_Q.match(q) and q != "Self":
                # A qualifier that names a module (`Enum.reject`, `Repo.rollback`)
                # and misses: the function lives outside the project (or the alias
                # is wrong). Nothing, not a guess: `Enum.reject` used to land on
                # every project function named reject.
                _nothing(project, f, c)
                continue
            if not targets and c.chain and f.file.endswith(".rs") and all(re.fullmatch(r"[a-z_]\w*", s_) for s_ in c.chain):
                # `utils::assert_output(...)` in an integration test: the `mod utils` of the
                # caller's own crate root (tests/builder/main.rs has its utils.rs, tests/derive another)
                path_ = "/".join(c.chain)
                root_ = _rs_crate_dir(project, f.file)
                mine = [g for g in project.by_name.get(c.name, []) if g.file.startswith(root_)
                        and (g.file[:-3].endswith("/" + path_) or g.file.endswith("/" + path_ + "/mod.rs"))]
                if len({g.file for g in mine}) == 1:
                    targets = [(mine[0], True)]
            if not targets and q and c.name in _RS_STD_METHODS and f.file.endswith(".rs"):
                # `.unwrap()`, `.len()`, `.push()` on a receiver of unknown type:
                # Option / Result / Vec / String, not a project function of that name
                _nothing(project, f, c)
                continue
            if not targets and not q and (f.file.endswith((".c", ".h")) or f.file.endswith(_CPP_EXTS) and any(
                    g.file.endswith((".c", ".h")) for g in project.by_name.get(c.name, ()))):
                # (a C++ file calling the project's C functions: the same linker rule)
                # C: no imports, the linker's rule. A `static` function is its file's;
                # any other is global to the program: the call reaches the file's own
                # definition, else the project's one non-static definition of that name
                cfuncs = [g for g in project.by_name.get(c.name, []) if g.kind != "top" and g.file.endswith((".c", ".h"))]
                inc_c = _c_included(project, f.file)  # (`#include "../cJSON.c"`: its statics are this file's too)
                own = [g for g in cfuncs if g.file == f.file or g.file == os.path.splitext(f.file)[0] + ".h"] or \
                    [g for g in cfuncs if g.file in inc_c]
                glob_ = [g for g in cfuncs if not _c_static(project, g) or g.file.endswith(".h")]  # (a header's static inline: its includers')
                if own:
                    targets = [(g, True) for g in own[:1]]
                elif len({g.file for g in glob_}) == 1 and glob_:
                    targets = [(glob_[0], True)]
                elif not cfuncs:
                    # libc, a library's, or a macro: a project macro (`JV_ARRAY(a, b)`) runs the
                    # functions its body calls, resolved by the same rule
                    for g in _c_macro_targets(project, f.file, c.name):
                        project.edges[f.id].append((g.id, c.line, True))
                        project.redges[g.id].append((f.id, c.line, True))
                    _nothing(project, f, c)
                    continue
            if not targets and not q and not c.chain and mod is not None and f.file.endswith(infer._JS_EXTS) and not any(
                    g.kind != "top" and _family(g.file) & _family(f.file) and not project.modules[g.module].owner
                    and project.modules[g.module].kind not in ("class", "interface") for g in project.by_name.get(c.name, ())):
                _nothing(project, f, c)  # `setTimeout(...)`: a JS method needs its receiver; no free function has the name
                continue
            if not targets:
                # A method on some object, or an import we can't see: every
                # function with that name is a candidate (a "fuzzy" edge).
                fam = _family(f.file)
                in_test = _is_test_file(project, f.file)
                cands = [g for g in project.by_name.get(c.name, []) if g.kind != "top" and fam & _family(g.file)
                         and (in_test or not _is_test_file(project, g.file))]  # product code never calls a test
                if q and mod and 0 < c.line <= len(mod.raw) and f"{q}::{c.name}" in mod.raw[c.line - 1]:
                    # `veridian::api::parseClaim(...)`: the namespace names where it lives
                    named = [g for g in cands if f"/{q}/" in "/" + g.file or g.module.split("::")[-1] == q
                             or g.module.endswith("." + q)]
                    if len(named) == 1:
                        cands = named
                        targets = [(named[0], True)]
                # A guess by name is never "exact", even when only one function has
                # the name: `icon()` on a lambda parameter matched PlacesNav.icon.
                # Except a method call (`store.getState().applyOp()`,
                # `gestures.onTouch()`) whose name exists in exactly one module
                # the file imports or shares a package with: that is the one.
                seen_mods = _scope(project, mod, scope_cache) if mod and not _shadowed(f, c.name) else set()
                mine = [g for g in cands if g.module in seen_mods]
                if f.file.endswith(".py"):
                    # x.f() reaches a module-level Python function only through
                    # the module's own name (resolved above), never `session.get()`
                    mine = [g for g in mine if project.modules[g.module].owner]
                if q:
                    # an untyped receiver can hold anything: a name only one module in scope
                    # has is no proof (`res.json()` on a fetch Response is not Context.json,
                    # `@app.call` not Logger#call); the receiver's type is what settles it
                    mine = []
                own = [g for g in cands if g.file == f.file and g.recv] if not f.file.endswith(".cs") else []
                fm_ = project.modules.get(mod.owner, mod) if mod and mod.owner else mod
                if not targets and c.chain and f.file.endswith(".gd") and mod is not None:
                    # `var part: RigidBody3D = ...` then `part.explode()`: the object is a script
                    # extending RigidBody3D; the project's one such script with an explode() it is
                    eng = _gd_declared(project, types, mod, f, c.chain)
                    sub = [g for g in cands if eng and _gd_extends(project, g.module, eng)] if eng else []
                    if len({g.module for g in sub}) == 1:
                        targets = [(sub[0], True)]
                if targets:
                    pass
                elif q and f.file.endswith((".kt", ".kts")) and len(cands) == 1 and cands[0].recv and fm_ is not None and \
                        (cands[0].file == f.file or fm_.imported.get(c.name, ("", ""))[1] == c.name):
                    # Kotlin: the project's one function of that name is an extension this file
                    # declares or imports by name (`import ...alias`): nothing else is in scope
                    targets = [(cands[0], True)]
                elif len(own) == 1 and q != "" and len(cands) > 1:
                    # an extension the caller's own file declares (each generated
                    # parser's private JSONObject.textOrNull): it shadows the others'
                    targets = [(own[0], True)]
                elif len(mine) == 1 and q != "":
                    targets = [(mine[0], True)]
                elif 1 < len(mine) <= 4 and q != "" and len({g.module for g in mine}) == 1:
                    # overloads in the one module in scope (JSONObject.textOrNull and
                    # JSONArray.textOrNull side by side): the call is one of them
                    targets = [(g, True) for g in mine]
                else:
                    if len(cands) > 8:
                        cands = []  # a very common name (run, get, init...): too vague to be a useful edge
                    targets = [(g, False) for g in cands]
            # Virtual dispatch: a call that lands on a parent's function can
            # run any subclass override of it too.
            more = []
            below = {s for r in recv if r in project.modules
                     for s in [r] + types.parts(project.modules[r]) + project.children.get(r, [])} if recv else None
            for t, exact in targets:
                if t.name in _CTOR_NAMES or (t.name[:1].isupper() and c.name == t.name):
                    continue  # a constructor builds that type, not a subclass
                n_args = _argc(mod, c)
                for sub in project.children.get(t.module, ()):
                    if below is not None and sub not in below:
                        continue  # a sibling's override: not this receiver's
                    o = project.funcs.get(f"{sub}.{t.name}")
                    if o and (n_args is None or types._takes(o, o.name, n_args)):  # (an override of another overload: not this call's)
                        more.append((o, exact))
            if not q and f.file.endswith(_PARAM_FLOW_EXTS) and not any(e for _, e in targets) and c.name in _param_names(f):
                pending.append((f, c))  # `checkFn(x)` on a parameter: what the callers pass (after this pass)
            if not targets:
                _nothing(project, f, c)
            elif _first_count(project, f, c):
                project.resolution[os.path.splitext(f.file)[1]][0 if any(e for _, e in targets) else 1] += 1
                if _TRACE is not None:
                    _TRACE.append((f.file, c.line, c.name, c.chain, "exact" if any(e for _, e in targets) else "guess",
                                   tuple(t.id for t, _ in targets[:4])))
            if f.file.endswith(".py") and mod is not None and 0 < c.line <= len(mod.raw) \
                    and mod.raw[c.line - 1].lstrip().startswith("@") and len(targets) == 1 and targets[0][1]:
                # `@app.route("/")`: route() runs, then the function it returns gets the decorated one
                inner = _py_returned_closure(project, targets[0][0])
                if inner is not None:
                    more.append((inner, True))
            seen_t = {t.id for t, _ in targets}
            more = [(o, e) for o, e in more if o.id not in seen_t and not seen_t.add(o.id)]
            for t, exact in targets + more:
                project.edges[f.id].append((t.id, c.line, exact))
                project.redges[t.id].append((f.id, c.line, exact))
    _param_flow(project, types, pending)


def _py_returned_closure(project: Project, g: Func):
    """The function a Python decorator factory returns: `def route(...): def decorator(f): ...;
    return decorator` gives route's `decorator`, or None."""
    rets = list(re.finditer(r"^([ \t]*)return\s+([A-Za-z_]\w*)\s*$", g.code, re.M))
    if not rets:
        return None
    m = min(rets, key=lambda r: len(r.group(1).expandtabs(4)))  # (the function's own return, not its closure's)
    name = m.group(2)
    for h in project.by_name.get(name, ()):
        if h.file == g.file and g.start < h.start <= g.end:
            return h
    return None  # (a file's same-named closures are one function, whose span is the first's: only that factory's)


def _passed(project: Project, types, f: Func, k: int, depth: int):
    """The one project function every caller of f passes as its k-th argument
    (its id), or None."""
    from . import lambdas
    callers = project.redges.get(f.id, [])
    if depth > 4 or not callers or not all(e for _, _, e in callers):
        return None
    got = set()
    pname = (_param_names(f) + [""] * (k + 1))[k]
    for src, line, _ in callers:
        g = project.funcs.get(src)
        gm = next((m for m in project.by_file.get(g.file, []) if m.name == g.module), None) if g else None
        site = next((c2 for c2 in (g.calls if g else []) if c2.line == line and c2.name == f.name and c2.col >= 0), None)
        if site is None or gm is None:
            return None
        code = "\n".join(gm.code[line - 1:line + 30])
        o = code.find("(", site.col)
        close = lambdas.matching(code, o) if o >= 0 else -1
        args = lambdas.split_top(code[o + 1:close]) if close > 0 else []
        arg = args[k].strip() if 0 <= k < len(args) else ""
        if not re.match(r"^[A-Za-z_$][\w$]*$", arg):
            return None
        if g.id == f.id and arg == pname:
            continue  # a recursive call handing the parameter on
        gparams = _param_names(g)
        if arg in gparams and arg not in g.bound:  # the caller's own parameter: what its callers pass
            hit_id = _passed(project, types, g, gparams.index(arg), depth + 1)
        else:
            hit = types.bare(gm, g, arg)
            hit_id = hit.id if hit is not None and hit.kind != "top" else None
        if hit_id is None:
            return None
        got.add(hit_id)
        if len(got) > 1:
            return None
    return got.pop() if len(got) == 1 else None


_PARAM_FLOW_EXTS = (".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".py")


def _param_names(f: Func) -> list[str]:
    """A JS / TS / Python function's parameter names, in order ("" for one
    this can't name: a destructuring, `*args`)."""
    from . import lambdas
    k = f.code.find("(")
    if k < 0 or k > 400:
        return []
    e = lambdas.matching(f.code, k)
    if e < 0:
        return []
    out = []
    for p_ in lambdas.split_top(f.code[k + 1:e]):
        m = re.match(r"^\s*(?:(?:public|private|protected|readonly)\s+)?([A-Za-z_$][\w$]*)\s*[?]?\s*(?:[:=].*)?$", p_, re.S)
        out.append(m.group(1) if m and m.group(1) not in ("self", "cls") else "")
    return out


def _param_flow(project: Project, types, pending: list) -> None:
    """A call through a parameter (`checkFn(x)` in `runCheck(checkFn)`): when
    every caller is exact and passes the same project function there by name
    (or hands on its own parameter, whose callers do), the call reaches that
    function."""
    for f, c in pending:
        params = _param_names(f)
        if c.name not in params:
            continue
        got = _passed(project, types, f, params.index(c.name), 0)
        if got is None:
            continue
        target = project.funcs[got]
        old = [(d, ln, e) for d, ln, e in project.edges[f.id] if ln == c.line and d.rsplit(".", 1)[-1] == c.name and not e]
        for d, ln, e in old:
            project.edges[f.id].remove((d, ln, e))
            if (f.id, ln, e) in project.redges.get(d, []):
                project.redges[d].remove((f.id, ln, e))
        project.edges[f.id].append((target.id, c.line, True))
        project.redges[target.id].append((f.id, c.line, True))
        res = project.resolution[os.path.splitext(f.file)[1]]
        if old and res[1] > 0:
            res[1] -= 1
            res[0] += 1
        elif not old and res[2] > 0:  # (it reached nothing: a closure's parameter)
            res[2] -= 1
            res[0] += 1
        if _TRACE is not None:
            for i, r in enumerate(_TRACE):
                if r[0] == f.file and r[1] == c.line and r[2] == c.name and r[4] in ("guess", "nothing", "named"):
                    _TRACE[i] = (r[0], r[1], r[2], r[3], "exact", (target.id,))
                    break


# --- queries ----------------------------------------------------------------------

def reach(project: Project, start: str, depth: int, reverse: bool = False, exact_only: bool = False) -> dict[str, int]:
    graph = project.redges if reverse else project.edges
    dist = {start: 0}
    dq = deque([start])
    while dq:
        u = dq.popleft()
        if dist[u] >= depth:
            continue
        for v, _, exact in graph.get(u, []):
            if exact_only and not exact:
                continue
            if v not in dist:
                dist[v] = dist[u] + 1
                dq.append(v)
    return dist


def path(project: Project, a: str, b: str, max_depth: int = 12, exact_only: bool = False) -> list[str]:
    prev = {a: None}
    dq = deque([(a, 0)])
    while dq:
        u, d = dq.popleft()
        if u == b:
            out = []
            while u:
                out.append(u)
                u = prev[u]
            return out[::-1]
        if d >= max_depth:
            continue
        for v, _, exact in project.edges.get(u, []):
            if exact_only and not exact:
                continue
            if v not in prev:
                prev[v] = u
                dq.append((v, d + 1))
    return []


def edge_split(project: Project, fid: str, reverse: bool = True) -> tuple[int, int]:
    """(exact, name-guess) distinct callers (or callees) of one function."""
    graph = project.redges if reverse else project.edges
    ex, guess = set(), set()
    for v, _, exact in graph.get(fid, []):
        (ex if exact else guess).add(v)
    return len(ex), len(guess - ex)


# --- rules --------------------------------------------------------------------------

@dataclass
class Finding:
    rule: str
    severity: str
    file: str
    line: int
    func: str
    message: str
    snippet: str = ""


def _allowed(project: Project, rel: str, func: Func | None, allow: list[str]) -> bool:
    for a in allow:
        pg, _, fg = a.partition(":")
        if glob_re(pg).match(rel) and (not fg or (func and fnmatch.fnmatchcase(func.name, fg))):
            return True
    return False


def _suppressed(raw: list[str], line: int, rule: str) -> bool:
    for ln in (line - 1, line - 2):
        if 0 <= ln < len(raw):
            t = raw[ln]
            m = re.search(r"seamcheck:\s*allow\b\s*([\w\-, ]*)", t)
            if m and (not m.group(1).strip() or rule in re.split(r"[,\s]+", m.group(1))):
                return True
    return False


def _text_for(project: Project, rel: str, mode: str) -> list[str]:
    mods = project.by_file.get(rel)
    if mode == "code" and mods:
        return mods[0].code
    # many rules read the same big files: blank each (file, mode) once per load
    key = (rel, mode)
    hit = project.text_cache.get(key)
    if hit is not None:
        return hit
    raw = project.text(rel)
    if mode == "raw":
        out = raw.split("\n")
    else:
        code, _ = blank(raw, scan_family(rel), keep_strings=(mode == "nocomment"))
        out = code.split("\n")
    project.text_cache[key] = out
    return out


def run_rules(project: Project, only: list[str] | None = None, skip: list[str] | None = None) -> list[Finding]:
    out: list[Finding] = [Finding("config", "error", pr.split(":")[0], 0, "", pr) for pr in project.config_problems]
    out += [Finding("config", "warn", n.split(":")[0], 0, "", n) for n in getattr(project, "env_notes", [])]
    # command rules are separate processes (node scripts, a few seconds each):
    # start them all at once, then collect in rule order
    from concurrent.futures import ThreadPoolExecutor
    cmds = [r for r in project.config.get("rule", []) if r.get("kind") == "command"
            and not (only and r["id"] not in only) and not (skip and r["id"] in skip)]
    pool = ThreadPoolExecutor(max_workers=max(1, min(8, len(cmds)))) if cmds else None
    pending = {r["id"]: pool.submit(_command, project, r) for r in cmds} if pool else {}
    for r in project.config.get("rule", []):
        rid = r["id"]
        if (only and rid not in only) or (skip and rid in skip):
            continue
        kind = r.get("kind", "forbid")
        fn = {"forbid": _forbid, "together": _together, "reach": _reach, "coverage": _coverage,
              "require": _require, "deps": _deps, "fanout": _fanout, "lint": _lint,
              "essential": _essential, "command": _command,
              "depend": _arch("depend"), "layers": _arch("layers"), "cycles": _arch("cycles"),
              "path": _arch("path")}.get(kind)
        if fn is None:
            out.append(Finding(rid, "error", r.get("_file", "seamcheck.toml"), 0, "", f"unknown rule kind {kind!r}"))
            continue
        try:
            out.extend(pending[rid].result() if rid in pending else fn(project, r))
        except re.error as e:  # a rule's regex, not the tool: name the rule, run the rest
            out.append(Finding(rid, "error", r.get("_file", "seamcheck.toml"), 0, "", f"bad pattern in rule {rid}: {e}"))
        except EnvMissing as e:
            out.append(Finding(rid, "warn", r.get("_file", "seamcheck.toml"), 0, "", f"rule {rid} not checked here: {e.args[0] if e.args else e}"))
        except KeyError as e:
            out.append(Finding(rid, "error", r.get("_file", "seamcheck.toml"), 0, "", f"rule {rid} needs {e}"))
    if pool:
        pool.shutdown()
    order = {"error": 0, "warn": 1, "info": 2}
    out.sort(key=lambda f: (order.get(f.severity, 3), f.rule, f.file, f.line))
    return out


_ENV_MISSING = re.compile(r"ERR_MODULE_NOT_FOUND|Cannot find (?:module|package) '[^']+'|command not found|"
                          r"No such file or directory: '?node_modules|\(Mix\) Could not find|could not compile dependency")


def _command(project: Project, r: dict) -> list[Finding]:
    """[[rule]] kind = "command": a project's own check script, run from the root.

      run    the command (a string, run through the shell)
      when   globs: in a gate it runs only if the change touches one of them
             (`check` always runs it)
      files  regex finding a file in each output line (group 1 if any); a line
             naming no file is about the whole check
      prefix prepended to each found file to make it repo-relative
      fix    one line telling how to fix (shown with each finding)

    Exit 0 is clean. Otherwise each non-empty output line is a finding. With
    `gate = "touched"` a finding blocks only when its file is part of the change
    (a line naming no file blocks whenever the check ran): old debt elsewhere
    doesn't stop an unrelated change, new debt in your files does."""
    changed = getattr(project, "gate_changed", None)
    when = r.get("when", [])
    if changed is not None and when and not any(match_any(f, when) for f in changed):
        return []
    try:
        res = subprocess.run(r["run"], shell=True, cwd=project.root, capture_output=True, text=True,
                             timeout=float(r.get("timeout", 300)))
    except subprocess.TimeoutExpired:
        return [Finding(r["id"], "error", "", 0, "", f"`{r['run']}` took longer than {r.get('timeout', 300)} s")]
    if res.returncode == 0:
        return []
    both = res.stdout + "\n" + res.stderr
    if res.returncode == 127 or _ENV_MISSING.search(both):
        # the check couldn't run here (a worktree without node_modules / deps):
        # it vouches neither way; its stack trace isn't twelve findings
        raise EnvMissing(f"`{r['run']}` couldn't run: {_ENV_MISSING.search(both).group(0) if _ENV_MISSING.search(both) else 'exit 127'}"
                         " (install this checkout's deps)")
    rx = re.compile(r["files"]) if r.get("files") else None
    prefix = r.get("prefix", "")
    sev = r.get("severity", "error")
    out = []
    for line in (res.stdout + "\n" + res.stderr).splitlines():
        line = line.strip()
        if not line or line.endswith(":"):
            continue  # a heading ("... must match the scan:")
        m = rx.search(line) if rx else None
        rel = (prefix + (m.group(1) if m.groups() else m.group(0))) if m else ""
        out.append(Finding(r["id"], sev, rel, 0, "", line + (f"  (fix: {r['fix']})" if r.get("fix") else "")))
    if not out:
        out.append(Finding(r["id"], sev, "", 0, "", f"`{r['run']}` exited {res.returncode} with no output"))
    return out


def _require(project: Project, r: dict) -> list[Finding]:
    """Each file in `in` must exist and contain every `pattern`: in the whole
    file, or in each function matching `func` (globs). `ordered = true`: the
    patterns appear in that order (inject before enqueue). A glob in `in` must
    match at least one file. Default text is "nocomment" (strings count)."""
    pats = [re.compile(p, re.M) for p in (r["pattern"] if isinstance(r["pattern"], list) else [r["pattern"]])]
    mode = r.get("text", "nocomment")
    funcs = r.get("func")
    funcs = [funcs] if isinstance(funcs, str) else funcs
    sev, rid = r.get("severity", "error"), r["id"]
    out = []

    def miss(rel, line, fname, msg):
        out.append(Finding(rid, sev, rel, line, fname, r.get("message", msg) if "missing" in msg else msg))

    for spec in r["in"]:
        if any(c in spec for c in "*?["):
            files = [p for p in project.files if match_any(p, [spec])]
            if not files:
                miss(spec, 0, "", f"no file matches {spec}")
        else:
            files = [spec]
        for rel in files:
            if not os.path.isfile(os.path.join(project.root, rel)):
                miss(rel, 0, "", f"required file is gone: {rel}")
                continue
            lines = _text_for(project, rel, mode)
            units = [(None, 1, len(lines))]
            if funcs:
                units = [(f, f.start, f.end) for m in project.by_file.get(rel, []) for f in m.funcs
                         if f.kind != "top" and any(fnmatch.fnmatchcase(f.name, g) for g in funcs)]
                if not units:
                    miss(rel, 0, "", f"no function {', '.join(funcs)} in {rel}")
            for func, a, b in units:
                body, pos = "\n".join(lines[a - 1:b]), 0
                for p in pats:
                    m = p.search(body, pos)
                    if not m:
                        miss(rel, a, func.name if func else "",
                             f"missing{' (in order)' if r.get('ordered') and pos else ''}: {p.pattern}")
                        break
                    if r.get("ordered"):
                        pos = m.end()
    return out


def _arch(kind: str):
    """Architecture rules (arch.py): depend, layers, cycles."""
    def run(project: Project, r: dict) -> list[Finding]:
        from . import arch
        return getattr(arch, kind)(project, r)
    return run


def _deps(project: Project, r: dict) -> list[Finding]:
    """[[rule]] kind = "deps": dependencies with a status in `report` (default
    unused / test-only / dead-only), minus `ignore`, one finding each."""
    from . import deps
    want = set(r.get("report", deps.ACTIONABLE))
    skip = set(r.get("ignore", []))
    out = []
    found = deps.analyze(project)
    # packages not installed (here or in the main checkout): one note per
    # manifest, never a finding per dependency (a fresh tree is not "unused")
    pending: dict[str, list] = {}
    for d in found:
        if d.status == "uninstalled" and d.name not in skip:
            pending.setdefault(d.manifest, []).append(d)
    for man, ds in sorted(pending.items()):
        cmd = {"mix": "mix deps.get", "npm": "npm ci"}.get(ds[0].eco, "install its dependencies")
        out.append(Finding(r["id"], "warn", man, ds[0].line, "",
                           f"{len(ds)} {ds[0].eco} dependencies not checked: nothing installed here or in the main "
                           f"checkout (run `{cmd}` in {os.path.dirname(man) or '.'} for a real answer)", ""))
    for d in found:
        if d.status in want and d.name not in skip and not _suppressed(project.text(d.manifest).split("\n"), d.line, r["id"]):
            why = {"unused": "no reference in code, tests, config or scripts",
                   "test-only": f"declared {d.scope} but only tests use it",
                   "dead-only": "used only in functions nothing calls: " + ", ".join(d.dead[:3])}.get(d.status, d.status)
            out.append(Finding(r["id"], r.get("severity", "warn"), d.manifest, d.line, "",
                               r.get("message", f"{d.eco} dependency {d.name}: {why}") if "message" not in r else r["message"],
                               "(name guessed: install deps for a sure answer)" if d.guessed else ""))
    return out


def _scope_files(project: Project, r: dict) -> list[str]:
    pool = project.files
    if r.get("repo"):  # scripts and config outside `[project] include`
        pool = list_files(project.root, r.get("in", ["**/*"]),
                          DEFAULT_EXCLUDE + project.config.get("project", {}).get("exclude", []))
    return [p for p in pool if match_any(p, r.get("in", ["**/*"])) and not match_any(p, r.get("skip", []))]


def _lint(project: Project, r: dict) -> list[Finding]:
    """[[rule]] kind = "lint", lint = "<check>": a structural check seamcheck
    knows (see lint.py), over `in` (default: the check's languages) minus
    `skip`, with `options` for its knobs. `allow` / waivers as for forbid."""
    from . import lint
    name = r.get("lint", "")
    check = lint.LINTS.get(name)
    if check is None:
        near = difflib.get_close_matches(name, lint.LINTS, 1)
        return [Finding(r["id"], "error", r.get("_file", "seamcheck.toml"), 0, "",
                        f"unknown lint {name!r}" + (f" (did you mean {near[0]!r}?)" if near else "")
                        + f"; known: {', '.join(sorted(lint.LINTS))}")]
    opts = r.get("options", {})
    bad = [k for k in opts if k not in lint.OPTIONS[name]]
    if bad:
        return [Finding(r["id"], "error", r.get("_file", "seamcheck.toml"), 0, "",
                        f"lint {name}: unknown option(s) {', '.join(bad)} (known: {', '.join(sorted(lint.OPTIONS[name])) or 'none'})")]
    scope = r.get("in", lint.DEFAULT_IN[name])
    # a lint over scripts / units / lockfiles reads the repo, not only `[project] include`
    pool = (list_files(project.root, scope, DEFAULT_EXCLUDE + project.config.get("project", {}).get("exclude", []))
            if name in lint.REPO_WIDE or r.get("repo") else project.files)
    files = [p for p in pool if match_any(p, scope) and not match_any(p, r.get("skip", []))]
    changed = getattr(project, "gate_changed", None)
    if changed is not None and name in lint.PER_FILE:
        # a gate: a file-local check can only change its answer where the change touched
        sib = {os.path.join(os.path.dirname(c), "package-lock.json") for c in changed if c.endswith("package.json")}
        sib |= {c[:-len(".service")] + ".path" for c in changed if c.endswith(".service")}
        files = [p for p in files if p in changed or p in sib]
    out = []
    for rel in files:
        raw_text = "\n".join(_text_for(project, rel, "raw"))
        code = "\n".join(_text_for(project, rel, "code"))
        try:
            hits = check(rel, raw_text, code, opts, project=project)
        except RecursionError:
            continue
        raw = raw_text.split("\n")
        for line, msg in hits:
            func = _enclosing(project, rel, line)
            if _allowed(project, rel, func, r.get("allow", [])) or _suppressed(raw, line, r["id"]):
                continue
            out.append(Finding(r["id"], r.get("severity", "error"), rel, line, func.name if func else "",
                               r.get("message", msg), raw[line - 1].strip()[:160] if 0 < line <= len(raw) else ""))
    return out


def _essential(project: Project, r: dict) -> list[Finding]:
    """[[rule]] kind = "essential": a manifest of controls each bar must show
    unconditionally (lint.essential)."""
    from . import lint
    out = []
    for rel, line, msg in lint.essential(project, r):
        raw = project.text(rel).split("\n") if line else []
        if line and _suppressed(raw, line, r["id"]):
            continue
        out.append(Finding(r["id"], r.get("severity", "error"), rel, line, "", msg,
                           raw[line - 1].strip()[:160] if 0 < line <= len(raw) else ""))
    return out


def _forbid(project: Project, r: dict) -> list[Finding]:
    pat = re.compile(r["pattern"])
    mode = r.get("text", "code")
    out = []
    files = _scope_files(project, r)
    changed = getattr(project, "gate_changed", None)
    if changed is not None:
        files = [f for f in files if f in changed]  # a pattern in one file: only touched files can gain one
    for rel in files:
        lines = _text_for(project, rel, mode)
        mod = project.module_of_file(rel)
        raw = mod.raw if mod else project.text(rel).split("\n")
        if r.get("scope") == "file":
            # a pattern spanning lines (`setTimeout(...` then `setProvider(`): search
            # the whole file, report the line each match starts on
            text = "\n".join(lines)
            hits = sorted({text.count("\n", 0, m.start()) for m in pat.finditer(text)})
        else:
            hits = [i for i, ln in enumerate(lines) if pat.search(ln)]
        for i in hits:
            func = None
            for m in project.by_file.get(rel, []):
                func = m.func_at(i + 1) or func
            if _allowed(project, rel, func, r.get("allow", [])) or _suppressed(raw, i + 1, r["id"]):
                continue
            out.append(Finding(r["id"], r.get("severity", "error"), rel, i + 1, func.name if func else "",
                               r.get("message", f"forbidden: {r['pattern']}"), raw[i].strip()[:160] if i < len(raw) else ""))
    return out


def _together(project: Project, r: dict) -> list[Finding]:
    """In every function (or file, scope="file") where `if` matches, each of
    `then` must match too (optionally `unless` excuses it). `func`: only
    functions with these names (globs). `depth = N`: a `then` may also match
    in what the function calls, N exact calls deep (a helper that logs)."""
    cond = re.compile(r["if"])
    thens = [re.compile(t) for t in (r["then"] if isinstance(r["then"], list) else [r["then"]])]
    unless = re.compile(r["unless"]) if r.get("unless") else None
    scope = r.get("scope", "func")
    mode = r.get("text", "code")
    out = []
    for rel in _scope_files(project, r):
        mods = project.by_file.get(rel, [])
        raw = mods[0].raw if mods else project.text(rel).split("\n")
        lines = _text_for(project, rel, mode)
        if scope == "near":
            # every occurrence of `if` needs each `then` within its window of lines
            before, after = (r.get("window") or [0, 6])
            for i, ln in enumerate(lines):
                if not cond.search(ln):
                    continue
                func = None
                for m in mods:
                    func = m.func_at(i + 1) or func
                win = "\n".join(lines[max(0, i - before):i + after + 1])
                if (unless and unless.search(win)) or _allowed(project, rel, func, r.get("allow", [])) or _suppressed(raw, i + 1, r["id"]):
                    continue
                missing = [t.pattern for t in thens if not t.search(win)]
                if missing:
                    out.append(Finding(r["id"], r.get("severity", "error"), rel, i + 1, func.name if func else "",
                                       r.get("message", "missing nearby: " + ", ".join(missing)), raw[i].strip()[:160] if i < len(raw) else ""))
            continue
        units = []
        funcs = r.get("func")
        funcs = [funcs] if isinstance(funcs, str) else funcs
        if scope == "file" or not mods:
            units.append((None, 1, len(lines)))
        else:
            for m in mods:
                for f in m.funcs:
                    if f.kind != "top" and (not funcs or any(fnmatch.fnmatchcase(f.name, g) for g in funcs)):
                        units.append((f, f.start, f.end))
        for func, a, b in units:
            body = "\n".join(lines[a - 1:b])
            m = cond.search(body)
            if not m or (unless and unless.search(body)):
                continue
            line = a + body[:m.start()].count("\n")
            if _allowed(project, rel, func, r.get("allow", [])) or _suppressed(raw, line, r["id"]):
                continue
            missing = [t for t in thens if not t.search(body)]
            if missing and func and r.get("depth"):
                for cid in reach(project, func.id, int(r["depth"]), exact_only=True):
                    callee = project.funcs.get(cid)
                    if callee and cid != func.id:
                        missing = [t for t in missing if not t.search(callee.code)]
            missing = [t.pattern for t in missing]
            if missing:
                out.append(Finding(r["id"], r.get("severity", "error"), rel, line, func.name if func else "",
                                   r.get("message", "missing: " + ", ".join(missing)),
                                   raw[line - 1].strip()[:160] if line - 1 < len(raw) else ""))
    return out


def _reach(project: Project, r: dict) -> list[Finding]:
    """Every function picked by `from` (specs; optionally only those whose
    code matches `when`) must reach one of `to` (function names) within
    `depth` calls ("all" mode: every one of them)."""
    when = re.compile(r["when"]) if r.get("when") else None
    targets = r["to"] if isinstance(r["to"], list) else [r["to"]]
    depth = int(r.get("depth", 4))
    mode = r.get("mode", "any")
    out = []
    picked = []
    for spec in (r["from"] if isinstance(r["from"], list) else [r["from"]]):
        picked.extend(project.find(spec))
    for f in dict.fromkeys(picked):
        if when and not when.search(f.code):
            continue
        if _allowed(project, f.file, f, r.get("allow", [])):
            continue
        dist = reach(project, f.id, depth, exact_only=r.get("strict", False))
        names = {project.funcs[k].name for k in dist if k in project.funcs and k != f.id}
        hit = [t for t in targets if t in names or t in f.refs]
        ok = bool(hit) if mode == "any" else len(hit) == len(targets)
        if not ok:
            miss = [t for t in targets if t not in hit]
            out.append(Finding(r["id"], r.get("severity", "error"), f.file, f.start, f.name,
                               r.get("message", f"doesn't reach {' / '.join(miss)} within {depth} calls")))
    return out


def _coverage(project: Project, r: dict) -> list[Finding]:
    """Keys `defined` in one place vs keys `used` elsewhere: report defined
    but never used, and used but never defined (typos, dead ids)."""
    def harvest(spec: dict) -> dict[str, tuple[str, int]]:
        pat = re.compile(spec["pattern"])
        found = {}
        for rel in [p for p in project.files if match_any(p, spec.get("in", ["**/*"]))]:
            for i, ln in enumerate(_text_for(project, rel, spec.get("text", "nocomment"))):
                for m in pat.finditer(ln):
                    key = next((g for g in m.groups() if g), m.group(0))
                    found.setdefault(key, (rel, i + 1))
        return found
    d, u = harvest(r["defined"]), harvest(r["used"])
    ignore = set(r.get("ignore", []))
    out = []
    checks = r.get("check", ["unused", "undefined"])
    if "unused" in checks:
        for k in sorted(set(d) - set(u) - ignore):
            rel, ln = d[k]
            out.append(Finding(r["id"], r.get("severity", "warn"), rel, ln, "", f"{k!r} is defined but never used" + (": " + r["message"] if r.get("message") else "")))
    if "undefined" in checks:
        for k in sorted(set(u) - set(d) - ignore):
            rel, ln = u[k]
            out.append(Finding(r["id"], r.get("severity", "error"), rel, ln, "", f"{k!r} is used but never defined" + (": " + r["message"] if r.get("message") else "")))
    return out


_RUN_CACHE: dict[tuple[str, str], str] = {}


class EnvMissing(KeyError):
    """A rule's helper command couldn't run here (deps not installed, a tool
    missing): the rule can't vouch either way. Reported as a warning naming
    the cause, never as a contract break."""


def _same_file(a: str, b: str) -> bool:
    try:
        with open(a, "rb") as fa, open(b, "rb") as fb:
            return fa.read() == fb.read()
    except OSError:
        return False


def _run_output(project: Project, cmd: str, spec: dict) -> str:
    """A command's output. With `inputs` (globs of what the output depends on,
    e.g. the router for `mix phx.routes`) it is cached on disk until one of
    those files changes: a 5 s command no longer runs on every check."""
    inputs = spec.get("inputs")
    path = None
    if inputs:
        # keyed by the inputs' content, not the checkout: a lane's worktree with
        # the main checkout's router gets the main checkout's `mix phx.routes`
        from . import fingerprint as fp
        files = sorted(list_files(project.root, inputs, []))
        digest = hashlib.sha1(json.dumps([cmd, fp.digest_files(project.root, files)]).encode()).hexdigest()[:20]
        path = fp.path(f"run-{digest}.txt")
        try:
            with open(path, encoding="utf-8") as fh:
                return fh.read()
        except OSError:
            pass
    res = subprocess.run(cmd, shell=True, cwd=project.root, capture_output=True, text=True,
                         timeout=spec.get("timeout", 300))
    if res.returncode != 0:
        # a git worktree (lanes) has no deps/_build: when the command's inputs are
        # the same as the main checkout's, the main checkout answers for it
        from .deps import _main_checkout
        main = _main_checkout(project.root)
        same = bool(main and inputs) and all(
            _same_file(os.path.join(project.root, rel), os.path.join(main, rel))
            for rel in list_files(project.root, inputs, []))
        if same:
            res = subprocess.run(cmd, shell=True, cwd=main, capture_output=True, text=True,
                                 timeout=spec.get("timeout", 300))
        if res.returncode != 0:
            raise EnvMissing(f"run {cmd!r} exited {res.returncode}: {res.stderr.strip()[-300:]}"
                             + ("" if same else " (in a worktree: install its deps, or its inputs differ from the "
                                                 "main checkout's)" if main else ""))
    if path:
        try:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(res.stdout)
        except OSError:
            pass
    return res.stdout
HARVEST_KEYS = {"in", "skip", "func", "within", "pattern", "values", "text", "run", "rewrite", "timeout", "join", "token",
                "inputs"}
VIEW_KEYS = {"match", "normalize", "fallback", "except", "ignore", "sep", "wildcard", "unhandled"}


def _token(spec: dict, m: re.Match) -> str:
    """A match's token: its first non-empty group (or the whole match); with
    `join`, all non-empty groups joined (`(client)_op\\("(\\w+)"` -> client.open_url)."""
    if "token" in spec:
        return m.group(spec["token"]) or ""
    groups = [g for g in m.groups() if g]
    if "join" in spec and groups:
        return spec["join"].join(groups)
    return groups[0] if groups else m.group(0)


def harvest_all(project: Project, spec: dict) -> list[tuple[str, str, int, "re.Match | None"]]:
    """Every occurrence a spec names: (token, file, line, match). A spec is:
      in / skip   file globs
      func        only inside these functions (globs), e.g. the one `switch`
      within      only inside this regex's match (group 1 if it has one), e.g.
                  '(?s)@widget_types ~w\\((.*?)\\)' or an enum body
      pattern     token regex (group 1, else the match); a list is several
      values      literal tokens (no file needed)
      text        code | nocomment (default: string contents count) | raw
      run         a shell command whose output is harvested instead of files
      inputs      globs the run's output depends on: cached on disk until they change
      join        join all non-empty groups with this string (a token built in parts)
      token       which group is the token (others stay available, e.g. to a bridge's target)
      rewrite     [[regex, replacement], ...] applied to each token (`:id` ->
                  `:p`, strip `?query`); a token rewritten to "" is dropped
    Patterns search whole text (a region can span lines)."""
    found: list = []
    rewrite = spec.get("rewrite", [])

    def keep(key: str, where: tuple[str, int], m=None) -> None:
        for pat, rep in rewrite:
            key = re.sub(pat, rep, key)
        if key:
            found.append((key, where[0], where[1], m))

    for v in spec.get("values", []):
        keep(v, (spec.get("_file", "seamcheck.toml"), 0))
    if "pattern" not in spec:
        return found
    if spec.get("run"):
        # the project's own tool knows best (`mix phx.routes`, `cargo metadata`)
        cmd = spec["run"]
        key = (cmd, project.root)
        if key not in _RUN_CACHE:
            _RUN_CACHE[key] = _run_output(project, cmd, spec)
        out = _RUN_CACHE[key]
        for pat in (spec["pattern"] if isinstance(spec["pattern"], list) else [spec["pattern"]]):
            for m in re.finditer(pat, out):
                keep(_token(spec, m), (f"$ {cmd}", out.count("\n", 0, m.start()) + 1), m)
        return found
    pats = [re.compile(p) for p in (spec["pattern"] if isinstance(spec["pattern"], list) else [spec["pattern"]])]
    within = re.compile(spec["within"]) if spec.get("within") else None
    funcs = spec.get("func")
    funcs = [funcs] if isinstance(funcs, str) else funcs
    files = [p for p in project.files if match_any(p, spec.get("in", ["**/*"])) and not match_any(p, spec.get("skip", []))]
    for rel in files:
        lines = _text_for(project, rel, spec.get("text", "nocomment"))
        text = "\n".join(lines)
        starts = [0]
        for ln in lines[:-1]:
            starts.append(starts[-1] + len(ln) + 1)
        regions: list[tuple[int, int]] = [(0, len(text))]
        if funcs and not project.by_file.get(rel):
            raise KeyError(f"func = {funcs} on {rel}, which seamcheck can't parse into functions: use within")
        if funcs:
            regions = [(starts[f.start - 1], starts[f.end] if f.end < len(starts) else len(text))
                       for m in project.by_file.get(rel, []) for f in m.funcs
                       if f.kind != "top" and any(fnmatch.fnmatchcase(f.name, g) for g in funcs)]
        if within:
            regions = [(m.start(1), m.end(1)) if m.re.groups and m.group(1) is not None else (m.start(), m.end())
                       for a, b in regions for m in within.finditer(text, a, b)]
        for a, b in regions:
            for pat in pats:
                for m in pat.finditer(text, a, b):
                    keep(_token(spec, m), (rel, bisect.bisect_right(starts, m.start())), m)
    return found


def harvest(project: Project, spec: dict) -> dict[str, tuple[str, int]]:
    """Tokens a spec names, each with where it is first seen (see harvest_all)."""
    out: dict[str, tuple[str, int]] = {}
    for tok, rel, line, _ in harvest_all(project, spec):
        out.setdefault(tok, (rel, line))
    return out


def view_tokens(project: Project, spec: dict) -> dict[str, tuple[str, int]]:
    """A view's handled tokens. With `unhandled` (a regex), a token whose every
    branch body matches it is not handled: `case "share_link": return
    {kind: "center"}` names the token and still sends it to the default. A
    branch body is the text from the token's match to the next match (a
    fallthrough `case "a": case "b":` shares the next body)."""
    if not spec.get("unhandled"):
        return harvest(project, spec)
    dead = re.compile(spec["unhandled"])
    occ = [o for o in harvest_all(project, spec) if o[3] is not None]
    occ.sort(key=lambda o: (o[1], o[3].start()))
    bodies: list[str] = []
    for i, (tok, rel, line, m) in enumerate(occ):
        nxt = occ[i + 1][3].start() if i + 1 < len(occ) and occ[i + 1][1] == rel else min(len(m.string), m.end() + 400)
        body = m.string[m.end():nxt]
        cut = re.search(r"\bdefault\s*:|\belse\s*(?:->|\{|$)|^\s*_\s*->", body, re.M)
        bodies.append(body[:cut.start()] if cut else body)
    for i in range(len(bodies) - 2, -1, -1):  # fallthrough: an empty body is the next one's
        if re.fullmatch(r"[\s,|:]*(?:case\s*)?", bodies[i]) and occ[i + 1][1] == occ[i][1]:
            bodies[i] = bodies[i + 1]
    out: dict[str, tuple[str, int]] = {}
    for (tok, rel, line, m), body in zip(occ, bodies):
        if not dead.search(body):
            out.setdefault(tok, (rel, line))
    return out


def _handles(spec: dict, got, token: str) -> bool:
    """Does a view's harvested set cover `token`? match = exact (default) |
    contains (the view tests `type.contains("stash")`) | prefix | regex (the
    view's tokens are regexes)."""
    # normalize = [[regex, replacement], ...]: what the view does to a token
    # before looking it up (OS-A drops `_embed` once instead of listing both)
    for pat, rep in spec.get("normalize", []):
        token = re.sub(pat, rep, token)
    how = spec.get("match", "exact")
    if how == "exact":
        return token in got
    if how == "contains":
        return any(g in token for g in got)
    if how == "prefix":
        return any(token.startswith(g) for g in got)
    if how == "regex":
        return any(re.fullmatch(g, token) for g in got)
    if how == "segments":
        # paths: same number of `sep`-separated segments, each equal or a
        # `wildcard` on either side (`/a/:p/b` meets `/a/:id/b` and `/a/x/b`)
        # A trailing `**` on the token is "any rest" (a client appending a sub-path).
        sep, wild = spec.get("sep", "/"), spec.get("wildcard", ":p")
        ts = token.split(sep)
        rest = ts[-1] == "**"
        if rest:
            ts = ts[:-1]

        def meets(gs):
            if rest:
                gs = gs[:len(ts)] if len(gs) >= len(ts) else None
            return gs is not None and len(gs) == len(ts) and all(a == b or wild in (a, b) for a, b in zip(ts, gs))
        return any(meets(g.split(sep)) for g in got)
    raise KeyError(f"match = {how!r} (exact | contains | prefix | regex | segments)")


def _covers_some(spec: dict, g: str, produced) -> bool:
    """A view token some produced token needs (not a stale handler)."""
    return any(_handles(spec, {g}, k) for k in produced)


def _fanout(project: Project, r: dict) -> list[Finding]:
    """One producer, many consumers: every token the `source` produces must be
    handled by each of `views` (missing), and a view shouldn't handle tokens
    nothing produces (extra: a stale handler, or a producer that lost it).
    Each view may list `except` (a list, or {token = "why"}) for intentional
    gaps, and `ignore` for extras it handles on purpose."""
    out = []
    for where, spec in [("source", r["source"])] + [(f"views.{v}", sp) for v, sp in r["views"].items()]:
        allowed = HARVEST_KEYS | (VIEW_KEYS if where != "source" else set())
        for k in spec:
            if k not in allowed:
                near = difflib.get_close_matches(k, allowed, 1)
                out.append(Finding(r["id"], "error", r.get("_file", "seamcheck.toml"), 0, "",
                                   f"{where}: unknown key {k!r}" + (f" (did you mean {near[0]!r}?)" if near else "")))
    src = harvest(project, dict(r["source"], _file=r.get("_file")))
    checks = r.get("check", ["missing"])
    if not src:
        return [Finding(r["id"], "error", r.get("_file", "seamcheck.toml"), 0, "", "source produced no tokens (pattern or path wrong?)")]
    for view, spec in r["views"].items():
        got = view_tokens(project, dict(spec, _file=r.get("_file")))
        if not got and not spec.get("values"):
            out.append(Finding(r["id"], "error", r.get("_file", "seamcheck.toml"), 0, "", f"view {view} handles nothing (pattern or path wrong?)"))
            continue
        exc = spec.get("except", [])
        exc = set(exc if isinstance(exc, list) else exc.keys())
        fb = spec.get("fallback")
        if "missing" in checks:
            for k in sorted(k for k in src if k not in exc and not _handles(spec, got, k)):
                rel, ln = src[k]
                out.append(Finding(r["id"], "warn" if fb else r.get("severity", "error"), rel, ln, "",
                                   f"{view} does not handle {k!r}" + (f" (falls back to {fb})" if fb else "")
                                   + (": " + r["message"] if r.get("message") else "")))
        if "extra" in checks:
            for k in sorted(g for g in got if g not in spec.get("ignore", []) and not _covers_some(spec, g, src)):
                rel, ln = got[k]
                out.append(Finding(r["id"], r.get("extra_severity", "warn"), rel, ln, "",
                                   f"{view} handles {k!r}, which the source never produces"))
        for k in sorted(k for k in exc if _handles(spec, got, k)):
            rel, ln = got[k]
            out.append(Finding(r["id"], "warn", rel, ln, "", f"{view} lists {k!r} under except but handles it now: drop the exception"))
    return out


def matrix(project: Project, r: dict) -> tuple[list[str], list[str], dict]:
    """(tokens, views, cell) for a fanout rule: cell[(token, view)] is "ok",
    "except", "fallback", "missing", "extra" (handled, never produced) or ""."""
    src = harvest(project, dict(r["source"], _file=r.get("_file")))
    views = list(r["views"])
    got, exc = {}, {}
    for v in views:
        spec = r["views"][v]
        got[v] = set(view_tokens(project, dict(spec, _file=r.get("_file")))) - set(spec.get("ignore", []))
        e = spec.get("except", [])
        exc[v] = set(e if isinstance(e, list) else e.keys())
    extra = {g for v in views for g in got[v] if not _covers_some(r["views"][v], g, src)}
    cell = {}
    for k in set(src) | extra:
        for v in views:
            spec = r["views"][v]
            if k in src:
                cell[(k, v)] = ("ok" if _handles(spec, got[v], k) else "except" if k in exc[v]
                                else "fallback" if spec.get("fallback") else "missing")
            else:
                cell[(k, v)] = "extra" if k in got[v] else ""
    return sorted(set(src) | extra, key=lambda k: (k not in src, k)), views, cell


def _enclosing(project: Project, rel: str, line: int) -> Func | None:
    """The innermost function around a line (the file's <top> if none)."""
    inside = [f for m in project.by_file.get(rel, []) for f in m.funcs if f.start <= line <= f.end]
    real = [f for f in inside if f.kind != "top"]
    if real:
        return max(real, key=lambda f: (f.start, -f.end))
    return inside[0] if inside else None


def _apply_bridges(project: Project) -> None:
    """[[bridge]] rules: edges no call graph can see, across languages. Each
    `from` occurrence (a client's `"/api/osa/inbox"`, a hub `push(socket,
    "ops")`) links its enclosing function to what the matching `to` names:
    `target` (a function id built from the match's groups, e.g. a route's
    "{2}.{3}" -> Controller.action) or the function enclosing each `to`
    occurrence (a client's `.on("ops")` handler). Matching is `match` (exact,
    segments...) like a fanout view. Direction is data flow: from -> to."""
    for b in project.config.get("bridge", []):
        bid = b.get("id", "bridge")
        try:
            froms = harvest_all(project, dict(b["from"], _file=b.get("_file")))
            tos = harvest_all(project, dict(b["to"], _file=b.get("_file")))
        except EnvMissing as e:  # couldn't run here (a worktree without deps): not a broken bridge
            project.env_notes = getattr(project, "env_notes", []) + [f"{b.get('_file', 'seamcheck.toml')}: bridge {bid} not built here: {e.args[0] if e.args else e}"]
            continue
        except (KeyError, re.error, subprocess.SubprocessError, OSError) as e:
            project.bridge_problems.append(f"{b.get('_file', 'seamcheck.toml')}: bridge {bid}: {e}")
            continue
        for side in ("from", "to"):
            bad = [k for k in b[side] if k not in HARVEST_KEYS | {"target"}]
            if bad:
                project.bridge_problems.append(f"{b.get('_file', 'seamcheck.toml')}: bridge {bid}: {side}: unknown key {bad[0]!r}")
        target = b["to"].get("target")
        dests: dict[str, list[str]] = defaultdict(list)
        for tok, rel, line, m in tos:
            if target:
                try:
                    fid = target.format(*([m.group(0)] + list(m.groups())) if m else [tok])
                except (IndexError, KeyError):
                    continue
                if fid in project.funcs:
                    dests[tok].append(fid)
            else:
                f = _enclosing(project, rel, line)
                if f:
                    dests[tok].append(f.id)
        if not dests:
            project.bridge_problems.append(f"{b.get('_file', 'seamcheck.toml')}: bridge {bid}: `to` found nothing")
            continue
        spec = {k: v for k, v in b.items() if k in ("match", "sep", "wildcard", "normalize")}
        exact_hits: dict[str, list[str]] = {}
        for tok, rel, line, _ in froms:
            if tok not in exact_hits:
                exact_hits[tok] = [d for t, ds in dests.items() if _handles(spec, {t}, tok) for d in ds]
            hits = exact_hits[tok]
            if not hits:
                continue
            src = _enclosing(project, rel, line)
            if not src:
                continue
            for dst in dict.fromkeys(hits):
                if dst == src.id:
                    continue
                project.edges[src.id].append((dst, line, True))
                project.redges[dst].append((src.id, line, True))
                project.bridged[(src.id, dst)] = bid


def impact(project: Project, start: set[str], depth: int = 4, fuzzy: bool = False) -> dict[str, int]:
    """Everything a change to `start` can reach: callers (who depends on
    it), transitively, plus what bridges carry forward (the client handlers
    of an event it emits). Distance per function."""
    dist = {s: 0 for s in start}
    dq = deque(start)
    while dq:
        u = dq.popleft()
        if dist[u] >= depth:
            continue
        # certain edges only unless asked: a name guess is not "this breaks"
        nxt = [c for c, _, ex in project.redges.get(u, []) if ex or fuzzy]
        nxt += [d for d, _, _ in project.edges.get(u, []) if (u, d) in project.bridged]
        for v in nxt:
            if v not in dist and v in project.funcs:
                dist[v] = dist[u] + 1
                dq.append(v)
    return dist


# --- built-in reports (no config needed) -----------------------------------------

ENGINE_CALLBACKS = re.compile(r"^(_ready|_process|_physics_process|_input|_unhandled_input|_gui_input|_draw|_init|"
                              r"_enter_tree|_exit_tree|_notification|_to_string|_get|_set|_on_\w+|handle_\w+|init|"
                              r"mount|render|main|__\w+__|setup|test_\w+|call|start_link|child_spec|terminate|code_change)$")


def dead(project: Project) -> list[Func]:
    called = set()
    refs = set()
    for f in project.funcs.values():
        refs |= f.refs
        for tgt, _, _ in project.edges.get(f.id, []):
            called.add(tgt)
    out = []
    for f in project.funcs.values():
        if f.kind in ("top", "test") or f.id in called or f.name in refs or ENGINE_CALLBACKS.match(f.name):
            continue
        # anything named in any file's text as a string or @rpc etc.: be conservative
        out.append(f)
    return out
