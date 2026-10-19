"""C++ and QML adapters (python3 -m unittest discover tests), from OS-D:
`25'000` digit separators blanked the rest of a file; `: QObject(parent)`
initializers read as functions; handlers and component instances in QML."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

CPP = """#include "Client.h"

Client::Client(QObject *parent)
    : QObject(parent)
{
  m_timer.setInterval(25'000);
}

void Client::connectTo(const QString &url)
{
  QString s(url);
  m_socket->open(url);
  helper(1);
}

static int helper(int x);

int helper(int x)
{
  return x * 2;
}
"""

QML = """import QtQuick

Item {
    id: root
    function reload() {
        client.connectTo("x")
    }
    onVisibleChanged: reload()
    Button {
        onClicked: {
            root.reload()
        }
    }
}
"""


class CppQmlTest(unittest.TestCase):
    def test_parse(self):
        with tempfile.TemporaryDirectory() as root:
            for name, body in {"src/Client.cpp": CPP, "qml/Panel.qml": QML, "qml/Button.qml": "Item {}\n"}.items():
                os.makedirs(os.path.join(root, os.path.dirname(name)), exist_ok=True)
                with open(os.path.join(root, name), "w") as fh:
                    fh.write(body)
            cfg = os.path.join(root, "seamcheck.toml")
            with open(cfg, "w") as fh:
                fh.write('[project]\nname = "q"\ninclude = ["**"]\n')
            p = core.load(root, cfg, use_cache=False)
        names = {f.name: (f.start, f.end) for f in p.funcs.values() if f.file == "src/Client.cpp"}
        self.assertEqual(names["Client"], (3, 7))
        self.assertEqual(names["connectTo"], (9, 14))
        self.assertEqual(names["helper"], (18, 21))
        self.assertNotIn("QObject", names)  # an initializer, not a function
        self.assertNotIn("s", names)  # `QString s(url);` is a variable
        calls = {(c.qual, c.name) for c in p.funcs["src/Client.connectTo"].calls}
        self.assertIn(("m_socket", "open"), calls)  # `->` is a member call
        q = {f.name: (f.start, f.end) for f in p.funcs.values() if f.file == "qml/Panel.qml"}
        self.assertEqual(q["reload"], (5, 7))
        self.assertEqual(q["onVisibleChanged"], (8, 8))
        self.assertEqual(q["onClicked"], (10, 12))
        self.assertIn("qml/Panel.reload", {d for d, _, _ in p.edges["qml/Panel.onVisibleChanged"]})
        self.assertIn("qml/Button.<top>", {f.id for f in p.funcs.values() if f.file == "qml/Button.qml"})
        self.assertIn("Button", {c.name for c in p.funcs["qml/Panel.<top>"].calls})


if __name__ == "__main__":
    unittest.main()
