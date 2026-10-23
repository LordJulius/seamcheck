"""Many repositories, one graph: a folder of checkouts (or symlinks to them) is
one project, so a service's call into a library repo is an exact edge, and
`search` answers each hit with the function it is in and that function's callers."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, queries  # noqa: E402

LIB = {"lib/src/httplib/__init__.py": "from httplib.api import get\n",
       "lib/src/httplib/api.py": "def get(url, timeout=None):\n    return request('GET', url)\n\n\ndef request(m, url):\n    return m\n"}
APP = {"svc/client.py": "import httplib\n\n\ndef fetch(url):\n    return httplib.get(url, timeout=3)\n\n\ndef main():\n    fetch('x')\n"}


def write(root, files):
    for name, body in files.items():
        os.makedirs(os.path.dirname(os.path.join(root, name)), exist_ok=True)
        with open(os.path.join(root, name), "w") as fh:
            fh.write(body)


class Workspace(unittest.TestCase):
    def test_symlinked_checkouts_are_one_graph(self):
        with tempfile.TemporaryDirectory() as d:
            write(os.path.join(d, "lib-repo"), LIB)
            write(os.path.join(d, "app-repo"), APP)
            ws = os.path.join(d, "ws")
            os.makedirs(ws)
            os.symlink(os.path.join(d, "lib-repo", "lib"), os.path.join(ws, "lib"))
            os.symlink(os.path.join(d, "app-repo", "svc"), os.path.join(ws, "svc"))
            p = core.load(ws, use_cache=False)
            got = {dst for dst, _l, exact in p.edges.get("svc/client.fetch", []) if exact}
            self.assertEqual(got, {"lib/src/httplib/api.get"})
            [hit] = queries.search(p, r"timeout=3")
            self.assertEqual((hit["file"], hit["line"], hit["func"], hit["exact_callers"]),
                             ("svc/client.py", 5, "svc/client.fetch", 1))
            self.assertEqual([h["file"] for h in queries.search(p, r"def \w+\(", ["lib/**"], code=True)],
                             ["lib/src/httplib/api.py"] * 2)


if __name__ == "__main__":
    unittest.main()
