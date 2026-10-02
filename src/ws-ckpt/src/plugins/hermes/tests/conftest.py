"""Make the hermes package importable by putting plugins/ on sys.path."""

import sys
from pathlib import Path

_plugins_dir = Path(__file__).resolve().parent.parent
if str(_plugins_dir) not in sys.path:
    sys.path.insert(0, str(_plugins_dir))
