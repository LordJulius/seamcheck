"""C: a definition with its return type on the line above (bison's
`int\\nyyparse (...)\\n{`), and a call through a function-like macro
(`JV_ARRAY(a, b)` runs `jv_array_append`)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "src/jv.h": "#define JV_ARRAY(a, b) jv_array_append(jv_array(), a, b)\nint jv_array(void);\nint jv_array_append(int x, int a, int b);\n",
    "src/jv.c": '#include "jv.h"\n\nint jv_array(void) {\n  return 0;\n}\n\nint jv_array_append(int x, int a, int b) {\n  return x + a + b;\n}\n',
    "src/parser.c": '#include "jv.h"\n\nint\nyyparse (int answer)\n{\n  return JV_ARRAY(answer, 1);\n}\n',
}


class CMacros(unittest.TestCase):
    def test_gnu_style_definition_and_macro_call(self):
        with tempfile.TemporaryDirectory() as d:
            for name, body in FILES.items():
                os.makedirs(os.path.join(d, os.path.dirname(name)), exist_ok=True)
                with open(os.path.join(d, name), "w") as fh:
                    fh.write(body)
            p = core.load(d, use_cache=False)
            self.assertIn("src/parser.yyparse", p.funcs)
            got = {dst for dst, _l, e in p.edges.get("src/parser.yyparse", []) if e}
            self.assertEqual(got, {"src/jv.jv_array", "src/jv.jv_array_append"})


if __name__ == "__main__":
    unittest.main()


CPP = {
    "src/config.h": """#define GETTER(KEY, TYPE) \\
    TYPE KEY() { return value(#KEY).value<TYPE>(); }
#define SETTER(FUNC, TYPE) \\
    void FUNC(const TYPE& v) { setValue(v); }
#define GETTER_SETTER(G, S, TYPE) \\
    GETTER(G, TYPE) \\
    SETTER(S, TYPE)

class Config
{
public:
    Config();
    GETTER_SETTER(showHelp, setShowHelp, bool)
    GETTER(savePath, QString)
};
""",
    "src/config.cpp": '#include "config.h"\n\nConfig::Config()\n{\n}\n',
    "src/widget.cpp": """#include "config.h"

void toggle(bool on)
{
    Config().setShowHelp(on);
    if (Config().showHelp()) {
    }
}
""",
}


class CppMacroMethods(unittest.TestCase):
    def test_methods_a_macro_defines_and_calls_on_a_temporary(self):
        # flameshot's CONFIG_GETTER_SETTER: each use in a class body declares methods named by its arguments
        with tempfile.TemporaryDirectory() as d:
            for name, body in CPP.items():
                os.makedirs(os.path.join(d, os.path.dirname(name)), exist_ok=True)
                with open(os.path.join(d, name), "w") as fh:
                    fh.write(body)
            p = core.load(d, use_cache=False)
            names = {f.name for f in p.funcs.values() if f.file == "src/config.h"} - {"<top>"}
            self.assertEqual(names, {"showHelp", "setShowHelp", "savePath"})  # (not the templates' KEY / FUNC)
            got = {p.funcs[dst].name for dst, _l, e in p.edges.get("src/widget.toggle", []) if e}
            self.assertEqual(got, {"Config", "setShowHelp", "showHelp"})
