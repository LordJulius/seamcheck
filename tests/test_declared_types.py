"""Declared types resolve calls on a variable (python3 -m unittest discover tests).

No inference: only what the file wrote down. Kotlin / TS `name: Type`
(declarations, parameters, class properties), C++ `Type* name`, `Type& name`,
`Type name(...)` and a class's header members for its .cpp. A type outside
the project resolves to nothing (`s: String; s.split()` is not ours), except
a Kotlin extension declared on it. GDScript is untouched (a `Node` local is
duck-typed).
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "app/Repo.kt": """package app

class Repo {
    fun save(x: Int) {}
}
""",
    "app/Other.kt": """package app

class Other {
    fun save(x: Int) {}
    fun split(s: String) {}
}

fun JSONObject.optText(k: String): String = ""
""",
    "app/Use.kt": """package app

class Use(private val repo: Repo) {
    fun local() {
        val r: Repo = make()
        r.save(1)
    }

    fun param(r: Repo, s: String, j: JSONObject) {
        r.save(2)
        s.split(",")
        j.optText("a")
    }

    fun field() {
        repo.save(3)
    }
}
""",
    "web/store.ts": """export class Store {
  load(id: string) { return id }
}
""",
    "web/other.ts": """export class Other {
  load(id: string) { return id }
}
""",
    "web/use.ts": """import { Store } from "./store"

export function viaParam(s: Store, c: CanvasRenderingContext2D) {
  s.load("a")
  c.fill()
}

export function viaConst() {
  const st: Store = make()
  st.load("b")
}
""",
    "src/Board.h": """class Board {
public:
  int indexOf(int x);
};
""",
    "src/Board.cpp": """#include "Board.h"
int Board::indexOf(int x) {
  return x;
}
""",
    "src/List.cpp": """int List::indexOf(int x) {
  return x;
}
""",
    "src/Client.h": """class Client {
  Board *m_board = nullptr;
public:
  void sync();
};
""",
    "src/Client.cpp": """#include "Client.h"
void Client::sync() {
  m_board->indexOf(1);
}
""",
    "src/main.cpp": """int run(Board *b, const Board &r, QString name) {
  b->indexOf(1);
  r.indexOf(2);
  Board v(3);
  v.indexOf(4);
  name.indexOf(5);
  return 0;
}
""",
    "game/a.gd": """extends Node

func save(x):
	pass
""",
    "game/b.gd": """extends Node

func save(x):
	pass
""",
    "game/use.gd": """extends Node

func f(n: Node) -> void:
	n.save(1)
""",
}


def _to(p, fid, name, line=None):
    return {(dst.rsplit("/", 1)[-1], exact) for dst, ln, exact in p.edges.get(fid, [])
            if dst.endswith("." + name) and (line is None or ln == line)}


class DeclaredTypes(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        for name, body in FILES.items():
            path = os.path.join(cls.dir.name, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cfg = os.path.join(cls.dir.name, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "t"\ninclude = ["**"]\n')
        cls.p = core.load(cls.dir.name, cfg, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def fid(self, file, name):
        return next(f.id for f in self.p.funcs.values() if f.file == file and f.name == name)

    def test_kotlin_local_param_field(self):
        for fn, line in (("local", 6), ("param", 10), ("field", 16)):
            self.assertEqual(_to(self.p, self.fid("app/Use.kt", fn), "save", line), {("Repo.save", True)}, fn)

    def test_kotlin_outside_type_is_nothing(self):
        self.assertEqual(_to(self.p, self.fid("app/Use.kt", "param"), "split"), set())

    def test_kotlin_extension_on_library_type(self):
        self.assertEqual(_to(self.p, self.fid("app/Use.kt", "param"), "optText"), {("Other.optText", True)})

    def test_ts_param_and_const(self):
        self.assertEqual(_to(self.p, self.fid("web/use.ts", "viaParam"), "load"), {("store.load", True)})
        self.assertEqual(_to(self.p, self.fid("web/use.ts", "viaConst"), "load"), {("store.load", True)})
        self.assertEqual(_to(self.p, self.fid("web/use.ts", "viaParam"), "fill"), set())

    def test_cpp_pointer_ref_value(self):
        got = {ln: _to(self.p, self.fid("src/main.cpp", "run"), "indexOf", ln) for ln in (2, 3, 5, 6)}
        self.assertEqual(got[2], {("Board.indexOf", True)})
        self.assertEqual(got[3], {("Board.indexOf", True)})
        self.assertEqual(got[5], {("Board.indexOf", True)})
        self.assertEqual(got[6], set())  # QString: not ours

    def test_cpp_header_member(self):
        self.assertEqual(_to(self.p, self.fid("src/Client.cpp", "sync"), "indexOf"), {("Board.indexOf", True)})

    def test_gdscript_node_still_guesses(self):
        got = _to(self.p, self.fid("game/use.gd", "f"), "save")
        self.assertTrue(got and not any(e for _d, e in got))


if __name__ == "__main__":
    unittest.main()
