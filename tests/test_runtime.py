"""Runtime checks (python3 -m unittest discover tests): invariants, contracts,
holds, the run-level guards, config merging, and the expression sandbox."""
import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, HERE)
from seamcheck import config as config_mod  # noqa: E402
from seamcheck.runtime import Checker, compile_expr  # noqa: E402

CFG = {
    "runtime": {},
    "invariant": [
        {"id": "hp", "each": "units", "where": "not dead", "assert": "0 <= hp <= max_hp"},
        {"id": "wet", "each": "units", "assert": "not wet", "for": 2.0},
        {"id": "stones", "assert": "not dupes(stones)"},
        {"id": "frozen", "when": "paused and prev and prev.paused", "each": "units",
         "assert": "prev_item is None or (x, hp) == (prev_item.x, prev_item.hp)"},
    ],
    "contract": [
        {"id": "refused-noop", "act": "*", "when": "ok == False", "assert": "after == before"},
        {"id": "price", "act": ["build", "recruit"], "when": "ok and price >= 0",
         "assert": "after.gold == before.gold - price"},
    ],
}


def write(path, text):
    with open(path, "w") as fh:
        fh.write(text)


def lines(*objs):
    return [f"noise before SEAMCHECK {json.dumps(o)}" for o in objs] + ["SEAMCHECK " + json.dumps({"k": "end"})]


def check(stream, cfg=CFG, strict=False):
    c = Checker(cfg, strict=strict)
    for ln in stream:
        c.feed_line(ln)
    return c, c.finish()


def unit(i, **kw):
    return dict({"id": i, "hp": 10, "max_hp": 10, "dead": False, "wet": False, "x": 0}, **kw)


def state(t, units, stones=(), paused=False):
    return {"k": "state", "t": t, "units": units, "stones": list(stones), "paused": paused}


class Invariants(unittest.TestCase):
    def test_clean_run_passes(self):
        _, vs = check(lines(state(0, [unit("a")]), state(1, [unit("a")])))
        self.assertEqual(vs, [])

    def test_item_rule_names_the_item_and_values(self):
        _, vs = check(lines(state(0, [unit("a"), unit("b", hp=12)])))
        self.assertEqual([(v.rule, v.subject) for v in vs], [("hp", "b")])
        self.assertIn("hp=12", vs[0].message)

    def test_where_skips(self):
        _, vs = check(lines(state(0, [unit("a", hp=-5, dead=True)])))
        self.assertEqual(vs, [])

    def test_hold_needs_to_last(self):
        short = lines(state(0, [unit("a", wet=True)]), state(1, [unit("a", wet=True)]), state(2, [unit("a")]))
        self.assertEqual(check(short)[1], [])
        long = lines(*[state(t, [unit("a", wet=True)]) for t in range(4)])
        self.assertEqual([v.rule for v in check(long)[1]], ["wet"])

    def test_repeats_are_counted_not_repeated(self):
        _, vs = check(lines(*[state(t, [unit("a", hp=99)]) for t in range(5)]))
        self.assertEqual(len(vs), 1)
        self.assertEqual(vs[0].count, 5)

    def test_prev_item_matches_by_key(self):
        s1 = state(0, [unit("a", x=1), unit("b", x=5)], paused=True)
        s2 = state(1, [unit("b", x=5), unit("a", x=2)], paused=True)  # reordered; a moved
        _, vs = check(lines(s1, s2))
        self.assertEqual([(v.rule, v.subject) for v in vs], [("frozen", "a")])

    def test_dupes(self):
        _, vs = check(lines(state(0, [], stones=["gale", "ember", "gale"])))
        self.assertEqual([v.rule for v in vs], ["stones"])
        self.assertIn("gale", vs[0].message)


class Contracts(unittest.TestCase):
    def act(self, name, ok, before, after, price=-1):
        return {"k": "act", "act": name, "ok": ok, "before": before, "after": after, "price": price}

    def test_refusal_that_changes_state(self):
        _, vs = check(lines(self.act("buy", False, {"gold": 5}, {"gold": 3})))
        self.assertEqual([v.rule for v in vs], ["refused-noop"])

    def test_fields_named_like_methods_and_specials(self):
        cfg = {"invariant": [], "contract": [
            {"id": "items", "act": "*", "assert": "sorted(before.items) == sorted(after.items)"},
            {"id": "ok", "act": "*", "when": "get(line, 'ok') == False", "assert": "act == 'buy'"}]}
        a = self.act("sell", False, {"items": ["a", "b"]}, {"items": ["a"]})
        _, vs = check(lines(a), cfg)
        self.assertEqual(sorted(v.rule for v in vs), ["items", "ok"])

    def test_price(self):
        good = self.act("build", True, {"gold": 50}, {"gold": 20}, 30)
        bad = self.act("recruit", True, {"gold": 50}, {"gold": 45}, 30)
        other = self.act("sell", True, {"gold": 50}, {"gold": 45}, 30)  # not in act globs
        _, vs = check(lines(good, bad, other))
        self.assertEqual([(v.rule, v.subject) for v in vs], [("price", "recruit#2")])


class RunGuards(unittest.TestCase):
    def test_silence_fails(self):
        _, vs = check(["hello", "world"])
        self.assertEqual([v.rule for v in vs], ["no-signal"])

    def test_missing_end_fails(self):
        _, vs = check(["SEAMCHECK " + json.dumps(state(0, [unit("a")]))])
        self.assertEqual([v.rule for v in vs], ["no-end"])

    def test_broken_rule_is_an_error_not_a_pass(self):
        cfg = {"invariant": [{"id": "typo", "each": "units", "assert": "hitpoints > 0"}], "contract": []}
        _, vs = check(lines(state(0, [unit("a")])), cfg)
        self.assertEqual([(v.rule, v.severity) for v in vs], [("typo", "error")])
        self.assertIn("hitpoints", vs[0].message)

    def test_strict_flags_unexercised_rules(self):
        _, vs = check(lines(state(0, [unit("a")])), strict=True)
        self.assertEqual(sorted(v.rule for v in vs), ["frozen", "price", "refused-noop"])

    def test_bad_json_line(self):
        _, vs = check(["SEAMCHECK {nope"] + lines(state(0, [])))
        self.assertEqual([v.rule for v in vs], ["protocol"])


    def test_fail_on_patterns(self):
        cfg = dict(CFG, runtime={"fail_on": [r"SCRIPT ERROR"]})
        c, vs = check(["SCRIPT ERROR: boom", "SCRIPT ERROR: again"] + lines(state(0, [])), cfg)
        self.assertEqual([(v.rule, v.count) for v in vs], [("fail_on", 2)])
        self.assertIn("boom", vs[0].message)


    def test_hold_restarts_when_the_subject_comes_back(self):
        st = [state(0, [unit("a", wet=True)]), state(1, [unit("a", wet=True)]), state(2, []),
              state(10, [unit("a", wet=True)]), state(11, [unit("a", wet=True)])]
        self.assertEqual(check(lines(*st))[1], [])  # 1 s wet each time: never 2 s in a row

    def test_summary_names_rules_that_never_ran(self):
        c, _ = check(lines(state(0, [unit("a")])))
        self.assertIn("not exercised: frozen, refused-noop, price", c.summary())


class Waivers(unittest.TestCase):
    def test_waived_rules_are_skipped_and_listed(self):
        w = {"k": "waive", "rules": ["hp"], "why": "the test overheals on purpose"}
        c, vs = check(lines(w, state(0, [unit("a", hp=99)])))
        self.assertEqual(vs, [])
        self.assertIn("WAIVED by the program: hp (the test overheals on purpose)", c.summary())

    def test_waiving_an_unknown_rule_is_an_error(self):
        _, vs = check(lines({"k": "waive", "rules": ["hpp"]}, state(0, [unit("a")])))
        self.assertEqual([(v.rule, v.subject) for v in vs], [("waive", "hpp")])


class Breaks(unittest.TestCase):
    """Ways a rule file could quietly mean something else than it says."""

    def test_typo_keys_and_bad_severity_are_config_errors(self):
        with tempfile.TemporaryDirectory() as d:
            write(os.path.join(d, "seamcheck.toml"), textwrap.dedent('''
                [[invariant]]
                id = "a"
                wehre = "x"
                assert = "True"
                severity = "warning"
                [[contract]]
                id = "b"
                acts = ["x"]
                assert = "True"
                [[rule]]
                id = "c"
                pattern = "x"
                alow = ["y"]
            '''))
            _, problems = config_mod.load(d)
            text = "\n".join(problems)
            self.assertIn("unknown key 'wehre' (did you mean 'where'?)", text)
            self.assertIn("severity 'warning'", text)
            self.assertIn("unknown key 'acts' (did you mean 'act'?)", text)
            self.assertIn("unknown key 'alow' (did you mean 'allow'?)", text)

    def test_run_timeout_kills_grandchildren_holding_the_pipe(self):
        import time
        with tempfile.TemporaryDirectory() as d:
            write(os.path.join(d, "seamcheck.toml"), '[[invariant]]\nid = "x"\nassert = "True"\n')
            sh = os.path.join(d, "hold.sh")
            write(sh, "#!/bin/bash\n(sleep 30; echo late) &\necho 'SEAMCHECK {\"k\": \"state\"}'\nsleep 60\n")
            os.chmod(sh, 0o755)
            t0 = time.time()
            r = subprocess.run([sys.executable, "-m", "seamcheck", "run", "-C", d, "--timeout", "2", "--", sh],
                               capture_output=True, text=True, env=dict(os.environ, PYTHONPATH=HERE), timeout=40)
            self.assertLess(time.time() - t0, 10, "the run must end at its timeout")
            self.assertIn("no-end", r.stdout)

    def test_child_exit_ends_the_run_even_if_a_grandchild_keeps_the_pipe(self):
        import time
        with tempfile.TemporaryDirectory() as d:
            write(os.path.join(d, "seamcheck.toml"), '[[invariant]]\nid = "x"\nassert = "True"\n')
            sh = os.path.join(d, "leave.sh")
            write(sh, "#!/bin/bash\n(sleep 30) &\necho 'SEAMCHECK {\"k\": \"state\"}'\necho 'SEAMCHECK {\"k\": \"end\"}'\n")
            os.chmod(sh, 0o755)
            t0 = time.time()
            r = subprocess.run([sys.executable, "-m", "seamcheck", "run", "-C", d, "--", sh],
                               capture_output=True, text=True, env=dict(os.environ, PYTHONPATH=HERE), timeout=40)
            self.assertLess(time.time() - t0, 10)
            self.assertEqual(r.returncode, 0, r.stdout)

    def test_nan_values_fail_comparisons_instead_of_crashing(self):
        cfg = {"invariant": [{"id": "fin", "each": "units", "assert": "isfinite(hp)"}], "contract": []}
        stream = ['SEAMCHECK {"k": "state", "t": 0, "units": [{"id": "a", "hp": NaN}]}', 'SEAMCHECK {"k": "end"}']
        _, vs = check(stream, cfg)
        self.assertEqual([(v.rule, v.subject) for v in vs], [("fin", "a")])


class Sandbox(unittest.TestCase):
    def test_refuses_calls_and_dunders(self):
        for src in ["__import__('os')", "open('x')", "x.__class__", "(lambda: 1)()", "[].append(1)"]:
            with self.assertRaises((ValueError, SyntaxError), msg=src):
                compile_expr(src)

    def test_generator_sees_names(self):
        cfg = {"invariant": [{"id": "g", "assert": "all(u.hp <= cap for u in units)"}], "contract": []}
        s = state(0, [unit("a", hp=3)])
        s["cap"] = 2
        _, vs = check(lines(s), cfg)
        self.assertEqual([v.rule for v in vs], ["g"])


class Config(unittest.TestCase):
    def test_merges_root_dir_and_extra_and_catches_dup_ids(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "seamcheck.d"))
            write(os.path.join(d, "seamcheck.toml"), '[project]\ninclude = ["a/**"]\n[[rule]]\nid = "r1"\npattern = "x"\n')
            write(os.path.join(d, "seamcheck.d", "rt.toml"), '[[invariant]]\nid = "i1"\nassert = "True"\n')
            extra = os.path.join(d, "extra.toml")
            write(extra, '[project]\ninclude = ["b/**"]\n[[rule]]\nid = "r1"\npattern = "y"\n')
            cfg, problems = config_mod.load(d, [extra])
            self.assertEqual(cfg["project"]["include"], ["a/**", "b/**"])
            self.assertEqual([r["id"] for r in cfg["rule"]], ["r1"])
            self.assertEqual(cfg["invariant"][0]["_file"], os.path.join("seamcheck.d", "rt.toml"))
            self.assertEqual(len(problems), 1)
            self.assertIn("already defined", problems[0])


class Cli(unittest.TestCase):
    def test_run_and_fuzz_end_to_end(self):
        with tempfile.TemporaryDirectory() as d:
            write(os.path.join(d, "seamcheck.toml"), textwrap.dedent('''
                [[invariant]]
                id = "small"
                assert = "n < 3"
            '''))
            prog = os.path.join(d, "prog.py")
            write(prog, textwrap.dedent('''
                import json, os, sys
                seed = int(sys.argv[1])
                print("SEAMCHECK " + json.dumps({"k": "state", "t": 0, "n": seed}))
                print("SEAMCHECK " + json.dumps({"k": "end"}))
            '''))
            env = dict(os.environ, PYTHONPATH=HERE)
            run = lambda *a: subprocess.run([sys.executable, "-m", "seamcheck", a[0], "-C", d, *a[1:]], capture_output=True, text=True, env=env)
            ok = run("run", "--", sys.executable, prog, "1")
            self.assertEqual(ok.returncode, 0, ok.stdout)
            bad = run("run", "--", sys.executable, prog, "5")
            self.assertEqual(bad.returncode, 1)
            self.assertIn("n=5", bad.stdout)
            fz = run("fuzz", "--seeds", "1-4", "--", sys.executable, prog, "{seed}")
            self.assertEqual(fz.returncode, 1)
            self.assertIn("2/4 seeds clean", fz.stdout)
            self.assertIn("replay seed 3", fz.stdout)
            empty = subprocess.run([sys.executable, "-m", "seamcheck", "run", "-C", HERE + "/tests", "--", "true"],
                                   capture_output=True, text=True, env=env)
            self.assertEqual(empty.returncode, 2, "no rules found must not pass")


if __name__ == "__main__":
    unittest.main()
