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


ZOD = {
    "src/types.ts": "export class ZodString {\n  static create = (p?: object): ZodString => {\n    return new ZodString();\n  };\n"
                    "  min(n: number): ZodString {\n    return this;\n  }\n}\nexport enum Kind {\n  ZodString = \"ZodString\",\n}\n"
                    "const stringType = ZodString.create;\nexport { stringType as string };\n",
    "src/coerce.ts": "export function number(): number {\n  return 1;\n}\n",
    "src/external.ts": "export * from \"./types.js\";\nexport * as coerce from \"./coerce.js\";\n",
    "src/index.ts": "import * as z from \"./external.js\";\nexport { z };\n",
    "test/s.test.ts": "import { z } from \"../src/index.js\";\n\ntest(\"s\", () => {\n  z.string().min(1);\n  z.coerce.number();\n});\n",
}


class ZodPatternFixes(unittest.TestCase):
    def test_function_values_namespace_reexports_and_ts_enums(self):
        p = load(ZOD)
        got = {(d, e) for k, es in p.edges.items() if k.startswith("test/s.test") for d, _l, e in es}
        self.assertIn(("src/types.ZodString.create", True), got)
        self.assertIn(("src/types.ZodString.min", True), got)
        self.assertIn(("src/coerce.number", True), got)


CS2 = {
    "src/Ext.cs": "using System;\nusing System.Collections.Generic;\npublic static class Ext\n{\n"
                  "    public static void ShouldBeThrownBy(this Type t, Action a) { }\n"
                  "    public static (List<int> Variables, List<string> Expressions) Scratchpad(this IConfig c) => (null, null);\n}\npublic interface IConfig { }\n",
    "src/Things.cs": "public class Item\n{\n    public void Add(int x) { }\n    public override string ToString() => \"\";\n}\n",
    "tests/Spec.cs": "using System;\npublic class Spec\n{\n    class Source { public int Value; }\n"
                     "    public void Run(int n, string? name, IConfig cfg)\n    {\n        typeof(Spec).ShouldBeThrownBy(() => { });\n"
                     "        n.ToString();\n        string.Join(\",\", name);\n        name.GetType();\n"
                     "        var (vars, statements) = cfg.Scratchpad();\n        statements.Add(\"x\");\n        new Source();\n    }\n}\n"
                     "public class Other\n{\n    class Source { public Source() { } }\n}\n",
}


class CsHoldoutFixes2(unittest.TestCase):
    def test_keyword_types_typeof_tuples_and_nested_types(self):
        p = load(CS2)
        got = {(d, e) for d, _l, e in p.edges.get("tests/Spec.Run", [])}
        self.assertIn(("src/Ext.ShouldBeThrownBy", True), got)
        names = {d for d, _ in got}
        self.assertNotIn("src/Things.Item.ToString", names)  # `n.ToString()` on an int
        self.assertNotIn("src/Things.Item.Add", names)  # `statements` is a List<string>
        self.assertNotIn("tests/Spec.Other.Source.Source", names)  # its own nested Source has no constructor


CPP2 = {
    "include/lib/common.h": "#pragma once\n#include <vector>\n#define LIB_NAMESPACE_BEGIN namespace lib {\n#define LIB_NAMESPACE_END }\n"
                            "LIB_NAMESPACE_BEGIN\nvoid drop(int n);\nLIB_NAMESPACE_END\n",
    "include/lib/registry.h": "#pragma once\n#include <vector>\nnamespace lib {\nclass registry {\npublic:\n    void drop(int n);\n"
                              "    void add(int n);\nprivate:\n    std::vector<int> items_;\n};\n}\n",
    "include/lib/registry-inl.h": "#pragma once\n#include \"registry.h\"\nnamespace lib {\n"
                                  "inline void registry::add(int n) { items_.push_back(n); }\ninline void registry::drop(int n) { }\n}\n",
    "include/lib/common-inl.h": "#pragma once\n#include \"common.h\"\nLIB_NAMESPACE_BEGIN\ninline void drop(int n) { }\nLIB_NAMESPACE_END\n",
    "include/lib/queue.h": "#pragma once\nnamespace lib {\nclass queue {\npublic:\n    void push_back(int n) { }\n};\n}\n",
    "bench/main.cpp": "#include \"lib/common.h\"\nint main() {\n    lib::drop(1);\n    return 0;\n}\n",
}


class CppHoldoutFixes2(unittest.TestCase):
    def test_inl_headers_see_their_fields_and_macro_namespaces(self):
        p = load(CPP2)
        add = {d for d, _l, _e in p.edges.get("include/lib/registry-inl.registry.add", [])}
        self.assertNotIn("include/lib/queue.queue.push_back", add)  # `items_` is a std::vector
        got = {(d, e) for d, _l, e in p.edges.get("bench/main.main", [])}
        self.assertIn(("include/lib/common-inl.drop", True), got)


JS3 = {
    "lib/isAxiosError.js": "export default function isAxiosError(p) {\n  return !!p;\n}\n",
    "lib/Axios.js": "class Axios {\n  request() {\n    return 1;\n  }\n}\nexport default Axios;\n",
    "lib/axios.js": "import Axios from './Axios.js';\nimport isAxiosError from './isAxiosError.js';\n\n"
                    "function createInstance() {\n  const context = new Axios();\n  const instance = bind(context);\n  utils.extend(instance, Axios.prototype);\n"
                    "  instance.create = function create() {\n    return createInstance();\n  };\n  return instance;\n}\n\n"
                    "const axios = createInstance();\naxios.isAxiosError = isAxiosError;\nexport default axios;\n",
    "lib/other.js": "export function isAxiosError() {\n  return 0;\n}\nexport function create() {\n  return 0;\n}\n",
    "test/a.test.js": "import axios from '../lib/axios.js';\n\nit('x', () => {\n  const api = axios.create();\n"
                      "  api.request();\n  axios.isAxiosError(1);\n  run((axios) => axios.go());\n});\n",
}


class JsPropertyFunctionFixes(unittest.TestCase):
    def test_functions_assigned_as_properties(self):
        p = load(JS3)
        got = {(d, e) for k, es in p.edges.items() if k.startswith("test/a.test") for d, _l, e in es}
        self.assertIn(("lib/axios.create", True), got)
        self.assertIn(("lib/Axios.request", True), got)
        self.assertIn(("lib/isAxiosError.isAxiosError", True), got)


CPP3 = {
    "include/lib/registry.h": "#pragma once\nnamespace lib {\nnamespace details {\nclass registry {\npublic:\n"
                              "    static registry &instance();\n    void set_level(int n);\n};\n}\n}\n",
    "include/lib/registry.cpp": "#include \"registry.h\"\nnamespace lib {\nnamespace details {\n"
                                "registry &registry::instance() { static registry r; return r; }\n"
                                "void registry::set_level(int n) { }\n}\n}\n",
    "include/lib/std.h": "#pragma once\nnamespace lib {\nclass logger {\npublic:\n    logger(const char *n) { }\n"
                         "    void set_level(int n) { }\n    void what() { }\n};\n}\n",
    "include/lib/api.cpp": "#include \"registry.h\"\n#include \"std.h\"\n#include <system_error>\nnamespace lib {\n"
                           "void set_level(int n) {\n    details::registry::instance().set_level(n);\n"
                           "    lib::logger oss_logger(\"oss\");\n    oss_logger.set_level(n);\n"
                           "    ::std::string s;\n    std::system_error(n).what();\n}\n}\n",
}


class CppHoldoutFixes3(unittest.TestCase):
    def test_namespace_heads_ctor_declarations_and_std(self):
        p = load(CPP3)
        got = {(d, e) for d, _l, e in p.edges.get("include/lib/api.set_level", [])}
        self.assertTrue(any(d.endswith("registry.set_level") and e for d, e in got), got)
        self.assertTrue(any(d.endswith("logger.set_level") and e for d, e in got), got)
        self.assertFalse(any(d.endswith(".what") for d, _ in got), got)


CPP4 = {
    "include/gt/gtest.h": "#pragma once\n#include <string>\nnamespace testing {\nclass Message {\npublic:\n    Message();\n"
                          "    Message(const Message& m) { }\n    std::string GetString() const;\n};\nclass FilePath {\n"
                          "    bool IsRoot() const;\n    std::string pathname_;\n};\n}\n",
    "src/all.cc": "#include \"gt/gtest.h\"\n#include <cstdio>\nnamespace testing {\nMessage::Message() { }\n"
                  "std::string Message::GetString() const { return \"\"; }\nbool FilePath::IsRoot() const {\n"
                  "    return pathname_.c_str()[0] == 0;\n}\nstd::string Format(int n) {\n    ::std::stringstream ss;\n"
                  "    fclose(nullptr);\n    Message().GetString().c_str();\n    return ss.str();\n}\n}\n",
    "src/string.cc": "namespace other {\nclass string {\npublic:\n    const char *c_str() { return \"\"; }\n    int str() { return 0; }\n};\n}\n",
    "test/mock.cc": "namespace test {\nint fclose(void *f);\n}\nint test::fclose(void *f) { return 0; }\n",
}


class CppHoldoutFixes4(unittest.TestCase):
    def test_std_types_out_of_line_classes_and_unseen_namespaced_functions(self):
        p = load(CPP4)
        got = {(d, e) for k, es in p.edges.items() if k.startswith("src/all") for d, _l, e in es}
        names = {d for d, _ in got}
        self.assertNotIn("src/string.other.string.c_str", names)  # std::string's, never a project `string`
        self.assertNotIn("src/string.other.string.str", names)
        self.assertFalse(any(d.endswith("fclose") for d in names), got)  # libc's
        self.assertTrue(any(d.endswith("Message.Message") and e for d, e in got), got)


CS3 = {
    "src/Lib.csproj": "<Project><ItemGroup><Using Include=\"System.Linq.Expressions.Expression\" Static=\"true\"/></ItemGroup></Project>\n",
    "src/Builder.cs": "public static class Other\n{\n    public static object Default(object t) => t;\n}\n"
                      "public class MappingBase<T>\n{\n    public void ConvertUsing(object t) { }\n}\n"
                      "public interface IMapping { }\n"
                      "public sealed class Mapping(int n) : MappingBase<object>(), IMapping { }\n",
    "src/Use.cs": "public class Use\n{\n    public string Run(Mapping m, object value)\n    {\n        Default(value);\n"
                  "        m.ConvertUsing(value);\n        switch (value)\n        {\n            case 1: return ((byte)value).ToString();\n        }\n"
                  "        return \"\";\n    }\n}\n",
}


class CsHoldoutFixes3(unittest.TestCase):
    def test_using_static_primary_ctor_bases_and_keyword_casts(self):
        p = load(CS3)
        got = {(d, e) for d, _l, e in p.edges.get("src/Use.Run", [])}
        names = {d for d, _ in got}
        self.assertNotIn("src/Builder.Other.Default", names)  # Expression.Default, through the .csproj's static using
        self.assertIn(("src/Builder.MappingBase`1.ConvertUsing", True), got)
        self.assertFalse(any(d.endswith("ToString") for d in names), got)


ZOD2 = {
    "src/types.ts": "export class ZodString {\n  static create = (p?: object): ZodString => {\n    return new ZodString();\n  };\n"
                    "  parse(x: unknown): string {\n    return \"\";\n  }\n}\n"
                    "export const coerce = {\n  string: ((arg) => ZodString.create({ ...arg })) as (typeof ZodString)[\"create\"],\n};\n",
    "src/schemas.ts": "export function ipv6(params?: string): ZodIPv6 {\n  return new ZodIPv6();\n}\n"
                      "export class ZodIPv6 {\n  safeParse(x: unknown) {\n    return 1;\n  }\n}\n"
                      "export const ZodStringProto = {\n    ipv6(params) {\n      return this.check(params);\n    },\n};\n",
    "test/c.test.ts": "import { coerce } from \"../src/types\";\nimport { ipv6 } from \"../src/schemas\";\n\n"
                      "test(\"c\", () => {\n  coerce.string().parse(1);\n  ipv6().safeParse(\"x\");\n});\n",
}


class ZodPatternFixes2(unittest.TestCase):
    def test_object_of_arrows_and_top_level_definition_wins(self):
        p = load(ZOD2)
        got = {(d, e) for k, es in p.edges.items() if k.startswith("test/c.test") for d, _l, e in es}
        self.assertIn(("src/types.ZodString.parse", True), got)
        self.assertIn(("src/schemas.ZodIPv6.safeParse", True), got)


JS4 = {
    "src/utils.js": "export function findIndex(arr, fn) {\n  return -1;\n}\nfunction close(server) {\n  server.close();\n}\n"
                    "export const helpers = { close };\n",
    "src/app.js": "export function run(items, srv) {\n  items.findIndex((x) => x);\n  srv.close();\n}\n",
}


class JsPlainFunctionFixes(unittest.TestCase):
    def test_a_plain_function_declaration_is_no_method(self):
        p = load(JS4)
        names = {d for d, _l, _e in p.edges.get("src/app.run", [])}
        self.assertNotIn("src/utils.findIndex", names)  # Array.prototype.findIndex
        self.assertIn("src/utils.close", names)  # (handed on as `helpers.close`: still a candidate)


GO2 = {
    "go.mod": "module example.com/cli\n\ngo 1.22\n",
    "flag.go": "package cli\n\ntype Flag interface {\n\tNames() []string\n}\n\ntype IntFlag struct{}\n\n"
               "func (f *IntFlag) Names() []string {\n\treturn nil\n}\n\nfunc (f *IntFlag) SchemaType() string {\n\treturn \"int\"\n}\n\n"
               "type SchemaTyper interface {\n\tSchemaType() string\n}\n",
    "other.go": "package cli\n\ntype Other struct{}\n\nfunc (o *Other) Names() []string {\n\treturn nil\n}\n\n"
                "func (o *Other) SchemaType() string {\n\treturn \"\"\n}\n",
    "flag_test.go": "package cli\n\nfunc TestFlags(t *testing.T) {\n\ttests := []struct {\n\t\tname string\n\t\tflag *IntFlag\n\t}{\n"
                    "\t\t{name: \"a\", flag: &IntFlag{}},\n\t}\n\tfor _, tt := range tests {\n\t\ttt.flag.Names()\n\t}\n"
                    "\tst, ok := any(&IntFlag{}).(SchemaTyper)\n\t_ = ok\n\tst.SchemaType()\n}\n",
}


class GoHoldoutFixes2(unittest.TestCase):
    def test_table_test_structs_and_type_assertions(self):
        p = load(GO2)
        got = {(d, e) for d, _l, e in p.edges.get("flag_test.TestFlags", [])}
        self.assertIn(("flag.IntFlag.Names", True), got)
        self.assertNotIn("other.Other.Names", {d for d, _ in got})
        self.assertIn(("flag.IntFlag.SchemaType", True), got)  # SchemaTyper's implementations


KT = {
    "src/Reader.kt": "class Reader {\n  fun peek(): Int = 0\n  private fun nextQuoted(): String = \"\"\n"
                     "  fun next(): String {\n    return when (peek()) {\n      1 -> nextQuoted()\n      else -> nextQuoted()\n    }\n  }\n}\n",
}


class KotlinWhenArrowFixes(unittest.TestCase):
    def test_a_when_arrow_is_no_member_access(self):
        p = load(KT)
        got = {(d, e) for d, _l, e in p.edges.get("src/Reader.next", [])}
        self.assertIn(("src/Reader.nextQuoted", True), got)


RB = {
    "lib/rack/response.rb": "module Rack\n  class Response\n    def write(s)\n      s\n    end\n  end\nend\n",
    "test/spec_pipe.rb": "describe 'x' do\n  it 'writes' do\n    rd, wr = IO.pipe\n    wr.write('a')\n  end\nend\n",
}


class RubyMultiAssignFixes(unittest.TestCase):
    def test_multiple_assignment_from_a_library_constant(self):
        p = load(RB)
        got = {d for es in p.edges.values() for d, _l, _e in es}
        self.assertNotIn("lib/rack/response.Rack::Response.write", got)


JS7 = {
    "package.json": "{\"name\": \"ax\", \"type\": \"module\"}\n",
    "lib/Headers.js": "class Headers {\n  set(h) {\n    function setHeader(v) {\n      return v;\n    }\n    return setHeader(h);\n  }\n"
                      "  has(h) {\n    return true;\n  }\n  static from(x) {\n    return x instanceof this ? x : new this(x);\n  }\n"
                      "  static concat(first, ...rest) {\n    const out = new this(first);\n    rest.forEach((r) => out.set(r));\n    return out;\n  }\n"
                      "  static accessor(n) {\n    return n;\n  }\n}\nexport default Headers;\n",
    "lib/list.js": "export function forEach(xs, fn) {\n  return fn(xs);\n}\n",
    "lib/resolve.js": "import Headers from './Headers.js';\nexport default function resolve(config) {\n  let headers = config.h;\n"
                      "  config.headers = headers = Headers.from(headers);\n  headers.has('a');\n  return config;\n}\n",
    "lib/form.js": "import Headers from './Headers.js';\nfunction fill(headers, k) {\n  headers.has(k);\n}\n"
                   "export function run() {\n  fill(new Headers(), 'a');\n  fill(Headers.from({}), 'b');\n}\n"
                   "const enc = typeof TextEncoder === 'function' ? new TextEncoder() : new util.TextEncoder();\n"
                   "export function encode(s) {\n  return enc.encode(s);\n}\n",
    "lib/http.js": "function direct(a) {\n  return a;\n}\nexport const __direct = direct;\n",
    "lib/platform/node.js": "export function generateString(n) {\n  return n;\n}\nexport default { generateString };\n",
    "lib/platform/index.js": "import node from './node.js';\nexport default {\n  ...node,\n};\n",
    "lib/stream.js": "import platform from './platform/index.js';\nexport function boundary() {\n  return platform.generateString(4);\n}\n",
    "tests/http.test.js": "import { __direct as direct } from '../lib/http.js';\nimport Headers from '../lib/Headers.js';\n"
                          "test('a', () => {\n  direct(1);\n  const h = new Headers();\n  h.constructor.accessor('x');\n  socket.has('x');\n});\n"
                          "test('pollution', () => {\n  Object.prototype.has = function () {\n    return 1;\n  };\n  delete Object.prototype.has;\n});\n",
}


class JsParamFlowAndPatchFixes(unittest.TestCase):
    def test_axios_shapes(self):
        p = load(JS7)
        def got(k):
            return {(d, e) for d, _l, e in p.edges.get(k, [])}
        self.assertIn(("lib/Headers.has", True), got("lib/resolve.resolve"))  # reassigned at its own level
        self.assertIn(("lib/Headers.has", True), got("lib/form.fill"))  # what every caller passes
        self.assertEqual(set(), {d for d, _ in got("lib/form.encode")})  # either branch a library's
        self.assertIn(("lib/Headers.set", True), got("lib/Headers.concat"))  # new this(); a rest parameter is an Array
        self.assertNotIn("lib/list.forEach", {d for d, _ in got("lib/Headers.concat")})
        self.assertIn(("lib/platform/node.generateString", True), got("lib/stream.boundary"))
        t = got("tests/http.test.test a")
        self.assertIn(("lib/http.direct", True), t)
        self.assertIn(("lib/Headers.accessor", True), t)
        self.assertNotIn("tests/http.test.has", {d for d, _ in t})  # another test's prototype patch


ZOD3 = {
    "core/core.ts": "export interface $constructor<T> {\n  init(inst: T): void;\n}\n"
                    "export function $constructor<T>(name: string, initializer: (inst: T) => void, proto?: object): $constructor<T> {\n"
                    "  function init(inst: T) {\n    initializer(inst);\n  }\n  function _(this: any) {\n    init(this);\n  }\n"
                    "  Object.defineProperty(_, \"init\", { value: init });\n  return _ as any;\n}\n",
    "core/index.ts": "export * from './core.js';\nexport * from './schemas.js';\n",
    "core/schemas.ts": "import * as core from './core.js';\nexport interface $ZodType {\n  x: number;\n}\n"
                       "export const $ZodType: core.$constructor<$ZodType> = /*@__PURE__*/ core.$constructor(\n  \"$ZodType\",\n  (inst) => {\n    inst.x = 1;\n  }\n);\n",
    "classic/schemas.ts": "import * as core from '../core/index.js';\nexport interface ZodType extends core.$ZodType {\n  safeParse(d: unknown): boolean;\n}\n"
                          "export const ZodType: core.$constructor<ZodType> = /*@__PURE__*/ core.$constructor(\n  \"ZodType\",\n"
                          "  (inst) => {\n    core.$ZodType.init(inst);\n  },\n  {\n    safeParse(d) {\n      return true;\n    },\n  }\n);\n"
                          "export function check(schema: ZodType) {\n  return schema.safeParse(1);\n}\n",
}


class TsFactoryInterfaceFixes(unittest.TestCase):
    def test_zod_constructor_values_and_interface_members(self):
        p = load(ZOD3)
        got = {(d, e) for k, es in p.edges.items() if k.startswith("classic/schemas") for d, _l, e in es}
        self.assertIn(("core/core.init", True), got)  # `$ZodType.init`: the factory's attached init
        self.assertIn(("classic/schemas.safeParse", True), got)  # ZodType's member, from its prototype object


JS8 = {
    "core/parse.ts": "export const _safeParse = (_Err: any) => (schema: any, value: unknown) => {\n  return schema;\n};\n"
                     "export const validate: V = ((\n  schema: any,\n): boolean => {\n  return true;\n}) as V;\n",
    "core/doc.ts": "export class Doc {\n  compile(): any {\n    return 1;\n  }\n}\n",
    "core/compile.ts": "export function compile(s: any) {\n  return s;\n}\n",
    "core/schemas.ts": "export function make(inst: any) {\n  inst.validate = (x: any) => x;\n}\n",
    "core/index.ts": "export * from './doc.js';\nexport * from './schemas.js';\nexport * from './compile.js';\nexport * from './parse.js';\n",
    "classic/parse.ts": "import * as core from '../core/index.js';\nexport const safeParse: <T>(\n  schema: T,\n  value: unknown\n) => boolean = "
                        "/* @__PURE__ */ core._safeParse(Error);\nexport { validate, compile } from '../core/index.js';\n",
    "tests/a.test.ts": "import * as z from '../classic/parse.js';\nimport { encode } from './enc.js';\n"
                       "test('t', () => {\n  z.safeParse(1, 2);\n  z.validate(1);\n  z.compile(1);\n  encode('x');\n"
                       "  build('/foo', {\n    encode(v) {\n      return v;\n    },\n  });\n});\n",
    "tests/enc.ts": "export function encode(s: string) {\n  return s;\n}\nexport function build(u: string, o: any) {\n  return o;\n}\n",
}


class JsScopeFixes(unittest.TestCase):
    def test_factories_reexports_and_object_members(self):
        p = load(JS8)
        got = {(d, e) for k, es in p.edges.items() if k.startswith("tests/a.test") for d, _l, e in es}
        self.assertIn(("core/parse._safeParse", True), got)  # a factory's closure
        self.assertIn(("core/parse.validate", True), got)  # not `inst.validate = ...` in schemas
        self.assertIn(("core/compile.compile", True), got)  # `export *` hands on Doc, not Doc.compile
        self.assertIn(("tests/enc.encode", True), got)  # not the object literal's encode()
        self.assertNotIn("core/doc.compile", {d for d, _ in got})


RB2 = {
    "lib/sq/config.rb": "module Sq\n  class Config\n    def redis\n      redis_pool.with do |conn|\n        yield conn\n      end\n    end\n"
                        "    def redis_pool\n      Thread.current[:pool] || local_pool\n    end\n"
                        "    def local_pool\n      @pool ||= ConnectionPool.new { 1 }\n    end\n    def logger\n      1\n    end\n  end\nend\n",
    "lib/sq/web/config.rb": "module Sq\n  class Web\n    class Config\n      def logger\n        2\n      end\n    end\n  end\nend\n",
    "lib/sq/web.rb": "module Sq\n  class Web\n    def show(config)\n      config.logger\n    end\n  end\nend\n",
    "lib/sq/job.rb": "module Sq\n  class Job\n    def set(k)\n      k\n    end\n    def run(config)\n      config.logger\n"
                     "      config.redis { |conn| conn.set(1) }\n    end\n  end\nend\n",
    "lib/sq/stats.rb": "module Sq\n  class Stats\n    def processed\n      1\n    end\n  end\nend\n",
    "lib/sq/app.rb": "module Sq\n  class App\n    get '/stats' do\n      stats = Sq::Stats.new\n      stats.processed\n    end\n  end\nend\n",
}


class RubyNamedReceiverAndBlockFixes(unittest.TestCase):
    def test_lexical_constant_lookup_yielded_values_and_top_level_locals(self):
        p = load(RB2)
        def got(k):
            return {(d, e) for d, _l, e in p.edges.get(k, [])}
        self.assertIn(("lib/sq/web/config.Sq::Web::Config.logger", True), got("lib/sq/web.Sq::Web.show"))
        run = got("lib/sq/job.Sq::Job.run")
        self.assertIn(("lib/sq/config.Sq::Config.logger", True), run)
        self.assertIn(("lib/sq/config.Sq::Config.redis", True), run)
        self.assertNotIn("lib/sq/job.Sq::Job.set", {d for d, _ in run})  # conn is the library's connection
        app = {(d, e) for k, es in p.edges.items() if k.startswith("lib/sq/app") for d, _l, e in es}
        self.assertIn(("lib/sq/stats.Sq::Stats.processed", True), app)


SW = {
    "Sources/Net/Task.swift": "class SessionTask {\n    func resume() {}\n    func stop() {}\n}\n",
    "Tests/Dependency/Nocilla/LSNocilla.h": "@interface LSNocilla : NSObject\n+ (LSNocilla *)sharedInstance;\n- (void)stop;\n@end\n",
    "Sources/Cache/Cache.swift": "class Cache {\n    func store() async throws {\n        try await withCheckedThrowingContinuation { continuation in\n"
                                 "            continuation.resume(returning: 1)\n        }\n    }\n    func tearDown() {\n"
                                 "        LSNocilla.sharedInstance().stop()\n    }\n}\n",
}


class SwiftHoldoutFixes(unittest.TestCase):
    def test_library_closures_and_types_named_like_other_languages_files(self):
        p = load(SW)
        got = {d for es in p.edges.values() for d, _l, _e in es}
        self.assertNotIn("Sources/Net/Task.SessionTask.resume", got)
        self.assertNotIn("Sources/Net/Task.SessionTask.stop", got)


GD2 = {
    "project.godot": "[application]\nconfig/name=\"x\"\n",
    "src/PixelCel.gd": "class_name PixelCel\nextends RefCounted\n\nfunc get_image() -> Image:\n\treturn null\n",
    "src/AudioCel.gd": "class_name AudioCel\nextends RefCounted\n\nfunc get_image() -> Image:\n\treturn null\n",
    "src/Project.gd": "class_name Project\nextends RefCounted\n\nfunc get_all_pixel_cels() -> Array[PixelCel]:\n\treturn []\n",
    "src/Algos.gd": "extends Node\n\nfunc crop(project := Project.new()) -> void:\n\tfor cel in project.get_all_pixel_cels():\n"
                    "\t\tcel.get_image()\n",
}


class GdHoldoutFixes(unittest.TestCase):
    def test_inferred_defaults_and_typed_array_returns_in_loops(self):
        p = load(GD2)
        got = {(d, e) for d, _l, e in p.edges.get("Algos.crop", [])}
        self.assertIn(("PixelCel.get_image", True), got)
        self.assertNotIn("AudioCel.get_image", {d for d, _ in got})


JS5 = {
    "test/helpers.js": "import http from 'http';\n\nexport function startServer(handler) {\n  return http.createServer(handler);\n}\n",
    "lib/response.js": "export class Response {\n  end() {\n    return 1;\n  }\n}\n",
    "test/s.test.js": "import { startServer } from './helpers.js';\n\nit('x', () => {\n  startServer((req, res) => {\n    res.end('ok');\n  });\n});\n",
}


class JsForwardedCallbackFixes(unittest.TestCase):
    def test_a_callback_forwarded_to_a_library_gets_its_values(self):
        p = load(JS5)
        got = {d for es in p.edges.values() for d, _l, _e in es}
        self.assertNotIn("lib/response.Response.end", got)


JS6 = {
    "package.json": "{\"name\": \"koa\", \"main\": \"lib/application.js\"}\n",
    "lib/application.js": "module.exports = class Application {\n  constructor () {\n    this.x = 1\n  }\n\n  use (fn) {\n    return this\n  }\n\n"
                          "  static get default () {\n    return Application\n  }\n}\n",
    "test/app.test.js": "const Koa = require('..')\n\nit('x', () => {\n  const app = new Koa()\n  app.use(() => 1)\n})\n",
}


class JsPackageMainFixes(unittest.TestCase):
    def test_require_of_a_folder_reads_its_package_main(self):
        p = load(JS6)
        got = {(d, e) for k, es in p.edges.items() if k.startswith("test/app.test") for d, _l, e in es}
        self.assertIn(("lib/application.constructor", True), got)
        self.assertIn(("lib/application.use", True), got)
        self.assertNotIn("lib/application.default", {d for d, _ in got})


CS4 = {
    "src/SqlMapper.cs": "using System.Data;\npublic static class SqlMapper\n{\n    public static int Execute(this IDbConnection cnn, string sql) => 0;\n"
                        "    public static int Execute(this IDbConnection cnn, string sql, object p) => 0;\n}\n",
    "src/Other.cs": "public class Other\n{\n    public int Execute(string s) => 1;\n}\n",
    "tests/T.cs": "using System.Data.Common;\npublic class T\n{\n    protected DbConnection connection;\n"
                  "    public void Run()\n    {\n        connection.Execute(\"x\");\n    }\n}\n",
}


class CsLibraryInterfaceExtensionFixes(unittest.TestCase):
    def test_extension_on_a_library_interface_the_value_implements(self):
        p = load(CS4)
        got = {(d, e) for d, _l, e in p.edges.get("tests/T.Run", [])}
        self.assertIn(("src/SqlMapper.Execute", True), got)
        self.assertNotIn("src/Other.Execute", {d for d, _ in got})


CPP5 = {
    "include/sinks/base_sink.h": "#pragma once\nnamespace sinks {\ntemplate <typename Mutex>\nclass base_sink {\npublic:\n"
                                 "    void set_pattern(int p);\nprotected:\n    virtual void set_pattern_(int p);\n};\n}\n",
    "include/sinks/base_sink-inl.h": "#pragma once\n#include \"base_sink.h\"\ntemplate <typename Mutex>\n"
                                     "void sinks::base_sink<Mutex>::set_pattern(int p) {\n    set_pattern_(p);\n}\n"
                                     "template <typename Mutex>\nvoid sinks::base_sink<Mutex>::set_pattern_(int p) {\n}\n",
}


class CppTemplateQualifierFixes(unittest.TestCase):
    def test_out_of_line_template_members_are_functions(self):
        p = load(CPP5)
        fid = next(k for k in p.funcs if k.endswith("base_sink.set_pattern"))
        self.assertTrue(any(d.endswith("set_pattern_") and e for d, _l, e in p.edges.get(fid, [])))


TS2 = {
    "package.json": "{\"name\": \"ky\", \"main\": \"./distribution/index.js\"}\n",
    "source/index.ts": "type Inst = { create: (o?: object) => Inst; (url: string): void };\n"
                       "const createInstance = (defaults?: object): Inst => {\n  const ky = ((url: string) => {}) as Inst;\n"
                       "  ky.create = (newDefaults?: object) => createInstance(newDefaults);\n  return ky;\n};\n"
                       "const ky = createInstance();\nexport default ky;\n",
    "source/other.ts": "export class Queue {\n  close() {\n    return 1;\n  }\n}\n",
    "test/t.ts": "import ky from 'ky';\n\ntest('x', () => {\n  const api = ky.create({});\n"
                 "  const s = new ReadableStream<Uint8Array>({\n    start(controller) {\n      controller.close();\n    },\n  });\n});\n",
}


class TsPropertyArrowFixes(unittest.TestCase):
    def test_arrows_stored_as_properties_and_library_options_methods(self):
        p = load(TS2)
        got = {(d, e) for k, es in p.edges.items() if k.startswith("test/t") for d, _l, e in es}
        self.assertIn(("source/index.create", True), got)
        self.assertNotIn("source/other.Queue.close", {d for d, _ in got})


class PossibleCallersTier(unittest.TestCase):
    def test_a_library_typed_call_is_a_possible_caller_not_an_edge(self):
        from seamcheck import queries
        p = load({"Wrapped.cs": "using System.Data;\nnamespace D {\n  public class Wrapped : IDataReader {\n"
                                "    public bool Read() { return true; }\n  }\n  public static class Use {\n"
                                "    public static void Run(IDataReader reader) {\n      reader.Read();\n    }\n  }\n}\n"})
        g = p.find("Wrapped.Read")[0]
        self.assertFalse(any(d == g.id for es in p.edges.values() for d, _l, _e in es))  # no edge
        got = queries.walk(p, "Wrapped.Read", 1, True, wide=True)[0]
        self.assertEqual(1, got["possible"])
        self.assertEqual(8, got["possible_sites"][0]["line"])


class FieldInitializerFixes(unittest.TestCase):
    def test_java_and_csharp_field_initializers_and_nested_generic_returns(self):
        p = load({"src/a/B.java": "package a;\n@Deprecated(since = \"1\")\npublic final class B {\n  private final int n = size();\n"
                                  "  static final B ONE = B.get(\"x\");\n  public static B get(String s) { return new B(); }\n"
                                  "  int size() { return 1; }\n}\n",
                  "src/a/D.cs": "namespace a {\n  [Obsolete(\"x\")]\n  public class D {\n    private static readonly D Shared = Create();\n"
                                "    public int Count => Create().N();\n    public static D Create() { return new D(); }\n"
                                "    public int N() { return 1; }\n"
                                "    protected virtual Expression<Func<A, B, C, D, bool>> Build(Type t)\n    {\n      return Create().Make();\n    }\n"
                                "    public E Make() { return null; }\n  }\n}\n"})
        got = {(k, d, e) for k, es in p.edges.items() for d, _l, e in es}
        self.assertIn(("src/a/B.<init>", "src/a/B.size", True), got)  # a field initializer, in its class
        self.assertIn(("src/a/B.<init>", "src/a/B.get", True), got)
        self.assertIn(("src/a/D.<init>", "src/a/D.Create", True), got)
        self.assertNotIn("src/a/D.N", {d for k, d, _ in got if k.endswith("<init>")})  # an expression-bodied member: not one
        self.assertIn(("src/a/D.Build", "src/a/D.Make", True), got)  # `Expression<Func<A, B, C, D, bool>> Build(`
        self.assertFalse(any(c.name in ("Deprecated", "Obsolete") for f in p.funcs.values() for c in f.calls))


class CsExpressionBodies(unittest.TestCase):
    def test_expression_bodied_members_keep_their_calls(self):
        p = load({"W.cs": "namespace a {\n  internal sealed class W\n  {\n    private W _reader;\n    public bool Read() => _reader.Read();\n"
                          "    private static W Make(int type) =>\n        type > 0 ? Build(type) :\n        Build(type);\n"
                          "    static W Build(int t) { return null; }\n  }\n}\n"})
        self.assertEqual([("Read", 5)], [(c.name, c.line) for c in p.funcs["W.W.Read"].calls] if "W.W.Read" in p.funcs
                         else [(c.name, c.line) for c in p.find("W.Read")[0].calls])
        mk = p.find("W.Make")[0]
        self.assertEqual((6, 8), (mk.start, mk.end))  # (to its `;`, over lines)


class LocalImportAndShorthandFixes(unittest.TestCase):
    def test_python_local_import_shadows_and_js_shorthand_is_no_call(self):
        p = load({"pkg/__init__.py": "", "pkg/termui.py": "def get_pager_file(color=None):\n    from ._impl import get_pager_file\n"
                                                          "    return get_pager_file(color=color)\n\ndef again():\n    return get_pager_file()\n",
                  "pkg/_impl.py": "def get_pager_file(color=None):\n    return 1\n",
                  "a.js": "class H {\n  set(v) {\n    return v;\n  }\n}\nreduce(H.prototype, (k) => {\n  return {\n    set(headerValue) {\n"
                          "      this.x = headerValue;\n    },\n  };\n});\n"})
        self.assertEqual([("pkg/_impl.get_pager_file", 3, True)], p.edges["pkg/termui.get_pager_file"])
        self.assertEqual([("pkg/termui.get_pager_file", 6, True)], p.edges["pkg/termui.again"])
        self.assertFalse(p.edges.get("a.<top>"))  # (`set(headerValue) {` defines, calls nothing)


class PyMultilineWith(unittest.TestCase):
    def test_with_over_lines_binds_its_target(self):
        p = load({"pkg/__init__.py": "", "pkg/c.py": "class Client:\n    def __init__(self, transport=None):\n        pass\n"
                                                    "    def get(self, url):\n        return url\n\ndef mock(app):\n    return app\n",
                  "tests/test_x.py": "from pkg.c import Client, mock\n\ndef test_a():\n    with Client(\n        transport=mock(1)\n"
                                     "    ) as http:\n        http.get('/')\n"})
        self.assertIn(("pkg/c.Client.get", 7, True), p.edges["tests/test_x.test_a"])


class GoMethodAndTsGenericFixes(unittest.TestCase):
    def test_go_method_is_no_extension_and_ts_generic_call_over_lines(self):
        p = load({"go.mod": "module x\n", "e.go": "package x\n\ntype SliceErr []error\n\nfunc (err SliceErr) Error() string {\n"
                                                  "\ts := \"\"\n\tfor i := range err {\n\t\ts += err[i].Error()\n\t}\n\treturn s\n}\n",
                  "c.ts": "export function validate<T>(x: T): T {\n  return x;\n}\nexport const A = validate<[\n  number,\n  string,\n]>([1, 'a']);\n"})
        self.assertNotIn(("e.SliceErr.Error", 8, True), p.edges.get("e.SliceErr.Error", []))  # (an `error`'s Error, not this one)
        self.assertIn(("c.validate", 4, True), p.edges["c.<top>"])
