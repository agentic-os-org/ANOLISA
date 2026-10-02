#!/usr/bin/env python3
"""Adapt shared adapter manifests for the self-contained raw payload."""

import argparse
from pathlib import Path

RAW_HOOK_MANIFESTS = (
    Path("adapters/sec-core/codex/hooks/hooks.json"),
    Path("adapters/sec-core/qoder/hooks/hooks.json"),
    Path("adapters/sec-core/qwencode/qwen-extension.json"),
    Path("adapters/sec-core/cosh/cosh-extension.json"),
)


def adapt_hook_manifest(path: Path) -> None:
    """Replace native Python hook launchers in one staged JSON manifest."""
    if not path.is_file():
        raise SystemExit(f"ERROR: staged hook manifest not found: {path}")
    content = path.read_text(encoding="utf-8")
    adapted = content.replace(
        '"command": "python3 ', '"command": "agent-sec-python '
    ).replace('"command": "python3"', '"command": "agent-sec-python"')
    if adapted == content:
        raise SystemExit(f"ERROR: {path} has no native Python hook launcher")
    path.write_text(adapted, encoding="utf-8")


def adapt_payload(payload_root: Path) -> None:
    """Apply raw-only hook launcher changes under ``payload_root``."""
    for relative_path in RAW_HOOK_MANIFESTS:
        adapt_hook_manifest(payload_root / relative_path)


def parse_args() -> argparse.Namespace:
    """Parse command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument("payload_root", type=Path)
    return parser.parse_args()


def main() -> int:
    """Adapt a staged raw payload in place."""
    args = parse_args()
    adapt_payload(args.payload_root.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
