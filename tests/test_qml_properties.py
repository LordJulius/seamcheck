"""QML reads a C++ object's Q_PROPERTY by name: `RoomManager.treeModel.current()`
is SortModel::current through `Q_PROPERTY(SortModel *treeModel READ ...)`."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "src/sortmodel.h": """#pragma once
#include <QObject>

class SortModel : public QObject
{
    Q_OBJECT
public:
    Q_INVOKABLE int current() const;
};
""",
    "src/sortmodel.cpp": '#include "sortmodel.h"\n\nint SortModel::current() const\n{\n    return 0;\n}\n',
    "src/othermodel.h": """#pragma once
#include <QObject>

class OtherModel : public QObject
{
    Q_OBJECT
public:
    Q_INVOKABLE int current() const;
};
""",
    "src/othermodel.cpp": '#include "othermodel.h"\n\nint OtherModel::current() const\n{\n    return 1;\n}\n',
    "src/roommanager.h": """#pragma once
#include <QObject>
#include <QtQml>
#include "sortmodel.h"

class RoomManager : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON
    Q_PROPERTY(SortModel *treeModel READ treeModel CONSTANT)
public:
    SortModel *treeModel() const;
private:
    SortModel *m_model;
};
""",
    "src/roommanager.cpp": '#include "roommanager.h"\n\nSortModel *RoomManager::treeModel() const\n{\n    return m_model;\n}\n',
    "src/qml/RoomList.qml": """import QtQuick

Item {
    function next() {
        return RoomManager.treeModel.current()
    }
}
""",
}


class QmlProperties(unittest.TestCase):
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

    def test_q_property_types_the_member(self):
        [fid] = [f for f in self.p.funcs if f.endswith("RoomList.next")]
        got = {(d, e) for d, _l, e in self.p.edges.get(fid, []) if d.endswith(".current")}
        self.assertEqual(got, {("src/sortmodel.current", True)})


if __name__ == "__main__":
    unittest.main()
