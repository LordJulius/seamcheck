"""C#: generic arity (`Policy` and `Policy<T>` are two classes; `NoOp()` and
`NoOp<T>()` two methods), properties as fields, tuple returns, extension
overloads per receiver."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "src/Builder.cs": """namespace P;

public sealed partial class Builder
{
    public Plain Make() => new();
}

public sealed partial class Builder<TResult>
{
    public Typed Make() => new();
}
""",
    "src/Plain.cs": "namespace P;\npublic class Plain\n{\n    public void Run()\n    {\n    }\n}\n",
    "src/Typed.cs": "namespace P;\npublic class Typed\n{\n    public void Run()\n    {\n    }\n}\n",
    "src/Policy.cs": """namespace P;

public partial class Policy
{
    public static Builder Handle() => new();

    public static Builder<TResult> Handle<TResult>() => new();
}
""",
    "src/Pool.cs": """namespace P;

public partial class Pool
{
    public static Pool Shared { get; } = new Pool();

    public (bool, object?) TryGet(string key)
#pragma warning disable CA1000
        => (false, null);
#pragma warning restore CA1000

    public void Get()
    {
    }
}
""",
    "src/Pool.Shared.cs": """namespace P;

public partial class Pool
{
    private sealed class SharedPool
    {
        public void Get()
        {
        }
    }
}
""",
    "src/Other.cs": "namespace P;\npublic class Other\n{\n    public void TryGet(string k)\n    {\n    }\n    public void Get()\n    {\n    }\n}\n",
    "src/Services.cs": """namespace P;

public static class Services
{
    public static void AddPipeline(this IServiceCollection services, string key, Action<Builder> configure)
    {
    }

    public static void AddPipeline<TResult>(this IServiceCollection services, string key, Action<Builder<TResult>> configure)
    {
    }

    public static void AddPipeline(this IServiceCollection services, string key, Action<Plain, Typed> configure)
    {
    }
}
""",
    "src/Wire.cs": """namespace P;

public class Wire
{
    public void One(IServiceCollection services)
    {
        services.AddPipeline("a", b => b.Make());
    }

    public void Two(IServiceCollection services)
    {
        services.AddPipeline("a", (p, t) => p.Run());
    }

    public void Generic(IServiceCollection services)
    {
        services.AddPipeline<int>("a", g => g.Make());
    }
}
""",
    "src/Use.cs": """namespace P;

public class Use
{
    public void Plain()
    {
        Policy.Handle().Make().Run();
    }

    public void Typed()
    {
        Policy.Handle<int>().Make().Run();
    }

    public void Shared()
    {
        Pool.Shared.Get();
    }

    public void Tuple(Pool pool)
    {
        pool.TryGet("k");
    }

    public void Chained()
    {
        var b = new Builder().Make();
        b.Run();
    }
}
""",
}


def _to(p, fid, name):
    return {(dst, exact) for dst, _ln, exact in p.edges.get(fid, []) if dst.endswith("." + name)}


class CSharp(unittest.TestCase):
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

    def test_generic_class_is_its_own_type(self):
        self.assertEqual(_to(self.p, "src/Use.Plain", "Run"), {("src/Plain.Run", True)})
        self.assertEqual(_to(self.p, "src/Use.Typed", "Run"), {("src/Typed.Run", True)})

    def test_property_is_a_field(self):
        self.assertEqual(_to(self.p, "src/Use.Shared", "Get"), {("src/Pool.Get", True)})

    def test_tuple_return_and_pragma(self):
        self.assertEqual(_to(self.p, "src/Use.Tuple", "TryGet"), {("src/Pool.TryGet", True)})

    def test_new_then_chain_is_the_chain_end(self):
        self.assertEqual(_to(self.p, "src/Use.Chained", "Run"), {("src/Plain.Run", True)})

    def test_lambda_parameter_from_callee(self):
        self.assertEqual(_to(self.p, "src/Wire.One", "Make"), {("src/Builder.Make", True)})
        self.assertEqual(_to(self.p, "src/Wire.Two", "Run"), {("src/Plain.Run", True)})
        self.assertEqual(_to(self.p, "src/Wire.Generic", "Make"), {("src/Builder.Builder`1.Make", True)})


if __name__ == "__main__":
    unittest.main()
