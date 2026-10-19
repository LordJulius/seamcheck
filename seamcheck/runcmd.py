"""`seamcheck trace | run | fuzz`: feed a program's SEAMCHECK lines to the
runtime rules (runtime.py). Only the config is read, never the code, so these
start instantly on any repo."""
from __future__ import annotations

import json
import os
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor

from . import config as config_mod
from .runtime import Checker, format_violation


def _config(a, root):
    cfg, problems = config_mod.load(root, a.config)
    for p in problems:
        print(f"[error] config: {p}")
    if not cfg["invariant"] and not cfg["contract"]:
        print(f"no [[invariant]] or [[contract]] in {', '.join(config_mod.files_for(root, a.config)) or '(no config found)'}")
    return cfg, problems


def _report(chk: Checker, a, extra: str = "") -> int:
    vs = chk.finish()
    errs = sum(1 for v in vs if v.severity == "error")
    if a.json:
        print(json.dumps([v.__dict__ for v in vs], indent=1))
    else:
        for v in vs:
            rule = next((r.spec for r in chk.rules if r.id == v.rule), {})
            print(format_violation(v) + (f"\n    why: {rule['why']}" if rule.get("why") else ""))
        print(f"\nseamcheck runtime: {chk.summary()}; {errs} errors, {len(vs) - errs} warnings{extra}")
    return 1 if errs else 0


def _run_one(cmd: list[str], cfg: dict, strict: bool, log_path: str | None, echo: bool, timeout: float,
             env: dict | None = None) -> tuple[Checker, int, float]:
    chk = Checker(cfg, strict=strict)
    t0 = time.time()
    log = open(log_path, "w") if log_path else None
    try:
        # Its own process group: a timeout (or the end) takes the children it
        # started too. A grandchild holding stdout open used to hang the run.
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1,
                                errors="replace", env=env, start_new_session=True)
    except OSError as e:
        print(f"[error] can't start {cmd[0]!r}: {e}")
        return chk, 127, 0.0

    def kill_group():
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass

    timer = None
    if timeout > 0:
        timer = threading.Timer(timeout, kill_group)
        timer.start()
    # The command's own exit ends the run even if something it left behind
    # still holds the pipe.
    threading.Thread(target=lambda: (proc.wait(), time.sleep(2.0), kill_group()), daemon=True).start()
    for line in proc.stdout:
        if log:
            log.write(line)
        if echo:
            sys.stdout.write(line)
        chk.feed_line(line.rstrip("\n"))
    code = proc.wait()
    if timer:
        timer.cancel()
    if log:
        log.close()
    return chk, code, time.time() - t0


def _seeds(spec: str) -> list[int]:
    out = []
    for part in spec.split(","):
        if "-" in part.strip()[1:]:
            lo, hi = part.split("-", 1)
            out += list(range(int(lo), int(hi) + 1))
        elif part.strip():
            out.append(int(part))
    return out


def main(a, root) -> int:
    cfg, problems = _config(a, root)
    bad = 1 if problems else 0
    if not cfg["invariant"] and not cfg["contract"]:
        return 2  # nothing to check with: never a pass

    if a.cmd == "trace":
        if not a.args:
            print("trace needs a log file (or - for stdin)")
            return 2
        code = 0
        for path in a.args:
            chk = Checker(cfg, strict=a.strict)
            fh = sys.stdin if path == "-" else open(path, errors="replace")
            try:
                for line in fh:
                    chk.feed_line(line.rstrip("\n"))
            finally:
                if fh is not sys.stdin:
                    fh.close()
            if len(a.args) > 1:
                print(f"== {path}")
            code |= _report(chk, a)
        return code | bad

    if not a.args:
        print(f"{a.cmd} needs a command after --")
        return 2

    if a.cmd == "run":
        chk, code, secs = _run_one(a.args, cfg, a.strict, a.log, a.echo, a.timeout)
        return _report(chk, a, f"  (command exit {code}, {secs:.0f} s{', log ' + a.log if a.log else ''})") | bad

    # fuzz: the same command once per seed, a few at a time.
    seeds = _seeds(a.seeds)
    logs = os.path.join(tempfile.gettempdir(), "seamcheck-fuzz")
    os.makedirs(logs, exist_ok=True)
    templ = a.args
    has_slot = any("{seed}" in x for x in templ)

    def one(seed):
        cmd = [x.replace("{seed}", str(seed)) for x in templ]
        env = dict(os.environ, SEAMCHECK_SEED=str(seed))
        log = os.path.join(logs, f"seed-{seed}.log")
        chk, code, secs = _run_one(cmd, cfg, a.strict, log, False, a.timeout, env)
        return seed, cmd, chk, code, secs, log

    failed = []
    with ThreadPoolExecutor(max_workers=max(1, a.jobs)) as ex:
        for seed, cmd, chk, code, secs, log in ex.map(one, seeds):
            vs = chk.finish()
            errs = [v for v in vs if v.severity == "error"]
            print(f"seed {seed:<5} {'FAIL' if errs else 'ok  '}  {chk.states} states, {chk.acts} actions, "
                  f"{len(errs)} errors  ({secs:.0f} s, exit {code})")
            for v in errs[:4]:
                print("    " + format_violation(v).replace("\n", "\n    "))
            if errs:
                failed.append((seed, cmd, log))
    print(f"\nseamcheck fuzz: {len(seeds) - len(failed)}/{len(seeds)} seeds clean. Logs: {logs}")
    for seed, cmd, log in failed:
        env = "" if has_slot else f"SEAMCHECK_SEED={seed} "
        print(f"  replay seed {seed}: {env}seamcheck run -- {shlex.join(cmd)}")
    return (1 if failed else 0) | bad
