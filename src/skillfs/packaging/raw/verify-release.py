#!/usr/bin/env python3
"""Validate skillfs release identity and bundled skill entry point."""

from __future__ import annotations

import argparse
import re
import tomllib
from pathlib import Path


def verify(root: Path, contract_path: Path, os_name: str, arch: str) -> str:
    """Reject mismatched versions, unsupported targets, or a missing bundled skill."""
    workspace = tomllib.loads((root / "Cargo.toml").read_text())
    version = workspace["workspace"]["package"]["version"]
    if (
        re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version)
        is None
    ):
        raise ValueError(f"invalid workspace version: {version!r}")
    contract = tomllib.loads(contract_path.read_text())
    component = contract["component"]
    if component["name"] != "skillfs" or component["version"] != version:
        raise ValueError("raw contract identity does not match skillfs workspace")
    platform = component["platform"]
    if os_name not in platform["os"] or arch not in platform["arch"]:
        raise ValueError(f"raw contract does not support {os_name}/{arch}")
    if component["layout"]["modes"] != ["system"]:
        raise ValueError("skillfs requires system install mode")
    rpm = tomllib.loads((root / "component.toml").read_text())
    if rpm["component"]["name"] != "skillfs" or rpm["component"]["version"] != version:
        raise ValueError("RPM component identity does not match workspace")
    if not (root / "docs/skills/skillfs-mount/SKILL.md").is_file():
        raise ValueError("missing bundled skillfs-mount/SKILL.md")
    return version


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_root", type=Path)
    parser.add_argument("contract", type=Path)
    parser.add_argument("--os", required=True)
    parser.add_argument("--arch", required=True)
    args = parser.parse_args()
    try:
        print(verify(args.source_root, args.contract, args.os, args.arch))
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"ERROR: {error}\n")


if __name__ == "__main__":
    main()
