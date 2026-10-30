#!/usr/bin/env python3
"""Render installation descriptors from the selected final binary directory."""

import argparse
import json
from pathlib import Path


def render(kind: str, source: Path, destination: Path, bindir: Path) -> None:
    """Keep DESTDIR out of commands used after installation."""
    if kind == "mcp":
        descriptor = json.loads(source.read_text())
        descriptor["command"] = str(bindir / "agent-memory")
        content = json.dumps(descriptor, ensure_ascii=False, indent=2) + "\n"
    else:
        content = source.read_text().replace("{bindir}", str(bindir))
    destination.write_text(content)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("mcp", "systemd"))
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("bindir", type=Path)
    args = parser.parse_args()
    render(args.kind, args.source, args.destination, args.bindir)
