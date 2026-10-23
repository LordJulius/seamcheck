"""JS: a module's own top-level calls (`var app = module.exports = express()`,
a test file's `before(function () { createApp() })`), and bare calls in a
CommonJS file reaching only what the file defines or requires. Python:
`sessions.Session()` runs Session.__init__."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "lib/express.js": "exports = module.exports = createApplication;\nfunction createApplication() {\n  return {};\n}\n",
    "examples/mvc/index.js": "exports.before = function (req, res, next) {\n  next();\n};\n",
    "examples/api.js": "/**\n * var express = require('../lib/express')\n */\nmodule.exports = function () {};\n",
    "examples/app.js": "var app = {};\napp.use('/api', require('./api'));\n",
    "test/app.js": """var express = require('../lib/express');

describe('app', function () {
  before(function () {
    this.app = createApp();
  });
});

function createApp() {
  var app = module.exports = express();
  return app;
}
""",
    "pkg/sessions.py": "class Session:\n    def __init__(self):\n        self.a = 1\n",
    "pkg/api.py": "from . import sessions\n\n\ndef request():\n    with sessions.Session() as s:\n        return s\n",
    "pkg/__init__.py": "",
}


class JsTop(unittest.TestCase):
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

    def _edges(self, fid):
        return {(d, e) for d, _l, e in self.p.edges.get(fid, [])}

    def test_top_level_calls_count(self):
        self.assertIn(("test/app.createApp", True), self._edges("test/app.<top>"))

    def test_a_global_test_hook_is_no_project_function(self):
        self.assertNotIn("examples/mvc/index.before", {d for d, _e in self._edges("test/app.<top>")})

    def test_chained_assignment(self):
        self.assertIn(("lib/express.createApplication", True), self._edges("test/app.createApp"))

    def test_an_inline_require_is_a_dependency_and_a_commented_one_is_not(self):
        deps = {w for w, _n in self.p.modules["examples/app"].imported.values()}
        self.assertIn("examples/api", deps)
        self.assertNotIn("lib/express", {w for w, _n in self.p.modules["examples/api"].imported.values()})

    def test_python_constructor_through_its_module(self):
        self.assertIn(("pkg/sessions.Session.__init__", True), self._edges("pkg/api.request"))


if __name__ == "__main__":
    unittest.main()
