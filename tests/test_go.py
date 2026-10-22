"""Go: package-level vars across files, slice / map elements, new(T)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "go.mod": "module example.com/app\n\ngo 1.22\n",
    "cmd.go": """package app

type Command struct {
	commands []*Command
}

func (c *Command) Name() string { return "" }

func (c *Command) Commands() []*Command { return c.commands }

type Other struct{}

func (o *Other) Name() string { return "" }
""",
    "vars_test.go": """package app

var rootCmd = &Command{}
""",
    "use_test.go": """package app

func TestNames(t *testing.T) {
	rootCmd.Name()
	for _, c := range rootCmd.Commands() {
		c.Name()
	}
	cmds := rootCmd.Commands()
	cmds[0].Name()
	b := new(Command)
	b.Name()
}
""",
}


class Go(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        for name, body in FILES.items():
            path = os.path.join(cls.dir.name, name)
            with open(path, "w") as fh:
                fh.write(body)
        cls.p = core.load(cls.dir.name, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def test_every_receiver_is_typed(self):
        fid = next(f for f in self.p.funcs if f.endswith("TestNames"))
        names = [(d, e) for d, _l, e in self.p.edges[fid] if d.endswith(".Name")]
        self.assertEqual(len(names), 4)
        self.assertTrue(all(e and d.endswith("Command.Name") for d, e in names), names)


if __name__ == "__main__":
    unittest.main()
