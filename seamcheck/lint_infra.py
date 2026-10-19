"""Lints across deploy scripts, containers and services: the seams between a
program and the machines it runs on. Every check here names the bug it was
written after (Veridian, the night of 2026-10-06, when the hub moved hosts).

  shell-reexec-watch   a long-running script that re-execs itself when its
                       inputs change must watch every file it sources (the
                       agent never reloaded a nodes.env-only deploy, and
                       watched node-$NODE.sh while sourcing
                       node-${ADAPTER:-$NODE}.sh, a file that never existed)
  shell-name-domain    a node/host name used as a shell variable name, an
                       identifier, or filtered through a character class
                       that drops "-" ("pylon-primos" became "pylonprimos",
                       its ${name}_IP lookup came back empty)
  home-paths-mounted   a path under $HOME the code reads or writes that no
                       volume of the container service covers (Forms uploads,
                       the voice pack and the speech cache landed in the
                       container and died on recreate)
  auth-in-background   a timer / handle_info / handle_continue that reaches a
                       login or reconnect function: an unprompted auth
                       challenge on the user's phone at a random time
  stale-module-ref     a doc or comment naming one of the project's own
                       modules that no longer exists (ten comments still
                       described the removed Robinhood connector)
  lockfile-sync        package.json and package-lock.json disagree on a
                       declared range (`npm ci` refuses on a fresh node)
  path-unit-oneshot    a systemd .path unit triggering a Type=oneshot service:
                       changes that land while it runs are dropped, so rapid
                       writes end on the first one, not the last
  css-braces           an unclosed { in a stylesheet (a merge dropped one)
  toolchain-pin        a script's rustup/cargo toolchain differs from the
                       rust-toolchain.toml pin (1.75 vs 1.75.0)
  test-reads-home      a test that reads ~ passes only on one machine
  shell-pkill-self     pkill -f inside the command string it matches kills
                       its own shell (2026-10-19)
"""
from __future__ import annotations

import json
import os
import re

# --- shared ---------------------------------------------------------------------


def _line(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def _shell(rel: str, raw: str) -> bool:
    if rel.endswith((".sh", ".bash")):
        return True
    first = raw.split("\n", 1)[0]
    return first.startswith("#!") and re.search(r"\b(ba|z|da)?sh\b", first) is not None


def _read_text(path: str) -> str:
    with open(path, encoding="utf-8") as fh:
        return fh.read()


def _norm_path(p: str) -> str:
    return p.strip().strip('"').strip("'")


# --- shell-reexec-watch -----------------------------------------------------------

_SOURCE = re.compile(r"""(?:^|&&|\|\||;|\bthen|\bdo)\s*(?:\.|source)\s+("[^"]+"|'[^']+'|[^\s;&|]+)""", re.M)
_REEXEC = re.compile(r"""\bexec\s+("?\$(?:self|0|\{0\})"?|"?\$\(readlink -f "?\$0"?\)"?)""")


def shell_reexec_watch(rel, raw, code, o, project=None):
    if not _shell(rel, raw) or not _REEXEC.search(raw):
        return []
    # the stamp: the stat / sha / mtime expression the loop compares
    stamp_lines = [m for m in re.finditer(r"^.*\b(?:stat|sha1sum|sha256sum|md5sum|cksum)\b.*$", raw, re.M)]
    if not stamp_lines:
        return []
    watched_text = "\n".join(m.group(0) for m in stamp_lines)
    watched = {_norm_path(p) for p in re.findall(r"""("\$[^"]+"|\$\{?\w+\}?/[\w.${}:/-]+)""", watched_text)}
    out = []
    sourced = set()
    for m in _SOURCE.finditer(raw):
        p = _norm_path(m.group(1))
        if not p.startswith(("$", "/")) or p.startswith("/dev/") or p.startswith("/etc/"):
            continue  # a system file (/etc/os-release) is not a deploy input
        var = re.match(r"\$\{?(\w+)\}?$", p)
        if var and re.search(rf"""^\s*(?:readonly\s+|export\s+)?{var.group(1)}=["']?(?:\$\{{\w+:-)?/etc/""", raw, re.M):
            continue  # host config under /etc: changed by an operator, who restarts the unit
        sourced.add(p)
        if p not in watched and p not in watched_text:
            out.append((_line(raw, m.start()),
                        f"sources {p} but the re-exec stamp doesn't watch it: a deploy that only changes {os.path.basename(p)} "
                        f"never reaches the running process. Add it to the stamp's stat list."))
    for p in sorted(watched):
        base = os.path.basename(p)
        if not base.endswith((".sh", ".env", ".conf")) or p in sourced:
            continue
        # watched like a sourced file, but sourced under another name (node-$NODE.sh vs node-${ADAPTER:-$NODE}.sh)
        stem = re.split(r"\$", base, maxsplit=1)[0]
        twins = [s for s in sourced if os.path.basename(s).startswith(stem) and s != p] if stem else []
        if twins:
            ln = next((_line(raw, m.start()) for m in stamp_lines if p in m.group(0)), 1)
            out.append((ln, f"the stamp watches {p} but the script sources {twins[0]}: the watched path is not the "
                            f"file that runs (it may not exist for every node). Use the same expression in both places."))
    return out


# --- shell-name-domain ------------------------------------------------------------

_DEFAULT_NAMES = r"(?i)^(?:node|name|peer|host|target|to|who|other|partner|n|nd)$"


def shell_name_domain(rel, raw, code, o, project=None):
    if not _shell(rel, raw):
        return []
    names = re.compile(o.get("names", _DEFAULT_NAMES))
    out = []
    lines = raw.split("\n")
    # names converted somewhere in the file (${x//-/_}, tr - _): those uses are fine
    converted = set(re.findall(r"\$\{(\w+)//-/_\}", raw)) | set(re.findall(r"(\w+)=\$\(\s*(?:echo|printf[^|]*)\s+\"?\$\{?\w+\}?\"?\s*\|\s*tr\s+['\"]?-['\"]?\s+['\"]?_", raw))
    for i, ln in enumerate(lines, 1):
        s = ln.split("#", 1)[0] if not ln.lstrip().startswith("#!") else ""
        # 1. indirect expansion through the name: eval "\${${name}_IP}", ${!x} built from a name
        for m in re.finditer(r"\\\$\{\$\{?(\w+)\}?_\w+(?::?-[^}]*)?\}", s):
            v = m.group(1)
            if names.match(v) and v not in converted:
                out.append((i, f"${{{v}}} becomes part of a shell variable name: a name with '-' (pylon-primos) makes an "
                               f"invalid variable and the lookup comes back empty. Use ${{{v}//-/_}} for the key."))
        # 2. a filter that drops '-' from a name (tr -dc 'a-z', a [a-z0-9.] class)
        for m in re.finditer(r"\btr\s+-d?c?d?\s+(['\"])([^'\"]+)\1", s):
            flags, chars = m.group(0), m.group(2)
            if ("-dc" in flags or "-cd" in flags) and re.search(r"(?i)node|peer|handoff|request|target|host|\bto\b|\bwho\b", s):
                if not re.search(r"(^|[^a-z])-|-$|\\-", chars) and re.search(r"a-z", chars):
                    out.append((i, f"`tr -dc '{chars}'` strips '-' from what it filters: a node named pylon-primos reads "
                                   f"as pylonprimos and matches nothing. Allow '-' in the set."))
        # 3. a name spliced into an identifier with '_' (slot veridian_$NODE)
        for m in re.finditer(r"\b[a-z]+_\$\{?(\w+)\}?\b", s):
            v = m.group(1)
            if names.match(v) and v not in converted and re.search(r"slot|replication|_slot|create_physical|primary_slot", s, re.I):
                out.append((i, f"identifier built from ${v}: a name with '-' is not a valid Postgres/shell identifier. "
                               f"Convert with ${{{v}//-/_}}."))
    return out


# --- home-paths-mounted -----------------------------------------------------------

_HOME_LIT = re.compile(r'"~/([^"#{}]+)"')
_HOME_JOIN = re.compile(r'System\.user_home!?\(\)\s*,\s*((?:"[^"]+"\s*,?\s*)+)')
_COMPOSE_CACHE: dict = {}


def _compose_targets(root: str, pattern: str, service: str) -> list[str] | None:
    key = (root, pattern, service)
    if key in _COMPOSE_CACHE:
        return _COMPOSE_CACHE[key]
    from .core import list_files, match_any
    targets = None
    for rel in list_files(root, [pattern], []):
        try:
            text = _read_text(os.path.join(root, rel))
        except OSError:
            continue
        m = re.search(rf"^(\s+){re.escape(service)}:\s*$", text, re.M)
        if not m:
            continue
        ind = len(m.group(1))
        block = []
        for ln in text[m.end():].split("\n")[1:]:
            if ln.strip() and (len(ln) - len(ln.lstrip())) <= ind:
                break
            block.append(ln)
        targets = targets or []
        in_vol = False
        for ln in block:
            if re.match(r"^\s+volumes:\s*$", ln):
                in_vol = True
                continue
            if in_vol:
                vm = re.match(r"""^\s+-\s+["']?([^"':]+):([^"':]+)""", ln)
                if vm:
                    targets.append(vm.group(2).rstrip("/"))
                elif ln.strip() and not ln.strip().startswith("#"):
                    in_vol = False
    _COMPOSE_CACHE[key] = targets
    return targets


def home_paths_mounted(rel, raw, code, o, project=None):
    if not rel.endswith((".ex", ".exs")) or project is None:
        return []
    service = o.get("service", "hub")
    targets = _compose_targets(project.root, o.get("compose", "**/compose.yml"), service)
    if targets is None:
        return []
    # the container's home: `home`, else the one home its volumes mount into
    homes = {m.group(1) for t in targets if (m := re.match(r"^(/home/[^/]+|/root)(?:/|$)", t))}
    home = o.get("home") or (homes.pop() if len(homes) == 1 else os.path.expanduser("~"))
    home = home.rstrip("/")
    allow = [re.compile(a) for a in o.get("allow", [])]
    out = []
    hits = [(m.start(), m.group(1)) for m in _HOME_LIT.finditer(raw)]
    for m in _HOME_JOIN.finditer(raw):
        parts = re.findall(r'"([^"]+)"', m.group(1))
        hits.append((m.start(), "/".join(parts)))
    seen = set()
    for pos, sub in hits:
        sub = sub.strip("/")
        if not sub or sub in seen or any(a.search(sub) for a in allow):
            continue
        seen.add(sub)
        full = f"{home}/{sub}"
        if any(full == t or full.startswith(t + "/") or t.startswith(full + "/") for t in targets):
            continue  # covered, or a parent the code narrows further (can't judge statically)
        out.append((_line(raw, pos), f"~/{sub} is not under any volume of the `{service}` container: on a node that runs "
                                     f"the hub in a container it is missing, and anything written there is lost on "
                                     f"recreate. Mount it (and sync it between nodes), or list it in `allow` if it is "
                                     f"host-only by design."))
    return out


# --- auth-in-background -----------------------------------------------------------

_AUTH_NAME = re.compile(r"^(?:login|log_in|sign_in|signin|authenticate|reauth\w*|reconnect\w*|relogin|refresh_login)$")


def auth_in_background(rel, raw, code, o, project=None):
    if not rel.endswith(".ex") or project is None:
        return []
    from . import core
    auth = re.compile(o.get("auth", _AUTH_NAME.pattern))
    roots = re.compile(o.get("roots", r"^(?:handle_info|handle_continue|init)$"))
    depth = int(o.get("depth", 3))
    out = []
    for mod in project.by_file.get(rel, []):
        for f in mod.funcs:
            if not roots.match(f.name):
                continue
            reach = core.reach(project, f.id, depth, exact_only=True)
            bad = sorted(fid for fid, d in reach.items() if d and fid in project.funcs and auth.match(project.funcs[fid].name))
            if bad:
                out.append((f.start, f"{f.name} reaches {bad[0]}: a timer or message can start an auth/login without the "
                                     f"user asking (an approval prompt at a random time). Renew silently (refresh token) "
                                     f"and leave login to an explicit user action."))
    return out


# --- stale-module-ref -------------------------------------------------------------


_STALE_INDEX: dict = {}  # (project, namespaces) -> (module-name prefixes, reference regex): built once per run


def stale_module_ref(rel, raw, code, o, project=None):
    if not rel.endswith((".ex", ".exs")) or project is None:
        return []
    key = (id(project), tuple(o.get("namespaces") or ()))
    if key not in _STALE_INDEX:
        roots = o.get("namespaces")
        if not roots:
            tops = {}
            for name in project.modules:
                top = name.split(".")[0]
                tops[top] = tops.get(top, 0) + 1
            roots = [t for t, n in tops.items() if n >= 5]
        prefixes = {".".join(n.split(".")[:k]) for n in project.modules for k in range(1, n.count(".") + 2)}
        rx = re.compile(r"(?<![\w.])((?:" + "|".join(map(re.escape, roots)) + r")(?:\.[A-Z]\w*)+)") if roots else None
        _STALE_INDEX.clear()
        _STALE_INDEX[key] = (prefixes, rx)
    prefixes, rx = _STALE_INDEX[key]
    if rx is None:
        return []
    out = []
    for m in rx.finditer(raw):
        if m.start() < len(code) and code[m.start():m.end()] == raw[m.start():m.end()]:
            continue  # in code (not a doc or comment): the compiler judges it
        ref = m.group(1)
        if ref in prefixes:
            continue
        line = raw[raw.rfind("\n", 0, m.start()) + 1:raw.find("\n", m.end()) if raw.find("\n", m.end()) >= 0 else len(raw)]
        if re.search(r"(?i)\b(removed|retired|replaced|deleted|gone|formerly|renamed|was|no longer|used to)\b", line):
            continue  # history, told as history
        out.append((_line(raw, m.start()), f"{ref} is named in a doc or comment but no such module exists any more: "
                                           f"rewrite the text for what replaced it (a stale doc misleads the next reader "
                                           f"and every agent)."))
    return out


# --- lockfile-sync ----------------------------------------------------------------


def lockfile_sync(rel, raw, code, o, project=None):
    if os.path.basename(rel) != "package-lock.json" or project is None:
        return []
    try:
        lock = json.loads(raw)
        pkg = json.loads(_read_text(os.path.join(project.root, os.path.dirname(rel), "package.json")))
    except (OSError, ValueError):
        return []
    root = (lock.get("packages") or {}).get("") or {}
    out = []
    for sect in ("dependencies", "devDependencies", "optionalDependencies", "peerDependencies"):
        want, have = pkg.get(sect) or {}, root.get(sect) or {}
        for name, rng in want.items():
            if have.get(name) != rng:
                out.append((1, f"package.json {sect}.{name} is {rng!r} but package-lock.json records "
                               f"{have.get(name)!r}: `npm ci` refuses on a fresh node. Run `npm install` and commit the lock."))
        for name in set(have) - set(want):
            out.append((1, f"package-lock.json still records {sect}.{name}, which package.json no longer declares: "
                           f"run `npm install` and commit the lock."))
    return out


# --- path-unit-oneshot ------------------------------------------------------------


def path_unit_oneshot(rel, raw, code, o, project=None):
    if not rel.endswith(".path") or project is None:
        return []
    m = re.search(r"^\s*Unit\s*=\s*(\S+)", raw, re.M)
    svc = m.group(1) if m else os.path.basename(rel)[:-5] + ".service"
    path = os.path.join(project.root, os.path.dirname(rel), svc)
    try:
        body = _read_text(path)
    except OSError:
        return []
    if not re.search(r"^\s*Type\s*=\s*oneshot\b", body, re.M):
        return []
    if re.search(o.get("settles", r"(?i)settle|debounce|re-?read|until .* stable|converge"), body):
        return []
    trig = next((mm for mm in re.finditer(r"^\s*Path(?:Changed|Modified)\s*=", raw, re.M)), None)
    return [(_line(raw, trig.start()) if trig else 1,
             f"triggers {svc} (Type=oneshot): a change that lands while it runs is dropped, so rapid writes end on the "
             f"first one. Have the service re-read until the input is stable (and say so in a comment: settle/re-read).")]


# --- css-braces ---------------------------------------------------------------------


def css_braces(rel, raw, code, o, project=None):
    """Every `{` closed: a merge that dropped one closing brace in surface.css
    swallowed the rest of the sheet and unstyled Surface (51cfea01f)."""
    if not rel.endswith((".css", ".scss")):
        return []
    text = re.sub(r"/\*.*?\*/", lambda m: re.sub(r"[^\n]", " ", m.group(0)), raw, flags=re.S)
    text = re.sub(r"\"(?:\\.|[^\"\\\n])*\"|'(?:\\.|[^'\\\n])*'", lambda m: " " * len(m.group(0)), text)
    depth, opens = 0, []
    for i, ch in enumerate(text):
        if ch == "{":
            depth += 1
            opens.append(i)
        elif ch == "}":
            depth -= 1
            if depth < 0:
                return [(_line(text, i), "a `}` closes nothing: a block above lost its opening brace (a bad merge?)")]
            opens.pop()
    if depth:
        return [(_line(text, opens[-1]), f"{depth} `{{` never closed (from this block on): a merge dropped a closing brace "
                                         f"and the rest of the sheet is swallowed into this rule")]
    return []


# --- toolchain-pin --------------------------------------------------------------------


def toolchain_pin(rel, raw, code, o, project=None):
    """`rustup run X` / `cargo +X` must name the exact channel the crate's
    rust-toolchain.toml pins: build_rd_core.sh ran `1.75` while the crate
    pinned `1.75.0`, so a node with only the pinned one failed mid-publish."""
    if project is None:
        return []
    pins = {}
    from . import core
    for t in core.list_files(project.root, ["**/rust-toolchain.toml", "**/rust-toolchain"], core.DEFAULT_EXCLUDE):
        if os.path.basename(t) in ("rust-toolchain.toml", "rust-toolchain"):
            try:
                body = _read_text(os.path.join(project.root, t))
            except OSError:
                continue
            m = re.search(r'channel\s*=\s*"([^"]+)"', body) or re.match(r"\s*(\S+)\s*$", body)
            if m:
                pins[os.path.dirname(t)] = m.group(1)
    if not pins:
        return []
    out = []
    for m in re.finditer(r"rustup\s+run\s+([\w.\-]+)|\bcargo\s+\+([\w.\-]+)", code):
        used = m.group(1) or m.group(2)
        if used.startswith("$"):
            continue
        # the crate this script builds: the pinned dir it names, else the only pin
        named = [d for d in pins if d and d in raw]
        want = {pins[d] for d in (named or (list(pins) if len(pins) == 1 else []))}
        if want and used not in want:
            out.append((_line(code, m.start()),
                        f"runs toolchain {used!r} but {sorted(want)[0]!r} is pinned in rust-toolchain.toml: "
                        f"two names for one toolchain, and a build node with only the pinned one fails"))
    return out


# --- test-reads-home ----------------------------------------------------------------


def test_reads_home(rel, raw, code, o, project=None):
    """A test reading the home directory passes only on the machine that has
    those files (video.stageable read ~/.local/share/elixir-pot/...; it failed
    on Pylon Primos): fixtures go in the repo or a tmp dir the test makes."""
    pat = re.compile(o.get("pattern", r'Path\.expand\(\s*"~|System\.user_home|os\.homedir\(\)|expanduser\(|'
                                      r'getenv\(\s*["\']HOME["\']\)|System\.get_env\(\s*"HOME"'))
    # raw text: the tell is inside a string ("~/...") that `code` blanks out
    return [(_line(raw, m.start()), "a test reading the home directory passes only on the machine that has those "
                                    "files: put the fixture in the repo or a temp dir")
            for m in pat.finditer(raw) if not raw[raw.rfind("\n", 0, m.start()) + 1:m.start()].lstrip().startswith(("#", "//"))]


# --- shell-pkill-self -------------------------------------------------------------

# a command string another shell runs: `ssh host "..."`, `bash -lc '...'`
_INNER = re.compile(r"""\b(?:ssh\b[^\n"'|;&]*?|(?:ba|z|da)?sh\s+-\w*c\s+)(["'])(.*?)(?<!\\)\1""", re.S)
_PKILL = re.compile(r"""\bp(?:kill|grep)\s+(?:-\w+\s+)*-\w*f\w*\s+(?:\\?(["'])(.+?)\\?\1|([^\s;|&"']+))""")


def shell_pkill_self(rel, raw, code, o, project=None):
    """`pkill -f PATTERN` matches every command line containing PATTERN, and
    the shell that runs it is one of them when PATTERN is in its own command
    line: an `ssh host "pkill -f x; start x"` or `bash -c "..."` string, or a
    script whose own path holds PATTERN. The shell kills itself (exit 143/144)
    and nothing after it runs (2026-10-19: the TV laptop's restart killed its
    own shell and the game server never came back)."""
    if not _shell(rel, raw):
        return []
    out = []
    own = os.path.basename(rel)
    for m in _INNER.finditer(raw):
        for k in _PKILL.finditer(m.group(2)):
            pat = k.group(2) or k.group(3)
            if not pat.startswith(("[", "$")):
                out.append((_line(raw, m.start(2) + k.start()),
                            f"pkill/pgrep -f {pat!r} runs inside a command string that contains {pat!r}: it "
                            f"matches the shell running it, which dies before the rest runs. Write the pattern "
                            f"as '[{pat[0]}]{pat[1:]}', or use a pid file."))
    for k in _PKILL.finditer(raw):
        pat = k.group(2) or k.group(3)
        if len(pat) >= 3 and not pat.startswith(("[", "$")) and pat in own:
            out.append((_line(raw, k.start()), f"pkill/pgrep -f {pat!r} matches this script's own command line "
                                               f"(bash {own}): it kills itself. Write '[{pat[0]}]{pat[1:]}'."))
    return sorted(set(out))


LINTS = {
    "shell-reexec-watch": shell_reexec_watch,
    "shell-name-domain": shell_name_domain,
    "home-paths-mounted": home_paths_mounted,
    "auth-in-background": auth_in_background,
    "stale-module-ref": stale_module_ref,
    "lockfile-sync": lockfile_sync,
    "path-unit-oneshot": path_unit_oneshot,
    "css-braces": css_braces,
    "toolchain-pin": toolchain_pin,
    "test-reads-home": test_reads_home,
    "shell-pkill-self": shell_pkill_self,
}
DEFAULT_IN = {
    "shell-reexec-watch": ["**/*.sh", "deploy/**", "scripts/**"],
    "shell-name-domain": ["**/*.sh", "deploy/**", "scripts/**"],
    "home-paths-mounted": ["lib/**/*.ex"],
    "auth-in-background": ["lib/**/*.ex"],
    "stale-module-ref": ["lib/**/*.ex"],
    "lockfile-sync": ["**/package-lock.json"],
    "path-unit-oneshot": ["**/*.path"],
    "css-braces": ["**/*.css", "**/*.scss"],
    "toolchain-pin": ["scripts/**", "**/*.sh", "**/build.gradle.kts"],
    "test-reads-home": ["test/**/*.exs", "**/*.test.mjs", "**/*.test.ts", "**/src/test/**/*.kt"],
    "shell-pkill-self": ["**/*.sh", "deploy/**", "scripts/**", "bin/**", "tools/**"],
}
OPTIONS = {
    "shell-reexec-watch": set(),
    "shell-name-domain": {"names"},
    "home-paths-mounted": {"compose", "service", "home", "allow"},
    "auth-in-background": {"auth", "roots", "depth"},
    "stale-module-ref": {"namespaces"},
    "lockfile-sync": set(),
    "path-unit-oneshot": {"settles"},
    "css-braces": set(),
    "toolchain-pin": set(),
    "test-reads-home": {"pattern"},
    "shell-pkill-self": set(),
}
# a pure function of the one file (plus a sibling named here): in a gate only the
# files the change touches can change their answer
PER_FILE = {"test-reads-home", "shell-pkill-self", "css-braces", "shell-reexec-watch", "shell-name-domain", "lockfile-sync", "path-unit-oneshot", "stale-module-ref"}
# these read files outside `[project] include` (scripts, units, lockfiles)
REPO_WIDE = {"shell-pkill-self", "shell-reexec-watch", "shell-name-domain", "lockfile-sync", "path-unit-oneshot", "toolchain-pin"}
