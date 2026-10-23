"""QML pages and Qt D-Bus, cut down from kdeconnect: a page's untyped
`property QtObject` holds what every push hands it (per instance of the
pushing component when the page is one of its properties), a delegate's role
name is the C++ model's role, `cond ? X : null` is X, and a generated D-Bus
proxy (`OrgKdeAppDeviceInterface`) calls the class that declares that interface."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "core/device.h": """#pragma once
#include <QObject>

class Device : public QObject
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.kde.app.device")
public:
    Q_SCRIPTABLE void unpair();
};
""",
    "core/device.cpp": '#include "device.h"\n\nvoid Device::unpair()\n{\n}\n',
    "plugins/remote.h": """#pragma once
#include <QObject>

class RemotePlugin : public QObject
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.kde.app.device.remote")
public:
    Q_SCRIPTABLE void sendCommand(const QVariantMap &cmd);
};
""",
    "plugins/remote.cpp": '#include "remote.h"\n\nvoid RemotePlugin::sendCommand(const QVariantMap &cmd)\n{\n}\n',
    "iface/ifaces.h": """#pragma once
#include "deviceinterface.h"

class DeviceDbusInterface : public OrgKdeAppDeviceInterface
{
    Q_OBJECT
public:
    explicit DeviceDbusInterface(const QString &id);
};

class RemoteDbusInterface : public OrgKdeAppDeviceRemoteInterface
{
    Q_OBJECT
public:
    explicit RemoteDbusInterface(const QString &id);
};

class RemoteDbusInterfaceFactory : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON
public:
    Q_INVOKABLE RemoteDbusInterface *create(const QString &id)
    {
        return new RemoteDbusInterface(id);
    }
};
""",
    "models/devicesmodel.h": """#pragma once
#include <QAbstractListModel>
#include "../iface/ifaces.h"

class DevicesModel : public QAbstractListModel
{
    Q_OBJECT
    QML_ELEMENT
public:
    enum Roles { DeviceRole = Qt::UserRole };
    QHash<int, QByteArray> roleNames() const override;
    QVariant data(const QModelIndex &index, int role) const override;
    Q_INVOKABLE DeviceDbusInterface *deviceForId(const QString &id) const;
private:
    QList<DeviceDbusInterface *> m_list;
};
""",
    "models/devicesmodel.cpp": """#include "devicesmodel.h"

QHash<int, QByteArray> DevicesModel::roleNames() const
{
    QHash<int, QByteArray> names;
    names.insert(DeviceRole, "device");
    return names;
}

QVariant DevicesModel::data(const QModelIndex &index, int role) const
{
    DeviceDbusInterface *device = m_list[index.row()];
    switch (role) {
    case DeviceRole:
        return QVariant::fromValue<QObject *>(device);
    default:
        return QVariant();
    }
}

DeviceDbusInterface *DevicesModel::deviceForId(const QString &id) const
{
    return m_list.first();
}
""",
    "qml/PluginItem.qml": """import QtQuick

QtObject {
    property var interfaceFactory
    property var component
    property var onClick: () => {
        var obj = interfaceFactory.create("id");
        pageStack.push(Qt.resolvedUrl(component), { pluginInterface: obj });
    }
}
""",
    "qml/mousepad.qml": """import QtQuick

Item {
    id: mousepad
    property QtObject pluginInterface
    function click() {
        mousepad.pluginInterface.sendCommand({"singleclick": true});
    }
}
""",
    "qml/DevicePage.qml": """import QtQuick

Item {
    id: root
    property QtObject currentDevice
    readonly property var remote: root.currentDevice ? RemoteDbusInterfaceFactory.create("x") : null
    property list<QtObject> plugins: [
        PluginItem {
            interfaceFactory: RemoteDbusInterfaceFactory
            component: "mousepad.qml"
        }
    ]
    function forget() {
        root.currentDevice.unpair();
    }
    function poke() {
        remote.sendCommand({});
    }
}
""",
    "qml/Main.qml": """import QtQuick

Item {
    DevicesModel {
        id: devicesModel
    }
    Component {
        id: deviceComp
        DevicePage {}
    }
    function openDevice(deviceId) {
        let device = devicesModel.deviceForId(deviceId);
        pageStack.push(deviceComp, { currentDevice: device });
    }
    ListView {
        model: DevicesSortProxyModel {
            sourceModel: devicesModel
        }
        delegate: ItemDelegate {
            onClicked: {
                pageStack.push(deviceComp, { currentDevice: device });
            }
        }
    }
}
""",
}


class QmlFlow(unittest.TestCase):
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

    def _reaches(self, suffix, where, name):
        [fid] = [f for f in self.p.funcs if f.endswith(suffix)]
        return any(e and d.startswith(where) and d.endswith("." + name) for d, _l, e in self.p.edges.get(fid, []))

    def test_pushed_per_instance_and_across_dbus(self):
        self.assertTrue(self._reaches("qml/mousepad.click", "plugins/remote", "sendCommand"))

    def test_both_pushes_and_the_delegate_role_agree(self):
        self.assertTrue(self._reaches("qml/DevicePage.forget", "core/device", "unpair"))

    def test_ternary_binding(self):
        self.assertTrue(self._reaches("qml/DevicePage.poke", "plugins/remote", "sendCommand"))


if __name__ == "__main__":
    unittest.main()
