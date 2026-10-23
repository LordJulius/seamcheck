"""Strings that span lines and quote differently: a C# verbatim string (`@"..."`,
`""` for a quote) and a Go raw string (`` `O'Brien` ``) used to end at the line's
end, and everything after one in the file went unread (Newtonsoft.Json's tests:
11,000 calls). And an ES5 class, `function UserDAO(db) { this.get = ... }`, is a
type `new UserDAO(db)` has methods on."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "Tests.cs": '''public class Tests
{
    public void First()
    {
        Check(@"{
  ""rc"": 200,
  ""o"": ""<div class='s1'></div>""
}");
    }

    public void Second()
    {
        Check("x");
    }

    public void Check(string s)
    {
    }
}
''',
    "main.go": '''package main

func first() {
	run(`C:\\Windows\\cmd.exe /c 'O'Brien'`)
}

func second() {
	run("x")
}

func run(s string) {
}
''',
    "dao.js": '''function UserDAO(db) {
    this.validateLogin = (name, cb) => {
        return db.find(name, cb);
    };
}

module.exports = { UserDAO };
''',
    "session.js": '''const UserDAO = require("./dao").UserDAO;

function SessionHandler(db) {
    const userDAO = new UserDAO(db);
    this.login = (req, res) => {
        userDAO.validateLogin(req.body.name, () => {});
    };
}

module.exports = SessionHandler;
''',
}


class Strings(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        for name, body in FILES.items():
            with open(os.path.join(cls.dir.name, name), "w") as fh:
                fh.write(body)
        cls.p = core.load(cls.dir.name, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def exact(self, fid):
        return {dst for dst, _l, e in self.p.edges.get(fid, []) if e}

    def test_code_after_a_csharp_verbatim_string_is_read(self):
        self.assertEqual(self.exact("Tests.Second"), {"Tests.Check"})

    def test_code_after_a_go_raw_string_is_read(self):
        self.assertEqual(self.exact("main.second"), {"main.run"})

    def test_an_es5_constructor_function_is_a_class(self):
        self.assertEqual(self.exact("session.SessionHandler.login"), {"dao.UserDAO.validateLogin"})


if __name__ == "__main__":
    unittest.main()
