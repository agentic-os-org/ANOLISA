"""Render and exercise the real reusable QML panel locally, without a guest VM."""

import argparse
import json
import os
import sys
from pathlib import Path

from PySide6.QtCore import QPoint, Qt, QUrl, qInstallMessageHandler
from PySide6.QtGui import QGuiApplication
from PySide6.QtQml import QQmlComponent, QQmlEngine
from PySide6.QtQuick import QQuickItem, QQuickWindow
from PySide6.QtTest import QTest

os.environ["QT_QPA_PLATFORM"] = "offscreen"
os.environ["QT_QUICK_BACKEND"] = "software"

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--output", type=Path, required=True)
output = parser.parse_args().output.resolve()
output.mkdir(parents=True, exist_ok=True)
warnings = []
qInstallMessageHandler(lambda kind, context, message: warnings.append(message))
app = QGuiApplication(sys.argv)
engine = QQmlEngine()
component = QQmlComponent(engine, QUrl.fromLocalFile(str(root / "plugin/AssistantPanel.qml")))
if component.isError():
    raise RuntimeError("\n".join(error.toString() for error in component.errors()))
panel = component.create()
if panel is None:
    raise RuntimeError("QML did not create the real panel")
window = QQuickWindow()
window.setColor(Qt.transparent)
panel.setParentItem(window.contentItem())
panel.setProperty("width", 400)
ready = {
    "schema_version": 2,
    "ok": True,
    "setup": {"needs_prompt": False},
    "qoder": {
        "installation": "compatible",
        "enhancement": "configured",
        "source": "bundled",
        "version": "1.1.84",
        "auth_mode": "native",
    },
}
panel.setProperty("report", {**ready, "setup": {"needs_prompt": True}})
events = []
panel.actionRequested.connect(lambda action: events.append(("action", action)))
panel.navigateRequested.connect(lambda page: panel.setProperty("page", page))
panel.closeRequested.connect(lambda: events.append(("close", "")))


def settle() -> None:
    app.processEvents()
    QTest.qWait(100)


def fit(height: int | None = None) -> None:
    settle()
    panel.setProperty("height", height or int(panel.property("implicitHeight")))
    window.resize(int(panel.width()), int(panel.height()))
    window.show()
    settle()


def click(name: str) -> None:
    item = panel.findChild(QQuickItem, name)
    assert item is not None, name
    point = item.mapToScene(item.boundingRect().center())
    QTest.mouseClick(window, Qt.LeftButton, Qt.NoModifier, QPoint(int(point.x()), int(point.y())))
    # The native popup binds its height to this view's implicitHeight.
    fit()


def screenshot(name: str) -> None:
    fit()
    image = window.grabWindow()
    assert not image.isNull()
    assert image.save(str(output / f"native-{name}.png"))


fit()
assert panel.property("color").name() == "#21262c"
assert panel.findChild(QQuickItem, "coshButton").property("color").name() == "#b7d9be"
screenshot("welcome")
click("setupButton")
assert events[-1] == ("action", "setup")
click("deferButton")
assert events[-1] == ("action", "defer")
panel.setProperty("report", ready)
fit()
screenshot("home")
click("coshButton")
assert events[-1] == ("action", "cosh")
click("qoderButton")
assert events[-1] == ("action", "qoder")
click("settingsButton")
assert panel.property("page") == "settings"
screenshot("settings")
click("setupCoshButton")
assert events[-1] == ("action", "setup-cosh")
click("setupQoderButton")
assert events[-1] == ("action", "setup-qoder")
click("doctorButton")
assert events[-1] == ("action", "doctor")
click("settingsButton")
click("helpButton")
assert panel.property("page") == "help"
assert panel.findChild(QQuickItem, "helpButton").property("text") == "Help"
commands = panel.findChild(QQuickItem, "helpCommands").property("text")
assert all(f"anolisa-omarchy {name}" in commands for name in ("help", "status", "doctor"))
screenshot("plugin-help")
click("settingsButton")
assert panel.property("page") == "home"
click("securityRow")
assert panel.property("page") == "security"
screenshot("security-empty")
assert panel.findChild(QQuickItem, "metricValue").property("text") == "— records"
click("detailsButton")
assert events[-1] == ("action", "security")
click("settingsButton")
click("tokenlessRow")
assert panel.property("page") == "tokenless"
screenshot("tokenless-empty")
assert panel.findChild(QQuickItem, "metricValue").property("text") == "— tokens"
click("detailsButton")
assert events[-1] == ("action", "tokens")
panel.setProperty("report", {**ready, "tokenless": {"available": True, "saved": 89}})
settle()
assert panel.findChild(QQuickItem, "metricValue").property("text") == "89 tokens"
panel.setProperty("report", {**ready, "tokenless": {"available": True, "saved": 0}})
settle()
assert panel.findChild(QQuickItem, "metricValue").property("text") == "0 tokens"
panel.setProperty("page", "home")
panel.setProperty("width", 320)
fit(420)
viewport = panel.findChild(QQuickItem, "panelViewport")
assert viewport.property("contentHeight") > viewport.height()
viewport.setProperty("contentY", 100)
settle()
assert viewport.property("contentY") > 0
image = window.grabWindow()
assert image.save(str(output / "native-small-viewport.png"))
click("closeButton")
assert events[-1] == ("close", "")
bad_warnings = [warning for warning in warnings if "XDG_RUNTIME_DIR" not in warning]
assert not bad_warnings, bad_warnings
(output / "qml-checks.json").write_text(
    json.dumps(
        {
            "kind": "actual AssistantPanel.qml local offscreen render, not a VM screenshot",
            "checks": [
                "QML load",
                "approved palette",
                "welcome and defer signals",
                "settings and component actions",
                "cosh and Qoder click signals",
                "help and back",
                "component row navigation",
                "terminal detail actions",
                "unavailable metrics",
                "available metrics",
                "valid zero is shown",
                "320x420 viewport",
                "close action",
                "no QML warnings",
            ],
            "events": events,
            "warnings": warnings,
        },
        ensure_ascii=False,
        indent=2,
    )
    + "\n",
    encoding="utf-8",
)
print("PASS: real QML renders and button/navigation/metrics checks; guest acceptance not run")
