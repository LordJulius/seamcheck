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
