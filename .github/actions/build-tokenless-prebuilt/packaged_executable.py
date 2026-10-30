#!/usr/bin/env python3
"""Print which executable artifacts built from a Tokenless component root ship."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


def packaged_executable(component_root: Path) -> str:
    """Return ``tokenless`` for trees that link RTK, ``rtk`` for older tags.

    Older tags still build from this action and ship RTK as a separate binary,
    which runtime wheels embed instead of tokenless.
    """
    manifest = tomllib.loads((component_root / "Cargo.toml").read_text(encoding="utf-8"))
    rtk = manifest.get("workspace", {}).get("dependencies", {}).get("rtk")
    return "tokenless" if isinstance(rtk, dict) and rtk.get("path") == "third_party/rtk" else "rtk"


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: packaged_executable.py COMPONENT_ROOT")
    print(packaged_executable(Path(sys.argv[1])))
