"""Runtime checks: judge what a running program reports about itself.

The program under test prints lines `SEAMCHECK {json}` (the prefix is
`[runtime] prefix` in the config). Each JSON object has a kind `k`:

  {"k": "state", "t": 12.5, ...}   a snapshot of the world (any fields)
  {"k": "act", "act": "build", "ok": true, "before": {...}, "after": {...}, ...}
                                   one action and its effect (any extra fields)
  {"k": "end"}                     the run finished cleanly
  {"k": "waive", "rules": [...], "why": "..."}
                                   a test breaks these on purpose: skipped from
                                   here on, listed in the summary

The rules live in the project's own config files:

  [[invariant]]              checked on every state line
  id = "hp-in-range"
  each = "units"             optional: once per item of this list (item fields are bare names)
  where = "not dead"         optional: skip items / states where this is false
  assert = "0 <= hp <= max_hp"
  for = 2.0                  optional: only a finding once it stays false this long (by `t`)
  key = "id"                 optional: what identifies an item across states (default "id")

  [[contract]]               checked on every act line
  id = "refusal-changes-nothing"
  act = ["*"]                action name globs
  when = "ok == False"       optional
  assert = "after == before"

Expressions are small Python expressions (no calls except the helpers below,
no attribute starting with `_`). Names resolve to: the item's fields (with
`each`), then the line's fields. These names always mean the same thing and
win over fields: `line` (the whole line), `state` (the current state; for a
contract, the last state before it), `prev` (the previous state), `item`,
`prev_item` (this item in the previous state, matched by `key`, or None).
Dicts read as `a.b` or `a["b"]` (`a.b` is always the field, never a method). A field that may be missing: `get(line, "ok")`.
Helpers: len set sorted min max sum abs any all round count(xs, v)
dupes(xs) isfinite(x) get(d, k, default) keys(d) values(d) diff(a, b).

Every run also fails when: no state line arrived at all (a silent emitter
would pass anything), the stream ended without an `end` line (a crash or a
kill), or a rule never had anything to check (`--strict`).
"""
from __future__ import annotations

import ast
import fnmatch
import json
import math
import re
from dataclasses import dataclass, field


def _field(obj, name):
    """`a.b` in a rule: the field b of dict a (rules never see methods)."""
    if isinstance(obj, dict):
        if name in obj:
            return obj[name]
        raise NameError(f"no field {name!r} (has: {', '.join(sorted(obj)[:12])})")
    raise NameError(f"can't read .{name} of {type(obj).__name__} {json.dumps(obj, default=str)[:60]}")


class _Fields(ast.NodeTransformer):
    def visit_Attribute(self, n):
        self.generic_visit(n)
        return ast.copy_location(ast.Call(ast.Name("__field__", ast.Load()), [n.value, ast.Constant(n.attr)], []), n)


def _count(xs, v):
    return sum(1 for x in xs if x == v)


def _dupes(xs):
    seen, out = set(), []
    for x in xs:
        k = json.dumps(x, sort_keys=True)
        if k in seen and x not in out:
            out.append(x)
        seen.add(k)
    return out


def _diff(a, b):
    """Keys whose values differ between two dicts (for readable messages)."""
    a, b = a or {}, b or {}
    return {k: [dict.get(a, k), dict.get(b, k)] for k in sorted(set(a) | set(b)) if dict.get(a, k) != dict.get(b, k)}


HELPERS = {
    "len": len, "set": lambda xs=(): set(json.dumps(x, sort_keys=True) if isinstance(x, (dict, list)) else x for x in xs),
    "sorted": sorted, "min": min, "max": max, "sum": sum, "abs": abs, "any": any, "all": all, "round": round,
    "count": _count, "dupes": _dupes, "isfinite": lambda x: isinstance(x, (int, float)) and math.isfinite(x),
    "get": lambda d, k, default=None: dict.get(d or {}, k, default), "keys": lambda d: list(dict.keys(d or {})),
    "values": lambda d: list(dict.values(d or {})), "diff": _diff, "None": None, "True": True, "False": False,
}
_OK_NODES = (ast.Expression, ast.BoolOp, ast.BinOp, ast.UnaryOp, ast.Compare, ast.Call, ast.Name, ast.Load,
             ast.Attribute, ast.Subscript, ast.Constant, ast.List, ast.Tuple, ast.Set, ast.Dict, ast.IfExp,
             ast.ListComp, ast.SetComp, ast.GeneratorExp, ast.comprehension, ast.Slice, ast.Store,
             ast.And, ast.Or, ast.Not, ast.USub, ast.UAdd, ast.Add, ast.Sub, ast.Mult, ast.Div, ast.Mod,
             ast.FloorDiv, ast.Pow, ast.Eq, ast.NotEq, ast.Lt, ast.LtE, ast.Gt, ast.GtE, ast.In, ast.NotIn,
             ast.Is, ast.IsNot)


def compile_expr(src: str):
    """Compile a rule expression, refusing anything but plain expressions."""
    tree = ast.parse(src.strip(), mode="eval")
    for n in ast.walk(tree):
        if not isinstance(n, _OK_NODES):
            raise ValueError(f"not allowed in a rule: {type(n).__name__}")
        if isinstance(n, ast.Attribute) and n.attr.startswith("_"):
            raise ValueError("attributes starting with _ are not allowed")
        if isinstance(n, ast.Call) and not (isinstance(n.func, ast.Name) and n.func.id in HELPERS):
            raise ValueError(f"only helper calls are allowed ({', '.join(k for k in HELPERS if k[0].islower())})")
    tree = ast.fix_missing_locations(_Fields().visit(tree))
    return compile(tree, "<rule>", "eval")


def evaluate(code, layers):
    """Evaluate with names from the layers (earlier layers win), then helpers.
    One flat dict as globals: generator expressions read names from there."""
    env = dict(HELPERS, __field__=_field)
    for layer in reversed(layers):
        if isinstance(layer, dict):
            env.update(layer)
    env["__builtins__"] = {}
    return eval(code, env)


@dataclass
class Violation:
    rule: str
    kind: str  # invariant | contract | run
    severity: str
    t: float
    subject: str  # item key, action name, ...
    message: str
    line: int  # line number in the stream
    count: int = 1


@dataclass
class Rule:
    spec: dict
    kind: str
    code: object = None
    when: object = None
    where: object = None
    each: object = None
    evaluated: int = 0
    broken: int = 0
    error: str = ""
    waived: str = ""  # why the program under test switched it off (a "waive" line)

    @property
    def id(self) -> str:
        return self.spec.get("id", "?")


@dataclass
class Checker:
    config: dict
    strict: bool = False
    rules: list[Rule] = field(default_factory=list)
    violations: dict = field(default_factory=dict)  # (rule, subject) -> Violation
    order: list = field(default_factory=list)
    prev: dict | None = None
    states: int = 0
    acts: int = 0
    ended: bool = False
    lineno: int = 0
    _since: dict = field(default_factory=dict)  # (rule, subject) -> t it first went false
    prefix: str = "SEAMCHECK"
    fail_on: list = field(default_factory=list)

    def __post_init__(self):
        rt = self.config.get("runtime", {})
        self.prefix = rt.get("prefix", "SEAMCHECK")
        # Lines the program prints when it breaks outside any rule (a crash, a
        # script error): each pattern that matches is a finding.
        for pat in rt.get("fail_on", []):
            try:
                self.fail_on.append(re.compile(pat))
            except re.error as e:
                self.rules.append(Rule({"id": "fail_on"}, "run", error=f"bad fail_on pattern {pat!r}: {e}"))
        for kind in ("invariant", "contract"):
            for spec in self.config.get(kind, []):
                r = Rule(spec, kind)
                try:
                    r.code = compile_expr(spec["assert"])
                    if spec.get("when"):
                        r.when = compile_expr(spec["when"])
                    if spec.get("where"):
                        r.where = compile_expr(spec["where"])
                    if spec.get("each"):
                        r.each = compile_expr(spec["each"])
                except (KeyError, SyntaxError, ValueError) as e:
                    r.error = f"bad rule: {e}"
                self.rules.append(r)

    # --- feeding
    def feed_line(self, raw: str) -> None:
        self.lineno += 1
        i = raw.find(self.prefix + " ")
        if i < 0:
            for rx in self.fail_on:
                if rx.search(raw):
                    self._flag("fail_on", "run", "error", 0.0, rx.pattern, raw.strip()[:300])
            return
        try:
            obj = json.loads(raw[i + len(self.prefix) + 1:])
        except json.JSONDecodeError as e:
            self._flag("protocol", "run", "error", 0.0, "line", f"unreadable {self.prefix} line: {e}")
            return
        if not isinstance(obj, dict):
            return
        self.feed(obj)

    def feed(self, obj: dict) -> None:
        k = obj.get("k")
        if k == "state":
            self.states += 1
            self._state(obj)
            self.prev = obj
        elif k == "act":
            self.acts += 1
            self._act(obj)
        elif k == "end":
            self.ended = True
        elif k == "waive":
            # The program says a test breaks these on purpose (it clears the
            # map, freezes the AI...). Visible in the summary, never silent,
            # and a rule id that doesn't exist is an error (a typo'd waiver
            # must not hide anything).
            why = str(obj.get("why", "")) or "(no reason given)"
            ids = obj.get("rules", [])
            known = {r.id: r for r in self.rules}
            for rid in ids if isinstance(ids, list) else [ids]:
                if rid in known:
                    known[rid].waived = why
                else:
                    self._flag("waive", "run", "error", 0.0, str(rid), f"waived a rule that doesn't exist: {rid!r}")

    def _flag(self, rule, kind, severity, t, subject, message):
        key = (rule, subject)
        v = self.violations.get(key)
        if v:
            v.count += 1
            return
        v = Violation(rule, kind, severity, t, subject, message, self.lineno)
        self.violations[key] = v
        self.order.append(key)

    def _check(self, r: Rule, layers, t, subject, hold: float = 0.0):
        try:
            if r.where is not None and not evaluate(r.where, layers):
                self._since.pop((r.id, subject), None)
                return
            r.evaluated += 1
            ok = evaluate(r.code, layers)
        except Exception as e:  # a rule that can't be evaluated is a finding, never a pass
            r.error = r.error or f"{type(e).__name__}: {e}"
            return
        if ok:
            self._since.pop((r.id, subject), None)
            return
        if hold > 0.0:
            first = self._since.setdefault((r.id, subject), t)
            if t - first < hold:
                return
        r.broken += 1
        self._flag(r.id, r.kind, r.spec.get("severity", "error"), t, subject, self._explain(r, layers))

    def _explain(self, r: Rule, layers) -> str:
        """The assertion, plus the value of each name it reads (to see why)."""
        tree = ast.parse(r.spec["assert"].strip(), mode="eval")
        bound = {t.id for c in ast.walk(tree) if isinstance(c, ast.comprehension)
                 for t in ast.walk(c.target) if isinstance(t, ast.Name)}
        attrs = [n for n in ast.walk(tree) if isinstance(n, ast.Attribute)]
        inner = {id(a.value) for a in attrs}  # a.b.c: show a.b.c, not a.b or a
        roots = set()
        for a in attrs:
            base = a
            while isinstance(base, ast.Attribute):
                base = base.value
            if isinstance(base, ast.Name):
                roots.add(base.id)  # `p is None or p.x == 1`: show p.x, not all of p
        names = []
        for n in ast.walk(tree):
            if id(n) in inner or not isinstance(n, (ast.Name, ast.Attribute)):
                continue
            root = n
            while isinstance(root, ast.Attribute):
                root = root.value
            if isinstance(root, ast.Name) and (root.id in bound or root.id in HELPERS):
                continue
            if isinstance(n, ast.Name) and n.id in roots:
                continue
            src = ast.unparse(n)
            if src not in names:
                names.append(src)
        vals = []
        for src in names[:6]:
            try:
                v = evaluate(compile_expr(src), layers)
            except Exception:
                continue
            s = json.dumps(v, sort_keys=True, default=str)
            vals.append(f"{src}={s[:160]}")
        return f"{r.spec['assert'].strip()}  [{', '.join(vals)}]"

    def _state(self, st):
        t = float(dict.get(st, "t", self.states))
        for r in self.rules:
            if r.kind != "invariant" or r.error.startswith("bad rule") or r.waived:
                continue
            base = [{"state": st, "line": st, "prev": self.prev}, st]
            try:
                if r.when is not None and not evaluate(r.when, base):
                    continue
            except Exception as e:
                r.error = r.error or f"{type(e).__name__}: {e}"
                continue
            hold = float(r.spec.get("for", 0.0))
            if r.each is None:
                self._check(r, base, t, "", hold)
                continue
            try:
                items = evaluate(r.each, base)
            except Exception as e:
                r.error = r.error or f"each: {type(e).__name__}: {e}"
                continue
            key = r.spec.get("key", "id")
            if hold > 0.0:
                # A subject gone from this state (dead, despawned) starts its
                # hold over if it comes back: no stale "false since".
                here = {json.dumps(dict.get(it, key, i)).strip('"') if isinstance(it, dict) else str(i)
                        for i, it in enumerate(items or [])}
                for k in [k for k in self._since if k[0] == r.id and k[1] not in here]:
                    del self._since[k]
            prev_items = {}
            if self.prev is not None:
                try:
                    for p in evaluate(r.each, [{"state": self.prev, "line": self.prev}, self.prev]) or []:
                        if isinstance(p, dict) and key in p:
                            prev_items[json.dumps(p[key])] = p
                except Exception:
                    pass
            for idx, it in enumerate(items or []):
                subj = json.dumps(dict.get(it, key, idx)) if isinstance(it, dict) else str(idx)
                layers = [{"state": st, "line": st, "prev": self.prev, "item": it, "prev_item": prev_items.get(subj)},
                          it if isinstance(it, dict) else {}, st]
                self._check(r, layers, t, subj.strip('"'), hold)

    def _act(self, a):
        t = float(dict.get(a, "t", 0.0))
        name = str(dict.get(a, "act", ""))
        for r in self.rules:
            if r.kind != "contract" or r.error.startswith("bad rule") or r.waived:
                continue
            globs = r.spec.get("act", ["*"])
            globs = [globs] if isinstance(globs, str) else globs
            if not any(fnmatch.fnmatchcase(name, g) for g in globs):
                continue
            layers = [{"line": a, "state": self.prev, "prev": self.prev}, a]
            try:
                if r.when is not None and not evaluate(r.when, layers):
                    continue
            except Exception as e:
                r.error = r.error or f"{type(e).__name__}: {e}"
                continue
            self._check(r, layers, t, f"{name}#{self.acts}")

    # --- verdict
    def finish(self) -> list[Violation]:
        out = [self.violations[k] for k in self.order]
        if self.states == 0 and self.acts == 0:
            out.insert(0, Violation("no-signal", "run", "error", 0.0, "", f"no {self.prefix} lines at all: "
                                    "the emitter isn't on (or printed nothing), so nothing was checked", self.lineno))
        elif not self.ended:
            out.append(Violation("no-end", "run", "error", 0.0, "", f"the stream stopped without a {{\"k\": \"end\"}} "
                                 "line: the program crashed, was killed, or never finished", self.lineno))
        for r in self.rules:
            if r.error:
                out.append(Violation(r.id, r.kind, "error", 0.0, "", r.error, 0))
            elif r.evaluated == 0 and self.strict:
                out.append(Violation(r.id, r.kind, "error", 0.0, "", "never had anything to check in this run", 0))
        return out

    def summary(self) -> str:
        ran = sum(1 for r in self.rules if r.evaluated)
        idle = [r.id for r in self.rules if not r.evaluated and not r.error and not r.waived]
        waived = [f"{r.id} ({r.waived})" for r in self.rules if r.waived]
        # (a rule that never ran proved nothing: say which, every time)
        return (f"{len(self.rules)} runtime rules ({ran} exercised"
                + (f"; not exercised: {', '.join(idle)}" if idle else "")
                + (f"; WAIVED by the program: {', '.join(waived)}" if waived else "") + f"), {self.states} states, {self.acts} actions")


def format_violation(v: Violation) -> str:
    where = f"t={v.t:g}" + (f" {v.subject}" if v.subject else "")
    more = f" (x{v.count})" if v.count > 1 else ""
    return f"[{v.severity}] {v.rule}: {where}{more}\n    {v.message}"
