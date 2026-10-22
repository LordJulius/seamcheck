"""The command line around the checks (python3 -m unittest discover tests).

- `seamcheck dead | head` died with a BrokenPipeError traceback;
- a repo had no way to start: `init` writes a first seamcheck.toml from what
  the repo holds, and `--hook` installs the pre-commit gate (never over an
  existing hook);
- CI could only read text or seamcheck's own JSON: `--format sarif` is for
  code scanning, `--format github` annotates the pull request's diff.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
from seamcheck import init, output  # noqa: E402


def run(*args, cwd):
    env = dict(os.environ, PYTHONPATH=ROOT)
    return subprocess.run([sys.executable, "-m", "seamcheck", *args], cwd=cwd, env=env, capture_output=True, text=True)


class Cli(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name
        files = {
            "src/app/store.py": "def save(x):\n    db.execute(x)\n",
            "src/app/view.py": "from .store import save\n\n\ndef show(x):\n    db.execute(x)\n    return save(x)\n",
            "tests/test_view.py": "from app.view import show\n\n\ndef test_show():\n    show(1)\n",
            "web/src/main.ts": "export function main() {\n  return 1;\n}\n",
        }
        for name, body in files.items():
            path = os.path.join(self.root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)

    def tearDown(self):
        self.dir.cleanup()

    def write_rule(self):
        with open(os.path.join(self.root, "seamcheck.toml"), "w") as fh:
            fh.write('[project]\nname = "t"\n\n[[rule]]\nid = "writes-go-through-store"\nkind = "forbid"\n'
                     'pattern = \'db\\.execute\'\nallow = ["src/app/store.py"]\nwhy = "the store clears the cache"\n'
                     'fix = "call save()"\n')

    def test_init_writes_a_config_that_loads(self):
        r = run("init", cwd=self.root)
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(os.path.join(self.root, "seamcheck.toml")) as fh:
            text = fh.read()
        self.assertIn('include = ["src/**", "tests/**", "web/**"]', text)
        self.assertIn('areas = { src = ["src/**"], web = ["web/**"] }', text)
        self.assertIn("Python", r.stdout)
        r = run("check", cwd=self.root)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("config", r.stdout)
        # a second init leaves the file alone
        r = run("init", cwd=self.root)
        self.assertIn("exists", r.stdout)

    def test_hook_never_overwrites(self):
        os.makedirs(os.path.join(self.root, ".git", "hooks"))
        self.assertIn("installed", init.install_hook(self.root))
        self.assertTrue(os.access(os.path.join(self.root, ".git", "hooks", "pre-commit"), os.X_OK))
        self.assertIn("exists", init.install_hook(self.root))

    def test_sarif_and_github_formats(self):
        self.write_rule()
        r = run("check", "--format", "sarif", cwd=self.root)
        self.assertEqual(r.returncode, 1)
        log = json.loads(r.stdout)
        run_ = log["runs"][0]
        self.assertEqual(run_["tool"]["driver"]["rules"][0]["id"], "writes-go-through-store")
        self.assertEqual(run_["tool"]["driver"]["rules"][0]["shortDescription"]["text"], "the store clears the cache")
        res = run_["results"]
        self.assertEqual(len(res), 1)
        self.assertEqual(res[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"], "src/app/view.py")
        self.assertEqual(res[0]["locations"][0]["physicalLocation"]["region"]["startLine"], 5)
        r = run("check", "--format", "github", cwd=self.root)
        self.assertTrue(r.stdout.startswith("::error file=src/app/view.py,line=5,title=seamcheck writes-go-through-store::"),
                        r.stdout)
        self.assertIn("(fix: call save())", r.stdout)

    def test_github_escapes(self):
        line = output.github([{"rule": "r", "severity": "warn", "file": "a,b.py", "line": 3, "message": "50% x\ny"}])
        self.assertEqual(line, "::warning file=a%2Cb.py,line=3,title=seamcheck r::50%25 x%0Ay")

    def test_a_closed_pipe_is_not_a_crash(self):
        with open(os.path.join(self.root, "src/app/many.py"), "w") as fh:  # more output than a pipe buffers
            fh.write("".join(f"def function_number_{i}():\n    return {i}\n\n" for i in range(3000)))
        env = dict(os.environ, PYTHONPATH=ROOT)
        p = subprocess.Popen(f"{sys.executable} -m seamcheck find '*' | head -1", shell=True, cwd=self.root, env=env,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        out, err = p.communicate(timeout=60)
        self.assertNotIn("Traceback", err)


if __name__ == "__main__":
    unittest.main()
