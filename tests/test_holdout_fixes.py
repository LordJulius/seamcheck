"""Fixes found on repos never used for tuning (`bench/bench.py --holdout`):
C++ parameters of snake_case / namespaced types (`const details::log_msg &msg`,
`memory_buf_t &dest`), namespace-qualified calls (`detail::write(...)`, never
`std::move`), type aliases, and a C# lambda handed to a constructor
(`new MapperConfiguration(cfg => cfg.CreateMap())`) whose interface method an
implementing class inherits from its base."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

CPP = {
    "include/lib/buf.h": """#pragma once
namespace lib {
struct basic_buffer {
  void push_back(char c) {}
};
using memory_buf_t = basic_buffer;
namespace detail {
inline void write(basic_buffer &b) { b.push_back('w'); }
}
struct other { void write() {} };
}
""",
    "include/lib/use.h": """#pragma once
#include "buf.h"
namespace lib {
inline void emit(memory_buf_t &dest, int n) {
  dest.push_back('x');
  detail::write(dest);
  std::move(dest);
}
}
""",
}

CS = {
    "src/Profile.cs": """public interface IProfileExpression
{
    void CreateMap();
}

public interface IMapperConfigurationExpression : IProfileExpression
{
}

public class Profile : IProfileExpression
{
    public void CreateMap()
    {
    }
}

public class MapperConfigurationExpression : Profile, IMapperConfigurationExpression
{
}

public class MapperConfiguration
{
    public MapperConfiguration(System.Action<IMapperConfigurationExpression> configure)
    {
    }
}
""",
    "tests/Spec.cs": """public class Spec
{
    public void Test()
    {
        var config = new MapperConfiguration(cfg => {
            cfg.CreateMap();
        });
    }

    protected MapperConfiguration Make() => new(cfg =>
    {
        cfg.CreateMap();
    });
}
""",
}


def load(files):
    d = tempfile.mkdtemp()
    for name, body in files.items():
        os.makedirs(os.path.dirname(os.path.join(d, name)), exist_ok=True)
        with open(os.path.join(d, name), "w") as fh:
            fh.write(body)
    return core.load(d, use_cache=False)


class HoldoutFixes(unittest.TestCase):
    def test_cpp_snake_case_params_namespaces_and_aliases(self):
        p = load(CPP)
        got = {(p.funcs[dst].id, ex) for dst, _l, ex in p.edges.get("include/lib/use.emit", [])}
        self.assertIn(("include/lib/buf.basic_buffer.push_back", True), got)
        self.assertIn(("include/lib/buf.write", True), got)  # detail::write, not other::write
        self.assertNotIn("include/lib/buf.other.write", {g for g, _ in got})

    def test_csharp_lambda_to_a_constructor_and_inherited_interface_method(self):
        p = load(CS)
        for fid in ("tests/Spec.Test", "tests/Spec.Make"):
            got = {(dst, ex) for dst, _l, ex in p.edges.get(fid, []) if dst.endswith("CreateMap")}
            self.assertEqual(got, {("src/Profile.CreateMap", True)}, fid)


if __name__ == "__main__":
    unittest.main()


PY = {
    "app/core.py": """class Group:
    def command(self):
        return self


class ConsoleOptions:
    def update_width(self, w):
        return self


def group() -> "Group | None":
    return Group()


def make() -> "Group":
    return Group()
""",
    "tests/test_cli.py": """from app.core import group


def test_group():
    @group()
    def cli():
        pass

    @cli.command()
    def sub():
        pass
""",
    "app/render.py": """def render(console: "Console", options: "ConsoleOptions") -> "Group":
    return options.update_width(3)


if __name__ == "__main__":
    from app.core import make
    g = make()
    g.command()
""",
}


class PythonHoldoutFixes(unittest.TestCase):
    def test_decorated_value_quoted_annotations_and_main_block(self):
        p = load(PY)
        self.assertIn(("app/core.Group.command", True), {(d, e) for d, _l, e in p.edges.get("tests/test_cli.test_group", [])})
        self.assertIn(("app/core.ConsoleOptions.update_width", True), {(d, e) for d, _l, e in p.edges.get("app/render.render", [])})
        self.assertIn(("app/core.Group.command", True), {(d, e) for d, _l, e in p.edges.get("app/render.<top>", [])})


RS = {
    "Cargo.toml": "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    "src/lib.rs": """pub mod arg;
pub mod filter;
""",
    "src/arg.rs": """pub struct Arg;

impl Arg {
    pub fn new(name: &str) -> Self {
        Arg
    }
    pub fn action(self, a: u8) -> Self {
        self
    }
}

#[macro_export]
macro_rules! arg {
    ($name:ident) => {
        $crate::arg::Arg::new(stringify!($name))
    };
}
""",
    "src/filter.rs": """pub struct TimeFilter;

impl TimeFilter {
    pub fn after(s: &str) -> Option<Self> {
        Some(TimeFilter)
    }
    pub fn applies_to(&self, t: u64) -> bool {
        true
    }
}
""",
    "tests/builder/main.rs": "mod utils;\nmod opts;\n",
    "tests/builder/utils.rs": "pub fn assert_output(s: &str) {}\n",
    "tests/derive/main.rs": "mod utils;\n",
    "tests/derive/utils.rs": "pub fn assert_output(s: &str) {}\n",
    "tests/builder/opts.rs": """use crate::utils;
use app::filter::TimeFilter;

fn check() {
    utils::assert_output("x");
    let a = arg!(x).action(1);
    TimeFilter::after("1s").unwrap().applies_to(3);
    let f = make();
    let f = f.into_thing();
}
""",
}


class RustHoldoutFixes(unittest.TestCase):
    def test_crate_local_modules_macro_values_and_option_self(self):
        p = load(RS)
        got = {(d, e) for d, _l, e in p.edges.get("tests/builder/opts.check", [])}
        self.assertIn(("tests/builder/utils.assert_output", True), got)
        self.assertNotIn("tests/derive/utils.assert_output", {d for d, _ in got})
        self.assertIn(("src/arg.action", True), got)
        self.assertIn(("src/filter.TimeFilter.applies_to", True), got)


GO = {
    "go.mod": "module example.com/app\n\ngo 1.22\n",
    "routergroup.go": "package gin\n\ntype RouterGroup struct {\n\tbase string\n}\n\nfunc (g *RouterGroup) GET(p string) {\n}\n",
    "gin.go": "package gin\n\ntype Engine struct {\n\tRouterGroup\n\ttrees int\n}\n\nfunc New() *Engine {\n\treturn &Engine{}\n}\n\nfunc (e *Engine) Run() error {\n\treturn nil\n}\n",
    "gin_test.go": "package gin\n\nfunc TestX(t *testing.T) {\n\trouter := New()\n\trouter.GET(\"/\")\n\t(&Engine{}).Run()\n}\n",
}


class GoHoldoutFixes(unittest.TestCase):
    def test_embedded_struct_methods_and_composite_literal_receivers(self):
        p = load(GO)
        got = {(d, e) for d, _l, e in p.edges.get("gin_test.TestX", [])}
        self.assertIn(("routergroup.RouterGroup.GET", True), got)
        self.assertIn(("gin.Engine.Run", True), got)


JS = {
    "package.json": '{"name": "app", "dependencies": {"delegates": "1"}}',
    "lib/response.js": "module.exports = {\n  back (alt) {\n    return alt\n  },\n  get body () {\n    return 1\n  }\n}\n",
    "lib/context.js": "const delegate = require('delegates')\nconst proto = module.exports = {\n  throw (s) {\n    return s\n  }\n}\n"
                      "delegate(proto, 'response')\n  .method('back')\n  .access('body')\n",
    "lib/application.js": "const context = require('./context')\nconst response = require('./response')\n"
                          "module.exports = class Application {\n  constructor () {\n    this.context = Object.create(context)\n"
                          "    this.response = Object.create(response)\n  }\n\n  createContext (req) {\n"
                          "    const context = Object.create(this.context)\n    context.response = Object.create(this.response)\n"
                          "    return context\n  }\n}\n",
    "test-helpers/context.js": "const Koa = require('../lib/application')\n\nmodule.exports = (req, app) => {\n"
                               "  app = app || new Koa()\n  return app.createContext(req)\n}\n",
    "test/back.test.js": "const context = require('../test-helpers/context')\n\nit('goes back', () => {\n"
                         "  const ctx = context({})\n  ctx.back()\n  ctx.throw(400)\n})\n",
}


class JsHoldoutFixes(unittest.TestCase):
    def test_object_create_expando_properties_and_delegates(self):
        p = load(JS)
        fid = next(k for k in p.edges if k.startswith("test/back.test."))
        got = {(d, e) for d, _l, e in p.edges[fid]}
        self.assertIn(("lib/response.back", True), got)
        self.assertIn(("lib/context.throw", True), got)


JS2 = {
    "lib/core/InterceptorManager.js": "class InterceptorManager {\n  use (fn) {\n    return 1\n  }\n}\nexport default InterceptorManager\n",
    "lib/core/Axios.js": "import InterceptorManager from './InterceptorManager.js'\n\nclass Axios {\n  constructor () {\n"
                         "    this.interceptors = {\n      request: new InterceptorManager(),\n    }\n  }\n\n  request () {\n    return 1\n  }\n}\n"
                         "export default Axios\n",
    "lib/axios.js": "import Axios from './core/Axios.js'\nimport utils from './utils.js'\n\nfunction createInstance () {\n"
                    "  const context = new Axios()\n  const instance = bind(Axios.prototype.request, context)\n"
                    "  utils.extend(instance, Axios.prototype, context)\n  return instance\n}\n\nconst axios = createInstance()\n"
                    "export default axios\n",
    "lib/utils.js": "function extend (a, b) {\n  return a\n}\nexport default { extend }\n",
    "index.js": "import axios from './lib/axios.js'\nexport { axios as default }\n",
    "test/i.test.js": "import axios from '../index.js'\n\nit('uses', () => {\n  axios.interceptors.request.use(() => 1)\n"
                      "  /refused/i.test('x')\n  setTimeout(() => 1)\n})\n",
}


class JsMixinFixes(unittest.TestCase):
    def test_extend_prototype_object_literal_fields_and_default_import_values(self):
        p = load(JS2)
        fid = next(k for k in p.edges if k.startswith("test/i.test."))
        got = {(d, e) for d, _l, e in p.edges[fid]}
        self.assertIn(("lib/core/InterceptorManager.use", True), got)
        self.assertEqual({d for d, _ in got} & {"lib/utils.test"}, set())


TS = {
    "src/types.ts": "export type Resp = {\n  text: () => Promise<string>;\n  blob(): Promise<Blob>;\n} & Promise<Response>;\n"
                    "export type Inst = {\n  (url: string): Resp;\n  get: (url: string) => Resp;\n  extend: (o: object) => Inst;\n};\n"
                    "export type Server = {\n  url: string;\n} & express.Express;\n",
    "src/index.ts": "import type {Inst} from './types.js';\n\nconst make = (): Inst => {\n  return null as any;\n};\n"
                    "const ky = make();\nexport default ky;\n",
    "src/other.ts": "export function text() {\n  return 1;\n}\n",
    "test/t.test.ts": "import ky from '../src/index.js';\nimport type {Server} from '../src/types.js';\n\n"
                      "test('x', (server: Server) => {\n  const api = ky.extend({});\n  api('u').text();\n  ky.get('u').text();\n"
                      "  server.post('/', (req, res) => { res.status(500); });\n});\n",
}


class TsTypeLiteralFixes(unittest.TestCase):
    def test_object_type_aliases_are_interfaces_with_member_signatures(self):
        p = load(TS)
        got = {d for k, es in p.edges.items() if k.startswith("test/t.test") for d, _l, _e in es}
        self.assertNotIn("src/other.text", got)
        self.assertIn("test/t.test.test x", p.funcs)
