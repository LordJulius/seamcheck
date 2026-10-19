"""Declared dependencies against what the code uses.

Every manifest under the root (mix.exs, package.json, build.gradle[.kts] with
libs.versions.toml, Cargo.toml, pyproject.toml / requirements*.txt, go.mod)
is read, and each dependency is looked for in the files that manifest owns
(its directory, minus nested projects of the same kind). What a dependency is
"called" in code comes from the installed package where it can be found (mix
`deps/<name>` module names, `node_modules/<name>` bins, Gradle-cache jar
packages, Python dist-info `top_level.txt`), and is guessed from the name
otherwise (the finding then says "guessed").

Each dependency gets one status:

  unused     no reference in code, tests, config or scripts
  test-only  a runtime dependency only tests use (belongs in the test scope)
  dead-only  used only inside functions nothing calls (goes with that code)
  peer       not referenced, but another declared dependency needs it
  plugin     not referenced, but it wires itself in at runtime (a service file,
             components in its Android manifest): check before removing
  config     referenced only from config / build files / scripts (tools)
  used       referenced from code; `files` says where (one file = removed with it)
"""
from __future__ import annotations

import bisect
import glob
import json
import os
import re
import sys
import tomllib
import zipfile
from dataclasses import dataclass, field

from .scan import blank

_PRUNE = {"node_modules", "deps", "_build", "build", ".git", "target", ".gradle", "dist", "vendor", "__pycache__",
          ".venv", "venv", ".mypy_cache", ".pytest_cache", ".idea", ".cache", "out", ".next", ".svelte-kit",
          "coverage", ".godot", ".elixir_ls", ".cxx", "intermediates"}
_TEST_PATH = re.compile(r"(^|/)(test|tests|__tests__|spec|androidTest|testFixtures|test-support)(/|$)"
                        r"|\.(test|spec)\.[cm]?[jt]sx?$|_test\.(exs|go|py|rs)$|(^|/)test_[^/]*\.py$")
_CONFIG_PATH = re.compile(r"(^|/)(config|scripts|\.github|deploy|gradle)/|\.config\.[cm]?[jt]s$"
                          r"|(^|/)(mix\.exs|build\.gradle(\.kts)?|settings\.gradle(\.kts)?|Makefile|Dockerfile|justfile"
                          r"|package\.json|Cargo\.toml|pyproject\.toml|setup\.py|go\.mod|tsconfig[\w.]*\.json)$"
                          r"|\.(toml|ya?ml|sh|ini|cfg)$")


@dataclass
class Dep:
    eco: str
    name: str
    manifest: str  # relative path
    line: int
    scope: str = "runtime"  # runtime | dev | test | tool | bom
    guessed: bool = False
    status: str = ""
    code: list[tuple[str, int]] = field(default_factory=list)  # (file, line) uses in product code
    tests: list[tuple[str, int]] = field(default_factory=list)
    config: list[tuple[str, int]] = field(default_factory=list)
    needed_by: list[str] = field(default_factory=list)  # declares it optional / peer: must be declared here
    via: list[str] = field(default_factory=list)  # pulls it in anyway (declaring it is redundant)
    plugin: str = ""  # wires itself in at runtime (service file, library manifest): why
    dead: list[str] = field(default_factory=list)  # functions (ids) the code uses sit in, when all are dead
    uninstalled: bool = False  # its manifest's packages aren't installed here or in the main checkout

    @property
    def files(self) -> list[str]:
        return sorted({f for f, _ in self.code})


def _line_of(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def _liner(text: str):
    """pos -> 1-based line, for many lookups in one text."""
    starts = [0] + [m.end() for m in re.finditer("\n", text)]
    return lambda pos: bisect.bisect_right(starts, pos)


def _walk(root: str, d: str, exts: tuple[str, ...], manifest: str, exclude) -> list[str]:
    out = []
    for dirpath, dirs, files in os.walk(d):
        keep = []
        for x in dirs:
            sub = os.path.join(dirpath, x)
            if x in _PRUNE or x.startswith(".") and x not in (".github",):
                continue
            if any(os.path.exists(os.path.join(sub, m)) for m in manifest.split("|")):
                continue  # a nested project owns its own files
            keep.append(x)
        dirs[:] = keep
        for fn in files:
            if fn.endswith(exts) or fn in exts:
                rel = os.path.relpath(os.path.join(dirpath, fn), root)
                if not exclude(rel):
                    out.append(rel)
    return out


def _read(root: str, rel: str) -> str:
    try:
        with open(os.path.join(root, rel), encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def _readp(path: str) -> str:
    return _read(os.path.dirname(path), os.path.basename(path))


_MAIN_ROOT: dict[str, str] = {}


def _main_checkout(root: str) -> str:
    """The main working tree when `root` is a linked git worktree (lanes under
    .claude/worktrees/), else "". A worktree has no deps/ or node_modules/ of
    its own; the main checkout's installed packages answer for it."""
    if root not in _MAIN_ROOT:
        import subprocess
        main = ""
        try:
            common = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], cwd=root,
                                    capture_output=True, text=True, timeout=10).stdout.strip()
            top = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=root,
                                 capture_output=True, text=True, timeout=10).stdout.strip()
            if common and os.path.basename(common) == ".git":
                cand = os.path.dirname(common)
                if top and os.path.realpath(cand) != os.path.realpath(top):
                    main = cand
        except (OSError, subprocess.TimeoutExpired):
            pass
        _MAIN_ROOT[root] = main
    return _MAIN_ROOT[root]


def _installed(root: str, mdir: str, sub: str) -> str:
    """The directory holding a manifest's installed packages (`sub`: deps,
    node_modules): its own, else the main checkout's for the same manifest
    (a worktree), else "" (not installed anywhere: names can't be resolved)."""
    own = os.path.join(mdir, sub)
    if os.path.isdir(own) and os.listdir(own):
        return own
    main = _main_checkout(root)
    if main:
        other = os.path.join(main, os.path.relpath(mdir, root), sub)
        if os.path.isdir(other) and os.listdir(other):
            return other
    return ""


# blanked code of files the project already parsed (analyze fills it)
_CODE: dict[str, str] = {}


def _where(rel: str) -> str:
    if _TEST_PATH.search(rel):
        return "tests"
    if _CONFIG_PATH.search(rel):
        return "config"
    return "code"


def _add(dep: Dep, rel: str, line: int, where: str | None = None) -> None:
    # judged inside the manifest's project: a sub-project under deploy/ has code
    where = where or _where(os.path.relpath(rel, os.path.dirname(dep.manifest) or "."))
    getattr(dep, where).append((rel, line))


# --- mix ------------------------------------------------------------------------

_MIX_DEP = re.compile(r"\{\s*:(\w+)\s*,([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}")
_EX_MODREF = re.compile(r"(?<![\w.])([A-Z]\w*(?:\.[A-Z]\w*)*)")
_EX_ATOM = re.compile(r"(?<![\w:]):(\w+)\b")


def _mix(root: str, mrel: str, exclude) -> list[Dep]:
    mdir = os.path.dirname(os.path.join(root, mrel))
    text = _read(root, mrel)
    m = re.search(r"defp?\s+deps\b.*?\bdo\b(.*?)^\s{2}end\b", text, re.S | re.M)
    if not m:
        return []
    deps: dict[str, Dep] = {}
    for d in _MIX_DEP.finditer(m.group(1)):
        name, opts = d.group(1), d.group(2)
        scope = "runtime"
        only = re.search(r"only:\s*(\[[^\]]*\]|:\w+)", opts)
        if only and ":prod" not in only.group(1):
            scope = "test" if re.fullmatch(r"\[?\s*:test\s*\]?", only.group(1).strip()) else "dev"
        if re.search(r"runtime:\s*false", opts):
            scope = "tool" if scope == "runtime" else scope
        deps[name] = Dep("mix", name, mrel, _line_of(text, m.start(1) + d.start()), scope)
    ddir = os.environ.get("MIX_DEPS_PATH") or _installed(root, mdir, "deps")
    if not ddir:
        for dep in deps.values():
            dep.uninstalled = True
        ddir = os.path.join(mdir, "deps")
    # module namespaces each dep defines (the shortest ones), and erlang modules
    ns_of: dict[str, str] = {}
    erl_of: dict[str, str] = {}
    for name, dep in deps.items():
        mods = set()
        for f in glob.glob(os.path.join(ddir, name, "lib", "**", "*.ex"), recursive=True):
            mods |= set(re.findall(r"^\s*defmodule\s+([A-Z][\w.]*)", _readp(f), re.M))
        for f in glob.glob(os.path.join(ddir, name, "src", "*.erl")):
            erl_of[os.path.basename(f)[:-4]] = name
        if not mods and not any(v == name for v in erl_of.values()):
            dep.guessed = True
            mods = {"".join(p.capitalize() for p in name.split("_"))}
            erl_of.setdefault(name, name)
        for mod in mods:
            if not any(mod.startswith(o + ".") for o in mods):
                ns_of.setdefault(mod, name)
    # who else needs it: every fetched package's mix.exs, declared or not (a
    # path dep's dev tools are fetched too: sagents -> credo -> file_system).
    # An optional dep must be declared to be there (postgrex for ecto_sql); a
    # plain one comes along by itself; a package's `only:` deps usually don't.
    for f in glob.glob(os.path.join(ddir, "*", "mix.exs")):
        other = os.path.basename(os.path.dirname(f))
        for need, opts in re.findall(r"\{\s*:(\w+)\s*,([^}]*)\}", _readp(f)):
            if need in deps and need != other:
                if "optional: true" in opts:
                    deps[need].needed_by.append(other)
                elif "only:" not in opts:
                    deps[need].via.append(other)
    for rel in _walk(root, mdir, (".ex", ".exs", ".heex", ".eex", ".leex"), "mix.exs", exclude):
        raw = _read(root, rel)
        code = _CODE.get(rel) or blank(raw, "elixir")[0]
        lof = _liner(code)
        where = _where(os.path.relpath(rel, os.path.dirname(mrel) or "."))
        seen: set[str] = set()
        for r in _EX_MODREF.finditer(code):
            ref = r.group(1)
            parts = ref.split(".")
            for k in range(len(parts), 0, -1):
                dn = ns_of.get(".".join(parts[:k]))
                if dn:
                    if (dn, where) not in seen or where == "code":
                        _add(deps[dn], rel, lof(r.start()), where)
                        seen.add((dn, where))
                    break
        for r in _EX_ATOM.finditer(code if where != "config" else raw):
            a = r.group(1)
            dn = erl_of.get(a) if code[r.end():r.end() + 1] == "." else None
            if dn is None and where == "config" and a in deps and rel != mrel:
                dn = a  # `config :tailwind, ...`, `compilers: [:phoenix_live_view]`
            if dn and (dn, where, "atom") not in seen:
                seen.add((dn, where, "atom"))
                _add(deps[dn], rel, lof(r.start()), where)
        if rel == mrel:  # the manifest's own aliases / compilers / listeners (atoms)
            body = code[:m.start()] + code[m.end():]
            for a in set(re.findall(r"(?<![\w:]):(\w+)", body)) & set(deps):
                _add(deps[a], rel, 1, "config")
    # assets: tailwind `@source "../../deps/heroicons"`, `@plugin`, JS `from "phoenix"` (mix-shipped JS)
    names = "|".join(sorted(map(re.escape, deps), key=len, reverse=True))
    asset = re.compile(rf"deps/({names})\b|[\"']({names})[\"'/]")
    for rel in _walk(root, mdir, (".css", ".js", ".ts", ".mjs"), "mix.exs|package.json", exclude):
        raw = _read(root, rel)
        for dn in {a or b for a, b in asset.findall(raw)}:
            _add(deps[dn], rel, 1, "config" if rel.endswith(".css") else None)
    # dev tools run as mix tasks: `mix dialyzer` in a script / CI step / alias
    runs = "\n".join(_read(root, r) for r in _walk(root, mdir, (".sh", ".yml", ".yaml", "Makefile", "justfile", "mix.exs"),
                                                    "mix.exs", exclude))
    for name, dep in deps.items():
        for t in glob.glob(os.path.join(ddir, name, "lib", "mix", "tasks", "*.ex")):
            task = os.path.basename(t)[:-3]
            if re.search(rf"(?:\bmix\s+|[\"']){re.escape(task)}\b", runs):
                _add(dep, mrel, dep.line, "config")
                break
    return list(deps.values())


# --- npm ------------------------------------------------------------------------

_JS_SPEC = re.compile(r"""(?:\bfrom\s*|\bimport\s*\(?\s*|\brequire\s*\(\s*|@(?:import|plugin|use|source|reference)\s+(?:url\()?)["']([^"'./][^"']*)["']""")


def _pkg_of(spec: str) -> str:
    if spec.startswith("node:"):
        return ""
    parts = spec.split("/")
    return "/".join(parts[:2]) if spec.startswith("@") else parts[0]


def _npm(root: str, mrel: str, exclude) -> list[Dep]:
    mdir = os.path.dirname(os.path.join(root, mrel))
    text = _read(root, mrel)
    try:
        pkg = json.loads(text)
    except ValueError:
        return []
    deps: dict[str, Dep] = {}
    for sect, scope in (("dependencies", "runtime"), ("devDependencies", "dev"), ("optionalDependencies", "runtime"),
                        ("peerDependencies", "runtime")):
        for name in (pkg.get(sect) or {}):
            pos = text.find(f'"{name}"')
            deps.setdefault(name, Dep("npm", name, mrel, _line_of(text, max(pos, 0)), scope))
    nm = _installed(root, mdir, "node_modules")
    if not nm:
        for dep in deps.values():
            dep.uninstalled = True
        nm = os.path.join(mdir, "node_modules")
    bins: dict[str, str] = {}
    for name, dep in deps.items():
        info = {}
        try:
            with open(os.path.join(nm, name, "package.json"), encoding="utf-8") as fh:
                info = json.load(fh)
        except (OSError, ValueError):
            dep.guessed = True
        b = info.get("bin")
        for bn in ([b and name.split("/")[-1]] if isinstance(b, str) else list((b or {}).keys())):
            if bn:
                bins[bn] = name
        for peer in (info.get("peerDependencies") or {}):
            if peer in deps and peer != name:
                deps[peer].needed_by.append(name)
    scripts = pkg.get("scripts") or {}
    for key, cmd in scripts.items():
        for word in re.findall(r"[\w@/.-]+", cmd):
            dn = bins.get(word) or (word if word in deps else None)
            if dn:
                deps[dn].config.append((mrel, _line_of(text, max(text.find(f'"{key}"'), 0))))
    # a tool with its own config file (stylelint.config.js, .prettierrc) is set up
    for fn in os.listdir(mdir):
        mt = re.match(r"\.?([\w-]+?)(?:\.config\.[cm]?[jt]s|rc(?:\.\w+)?)$", fn)
        dn = mt and (bins.get(mt.group(1)) or (mt.group(1) if mt.group(1) in deps else None))
        if dn:
            _add(deps[dn], os.path.relpath(os.path.join(mdir, fn), root), 1, "config")
    exts = (".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".vue", ".svelte", ".css", ".scss", ".html")
    for rel in _walk(root, mdir, exts, "package.json", exclude):
        raw = _read(root, rel)
        seen = set()
        for r in _JS_SPEC.finditer(raw):
            dn = _pkg_of(r.group(1))
            for cand in (dn, "@types/" + dn.lstrip("@").replace("/", "__") if dn else ""):
                if cand in deps and (cand, rel) not in seen:
                    seen.add((cand, rel))
                    _add(deps[cand], rel, _line_of(raw, r.start()))
    for rel in glob.glob(os.path.join(mdir, "tsconfig*.json")):
        raw = _readp(rel)
        for t in re.findall(r'"types"\s*:\s*\[([^\]]*)\]', raw):
            for nm_ in re.findall(r'"([^"]+)"', t):
                for cand in (nm_, "@types/" + nm_):
                    if cand in deps:
                        _add(deps[cand], os.path.relpath(rel, root), 1, "config")
    if "@types/node" in deps and not deps["@types/node"].code:
        if any(d.code for d in deps.values()):
            deps["@types/node"].config.append((mrel, deps["@types/node"].line))  # the toolchain's own types
    return list(deps.values())


# --- gradle ---------------------------------------------------------------------

_GRADLE_CONF = {"implementation": "runtime", "api": "runtime", "compileOnly": "runtime", "runtimeOnly": "runtime",
                "testImplementation": "test", "androidTestImplementation": "test", "testRuntimeOnly": "test",
                "debugImplementation": "dev", "kapt": "tool", "ksp": "tool", "annotationProcessor": "tool",
                "coreLibraryDesugaring": "tool", "lintChecks": "tool"}
_GRADLE_DEP = re.compile(r"\b(" + "|".join(_GRADLE_CONF) + r")\s*\(?\s*(platform\s*\(\s*)?(libs\.[\w.]+|[\"']([^\"':]+:[^\"':]+)(?::[^\"']*)?[\"'])")
_JVM_REF = re.compile(r"(?<![\w.])([a-z][\w]*(?:\.[\w]+){1,})")


def _catalog(root: str, start: str) -> dict[str, str]:
    """libs.versions.toml accessor ("libs.androidx.core.ktx") -> "group:artifact"."""
    d = start
    while True:
        p = os.path.join(d, "gradle", "libs.versions.toml")
        if os.path.exists(p):
            break
        if os.path.abspath(d) == os.path.abspath(root) or os.path.dirname(d) == d:
            return {}
        d = os.path.dirname(d)
    try:
        with open(p, "rb") as fh:
            cat = tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError):
        return {}
    out = {}
    for alias, v in (cat.get("libraries") or {}).items():
        if isinstance(v, str):
            ga = ":".join(v.split(":")[:2])
        elif "module" in v:
            ga = v["module"]
        else:
            ga = f"{v.get('group')}:{v.get('name')}"
        out["libs." + re.sub(r"[-_]", ".", alias)] = ga
    return out


def _jar_packages(group: str, artifact: str, plugin: list[str] | None = None) -> set[str]:
    """Packages a Gradle-cache jar / aar holds. `plugin` collects why it runs
    without being imported: META-INF/services, or components / startup
    initializers in the library's AndroidManifest."""
    base = os.path.expanduser(os.path.join("~", ".gradle", "caches", "modules-2", "files-2.1", group))
    pk: set[str] = set()
    plugin = plugin if plugin is not None else []
    for art in (artifact, artifact + "-android", artifact + "-jvm", artifact + "-desktop"):
        for f in glob.glob(os.path.join(base, art, "*", "*", f"{art}-*.jar")) + glob.glob(os.path.join(base, art, "*", "*", f"{art}-*.aar")):
            if f.endswith(("-sources.jar", "-javadoc.jar")):
                continue
            try:
                with zipfile.ZipFile(f) as z:
                    names = z.namelist()
                    if "AndroidManifest.xml" in names:
                        man = z.read("AndroidManifest.xml").decode("utf-8", "replace")
                        comp = re.findall(r"<(provider|service|receiver|activity)\b", man)
                        if comp:
                            plugin.append("its manifest declares " + ", ".join(sorted(set(comp))))
                    if f.endswith(".aar") and "classes.jar" in names:
                        import io
                        with zipfile.ZipFile(io.BytesIO(z.read("classes.jar"))) as cz:
                            names = cz.namelist()
                    svc = [n.rsplit("/", 1)[-1] for n in names if n.startswith("META-INF/services/") and not n.endswith("/")]
                    if svc:
                        plugin.append("provides " + ", ".join(svc[:2]))
                    for n in names:
                        if n.endswith(".class") and "/" in n and not n.startswith("META-INF"):
                            pk.add(n.rsplit("/", 1)[0].replace("/", "."))
            except (OSError, zipfile.BadZipFile):
                continue
            if pk:
                return pk
    return pk


def _class_refs(path: str) -> set[str]:
    """Class names (a/b/C) the classes in a jar / aar refer to."""
    import io
    out: set[str] = set()
    try:
        with zipfile.ZipFile(path) as z:
            if path.endswith(".aar"):
                if "classes.jar" not in z.namelist():
                    return out
                z = zipfile.ZipFile(io.BytesIO(z.read("classes.jar")))
            for n in z.namelist():
                if n.endswith(".class"):
                    out.update(m.decode() for m in re.findall(rb"(?<![\w/])([a-z]\w*(?:/[\w$]+){2,})", z.read(n)))
    except (OSError, zipfile.BadZipFile):
        pass
    return out


def _gradle(root: str, mrel: str, exclude) -> list[Dep]:
    mdir = os.path.dirname(os.path.join(root, mrel))
    text = _read(root, mrel)
    cat = _catalog(root, mdir)
    deps: dict[str, Dep] = {}
    for m in _GRADLE_DEP.finditer(blank(text, "kotlin", keep_strings=True)[0]):
        conf, plat, ref, lit = m.group(1), m.group(2), m.group(3), m.group(4)
        ga = lit or cat.get(ref.rstrip("."))
        if not ga:
            continue
        scope = "bom" if plat else _GRADLE_CONF[conf]
        if ga not in deps or deps[ga].scope != "runtime":
            deps[ga] = Dep("gradle", ga, mrel, _line_of(text, m.start()), scope)
    pkg_index: dict[str, set[str]] = {}
    for ga, dep in deps.items():
        if dep.scope == "bom":
            continue
        group, _, art = ga.partition(":")
        why: list[str] = []
        pks = _jar_packages(group, art, why)
        dep.plugin = "; ".join(dict.fromkeys(why))
        if not pks:
            dep.guessed = True
            pks = {group}
        for p in pks:
            pkg_index.setdefault(p, set()).add(ga)
    # what each declared artifact's POM brings along (optional ones must be declared)
    for ga, dep in deps.items():
        group, _, art = ga.partition(":")
        base = os.path.expanduser(os.path.join("~", ".gradle", "caches", "modules-2", "files-2.1", group))
        for art_ in (art, art + "-android", art + "-jvm"):
            poms = sorted(glob.glob(os.path.join(base, art_, "*", "*", "*.pom")))
            if not poms:
                continue
            for d in re.findall(r"<dependency>(.*?)</dependency>", _readp(poms[-1]), re.S):
                g = re.search(r"<groupId>\s*([^<\s]+)", d)
                a = re.search(r"<artifactId>\s*([^<\s]+)", d)
                if not (g and a):
                    continue
                need = f"{g.group(1)}:{a.group(1)}"
                if need in deps and need != ga and not re.search(r"<scope>\s*(test|provided)", d):
                    (deps[need].needed_by if "<optional>true" in d else deps[need].via).append(ga)
            break
    # local jars / aars (`files("libs/x.aar")`) are compiled code: their class
    # files name the packages they need (Spotify auth -> androidx.browser)
    for fm in re.finditer(r"files\s*\(\s*[\"']([^\"']+\.(?:aar|jar))[\"']", text):
        lib = os.path.normpath(os.path.join(os.path.relpath(mdir, root), fm.group(1)))
        for ref in _class_refs(os.path.join(root, lib)):
            parts = ref.split("/")
            for k in range(len(parts) - 1, 0, -1):
                hit = pkg_index.get(".".join(parts[:k]))
                if hit:
                    for ga in hit:
                        if (lib, 0) not in deps[ga].code:
                            deps[ga].code.append((lib, 0))
                    break
    exts = (".kt", ".java", ".kts", ".xml", ".pro")
    for rel in _walk(root, mdir, exts, "build.gradle.kts|build.gradle", exclude):
        if rel == mrel:
            continue
        raw = _read(root, rel)
        code = raw if rel.endswith((".xml", ".pro")) else (_CODE.get(rel) or blank(raw, "kotlin")[0])
        lof = _liner(code)
        seen = set()
        for r in _JVM_REF.finditer(code):
            parts = r.group(1).split(".")
            for k in range(len(parts) - 1, 0, -1):
                hit = pkg_index.get(".".join(parts[:k]))
                if hit:
                    for ga in hit:
                        if ga not in seen:
                            seen.add(ga)
                            _add(deps[ga], rel, lof(r.start()))
                    break
    return [d for d in deps.values() if d.scope != "bom"]


# --- cargo ----------------------------------------------------------------------

def _cargo_lib_name(crate: str, path: str | None) -> str:
    """A crate's `[lib] name` when it differs (rustdesk -> librustdesk), from its
    source: a path dependency, or the cargo registry / git checkouts."""
    home = os.environ.get("CARGO_HOME") or os.path.expanduser("~/.cargo")
    cands = [os.path.join(path, "Cargo.toml")] if path else []
    cands += glob.glob(os.path.join(home, "registry", "src", "*", f"{crate}-[0-9]*", "Cargo.toml"))
    cands += glob.glob(os.path.join(home, "git", "checkouts", "*", "*", "Cargo.toml"))
    for c in cands:
        try:
            with open(c, "rb") as fh:
                man = tomllib.load(fh)
        except (OSError, tomllib.TOMLDecodeError):
            continue
        if (man.get("package") or {}).get("name") == crate:
            return ((man.get("lib") or {}).get("name") or "").replace("-", "_")
    return ""


def _cargo(root: str, mrel: str, exclude) -> list[Dep]:
    mdir = os.path.dirname(os.path.join(root, mrel))
    text = _read(root, mrel)
    try:
        man = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return []
    tables = [(man.get("dependencies") or {}, "runtime"), (man.get("dev-dependencies") or {}, "test"),
              (man.get("build-dependencies") or {}, "tool")]
    for t in (man.get("target") or {}).values():
        tables.append((t.get("dependencies") or {}, "runtime"))
    deps: dict[str, Dep] = {}
    crate_of: dict[str, str] = {}
    for tbl, scope in tables:
        for name in tbl:
            pos = re.search(rf"(?m)^\s*{re.escape(name)}\s*=|\[[\w.-]*dependencies\.{re.escape(name)}\]", text)
            deps.setdefault(name, Dep("cargo", name, mrel, _line_of(text, pos.start()) if pos else 1, scope))
            crate_of[name.replace("-", "_")] = name
            spec = tbl[name] if isinstance(tbl[name], dict) else {}
            real = spec.get("package", name)
            lib = _cargo_lib_name(real, spec.get("path") and os.path.join(mdir, spec["path"]))
            if lib:
                crate_of[lib] = name
    for rel in _walk(root, mdir, (".rs",), "Cargo.toml", exclude):
        code = blank(_read(root, rel), "rust")[0]
        seen = set()
        for r in re.finditer(r"(?<![\w:])(\w+)\s*::|\bextern\s+crate\s+(\w+)|\buse\s+(\w+)\s*;", code):
            dn = crate_of.get(r.group(1) or r.group(2) or r.group(3))
            if dn and dn not in seen:
                seen.add(dn)
                _add(deps[dn], rel, _line_of(code, r.start()))
    return list(deps.values())


# --- python ---------------------------------------------------------------------

_PY_ALIAS = {"pyyaml": ["yaml"], "beautifulsoup4": ["bs4"], "pillow": ["PIL"], "scikit-learn": ["sklearn"],
             "opencv-python": ["cv2"], "opencv-python-headless": ["cv2"], "python-dateutil": ["dateutil"],
             "protobuf": ["google.protobuf"], "attrs": ["attr", "attrs"], "python-dotenv": ["dotenv"],
             "typing-extensions": ["typing_extensions"], "pyjwt": ["jwt"], "msgpack-python": ["msgpack"],
             "stashapp-tools": ["stashapi"]}


def _norm(n: str) -> str:
    return re.sub(r"[-_.]+", "-", n).lower()


def _py_top_levels() -> dict[str, list[str]]:
    out: dict[str, list[str]] = {}
    for sp in sys.path + glob.glob(".venv/lib/python*/site-packages"):
        for di in glob.glob(os.path.join(sp, "*.dist-info")):
            name = _norm(os.path.basename(di).rsplit("-", 1)[0])
            tl = os.path.join(di, "top_level.txt")
            if os.path.exists(tl):
                out.setdefault(name, _readp(tl).split())
    return out


def _python(root: str, mrel: str, exclude) -> list[Dep]:
    mdir = os.path.dirname(os.path.join(root, mrel))
    text = _read(root, mrel)
    reqs: list[tuple[str, str, int]] = []
    if mrel.endswith(".toml"):
        try:
            man = tomllib.loads(text)
        except tomllib.TOMLDecodeError:
            return []
        proj = man.get("project") or {}
        for s in proj.get("dependencies") or []:
            reqs.append((s, "runtime", 0))
        for grp in (proj.get("optional-dependencies") or {}).values():
            reqs += [(s, "dev", 0) for s in grp]
        for grp in (man.get("dependency-groups") or {}).values():
            reqs += [(s, "dev", 0) for s in grp if isinstance(s, str)]
        poetry = (man.get("tool") or {}).get("poetry") or {}
        reqs += [(n, "runtime", 0) for n in (poetry.get("dependencies") or {}) if n != "python"]
        reqs += [(n, "dev", 0) for n in (poetry.get("dev-dependencies") or {})]
    else:
        scope = "dev" if re.search(r"dev|test", os.path.basename(mrel)) else "runtime"
        for i, ln in enumerate(text.split("\n")):
            ln = ln.split("#", 1)[0].strip()
            if ln and not ln.startswith("-"):
                reqs.append((ln, scope, i + 1))
    tops = _py_top_levels()
    deps: dict[str, Dep] = {}
    mod_of: dict[str, str] = {}
    for spec, scope, line in reqs:
        name = re.match(r"[A-Za-z0-9_.-]+", spec.strip())
        if not name:
            continue
        name = name.group(0)
        if not line:
            pos = text.find(name)
            line = _line_of(text, max(pos, 0))
        d = deps.setdefault(_norm(name), Dep("python", name, mrel, line, scope))
        mods = tops.get(_norm(name)) or _PY_ALIAS.get(_norm(name))
        if not mods:
            d.guessed = True
            mods = [name.replace("-", "_").lower()]
        for mo in mods:
            mod_of[mo] = _norm(name)
    for rel in _walk(root, mdir, (".py",), "pyproject.toml|setup.py", exclude):
        code = blank(_read(root, rel), "python")[0]
        seen = set()
        for r in re.finditer(r"(?m)^\s*(?:from\s+([\w.]+)\s+import|import\s+([\w., ]+))", code):
            for mo in ([r.group(1)] if r.group(1) else [x.strip().split(" ")[0] for x in r.group(2).split(",")]):
                parts = mo.split(".")
                for k in range(len(parts), 0, -1):
                    dn = mod_of.get(".".join(parts[:k]))
                    if dn and dn not in seen:
                        seen.add(dn)
                        _add(deps[dn], rel, _line_of(code, r.start()))
                        break
    # tools (pytest, ruff, mypy...): named in config / CI / scripts
    for rel in _walk(root, mdir, (".toml", ".ini", ".cfg", ".yml", ".yaml", ".sh", "Makefile", "justfile"),
                     "pyproject.toml|setup.py", exclude):
        raw = _read(root, rel)
        for dn, d in deps.items():
            if not d.code and re.search(rf"(?<![\w-]){re.escape(d.name)}(?![\w-])", raw) and rel != mrel:
                _add(d, rel, 1, "config")
        if rel == mrel:
            for dn, d in deps.items():
                if re.search(rf"\[tool\.{re.escape(d.name)}\b", raw):
                    _add(d, rel, 1, "config")
    return list(deps.values())


# --- go -------------------------------------------------------------------------

def _gomod(root: str, mrel: str, exclude) -> list[Dep]:
    mdir = os.path.dirname(os.path.join(root, mrel))
    text = _read(root, mrel)
    deps: dict[str, Dep] = {}
    for m in re.finditer(r"(?m)^\s*(?:require\s+)?([\w.-]+\.[\w./-]+)\s+v[\w.+-]+(\s*//\s*indirect)?\s*$", text):
        if not m.group(2):
            deps[m.group(1)] = Dep("go", m.group(1), mrel, _line_of(text, m.start()))
    for rel in _walk(root, mdir, (".go",), "go.mod", exclude):
        raw = _read(root, rel)
        for r in re.finditer(r'"([\w.-]+\.[\w./-]+)"', raw):
            path = r.group(1)
            for name, d in deps.items():
                if path == name or path.startswith(name + "/"):
                    _add(d, rel, _line_of(raw, r.start()))
    return list(deps.values())


_MANIFESTS = {"mix.exs": _mix, "package.json": _npm, "build.gradle.kts": _gradle, "build.gradle": _gradle,
              "Cargo.toml": _cargo, "pyproject.toml": _python, "go.mod": _gomod}


def find_manifests(root: str, exclude) -> list[str]:
    out = []
    for dirpath, dirs, files in os.walk(root):
        dirs[:] = [x for x in dirs if x not in _PRUNE and not x.startswith(".")]
        for fn in files:
            if fn in _MANIFESTS or re.fullmatch(r"requirements[\w-]*\.txt", fn):
                rel = os.path.relpath(os.path.join(dirpath, fn), root)
                if not exclude(rel):
                    out.append(rel)
    return sorted(out)


def analyze(project) -> list[Dep]:
    """Every declared dependency with its status (see the module doc)."""
    from . import core
    root = project.root
    excl = project.config.get("project", {}).get("exclude", [])
    exclude = (lambda rel: core.match_any(rel, excl)) if excl else (lambda rel: False)
    ignore = set(project.config.get("deps", {}).get("ignore", []))
    dead_ids = {f.id for f in core.dead(project)}
    _CODE.clear()
    for rel, mods in project.by_file.items():
        _CODE[rel] = "\n".join(mods[0].code)
    out: list[Dep] = []
    for mrel in find_manifests(root, exclude):
        fn = _MANIFESTS.get(os.path.basename(mrel), _python)
        for d in fn(root, mrel, exclude):
            if d.name in ignore:
                continue
            out.append(d)
    for d in out:
        if d.code:
            # all code uses inside functions nothing calls: the dep goes with them
            funcs = set()
            live = False
            for rel, line in d.code:
                fs = [m.func_at(line) for m in project.by_file.get(rel, [])]
                f = next((x for x in fs if x and x.kind != "top"), None)
                if f is None or f.id not in dead_ids:
                    live = True
                    break
                funcs.add(f.id)
            d.status = "used" if live else "dead-only"
            d.dead = sorted(funcs) if not live else []
        elif d.tests:
            d.status = "test-only" if d.scope == "runtime" else "used"
        elif d.config:
            d.status = "config"
        elif d.needed_by:
            d.status = "peer"
        elif d.plugin:
            d.status = "plugin"
        else:
            d.status = "unused"
        if d.uninstalled and d.guessed and d.status in ACTIONABLE:
            # its real module / bin names are unknown: "unused" would be a guess
            d.status = "uninstalled"
    return out


ACTIONABLE = ("unused", "test-only", "dead-only")


def report(deps: list[Dep], show_all: bool, name: str | None = None) -> str:
    lines = []
    if name:
        for d in deps:
            if d.name == name or d.name.endswith(":" + name):
                lines.append(f"{d.name}  ({d.eco}, {d.scope}, {d.manifest}:{d.line})  status: {d.status}"
                             + ("  (name guessed)" if d.guessed else ""))
                for kind in ("code", "tests", "config"):
                    for rel, ln in getattr(d, kind):
                        lines.append(f"  {kind:<6} {rel}:{ln}")
                for n in d.needed_by:
                    lines.append(f"  needed by {n}")
        return "\n".join(lines) or f"no dependency named {name!r}"
    cur = None
    for d in sorted(deps, key=lambda d: (d.manifest, ACTIONABLE.index(d.status) if d.status in ACTIONABLE else 9, d.name)):
        if not show_all and d.status not in ACTIONABLE:
            continue
        if d.manifest != cur:
            cur = d.manifest
            n = [x for x in deps if x.manifest == cur]
            lines.append(f"\n{cur}  ({len(n)} deps: " + ", ".join(
                f"{sum(1 for x in n if x.status == s)} {s}" for s in ("used", *ACTIONABLE, "peer", "plugin", "config")
                if any(x.status == s for x in n)) + ")")
        extra = ""
        if d.status == "test-only":
            extra = f"only tests use it ({len({f for f, _ in d.tests})} files), declared {d.scope}"
        elif d.status == "dead-only":
            extra = "only in uncalled code: " + ", ".join(d.dead[:4])
        elif d.status == "peer":
            extra = "needed by " + ", ".join(d.needed_by[:4])
        elif d.status == "config":
            extra = "only " + ", ".join(sorted({f for f, _ in d.config})[:3])
        elif d.status == "used":
            fs = d.files
            extra = f"{len(fs)} file{'s' if len(fs) != 1 else ''}" + (f": {fs[0]}" if len(fs) == 1 else "")
        elif d.status == "plugin":
            extra = d.plugin
        elif d.status == "unused":
            extra = "no reference in code, tests, config or scripts" + (
                f" (and {', '.join(d.via[:3])} pull{'s' if len(d.via) == 1 else ''} it in anyway)" if d.via else "")
        lines.append(f"  {d.status:<9} {d.name:<40} {extra}" + ("  (name guessed)" if d.guessed else ""))
    return "\n".join(lines).lstrip("\n")
