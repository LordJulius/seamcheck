"""Precise mode: a language server settles the calls the static resolver can't.

seamcheck's own resolver reads names. A call on a receiver whose type it can't
infer is a name guess (every function of that name), and some calls reach
nothing though a project function has the name. With `--precise` (or
`[precise] enabled = true`) those calls go to the language's server as
definition requests: an answer inside the project makes the call one exact
edge; an answer outside it (the standard library, a dependency) makes the
guesses nothing. A call the server can't answer stays as it was, so precise
mode only ever replaces a guess with what the server knows.

Any LSP server over stdio works, per file extension (`[precise] servers`).
Built in, used when found: pyright (.py), rust-analyzer (.rs), gopls (.go)
and TypeScript's tsserver (.ts/.tsx/.js..., its own protocol, not LSP).
Answers are cached by the asking file's content, so a warm run asks nothing.
A missing or crashing server is a warning on stderr, never a failed run.
"""
from __future__ import annotations

import collections
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
import urllib.parse

from . import core
from . import fingerprint as fp

_TS = (".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs")
DEFAULTS = {".py": ["pyright-langserver", "--stdio"], ".rs": ["rust-analyzer"], ".go": ["gopls"],
            **{e: ["tsserver"] for e in _TS}}
LANG_ID = {".py": "python", ".rs": "rust", ".go": "go", ".ts": "typescript", ".mts": "typescript",
           ".cts": "typescript", ".tsx": "typescriptreact", ".js": "javascript", ".mjs": "javascript",
           ".cjs": "javascript", ".jsx": "javascriptreact", ".c": "c", ".h": "cpp", ".hpp": "cpp", ".cc": "cpp",
           ".cpp": "cpp", ".cxx": "cpp", ".cs": "csharp", ".java": "java", ".kt": "kotlin", ".kts": "kotlin",
           ".swift": "swift", ".ex": "elixir", ".exs": "elixir", ".gd": "gdscript", ".rb": "ruby", ".php": "php"}
WINDOW = 256  # requests in flight per server: enough to keep it busy, few enough to stay responsive
WAIT = 300.0  # seconds a server may take to load the workspace before its first answer
TIMEOUT = 60.0  # seconds without any answer before a busy server is given up on
# One process per server. Splitting a server's files across several made
# nothing faster (httpx: one pyright 10.4 s, four 13.0 s; excalidraw: one
# tsserver 30 s, three 33 s): each process analyses the same imports again.
_WORD = re.compile(r"[A-Za-z_$][\w$]*")
RETRY = "retry"  # an answer to ask again: the server was busy changing its mind (rust-analyzer while it loads)
_BUSY = {-32801, -32800, -32802}  # ContentModified, RequestCancelled, ServerCancelled


# --- framing: LSP and tsserver's output are both Content-Length framed --------

def frame(obj) -> bytes:
    body = json.dumps(obj, separators=(",", ":")).encode()
    return b"Content-Length: %d\r\n\r\n" % len(body) + body


def read_message(stream):
    """The next framed message, {} for a body that isn't a JSON object, None at EOF.
    tsserver ends each body with a newline the length may not count: a blank
    line before the headers is skipped."""
    length = None
    while True:
        line = stream.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            if length is not None:
                break
            continue
        k, _, v = line.partition(b":")
        if k.strip().lower() == b"content-length":
            length = int(v.strip())
    body = stream.read(length)
    if len(body) < length:
        return None
    try:
        msg = json.loads(body)
    except ValueError:
        return {}
    return msg if isinstance(msg, dict) else {}


class Dead(Exception):
    """The server exited, or went quiet for longer than it may."""


class _Conn:
    """A server process, a thread reading its messages into a queue and one
    keeping the tail of its stderr (for the warning when it dies)."""

    def __init__(self, argv: list[str], cwd: str, env: dict | None = None, on_request=None):
        # (in our process group: a Ctrl-C ends the servers with the run)
        self.proc = subprocess.Popen(argv, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, env=env)
        self.inbox: queue.Queue = queue.Queue()
        self.err: collections.deque = collections.deque(maxlen=3)
        self.lock = threading.Lock()
        self.on_request = on_request
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._drain, daemon=True).start()

    def _read(self) -> None:
        try:
            while (msg := read_message(self.proc.stdout)) is not None:
                if self.on_request and "method" in msg and "id" in msg:
                    self.on_request(msg)  # a server's own request: answered here, so it never waits on us
                else:
                    self.inbox.put(msg)
        except (OSError, ValueError):
            pass
        self.inbox.put(None)

    def _drain(self) -> None:
        try:
            for line in self.proc.stderr:
                if line.strip():
                    self.err.append(line.decode(errors="replace").strip()[:200])
        except (OSError, ValueError):
            pass

    def send(self, data: bytes) -> None:
        try:
            with self.lock:
                self.proc.stdin.write(data)
                self.proc.stdin.flush()
        except (OSError, ValueError) as e:
            raise Dead(f"exited ({self.why()})") from e

    def poll(self, timeout: float):
        """The next message, or None when none came in time."""
        try:
            msg = self.inbox.get(timeout=max(0.01, timeout))
        except queue.Empty:
            return None
        if msg is None:
            self.inbox.put(None)
            raise Dead(f"exited ({self.why()})")
        return msg

    def why(self) -> str:
        code = self.proc.poll()
        tail = self.err[-1] if self.err else ""
        return f"code {code}" + (f": {tail}" if tail else "") if code is not None else tail or "closed its output"

    def kill(self) -> None:
        """After a goodbye: a moment to exit on its own, then killed."""
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        for pipe in (self.proc.stdout, self.proc.stderr):
            try:
                pipe.close()
            except OSError:
                pass


# --- positions --------------------------------------------------------------------

def to_u16(line: str, col: int) -> int:
    """A character column as LSP counts it (UTF-16 code units)."""
    return col if line.isascii() else len(line[:col].encode("utf-16-le")) // 2


def from_u16(line: str, u: int) -> int:
    if line.isascii():
        return u
    n = 0
    for i, ch in enumerate(line):
        if n >= u:
            return i
        n += 2 if ord(ch) > 0xFFFF else 1
    return len(line)


def _column(text: str, name: str, col: int) -> int:
    """Where the called name starts on its line: the adapter's column when it
    holds the name, else the name's one occurrence (-1: none or several)."""
    if col >= 0 and text[col:col + len(name)] == name:
        return col
    hits = [m.start() for m in re.finditer(rf"(?<![\w$]){re.escape(name)}(?![\w$])", text)]
    return hits[0] if len(hits) == 1 else -1


# --- the clients ------------------------------------------------------------------

class Lsp:
    """textDocument/definition from any LSP server over stdio."""

    def __init__(self, argv: list[str], root: str, env=None, init_options=None):
        self.root, self.next_id = root, 0
        self.status: bool | None = None  # rust-analyzer's "quiescent", once it reports it
        self.active: set = set()  # $/progress tokens begun and not yet ended
        self.started = time.time()
        self.conn = _Conn(argv, root, env, self._answer_request)
        self.init_options = init_options

    def _answer_request(self, msg: dict) -> None:
        # workspace/configuration wants one value per item; everything else
        # (progress tokens, capability registration) a plain null acknowledges
        items = msg.get("params", {}).get("items") if msg.get("method") == "workspace/configuration" else None
        try:
            self.conn.send(frame({"jsonrpc": "2.0", "id": msg["id"], "result": [None] * len(items) if items else None}))
        except Dead:
            pass

    def request(self, method: str, params) -> int:
        self.next_id += 1
        self.conn.send(frame({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}))
        return self.next_id

    def notify(self, method: str, params) -> None:
        self.conn.send(frame({"jsonrpc": "2.0", "method": method, "params": params}))

    def uri(self, rel: str) -> str:
        return "file://" + urllib.parse.quote(os.path.join(self.root, rel))

    def start(self, wait: float) -> None:
        caps = {"textDocument": {"definition": {"linkSupport": True}, "synchronization": {}},
                "window": {"workDoneProgress": True}, "workspace": {"configuration": True},
                "experimental": {"serverStatusNotification": True}}
        rid = self.request("initialize", {"processId": os.getpid(), "rootUri": self.uri(""), "rootPath": self.root,
                                          "workspaceFolders": [{"uri": self.uri(""), "name": "root"}],
                                          "capabilities": caps, "initializationOptions": self.init_options})
        deadline = time.time() + wait
        while True:
            msg = self.conn.poll(deadline - time.time())
            if msg is None:
                raise Dead(f"no answer to initialize in {int(wait)} s")
            if msg.get("id") == rid:
                break
            self.answer(msg)
        if "error" in msg:
            raise Dead(f"refused to initialize: {msg['error'].get('message', '')[:120]}")
        self.notify("initialized", {})

    def open(self, rel: str, text: str) -> None:
        ext = os.path.splitext(rel)[1]
        self.notify("textDocument/didOpen", {"textDocument": {"uri": self.uri(rel), "languageId": LANG_ID.get(ext, ext[1:]),
                                                              "version": 1, "text": text}})

    def close(self, rel: str) -> None:
        self.notify("textDocument/didClose", {"textDocument": {"uri": self.uri(rel)}})

    def ask(self, rel: str, line0: int, u16: int) -> int:
        return self.request("textDocument/definition", {"textDocument": {"uri": self.uri(rel)},
                                                        "position": {"line": line0, "character": u16}})

    def answer(self, msg: dict):
        """(request id, [(abs path, line0, u16)] or None for an error) for a
        response; None for a notification (which may say the server is loaded)."""
        if "id" not in msg:
            self._progress(msg)
            return None
        if "error" in msg:
            return msg["id"], RETRY if msg["error"].get("code") in _BUSY else None
        res = msg.get("result") or []
        out = []
        for loc in res if isinstance(res, list) else [res]:
            uri = loc.get("targetUri") or loc.get("uri", "")
            rng = loc.get("targetSelectionRange") or loc.get("range") or {}
            if uri.startswith("file://"):
                start = rng.get("start", {})
                out.append((urllib.parse.unquote(urllib.parse.urlparse(uri).path), start.get("line", 0), start.get("character", 0)))
        return msg["id"], out

    def _progress(self, msg: dict) -> None:
        # rust-analyzer says when its workspace is loaded (experimental/serverStatus);
        # other servers report work as $/progress begin ... end
        p = msg.get("params") or {}
        if msg.get("method") == "experimental/serverStatus":
            self.status = bool(p.get("quiescent"))
        elif msg.get("method") == "$/progress":
            kind = (p.get("value") or {}).get("kind")
            if kind == "begin":
                self.active.add(str(p.get("token")))
            elif kind == "end":
                self.active.discard(str(p.get("token")))

    def loaded(self) -> bool:
        """Done loading the workspace, as far as the server says: its status
        when it reports one, else no progress under way after a grace period."""
        if self.status is not None:
            return self.status
        return not self.active and time.time() - self.started > 10

    def stop(self) -> None:
        try:
            rid = self.request("shutdown", None)
            deadline = time.time() + 3
            while (msg := self.conn.poll(deadline - time.time())) is not None and msg.get("id") != rid:
                pass
        except Dead:
            pass
        try:
            self.notify("exit", None)
        except Dead:
            pass
        self.conn.kill()


class TsServer:
    """TypeScript's tsserver: JSON lines in, Content-Length framed out, 1-based
    line and offset; `definition` answers once the file's project is loaded."""

    def __init__(self, argv: list[str], root: str):
        self.root, self.seq, self.answered = root, 0, False
        self.conn = _Conn(argv, root)

    def _send(self, command: str, args: dict) -> int:
        self.seq += 1
        self.conn.send(json.dumps({"seq": self.seq, "type": "request", "command": command, "arguments": args}).encode() + b"\n")
        return self.seq

    def start(self, wait: float) -> None:
        self._send("configure", {"preferences": {"includePackageJsonAutoImports": "off"}})

    def open(self, rel: str, text: str) -> None:
        kind = {".tsx": "TSX", ".jsx": "JSX", ".js": "JS", ".mjs": "JS", ".cjs": "JS"}.get(os.path.splitext(rel)[1], "TS")
        self._send("open", {"file": os.path.join(self.root, rel), "fileContent": text, "scriptKindName": kind,
                            "projectRootPath": self.root})

    def close(self, rel: str) -> None:
        self._send("close", {"file": os.path.join(self.root, rel)})

    def ask(self, rel: str, line0: int, u16: int) -> int:
        return self._send("definition", {"file": os.path.join(self.root, rel), "line": line0 + 1, "offset": u16 + 1})

    def answer(self, msg: dict):
        if msg.get("type") != "response" or msg.get("command") != "definition":
            return None
        self.answered = True
        if not msg.get("success"):
            # "No content available": nothing is defined there; anything else is an error
            return msg.get("request_seq"), [] if "No content" in str(msg.get("message")) else None
        return msg.get("request_seq"), [(d["file"], d["start"]["line"] - 1, d["start"]["offset"] - 1)
                                        for d in msg.get("body") or [] if d.get("file")]

    def loaded(self) -> bool:
        return self.answered  # (it answers only once the file's project is loaded)

    def stop(self) -> None:
        try:
            self._send("exit", {})
            self.conn.proc.wait(timeout=3)
        except (Dead, subprocess.TimeoutExpired):
            pass
        self.conn.kill()


# --- which server for which file --------------------------------------------------

def _which(exe: str) -> str | None:
    hit = shutil.which(exe)
    if hit or os.sep in exe:
        return hit
    if exe == "gopls":  # `go install` puts it in GOPATH/bin, often not on PATH
        for d in (os.environ.get("GOBIN", ""), os.path.join(os.environ.get("GOPATH", os.path.expanduser("~/go")), "bin")):
            if d and os.access(os.path.join(d, exe), os.X_OK):
                return os.path.join(d, exe)
    if exe == "tsserver" and (tsc := shutil.which("tsc")):  # the typescript package's own lib/tsserver.js
        js = os.path.join(os.path.dirname(os.path.dirname(os.path.realpath(tsc))), "lib", "tsserver.js")
        if os.path.exists(js) and (node := shutil.which("node")):
            return f"{node}\0{js}"
    return None


def servers(config: dict) -> tuple[dict[str, tuple[str, ...]], list[str]]:
    """ext -> the command to run, for each server that is installed, and a
    warning for each one the project configured that isn't. A built-in
    default that isn't installed is simply not used."""
    conf = (config.get("precise") or {}).get("servers") or {}
    out, warn = {}, []
    for ext, argv in {**DEFAULTS, **conf}.items():
        if not argv:
            continue  # `".js" = []`: that language stays static
        argv = [argv] if isinstance(argv, str) else list(argv)
        exe = _which(argv[0])
        if exe:
            out[ext] = tuple(exe.split("\0") + argv[1:])
        elif ext in conf:
            warn.append(f"precise: {argv[0]} ({ext}) is not installed")
    return out, warn


def key(config: dict) -> list:
    """What the graph of a precise run depends on besides the files: which servers ran."""
    return sorted(servers(config)[0].items())


def _is_ts(argv) -> bool:
    return any(os.path.basename(a).startswith("tsserver") for a in argv[:2])


def _name(argv) -> str:
    return "tsserver" if _is_ts(argv) else os.path.basename(argv[0])


def _client(argv: list[str], root: str):
    """A client for argv. The built-in servers get the options that keep them
    from fetching, building or checking anything: precise mode only asks
    where names are defined."""
    name = _name(argv)
    default = name in {_name(d) for d in DEFAULTS.values()}
    if _is_ts(argv):
        return TsServer(argv + (["--disableAutomaticTypingAcquisition", "--suppressDiagnosticEvents"] if default else []), root)
    env, opts = None, None
    if default and name == "gopls":
        env = dict(os.environ, GOPROXY="off")  # modules already downloaded only
    if default and name == "rust-analyzer":
        env = dict(os.environ, CARGO_NET_OFFLINE="true")
        opts = {"cargo": {"buildScripts": {"enable": False}, "extraEnv": {"CARGO_NET_OFFLINE": "true"}},
                "procMacro": {"enable": False}, "checkOnSave": False, "cachePriming": {"enable": False},
                "diagnostics": {"enable": False}}
        linked = _rust_project(root)
        if linked:
            opts["linkedProjects"] = [linked]
    return Lsp(argv, root, env, opts)


def _rust_project(root: str) -> dict | None:
    """When cargo can't resolve the workspace offline (its dependencies were
    never fetched), rust-analyzer would load nothing. The workspace's own
    crates, described from `cargo metadata --no-deps`, still let it follow
    every call between them (and into std): a rust-project.json, inline."""
    def meta(*extra):
        try:
            r = subprocess.run(["cargo", "metadata", "--format-version", "1", "--offline", *extra], cwd=root,
                               capture_output=True, text=True, timeout=120, env=dict(os.environ, CARGO_NET_OFFLINE="true"))
        except (OSError, subprocess.TimeoutExpired):
            return None
        return json.loads(r.stdout) if r.returncode == 0 else None
    if not os.path.exists(os.path.join(root, "Cargo.toml")) or meta() is not None:
        return None  # resolvable (or not cargo at all): rust-analyzer loads it the usual way
    m = meta("--no-deps")
    if not m:
        return None
    crates, lib = [], {}
    for pkg in m["packages"]:
        for t in pkg["targets"]:
            if {"lib", "rlib", "proc-macro", "bin"} & set(t["kind"]):
                if "bin" not in t["kind"]:
                    lib[pkg["name"]] = len(crates)
                crates.append((pkg, t))
    out = []
    for pkg, t in crates:
        deps = [{"crate": lib[d["name"]], "name": (d.get("rename") or d["name"]).replace("-", "_")}
                for d in pkg["dependencies"] if d["name"] in lib and lib[d["name"]] != len(out)]
        if "bin" in t["kind"] and pkg["name"] in lib:
            deps.append({"crate": lib[pkg["name"]], "name": pkg["name"].replace("-", "_")})
        out.append({"display_name": t["name"].replace("-", "_"), "root_module": t["src_path"], "edition": t.get("edition") or "2021",
                    "deps": deps, "is_workspace_member": True,
                    "cfg": ["test", "debug_assertions"] + [f'feature="{f}"' for f in pkg.get("features", {})]})
    proj: dict = {"crates": out}
    try:
        sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True, text=True, timeout=30).stdout.strip()
        if sysroot:  # (std's sources come with rustup's rust-src component)
            proj["sysroot"] = sysroot
            src = os.path.join(sysroot, "lib", "rustlib", "src", "rust", "library")
            if os.path.isdir(src):
                proj["sysroot_src"] = src
    except (OSError, subprocess.TimeoutExpired):
        pass
    return proj


# --- asking -----------------------------------------------------------------------

def _drive(client, files: list, timeout: float, first: float, out: dict | None = None) -> dict:
    """Pipeline definition requests: open a file, send all its questions, keep
    up to WINDOW in flight, close a file once all of its are answered. A busy
    answer is asked again a second later, for up to `timeout`.
    files: [(rel, text, [(line0, u16, key)])] -> {key: [(path, line0, u16)] or None}."""
    out = {} if out is None else out
    pending, left, tries = {}, {}, collections.Counter()
    later: collections.deque = collections.deque()  # (when, rel, q): busy answers to ask again
    todo = iter(files)
    wait = first
    while True:
        while len(pending) < WINDOW and (nxt := next(todo, None)):
            rel, text, asks = nxt
            client.open(rel, text)
            left[rel] = len(asks)
            for q in asks:
                pending[client.ask(rel, q[0], q[1])] = (rel, q)
        while later and later[0][0] <= time.time():
            _, rel, q = later.popleft()
            pending[client.ask(rel, q[0], q[1])] = (rel, q)
        if not pending and not later:
            return out
        quiet = time.time()
        msg = client.conn.poll(min(wait, later[0][0] - quiet) if later else wait)
        if msg is None:
            if later and time.time() - quiet < wait:
                continue  # (only waited for a retry to come due)
            raise Dead(f"no answer in {int(wait)} s")
        got = client.answer(msg)
        if got is None or got[0] not in pending:
            continue
        wait = timeout  # (the first answer may wait on the workspace loading)
        rel, q = pending.pop(got[0])
        if got[1] is RETRY and tries[q] < timeout:
            tries[q] += 1
            later.append((time.time() + 1.0, rel, q))
            continue
        out[q[2]] = None if got[1] is RETRY else got[1]
        left[rel] -= 1
        if not left[rel]:
            client.close(rel)


def _ready(client, probes: list, wait: float) -> bool:
    """Wait for the server to know the workspace: ask calls the static resolver
    found for certain until one is answered (rust-analyzer answers nothing
    until it has loaded the crates). Once the server says it is done loading
    and a round still finds nothing, the probes are calls it can't follow."""
    if not probes:
        return False
    deadline = time.time() + wait
    while True:
        done = client.loaded()
        left = max(1.0, deadline - time.time())
        if any(_drive(client, probes, left, left).values()):
            break
        if done or time.time() >= deadline:
            return False
        _listen(client, 1.0)
    # one crate answering is not all of them (nor std): a server that reports
    # its status (rust-analyzer) is asked only once it says it is done
    while getattr(client, "status", None) is False and time.time() < deadline:
        _listen(client, 1.0)
    return True


def _listen(client, secs: float) -> None:
    """Read what the server says about its progress for a while."""
    end = time.time() + secs
    while time.time() < end and (msg := client.conn.poll(end - time.time())) is not None:
        client.answer(msg)


def _ask_server(argv, root, probes, files, wait, timeout) -> tuple[dict, bool, str]:
    """{key: locations} from one server, whether it was ready (so an empty
    answer means "no definition", worth caching), and a warning if it failed.
    Answers that came before a crash are kept."""
    try:
        client = _client(list(argv), root)
    except OSError as e:
        return {}, False, f"precise: {argv[0]} won't start: {e}"
    out: dict = {}
    try:
        client.start(wait)
        ok = _ready(client, probes, wait)
        return _drive(client, files, timeout, wait, out), ok, ""
    except Dead as e:
        return out, False, f"precise: {_name(argv)} {e}"
    finally:
        client.stop()


# --- settling the calls ------------------------------------------------------------

def _named(project, f, c) -> bool:
    fam = core._family(f.file)
    return any(g.kind != "top" and fam & core._family(g.file) for g in project.by_name.get(c.name, ()))


def _outcomes(project, trace: list):
    """(func, call, outcome) per call, from the trace _build_edges wrote in the
    same order it walked the calls (a Python class with no __init__ writes none)."""
    i = 0
    for f in core.walk_order(project):
        for c in f.calls:
            if i < len(trace) and trace[i][:4] == (f.file, c.line, c.name, c.chain):
                yield f, c, trace[i][4], trace[i][5]
                i += 1


def _target(project, rel: str, line: int, ident: str):
    """The project function defined at rel:line named ident: [func], [] for a
    class with no constructor of its own, None when nothing there is a function."""
    inside = [g for m in project.by_file.get(rel, []) for g in m.funcs if g.start <= line <= g.end and g.name == ident]
    if inside:
        return [max(inside, key=lambda g: g.start)]
    for m in project.by_file.get(rel, []):  # a class: `Foo()` / `new Foo()` builds one
        if (m.owner or m.kind == "class") and (m.name.rsplit(".", 1)[-1] == ident or ident in m.decls):
            for ctor in ("__init__", "constructor"):
                t = core._lookup(project, m.name, ctor)
                if t:
                    return [t]
            return []
    return None


def _settle(project, locs: list, files: set, name: str):
    """What a server's answer makes of a call: the project functions it names,
    "exact" for a project class with no constructor of its own, "lib" when
    all of it is outside the project, None when some of it isn't a function
    seamcheck knows (a variable, an interface's signature: stay static).
    An overload's signature beside the function it declares is that function.
    An answer at no name (tsserver points `f = () => {}` at its arrow) means
    the called name."""
    funcs, lib, klass, other = [], False, False, []
    for rel, line, ident in locs:
        if not rel or rel not in files:
            lib = True  # outside the root, or a file the project excludes (node_modules, .venv, target)
            continue
        ident = ident or name
        t = _target(project, rel, line, ident)
        if t is None:
            other.append((rel, ident))
        funcs += t or []
        klass = klass or t == []
    if any(not any(g.file == rel and g.name == ident for g in funcs) for rel, ident in other):
        return None
    return funcs or ("exact" if klass else "lib" if lib else None)


class _Lines:
    """The name an answer points at, from the file it points into (each read once)."""

    def __init__(self):
        self.cache: dict = {}

    def word(self, path: str, line0: int, u16: int) -> str:
        if path not in self.cache:
            try:
                with open(path, encoding="utf-8", errors="replace") as fh:
                    self.cache[path] = fh.read().split("\n")
            except OSError:
                self.cache[path] = []
        lines = self.cache[path]
        if not 0 <= line0 < len(lines):
            return ""
        m = _WORD.match(lines[line0], from_u16(lines[line0], u16))
        return m.group(0) if m else ""


def _questions(project, trace: list, found: dict, texts) -> tuple[dict, dict, collections.Counter]:
    """Per server: the calls to ask about (guesses, and nothing-though-named),
    a few calls the static resolver is sure of (to tell when it's ready), and
    how many guesses each (caller, line, name) holds."""
    asks, probes, guessed = collections.defaultdict(list), collections.defaultdict(list), collections.Counter()
    for f, c, outcome, targets in _outcomes(project, trace):
        if outcome == "guess":
            guessed[(f.id, c.line, c.name)] += 1
        argv = found.get(os.path.splitext(f.file)[1])
        if not argv or outcome not in ("guess", "named", "exact"):
            continue
        if outcome == "exact" and (len(targets) != 1 or len(probes[argv]) >= 3 or any(r == f.file for r, _ in probes[argv])):
            continue
        ls = texts(f.file)
        col = _column(ls[c.line - 1], c.name, c.col) if ls and 0 < c.line <= len(ls) else -1
        if col < 0:
            continue
        pos = (c.line - 1, to_u16(ls[c.line - 1], col))
        if outcome == "exact":
            probes[argv].append((f.file, pos))
        else:
            asks[argv].append((f, c, outcome, pos))
    return asks, probes, guessed


def _digests(root: str, rels) -> dict:
    out = {}
    for rel in rels:
        try:
            with open(os.path.join(root, rel), "rb") as fh:
                out[rel] = fp.digest_bytes(fh.read())
        except OSError:
            pass
    return out


def _ask_all(jobs: list, root: str, wait: float, timeout: float) -> list:
    """Every server at once, one thread each (a repo in three languages waits
    for the slowest server, not for the sum)."""
    results: list = []

    def run(argv, probes, batch):
        s0 = time.time()
        results.append((argv, *_ask_server(argv, root, probes, batch, wait, timeout), time.time() - s0,
                        sum(len(b[2]) for b in batch)))
    threads = [threading.Thread(target=run, args=j, daemon=True) for j in jobs]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return results


def refine(project, trace: list, use_cache: bool = True) -> None:
    """Replace the static outcome of every guessed call (and of every call that
    reached nothing though a project function has the name) with what the
    language servers say. Fills project.precise for `stats`."""
    t0 = time.time()
    cfg = project.config.get("precise") or {}
    wait, timeout = float(cfg.get("wait", WAIT)), float(cfg.get("timeout", TIMEOUT))
    found, warnings = servers(project.config)
    memo: dict = {}

    def texts(rel):
        if rel not in memo:
            try:
                memo[rel] = project.text(rel).split("\n")
            except OSError:
                memo[rel] = None
        return memo[rel]

    asks, probes, guessed = _questions(project, trace, found, texts)
    # answers are cached per file, per content digest, per (server, line, u16)
    cache_name = f"{fp.repo_key(project.root)}-precise.pkl"
    cache = fp.load(cache_name, {}) if use_cache else {}
    if not (isinstance(cache, dict) and cache.get("v") == 1):
        cache = {"v": 1, "files": {}}
    digests = _digests(project.root, {f.file for v in asks.values() for f, *_ in v})
    answers, jobs, cached = {}, [], 0
    for argv, todo in asks.items():
        sk = " ".join(argv)
        need = collections.defaultdict(set)
        for f, c, outcome, pos in todo:
            hit = cache["files"].get(f.file, {}).get(digests.get(f.file), {}).get((sk, *pos))
            if hit is not None:
                answers[(f.file, *pos, sk)] = hit
                cached += 1
            elif f.file in digests:
                need[f.file].add(pos)
        if need:
            batch = [(rel, "\n".join(texts(rel)), [(*pos, (rel, *pos, sk)) for pos in sorted(ps)])
                     for rel, ps in sorted(need.items())]
            pr = [(rel, "\n".join(texts(rel)), [(*pos, ("probe", rel, *pos))]) for rel, pos in probes[argv]]
            jobs.append((argv, pr, batch))

    words, root, per_server = _Lines(), project.root + os.sep, {}
    for argv, got, ready, warn, secs, n in _ask_all(jobs, project.root, wait, timeout):
        if warn:
            warnings.append(warn)
        sk, answered = " ".join(argv), 0
        for k, locs in got.items():
            if k[0] == "probe" or locs is None or (not locs and not ready):
                continue  # an error, or "nothing" from a server that may not have loaded yet
            answered += 1
            answers[k] = norm = [(p[len(root):] if p.startswith(root) else "", line0 + 1, words.word(p, line0, u))
                                 for p, line0, u in locs]
            slot = cache["files"].setdefault(k[0], {})
            slot.setdefault(digests[k[0]], {})[(sk, k[1], k[2])] = norm
            for old in list(slot)[:-2]:  # two versions per file: the main checkout's and a lane's
                del slot[old]
        per_server[_name(argv)] = (n, answered, round(secs, 1))
    if use_cache and jobs:
        fp.save(cache_name, cache)

    stats = collections.Counter()
    groups = collections.defaultdict(list)  # (caller, line, name) -> its guessed calls, which share their edges
    files = set(project.files)
    for argv, todo in asks.items():
        sk = " ".join(argv)
        for f, c, outcome, pos in todo:
            stats["asked"] += 1
            locs = answers.get((f.file, *pos, sk))
            verdict = _settle(project, locs, files, c.name) if locs else None
            if outcome == "guess":
                groups[(f.id, c.line, c.name)].append((f, c, verdict))
            else:
                _apply(project, f, c, outcome, verdict, stats)
    for k, calls in groups.items():
        # a guess's edges go only when every guess of that name on the line is settled
        if len(calls) == guessed[k] and all(v is not None for *_, v in calls):
            _drop_guesses(project, *k)
            for f, c, v in calls:
                _apply(project, f, c, "guess", v, stats)
    settled = stats["exact_guess"] + stats["exact_named"] + stats["lib"] + stats["confirmed"]
    project.precise = {"asked": stats["asked"], "cached": cached, "exact_from_guess": stats["exact_guess"],
                       "exact_from_nothing": stats["exact_named"], "library": stats["lib"], "confirmed": stats["confirmed"],
                       "unsettled": stats["asked"] - settled, "servers": per_server, "warnings": warnings,
                       "ms": int((time.time() - t0) * 1000)}
    for w in warnings:
        print(f"seamcheck: {w}", file=sys.stderr)


def _drop_guesses(project, fid: str, line: int, name: str) -> None:
    gone = [e for e in project.edges.get(fid, []) if e[1] == line and not e[2] and project.funcs[e[0]].name == name]
    if gone:
        project.edges[fid] = [e for e in project.edges[fid] if e not in gone]
        for tid in {e[0] for e in gone}:
            project.redges[tid] = [r for r in project.redges[tid] if not (r[0] == fid and r[1] == line and not r[2])]


def _apply(project, f, c, outcome: str, verdict, stats) -> None:
    """Move one call from guess / nothing to what the server said, keeping
    project.resolution's counts in step."""
    if verdict is None:
        return
    res = project.resolution[os.path.splitext(f.file)[1]]
    if verdict == "lib":
        if outcome == "guess":
            res[1] -= 1
            res[2] += 1
            res[3] += _named(project, f, c)
            stats["lib"] += 1
        stats["confirmed"] += outcome == "named"  # (it was right to reach nothing)
        return
    if outcome == "guess":
        res[1] -= 1
    else:
        res[2] -= 1
        res[3] -= 1
    res[0] += 1
    stats["exact_guess" if outcome == "guess" else "exact_named"] += 1
    targets = [] if verdict == "exact" else verdict
    # like the static resolver: a subclass's override may run too
    more = [o for t in targets for sub in project.children.get(t.module, ()) if (o := project.funcs.get(f"{sub}.{t.name}"))]
    for t in targets + more:
        project.edges[f.id].append((t.id, c.line, True))
        project.redges[t.id].append((f.id, c.line, True))


def summary(p: dict) -> str:
    """`stats --precise`: what the servers settled."""
    if not p:
        return ""
    srv = "; ".join(f"{k}: {n} asked, {a} answered, {s} s" for k, (n, a, s) in sorted(p["servers"].items()))
    out = (f"precise: {p['asked']} calls asked ({p['cached']} answered from cache): {p['exact_from_guess']} guesses "
           f"and {p['exact_from_nothing']} nothings made exact, {p['library']} guesses were library calls, "
           f"{p.get('confirmed', 0)} nothings confirmed, {p['unsettled']} unsettled  ({p['ms']} ms{'; ' + srv if srv else ''})")
    return "\n".join([out] + [f"  {w}" for w in p["warnings"]])
