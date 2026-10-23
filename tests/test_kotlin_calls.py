"""Kotlin calls found by comparing okhttp against CodeQL's Kotlin extractor:
a call with only a trailing lambda (`withLock { }`), an extension on a generic
receiver (`fun <T> List<T>.toImmutableList()`), an extension on a library
supertype a project class implements (`class Response : Closeable` and
`fun Closeable.closeQuietly()`), and a primary constructor long enough (its
docs) that the class header was cut before `: Closeable`."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

DOCS = "".join(f"  /** property {i}: " + "words " * 12 + "*/\n  val p{i}: Int = 0,\n" for i in range(40))
FILES = {
    "src/Util.kt": """package app

import java.io.Closeable

interface Lockable

inline fun <T> Lockable.withLock(action: () -> T): T = action()

fun <T> List<T>.toImmutableList(): List<T> = this

fun Closeable.closeQuietly() {
  close()
}
""",
    "src/Response.kt": """package app

import java.io.Closeable

class Response(
""" + DOCS + """) : Closeable {
  override fun close() {
  }
}
""",
    "src/Conn.kt": """package app

class Conn : Lockable {
  fun count(xs: List<Int>, r: Response): Int {
    val ys = xs.toImmutableList()
    r.closeQuietly()
    return withLock {
      ys.size
    }
  }
}
""",
}


class KotlinCalls(unittest.TestCase):
    def test_trailing_lambda_generic_receiver_and_library_supertype(self):
        with tempfile.TemporaryDirectory() as d:
            for name, body in FILES.items():
                os.makedirs(os.path.dirname(os.path.join(d, name)), exist_ok=True)
                with open(os.path.join(d, name), "w") as fh:
                    fh.write(body)
            p = core.load(d, use_cache=False)
            got = {p.funcs[dst].name for dst, _l, e in p.edges.get("src/Conn.count", []) if dst in p.funcs}
            self.assertLessEqual({"toImmutableList", "closeQuietly", "withLock"}, got)


if __name__ == "__main__":
    unittest.main()
