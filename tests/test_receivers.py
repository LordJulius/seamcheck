"""A bare call never reaches a method in languages where a method needs its
receiver (cobra's `func (c *Command) MarkFlagRequired` calling the package's
`MarkFlagRequired`), but does reach a free function of the file (Rust's
`mod tests` helper). A TS method whose generic parameters span lines is a
method (hono's `route<\\n SubPath ...\\n>(`)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "go.mod": "module example.com/app\n\ngo 1.22\n",
    "flags.go": """package app

type Command struct{}

func (c *Command) MarkFlagRequired(name string) error {
	return MarkFlagRequired(name)
}

func MarkFlagRequired(name string) error {
	return nil
}
""",
    "Cargo.toml": '[package]\nname = "app"\nversion = "0.1.0"\n',
    "src/lib.rs": """pub struct Ranges;

impl Ranges {
    pub fn ranges(&self) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(rs: &[&str]) -> usize {
        rs.len()
    }

    #[test]
    fn simple() {
        let n = ranges(&["1:2"]);
    }
}
""",
    "package.json": '{"name": "t", "type": "module"}',
    "src/base.ts": """export class Base {
  route<
    SubPath extends string,
    SubEnv,
  >(
    path: SubPath,
  ): Base {
    return this
  }
}
""",
    "src/use.ts": """import { Base } from "./base";

export function mount(app: Base) {
  app.route("/x");
}
""",
}


class Receivers(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        for name, body in FILES.items():
            path = os.path.join(cls.dir.name, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cls.p = core.load(cls.dir.name, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def _edges(self, suffix):
        [fid] = [f for f in self.p.funcs if f.endswith(suffix)]
        return {(d, e) for d, _l, e in self.p.edges.get(fid, [])}

    def test_go_bare_call_is_the_package_function(self):
        got = self._edges("Command.MarkFlagRequired")
        self.assertIn(("flags.MarkFlagRequired", True), got)
        self.assertNotIn(("flags.Command.MarkFlagRequired", True), got)

    def test_rust_bare_call_reaches_the_test_module_helper(self):
        self.assertTrue(any(d.endswith(".ranges") and "Ranges.ranges" not in d and e for d, e in self._edges(".simple")))

    def test_ts_multiline_generic_method(self):
        self.assertIn("src/base.route", self.p.funcs)  # (a file named after its one class is that class)
        self.assertEqual(self._edges("src/use.mount"), {("src/base.route", True)})


if __name__ == "__main__":
    unittest.main()
