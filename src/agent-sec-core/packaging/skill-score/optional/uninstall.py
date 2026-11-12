#!/usr/bin/python3 -B
"""Strict pre-uninstall hook for optional Skill Score generated environments."""

import sys
from pathlib import Path

sys.dont_write_bytecode = True
from lifecycle import uninstall  # noqa: E402

if __name__ == "__main__":
    try:
        uninstall(Path(__file__).absolute().parent)
    except Exception as error:
        raise SystemExit(f"optional scoring uninstall refused: {error}") from None
