"""Delegate plugin requests to the owned installation without shell interpolation."""

import os
import sys
from pathlib import Path

if __name__ == "__main__":
    launcher = Path.home() / ".local/bin/anolisa-omarchy"
    os.execv(str(launcher), [str(launcher), *sys.argv[1:]])
