#!/usr/bin/env python3
"""Reject version drift before building or packaging agent-memory."""

import argparse
import json
import tomllib
from pathlib import Path


def verify(root: Path) -> str:
    """Return the Cargo version after checking all shipped version declarations."""
    package = tomllib.loads((root / "Cargo.toml").read_text())["package"]
    if package["name"] != "agent-memory":
        raise ValueError("expected agent-memory Cargo package")
    version = package["version"]
    for relative in (
        "adapters/agent-memory/manifest.json",
        "adapters/agent-memory/openclaw/package.json",
        "adapters/agent-memory/openclaw/openclaw.plugin.json",
        "config/mcp-server.json",
    ):
        if json.loads((root / relative).read_text())["version"] != version:
            raise ValueError(f"{relative}: version does not match Cargo {version}")
    lock = json.loads((root / "adapters/agent-memory/openclaw/package-lock.json").read_text())
    if lock["version"] != version or lock["packages"][""]["version"] != version:
        raise ValueError("plugin lockfile version does not match Cargo")
    for kind in ("rpm", "raw"):
        contract = tomllib.loads(
            (root / "packaging/anolisa" / kind / "component.toml.in")
            .read_text()
            .replace("@VERSION@", version)
        )
        if (
            contract["component"]["name"] != "agent-memory"
            or contract["component"]["version"] != version
        ):
            raise ValueError(f"{kind} contract: component identity does not match Cargo")
    return version


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    try:
        print(verify(args.source))
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"ERROR: {error}\n")
