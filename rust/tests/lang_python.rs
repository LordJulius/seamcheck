//! The Python adapter and its resolution, ported from the Python suite
//! (test_languages.py's Python and PythonPackages cases, test_module_level.py's
//! decorators and `sessions.Session()`, test_holdout_fixes.py's
//! PythonHoldoutFixes). A test that needs a receiver's type (`self.store.save`,
//! `with pkg.Client() as client`) waits for the infer lane and is `#[ignore]`d
//! with the reason; the rest of its assertions run in a sibling test.

mod common;

use std::collections::BTreeSet;

use common::write;
use seamcheck::graph::Graph;
use seamcheck::project::Project;

fn load(files: &[(&str, &str)]) -> (tempfile::TempDir, Graph) {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), files);
    write(d.path(), &[("seamcheck.toml", "[project]\nname = \"langs\"\n")]);
    let p = Project::load(d.path(), &[]);
    let g = Graph::load(&p);
    (d, g)
}

fn exact(g: &Graph, fid: &str) -> BTreeSet<String> {
    g.edges.get(fid).map(|es| es.iter().filter(|e| e.2).map(|e| e.0.clone()).collect()).unwrap_or_default()
}

fn guessed(g: &Graph, fid: &str) -> BTreeSet<String> {
    g.edges.get(fid).map(|es| es.iter().filter(|e| !e.2).map(|e| e.0.clone()).collect()).unwrap_or_default()
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

const SHOP: &[(&str, &str)] = &[
    ("src/shop/__init__.py", "from .cart import Cart, total\nfrom .util import *\n"),
    ("src/shop/util.py", "def fmt(x):\n    return str(x)\n"),
    (
        "src/shop/base.py",
        "class Model:\n    def get(self):\n        return self.load()\n\n    def load(self):\n        return None\n",
    ),
    (
        "src/shop/cart.py",
        "import json\nfrom .base import Model\nfrom . import util\n\n\nclass Cart(Model):\n    def __init__(self):\n        \
         self.store = Store()\n\n    def get(self):\n        raw = super().get()\n        self.store.save(raw)\n        \
         return util.fmt(total(raw))\n\n    def dump(self):\n        return json.dumps(self.get())\n\n\nclass Store:\n    \
         def save(self, x):\n        return x\n\n    def get(self):\n        return 1\n\n\ndef total(x):\n    return x\n\n\n\
         def register(session):\n    def handler(e):\n        return e\n    session.total(handler)\n\n\napp = Cart()\n\
         if __name__ == \"__main__\":\n    app.dump()\n",
    ),
    ("tests/fixtures/shop.py", "def total(x):\n    return 0\n"),
    (
        "tests/test_cart.py",
        "import shop\nfrom shop import Cart\n\n\ndef test_total():\n    assert shop.total(1) == 1\n    assert shop.fmt(1) == \"1\"\n\n\n\
         class TestCart:\n    def test_get(self):\n        c = Cart()\n        c.get()\n",
    ),
];

// test_languages.py: Python.test_same_named_methods_are_two_functions
#[test]
fn same_named_methods_are_two_functions() {
    let (_d, g) = load(SHOP);
    assert!(g.funcs.contains_key("src/shop/cart.Cart.get"));
    assert!(g.funcs.contains_key("src/shop/cart.Store.get"));
    assert_eq!(g.func("src/shop/cart.Store.get").unwrap().start, 23);
}

// test_languages.py: Python.test_self_super_and_typed_attribute (what needs
// no receiver chain: the typed attribute, module calls, self, the parent class)
#[test]
fn self_module_calls_and_typed_attribute() {
    let (_d, g) = load(SHOP);
    let got = exact(&g, "src/shop/cart.Cart.get");
    for want in ["src/shop/cart.Store.save", "src/shop/util.fmt", "src/shop/cart.total"] {
        assert!(got.contains(want), "{want} in {got:?}");
    }
    assert!(exact(&g, "src/shop/base.Model.get").contains("src/shop/base.Model.load"));
    // Model.get's self.load() may run Cart's override too: not here, Cart has none
    assert_eq!(g.modules["src/shop/cart.Cart"].parent, "src/shop/base.Model");
}

// test_languages.py: Python.test_self_super_and_typed_attribute
#[test]
#[ignore = "`super().get()` is a receiver chain (\"super()\"): the infer lane's"]
fn super_call_reaches_the_parent() {
    let (_d, g) = load(SHOP);
    let got = exact(&g, "src/shop/cart.Cart.get");
    assert!(got.contains("src/shop/base.Model.get"), "{got:?}");
}

// test_languages.py: Python.test_constructor_and_module_level_code
#[test]
fn constructor_and_module_level_code() {
    let (_d, g) = load(SHOP);
    // (no __init__: `Store()` builds it, no edge)
    assert!(g.modules.contains_key("src/shop/cart.Store"));
    assert!(exact(&g, "src/shop/cart.<top>").contains("src/shop/cart.Cart.__init__"));
}

// test_languages.py: Python.test_library_calls_reach_nothing
#[test]
fn library_calls_reach_nothing() {
    let (_d, g) = load(SHOP);
    assert_eq!(exact(&g, "src/shop/cart.Cart.dump"), set(&["src/shop/cart.Cart.get"]));
    assert!(guessed(&g, "src/shop/cart.Cart.dump").is_empty());
}

// test_languages.py: Python.test_nested_def_and_attribute_calls
#[test]
fn nested_def_and_attribute_calls() {
    // `def handler(e):` is no call to handler; `session.total()` is a method
    // of something unknown, not the module-level total
    let (_d, g) = load(SHOP);
    assert!(exact(&g, "src/shop/cart.register").is_empty());
}

// test_languages.py: Python.test_pytest_functions_and_package_reexports
#[test]
fn pytest_functions_and_package_reexports() {
    let (_d, g) = load(SHOP);
    let t = g.func("tests/test_cart.test_total").unwrap();
    assert_eq!(t.kind, "test");
    assert_eq!(exact(&g, &t.id), set(&["src/shop/cart.total", "src/shop/util.fmt"]));
    let m = g.func("tests/test_cart.TestCart.test_get").unwrap();
    assert_eq!(m.kind, "test");
    assert_eq!(exact(&g, &m.id), set(&["src/shop/cart.Cart.__init__", "src/shop/cart.Cart.get"]));
}

const PKG: &[(&str, &str)] = &[
    ("pkg/__init__.py", "from ._client import *\n"),
    (
        "pkg/_client.py",
        "__all__ = ['Client']\n\nclass Base:\n    def send(self):\n        pass\n\nclass Client(Base):\n    \
         def get(self, **kwargs):\n        kwargs.get('x')\n        super().send()\n        return vars(self).items()\n",
    ),
    ("tests/test_c.py", "import pkg\n\ndef test_get():\n    with pkg.Client() as client:\n        client.get()\n"),
];

// test_languages.py: PythonPackages.test_a_package_exported_class
#[test]
#[ignore = "`with pkg.Client() as client` needs the infer lane's context-manager types"]
fn a_package_exported_class() {
    let (_d, g) = load(PKG);
    assert_eq!(exact(&g, "tests/test_c.test_get"), set(&["pkg/_client.Client.get"]));
}

// test_languages.py: PythonPackages.test_b_super_kwargs_and_builtins
#[test]
#[ignore = "`super().send()` is a receiver chain (\"super()\"): the infer lane's"]
fn super_send_reaches_the_base() {
    let (_d, g) = load(PKG);
    assert_eq!(exact(&g, "pkg/_client.Client.get"), set(&["pkg/_client.Base.send"]));
    // kwargs.get / vars().items: Python's own
    assert!(guessed(&g, "pkg/_client.Client.get").is_empty());
}

const DECOS: &[(&str, &str)] = &[
    ("pkg/sessions.py", "class Session:\n    def __init__(self):\n        self.a = 1\n"),
    ("pkg/api.py", "from . import sessions\n\n\ndef request():\n    with sessions.Session() as s:\n        return s\n"),
    ("pkg/__init__.py", ""),
    (
        "pkg/scaffold.py",
        "def setupmethod(f):\n    return f\n\n\nclass Scaffold:\n    @setupmethod\n    def route(self, rule):\n        \
         def decorator(f):\n            self.add(rule, f)\n            return f\n\n        return decorator\n\n    \
         def add(self, rule, f):\n        pass\n",
    ),
    ("pkg/views.py", "from .scaffold import Scaffold\n\napp = Scaffold()\n\n\n@app.route(\"/\")\ndef index():\n    return \"hi\"\n"),
];

fn edges_of(g: &Graph, fid: &str) -> BTreeSet<(String, bool)> {
    g.edges.get(fid).map(|es| es.iter().map(|(t, _, e)| (t.clone(), *e)).collect()).unwrap_or_default()
}

// test_module_level.py: JsTop.test_python_decorators_are_calls
#[test]
fn python_decorators_are_calls() {
    let (_d, g) = load(DECOS);
    assert!(edges_of(&g, "pkg/scaffold.<top>").contains(&("pkg/scaffold.setupmethod".to_string(), true)));
    // `@app.route("/")` runs route (the receiver `app` is only a guess without its type)
    assert!(edges_of(&g, "pkg/views.<top>").iter().any(|(t, _)| t == "pkg/scaffold.Scaffold.route"));
}

// test_module_level.py: JsTop.test_python_decorators_are_calls
#[test]
#[ignore = "`app = Scaffold()` then `@app.route(...)` needs the infer lane's local types"]
fn python_decorator_factory_closure() {
    let (_d, g) = load(DECOS);
    let got = edges_of(&g, "pkg/views.<top>");
    assert!(got.contains(&("pkg/scaffold.Scaffold.route".to_string(), true)), "{got:?}");
    // (the closure route returns gets index)
    assert!(got.contains(&("pkg/scaffold.decorator".to_string(), true)), "{got:?}");
}

// test_module_level.py: JsTop.test_python_constructor_through_its_module
#[test]
fn python_constructor_through_its_module() {
    let (_d, g) = load(DECOS);
    assert!(edges_of(&g, "pkg/api.request").contains(&("pkg/sessions.Session.__init__".to_string(), true)));
}

// test_holdout_fixes.py: PythonHoldoutFixes.test_decorated_value_quoted_annotations_and_main_block
#[test]
#[ignore = "decorated values, quoted annotations and `g = make()` locals need the infer lane"]
fn decorated_value_quoted_annotations_and_main_block() {
    let (_d, g) = load(&[
        (
            "app/core.py",
            "class Group:\n    def command(self):\n        return self\n\n\nclass ConsoleOptions:\n    \
             def update_width(self, w):\n        return self\n\n\ndef group() -> \"Group | None\":\n    return Group()\n\n\n\
             def make() -> \"Group\":\n    return Group()\n",
        ),
        (
            "tests/test_cli.py",
            "from app.core import group\n\n\ndef test_group():\n    @group()\n    def cli():\n        pass\n\n    \
             @cli.command()\n    def sub():\n        pass\n",
        ),
        (
            "app/render.py",
            "def render(console: \"Console\", options: \"ConsoleOptions\") -> \"Group\":\n    return options.update_width(3)\n\n\n\
             if __name__ == \"__main__\":\n    from app.core import make\n    g = make()\n    g.command()\n",
        ),
    ]);
    assert!(edges_of(&g, "tests/test_cli.test_group").contains(&("app/core.Group.command".to_string(), true)));
    assert!(edges_of(&g, "app/render.render").contains(&("app/core.ConsoleOptions.update_width".to_string(), true)));
    assert!(edges_of(&g, "app/render.<top>").contains(&("app/core.Group.command".to_string(), true)));
}

// (no Python test: the import forms and class heads core resolves, pinned
// here since parity --parse proves the adapter against Python's)
#[test]
fn imports_and_class_heads_as_the_adapter_writes_them() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        &[(
            "pkg/a.py",
            "import os.path as osp, json\nimport a.b.c\nfrom .x import y as z, w\nfrom .. import q\n\
             from m import (\n    k,\n    l as ll,\n)\nfrom star import *\n\n\
             class Child(Base, Mixin[int], metaclass=Meta):\n    pass\n\nclass Plain(object):\n    pass\n",
        )],
    );
    let ms = seamcheck::lang::adapter_for("pkg/a.py")
        .unwrap()
        .parse(&seamcheck::lang::ParseCtx { root: d.path(), res: &Default::default() }, "pkg/a.py", &std::fs::read_to_string(d.path().join("pkg/a.py")).unwrap());
    let f = &ms[0];
    assert_eq!(f.name, "pkg/a");
    assert_eq!(f.aliases["osp"], "py:pkg\0os.path");
    assert_eq!(f.aliases["json"], "py:pkg\0json");
    assert_eq!(f.aliases["a"], "py:pkg\0a");
    assert_eq!(f.aliases["c"], "py:pkg\0a.b.c");
    assert_eq!(f.imported["z"], ("py:pkg\0.x".to_string(), "y".to_string()));
    assert_eq!(f.imported["w"], ("py:pkg\0.x".to_string(), "w".to_string()));
    assert_eq!(f.imported["q"], ("py:pkg\0..".to_string(), "q".to_string()));
    assert_eq!(f.imported["k"], ("py:pkg\0m".to_string(), "k".to_string()));
    assert_eq!(f.imported["ll"], ("py:pkg\0m".to_string(), "l".to_string()));
    assert_eq!(f.uses, vec!["py:pkg\0star".to_string()]);
    assert_eq!(f.reexports, f.uses);
    let child = ms.iter().find(|m| m.name == "pkg/a.Child").unwrap();
    assert_eq!(child.parent, "py:Base");
    assert_eq!(child.bases, vec!["py:Base".to_string(), "py:Mixin".to_string()]);
    assert_eq!(child.owner, "pkg/a");
    let plain = ms.iter().find(|m| m.name == "pkg/a.Plain").unwrap();
    assert_eq!(plain.parent, "");
}
