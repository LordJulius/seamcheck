"""`seamcheck serve`: an MCP server on stdin / stdout.

An agent asks seamcheck many small questions in a row (who calls this, what
does that reach, is it clean now). Each CLI run paid a process start and a
load (0.1 s warm on a small repo, about 2 s on a 4,100-file one); the server
keeps the loaded Project and answers from it. Before each tool call it stats
the project's files and config (mtime + size, the stamps the load cache uses)
and reloads only when one changed, appeared or went: an edit between two
calls is always seen, an unchanged tree never reloads.

The protocol is MCP 2025-06-18: JSON-RPC 2.0, one message per line. The
server speaks initialize, ping, tools/list and tools/call; every tool answers
with one text content holding JSON, the shape the matching `--json` prints.
"""
from __future__ import annotations

import contextlib
import json
import sys

from . import __version__, config as config_mod, core, queries, tidy

PROTOCOL = "2025-06-18"

_DEPTH = {"type": "integer", "minimum": 1, "description": "how many calls deep to walk (default 2)"}
_FUZZY = {"type": "boolean", "description": "also follow name-guess edges (default: exact edges only)"}
_SINCE = {"type": "string", "description": "only files changed since this git ref (e.g. HEAD, origin/main)"}


def _schema(props: dict | None = None, required: tuple = ()) -> dict:
    return {"type": "object", "properties": props or {}, "required": list(required), "additionalProperties": False}


class ToolError(Exception):
    """A call the tool can't answer (no such function): an error result, not
    a protocol error, so the agent reads why."""


# --- the tools ------------------------------------------------------------------------

def _check(p, args):
    return queries.flat(p, queries.check(p, args.get("only"), args.get("skip"), args.get("errors_only", False)))


def _gate(p, args, extra):
    from . import tidy_cli
    ref = args.get("since") or "HEAD"
    return tidy_cli.gate_summary(p, tidy.gate(p, ref, args.get("staged", False), extra), ref)


def _walk(reverse: bool):
    def run(p, args):
        got = queries.walk(p, args["name"], args.get("depth", 2), reverse, args.get("fuzzy", False))
        if not got:
            raise ToolError(f"no function matches {args['name']!r}")
        return got
    return run


def _path(p, args):
    chain, through_guesses = queries.shortest(p, args["from"], args["to"], args.get("fuzzy", False))
    return {"path": [queries.where(p, fid) for fid in chain], "through_guesses_only": through_guesses}


def _impact(p, args):
    start = queries.target_functions(p, args.get("targets", []))
    if args.get("since"):
        start |= queries.changed_functions(p, args["since"], args.get("staged", False))
    if not start:
        raise ToolError("nothing matches: give targets (function specs or files) or since (a git ref)")
    by, own = queries.impact(p, start, args.get("depth", 4), args.get("fuzzy", False))
    return {"start": sorted(start), "own_areas": sorted(own),
            "areas": {k: [dict(queries.where(p, fid), depth=d) for d, fid in v] for k, v in by.items()}}


def _since(p, args, rows):
    if not args.get("since"):
        return rows
    changed = tidy.changed_files(p, args["since"])
    return [r for r in rows if r["file"] in changed]


TOOLS = {
    "check": ("Run the project's rules. Returns the findings: rule, severity, file, line, message, why, fix.",
              _schema({"only": {"type": "array", "items": {"type": "string"}, "description": "only these rule ids"},
                       "skip": {"type": "array", "items": {"type": "string"}, "description": "leave out these rule ids"},
                       "errors_only": {"type": "boolean"}}), _check),
    "gate": ("May the working tree's change land? Rule errors anywhere, plus dead code, copies and do-nothing tests "
             "in the lines it touched, and a warning when a touched language resolves worse than at the ref. "
             "Returns {blocked, changed_files, findings}.",
             _schema({"since": {"type": "string", "description": "compare against this git ref (default HEAD)"},
                      "staged": {"type": "boolean", "description": "only what is staged"}}), _gate),
    "callers": ("Who calls a function, transitively. Each match: its exact and name-guess caller counts, and the "
                "callers reached with their depth.",
                _schema({"name": {"type": "string", "description": "`name` (glob ok), `Module.name` or `path/glob:func`"},
                         "depth": _DEPTH, "fuzzy": _FUZZY}, ("name",)), _walk(True)),
    "callees": ("What a function calls, transitively (same shape as callers).",
                _schema({"name": {"type": "string", "description": "`name` (glob ok), `Module.name` or `path/glob:func`"},
                         "depth": _DEPTH, "fuzzy": _FUZZY}, ("name",)), _walk(False)),
    "path": ("The shortest call chain between two functions. An empty path with through_guesses_only = true: "
             "a chain exists only through name guesses (ask again with fuzzy).",
             _schema({"from": {"type": "string"}, "to": {"type": "string"}, "fuzzy": _FUZZY}, ("from", "to")), _path),
    "find": ("Where functions are defined: id, file, lines, calls made, exact and name-guess callers.",
             _schema({"name": {"type": "string", "description": "`name` (glob ok), `Module.name` or `path/glob:func`"}},
                     ("name",)), lambda p, a: queries.find(p, a["name"])),
    "context": ("Everything to know before editing a function, in one call: where it is, its source, each caller "
                "(exact first, then name guesses), what each of its calls reaches, and the tests that reach it.",
                _schema({"name": {"type": "string", "description": "`name` (glob ok), `Module.name` or `path/glob:func`"}},
                        ("name",)), lambda p, a: queries.context(p, a["name"])),
    "refs": ("Where a name is defined, called and mentioned anywhere (scripts and config too).",
             _schema({"name": {"type": "string"}}, ("name",)), lambda p, a: tidy.refs(p, a["name"], tidy.Corpus(p))),
    "impact": ("What a change could break: the callers and bridged views of the targets (or of the functions "
               "changed since a ref), by area.",
               _schema({"targets": {"type": "array", "items": {"type": "string"},
                                    "description": "function specs or file paths"},
                        "since": {"type": "string", "description": "also the functions changed since this git ref"},
                        "staged": {"type": "boolean"},
                        "depth": {"type": "integer", "minimum": 1, "description": "default 4"}, "fuzzy": _FUZZY}),
               _impact),
    "stats": ("Size, load time, and per language how call sites resolve (project_exact: of the calls into project "
              "code, the share that is certain).", _schema(), lambda p, a: queries.stats(p)),
    "unused": ("Product functions nothing calls and no other file names.", _schema({"since": _SINCE}),
               lambda p, a: _since(p, a, tidy.unused(p, tidy.Corpus(p)))),
    "tests": ("Every test: how much product code it reaches and whether it reads product source "
              "(reach 0 or reads_source: a test that tests nothing).", _schema({"since": _SINCE}),
              lambda p, a: _since(p, a, tidy.test_audit(p))),
}


# --- the server -----------------------------------------------------------------------

class Server:
    def __init__(self, root: str, extra: list[str], use_cache: bool = True):
        self.root, self.extra, self.use_cache = root, extra, use_cache
        self.p: core.Project | None = None
        self.stamps: dict = {}
        self.own: set[str] = set()

    def _stamps(self) -> dict:
        """mtime + size of every file a load reads: the project's files as
        listed now (one appeared or went: a change too) and its config."""
        cfg, _ = config_mod.load(self.root, self.extra)
        pc = cfg.get("project", {})
        rels = core.list_files(self.root, pc.get("include", ["**/*"]),
                               core.default_exclude(pc.get("include", ["**/*"])) + pc.get("exclude", []))
        out = {rel: core._stamp(self.root, rel) for rel in rels}
        for path in config_mod.files_for(self.root, self.extra):
            out[path] = core._stamp(self.root, path)
        return out

    def project(self) -> core.Project:
        now = self._stamps()  # (before the load: an edit during it is seen by the next call)
        if self.p is None or now != self.stamps:
            self.p = core.load(self.root, self.extra, use_cache=self.use_cache)
            self.stamps = now
            self.own = set(vars(self.p))
        return self.p

    def _forget(self, p: core.Project) -> None:
        """What a call cached on the project that the stamps don't cover goes:
        text of files outside the project (repo-wide lints), a lint's module
        index, a gate's scope."""
        for k in set(vars(p)) - self.own:
            delattr(p, k)
        files = set(p.files)
        for k in [k for k in p.text_cache if k[0] not in files]:
            del p.text_cache[k]

    def call(self, name: str, args: dict):
        desc, schema, fn = TOOLS[name]
        missing = [k for k in schema["required"] if k not in args]
        unknown = [k for k in args if k not in schema["properties"]]
        if missing or unknown:
            raise ToolError("; ".join([f"missing {', '.join(missing)}"] * bool(missing) +
                                      [f"unknown {', '.join(unknown)}"] * bool(unknown)))
        p = self.project()
        try:
            return fn(p, args, self.extra) if fn is _gate else fn(p, args)
        finally:
            self._forget(p)

    def handle(self, msg) -> dict | None:
        """One JSON-RPC message in, the reply out (None for a notification)."""
        if not isinstance(msg, dict):
            return _error(None, -32600, "one JSON-RPC object per line (batches are not part of MCP 2025-06-18)")
        mid, method, params = msg.get("id"), msg.get("method"), msg.get("params") or {}
        if "id" not in msg or not isinstance(method, str):
            return None  # a notification (initialized, cancelled), or a response to nothing we asked
        if method == "initialize":
            return _result(mid, {"protocolVersion": PROTOCOL, "capabilities": {"tools": {"listChanged": False}},
                                 "serverInfo": {"name": "seamcheck", "version": __version__},
                                 "instructions": "seamcheck's call graph and rules for the project at " + self.root +
                                                 ". Queries walk exact edges; `fuzzy` adds name guesses."})
        if method == "ping":
            return _result(mid, {})
        if method == "tools/list":
            return _result(mid, {"tools": [{"name": n, "description": d, "inputSchema": s}
                                           for n, (d, s, _) in TOOLS.items()]})
        if method == "tools/call":
            name, args = params.get("name"), params.get("arguments") or {}
            if name not in TOOLS:
                return _error(mid, -32602, f"unknown tool {name!r}; tools: {', '.join(TOOLS)}")
            try:
                out, bad = self.call(name, args), False
            except ToolError as e:
                out, bad = {"error": str(e)}, True
            except Exception as e:  # a crash in one query must not end the session
                out, bad = {"error": f"{type(e).__name__}: {e}"}, True
            return _result(mid, {"content": [{"type": "text", "text": json.dumps(out, indent=1, default=list)}],
                                 "isError": bad})
        return _error(mid, -32601, f"method not found: {method}")


def _result(mid, result: dict) -> dict:
    return {"jsonrpc": "2.0", "id": mid, "result": result}


def _error(mid, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": mid, "error": {"code": code, "message": message}}


def main(root: str, extra: list[str], use_cache: bool = True, stdin=None, stdout=None) -> int:
    stdin = stdin or sys.stdin.buffer
    out = stdout or sys.stdout
    srv = Server(root, extra, use_cache)
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            reply = _error(None, -32700, "parse error: one JSON-RPC message per line")
        else:
            # stdout carries only protocol messages: anything a check prints goes to stderr
            with contextlib.redirect_stdout(sys.stderr):
                reply = srv.handle(msg)
        if reply is not None:
            out.write(json.dumps(reply) + "\n")
            out.flush()
    return 0
