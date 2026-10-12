#!/usr/bin/env python3
"""Validate ws-ckpt release identity and runtime plugin entry points."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import tomllib


def verify(root: Path, contract_path: Path, os_name: str, arch: str) -> str:
    """Reject mismatched versions, unsupported targets, or missing plugin entries."""
    workspace = tomllib.loads((root / "src/Cargo.toml").read_text())
    version = workspace["workspace"]["package"]["version"]
    if (
        re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version)
        is None
    ):
        raise ValueError(f"invalid workspace version: {version!r}")
    contract = tomllib.loads(contract_path.read_text())
    component = contract["component"]
    if component["name"] != "ws-ckpt" or component["version"] != version:
        raise ValueError("raw contract identity does not match ws-ckpt workspace")
    platform = component["platform"]
    if os_name not in platform["os"] or arch not in platform["arch"]:
        raise ValueError(f"raw contract does not support {os_name}/{arch}")
    if component["layout"]["modes"] != ["system"]:
        raise ValueError("ws-ckpt requires system install mode")
    rpm = tomllib.loads((root / "component.toml").read_text())
    if rpm["component"]["name"] != "ws-ckpt" or rpm["component"]["version"] != version:
        raise ValueError("RPM component identity does not match workspace")
    plugin = root / "src/plugins/openclaw"
    package = json.loads((plugin / "package.json").read_text())
    if package["version"] != version:
        raise ValueError("OpenClaw package version does not match workspace")
    lock = json.loads((plugin / "package-lock.json").read_text())
    if lock["version"] != version or lock["packages"][""]["version"] != version:
        raise ValueError("OpenClaw lock version does not match workspace")
    descriptor = json.loads((plugin / "openclaw.plugin.json").read_text())
    if descriptor["id"] != "ws-ckpt":
        raise ValueError("OpenClaw plugin ID must be ws-ckpt")
    for entry in [package["main"], *package["openclaw"]["extensions"]]:
        source = (plugin / entry).resolve()
        if not source.is_relative_to(plugin.resolve()) or not source.is_file():
            raise ValueError(f"missing or unsafe OpenClaw entry: {entry}")
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
