#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Resolve the host's standard virtualenv interpreter layout."""

import sys
from pathlib import Path


def python_path(venv_dir: Path) -> Path:
    """Return the Windows or POSIX interpreter within a virtualenv."""
    if sys.platform == "win32":
        return venv_dir / "Scripts" / "python.exe"
    return venv_dir / "bin" / "python"
