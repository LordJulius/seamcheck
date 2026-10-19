"""An interrupted parse leaves no workers behind (python3 -m unittest discover tests).

Ctrl-C used to break the pool's workers mid-task and leave the run waiting
forever in its shutdown; 17 idle workers were found three days later."""
import multiprocessing
import os
import signal
import sys
import threading
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402


def _slow_parse(args):
    time.sleep(30)
    return args[1], []


class InterruptTest(unittest.TestCase):
    def test_ctrl_c_mid_parse_ends_the_run_and_its_workers(self):
        if "fork" not in multiprocessing.get_all_start_methods():
            self.skipTest("needs fork")
        original = core._parse_one
        core._parse_one = _slow_parse  # forked workers inherit it
        try:
            todo = [(".", f"f{i}.py", None) for i in range(80)]
            threading.Timer(1.0, lambda: os.kill(os.getpid(), signal.SIGINT)).start()
            started = time.monotonic()
            with self.assertRaises(KeyboardInterrupt):
                core._parse_parallel(todo, multiprocessing.get_context("fork"))
            self.assertLess(time.monotonic() - started, 10)
            deadline = time.monotonic() + 5
            while multiprocessing.active_children() and time.monotonic() < deadline:
                time.sleep(0.1)
            self.assertEqual(multiprocessing.active_children(), [])
        finally:
            core._parse_one = original


if __name__ == "__main__":
    unittest.main()
