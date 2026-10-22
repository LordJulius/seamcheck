"""Per-language parsing and call resolution (python3 -m unittest discover tests).

Each case was a wrong answer running seamcheck on open-source repos:
- a Java method or a TypeScript class method ended at its first statement (the
  declaration regex took the body's `{`, so the scan stopped at the next `;`):
  gson's 3,000 methods made 5,300 calls instead of 23,600;
- C# methods with the brace on the next line (the C# default) were not
  functions at all: Polly parsed 802 files into 0 functions;
- a C# `Task<int> Run(` read its return type as a call to Task;
- Java/C# locals and fields (`Foo foo`, `var x = new Foo()`) were not types,
  so every method call on them was a name guess;
- xUnit `[Fact]`, NUnit `[Test]` and Rust `#[test]` functions were not tests;
- Python: two classes' `get` in one file overwrote each other (flask lost 300
  of 1,500 functions), no import was read (`flask.url_for` through the
  package's `__init__` re-export was a guess), `self.x()` was a guess, pytest
  functions were not tests, and module-level code (`app = create()`, a
  decorator, `if __name__ == "__main__": main()`) called nothing; a test
  fixture named `shop.py` won `import shop` over `src/shop/`.
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402


def load(files: dict[str, str]):
    d = tempfile.TemporaryDirectory()
    for name, body in files.items():
        path = os.path.join(d.name, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as fh:
            fh.write(body)
    cfg = os.path.join(d.name, "seamcheck.toml")
    if not os.path.exists(cfg):
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "langs"\n')
    return d, core.load(d.name, cfg, use_cache=False)


class Base(unittest.TestCase):
    FILES: dict[str, str] = {}

    @classmethod
    def setUpClass(cls):
        cls.dir, cls.p = load(cls.FILES)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def exact(self, fid):
        return {dst for dst, _, ex in self.p.edges.get(fid, []) if ex}

    def guessed(self, fid):
        return {dst for dst, _, ex in self.p.edges.get(fid, []) if not ex}


class BraceBodies(Base):
    FILES = {
        "src/a/Store.java": """package a;

public class Store {
    private final Index index = new Index();

    @Override
    public void put(String key,
                    int value) throws IOException {
        int y = 1;
        check(y);
        if (value > 0) {
            index.add(key);
        }
        flush();
    }
    private void check(int y) { }
    private void flush() { }
}
""",
        "src/a/Index.java": "package a;\n\npublic class Index {\n    public void add(String k) {\n        k.trim();\n    }\n}\n",
        "src/Cs/Pipeline.cs": """namespace Cs.Core;

public sealed class Pipeline : Base
{
    private readonly Registry _registry;

    public Pipeline(int x) : base(x)
    {
        Init();
    }

    public async Task<int> Run<T>(int x,
        string y) where T : class
    {
        Init();
        if (x > 1)
        {
            _registry.Lookup(y);
        }
        return x;
    }

    int Twice(int y) => y + Run<int>(y, "a").Result;

    void Init() { }
}
""",
        "src/Cs/Registry.cs": "namespace Cs.Core\n{\n    public class Registry\n    {\n        public void Lookup(string k)\n        {\n        }\n    }\n}\n",
        "test/Cs/PipelineTests.cs": """using Cs.Core;

public class PipelineTests
{
    [Fact]
    public void Runs()
    {
        var p = new Pipeline(1);
        p.Run<object>(1, "x");
    }
}
""",
        "web/store.ts": """export class Store {
  put(x: number) {
    const y = 1;
    this.check(y);
    if (x) {
      this.flush();
    }
  }
  check(y: number) {}
  flush() {}
}
""",
    }

    def test_java_method_body_runs_to_its_closing_brace(self):
        f = self.p.funcs["src/a/Store.put"]
        self.assertEqual((f.start, f.end), (7, 15))
        self.assertTrue({"src/a/Store.check", "src/a/Store.flush", "src/a/Index.add"} <= self.exact("src/a/Store.put"))

    def test_ts_class_method_body(self):
        f = self.p.funcs["web/store.put"]
        self.assertEqual((f.start, f.end), (2, 8))
        self.assertEqual(self.exact("web/store.put"), {"web/store.check", "web/store.flush"})

    def test_csharp_allman_methods(self):
        names = {f.name for f in self.p.by_file["src/Cs/Pipeline.cs"][0].funcs}
        self.assertTrue({"Pipeline", "Run", "Twice", "Init"} <= names)
        run = self.p.funcs["src/Cs/Pipeline.Run"]
        self.assertEqual((run.start, run.end), (12, 21))
        self.assertNotIn("Task", {c.name for c in run.calls})
        self.assertIn("src/Cs/Registry.Lookup", self.exact("src/Cs/Pipeline.Run"))
        self.assertIn("src/Cs/Pipeline.Run", self.exact("src/Cs/Pipeline.Twice"))

    def test_csharp_tests_and_typed_locals(self):
        t = self.p.funcs["test/Cs/PipelineTests.Runs"]
        self.assertEqual(t.kind, "test")
        self.assertIn("src/Cs/Pipeline.Run", self.exact(t.id))


class Python(Base):
    FILES = {
        "src/shop/__init__.py": "from .cart import Cart, total\nfrom .util import *\n",
        "src/shop/util.py": "def fmt(x):\n    return str(x)\n",
        "src/shop/base.py": """class Model:
    def get(self):
        return self.load()

    def load(self):
        return None
""",
        "src/shop/cart.py": """import json
from .base import Model
from . import util


class Cart(Model):
    def __init__(self):
        self.store = Store()

    def get(self):
        raw = super().get()
        self.store.save(raw)
        return util.fmt(total(raw))

    def dump(self):
        return json.dumps(self.get())


class Store:
    def save(self, x):
        return x

    def get(self):
        return 1


def total(x):
    return x


app = Cart()
if __name__ == "__main__":
    app.dump()
""",
        "tests/fixtures/shop.py": "def total(x):\n    return 0\n",
        "tests/test_cart.py": """import shop
from shop import Cart


def test_total():
    assert shop.total(1) == 1
    assert shop.fmt(1) == "1"


class TestCart:
    def test_get(self):
        c = Cart()
        c.get()
""",
    }

    def test_same_named_methods_are_two_functions(self):
        self.assertIn("src/shop/cart.Cart.get", self.p.funcs)
        self.assertIn("src/shop/cart.Store.get", self.p.funcs)
        self.assertEqual(self.p.funcs["src/shop/cart.Store.get"].start, 23)

    def test_self_super_and_typed_attribute(self):
        got = self.exact("src/shop/cart.Cart.get")
        self.assertTrue({"src/shop/base.Model.get", "src/shop/cart.Store.save", "src/shop/util.fmt",
                         "src/shop/cart.total"} <= got, got)
        self.assertIn("src/shop/base.Model.load", self.exact("src/shop/base.Model.get"))
        # Model.get's self.load() may run Cart's override too: not here, Cart has none
        self.assertEqual(self.p.modules["src/shop/cart.Cart"].parent, "src/shop/base.Model")

    def test_constructor_and_module_level_code(self):
        self.assertIn("src/shop/cart.Store", self.p.modules)  # (no __init__: `Store()` builds it, no edge)
        self.assertIn("src/shop/cart.Cart.__init__", self.exact("src/shop/cart.<top>"))

    def test_library_calls_reach_nothing(self):
        self.assertEqual(self.exact("src/shop/cart.Cart.dump"), {"src/shop/cart.Cart.get"})
        self.assertEqual(self.guessed("src/shop/cart.Cart.dump"), set())

    def test_pytest_functions_and_package_reexports(self):
        t = self.p.funcs["tests/test_cart.test_total"]
        self.assertEqual(t.kind, "test")
        self.assertEqual(self.exact(t.id), {"src/shop/cart.total", "src/shop/util.fmt"})
        m = self.p.funcs["tests/test_cart.TestCart.test_get"]
        self.assertEqual(m.kind, "test")
        self.assertEqual(self.exact(m.id), {"src/shop/cart.Cart.__init__", "src/shop/cart.Cart.get"})


if __name__ == "__main__":
    unittest.main()
