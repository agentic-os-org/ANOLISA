#!/usr/bin/env python3
"""Validate only this component's destination paths before copying files."""

import sys
from pathlib import Path


def check_ancestors(path: Path) -> None:
    """Reject linked ancestors without inspecting unrelated directory contents."""
    for current in (path, *path.parents):
        if current.is_symlink():
            raise ValueError(f"installation path must not be a symlink: {current}")
        if current != path and current.exists() and not current.is_dir():
            raise ValueError(f"installation ancestor is not a directory: {current}")


def validate(destination: str, targets: list[str]) -> None:
    """Check an optional temporary root and the exact files or trees written."""
    if destination:
        if not destination.startswith("/") or not destination.strip("/"):
            raise ValueError("DESTDIR must be an absolute non-root temporary directory")
        if any(part in (".", "..") for part in destination.split("/")):
            raise ValueError("DESTDIR contains traversal")
        check_ancestors(Path(destination))
    for target in targets:
        kind, raw_path = target.split(":", 1)
        if kind not in ("file", "tree") or not raw_path.startswith("/"):
            raise ValueError(f"invalid installation target: {target}")
        if any(part in (".", "..") for part in raw_path.split("/")):
            raise ValueError(f"installation target contains traversal: {raw_path}")
        if not raw_path.strip("/"):
            raise ValueError("installation target must not be the filesystem root")
        path = Path(destination + raw_path)
        check_ancestors(path)
        if kind == "file":
            if path.exists() and not path.is_file():
                raise ValueError(f"installation file target is not a file: {path}")
        elif path.exists():
            if not path.is_dir():
                raise ValueError(f"installation tree target is not a directory: {path}")
            for child in path.rglob("*"):
                if child.is_symlink():
                    raise ValueError(f"installation tree contains a symlink: {child}")


if __name__ == "__main__":
    try:
        validate(sys.argv[1], sys.argv[2:])
    except (OSError, ValueError) as error:
        raise SystemExit(f"ERROR: {error}") from error
