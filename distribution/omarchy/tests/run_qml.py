"""Run the shipped QML tests without a desktop compositor."""

import os
import sys
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
os.environ.setdefault("QT_QUICK_BACKEND", "software")
os.environ.setdefault("QML_XHR_ALLOW_FILE_READ", "1")

from PySide6.QtQuickTest import QUICK_TEST_MAIN

raise SystemExit(QUICK_TEST_MAIN("Omarchy", sys.argv, str(Path(__file__).parent / "qml")))
