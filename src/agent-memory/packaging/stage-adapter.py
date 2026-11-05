#!/usr/bin/env python3
"""Stage the adapter's declared npm runtime files without its build dependencies."""

from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path, PurePosixPath


def runtime_files(source: Path, dist_dir: Path | None = None) -> dict[Path, Path]:
    """Enumerate the declared plugin payload for staging and raw archives."""
    source = source.resolve()
    package = source / "openclaw"
    declared = json.loads((package / "package.json").read_text())["files"]
    files = {Path("manifest.json"): source / "manifest.json"}
    for name in ["package.json", *declared]:
        relative = PurePosixPath(name)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError(f"invalid npm runtime path: {name}")
        path = package / relative
        if dist_dir is not None and relative.parts[:1] == ("dist",):
            path = dist_dir.joinpath(*relative.parts[1:])
        if path.is_symlink() or not path.exists():
            raise ValueError(f"missing or linked runtime path: {path}")
        candidates = [path] if path.is_file() else list(path.rglob("*"))
        for candidate in candidates:
            if candidate.is_symlink():
                raise ValueError(f"runtime payload contains a symlink: {candidate}")
            if candidate.is_file():
                target = Path("openclaw") / relative
                if path.is_dir():
                    target /= candidate.relative_to(path)
                if target != Path("openclaw/dist/build-meta.json"):
                    files[target] = candidate
    if Path("openclaw/dist/index.js") not in files:
        raise ValueError("adapter runtime is missing dist/index.js")
    for path in files.values():
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"missing or linked runtime file: {path}")
    return files


def stage(source: Path, destination: Path) -> None:
    source = source.resolve()
    destination = destination.absolute()
    if destination == source or destination in source.parents or source in destination.parents:
        raise ValueError("adapter source and destination must not overlap")
    files = runtime_files(source)
    if destination.is_symlink():
        raise ValueError("adapter destination must not be a symlink")
    if destination.exists():
        shutil.rmtree(destination)
    for relative, source_file in files.items():
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source_file, target)
        target.chmod(0o755 if source_file.suffix == ".sh" else 0o644)
    for directory in [destination, *(path for path in destination.rglob("*") if path.is_dir())]:
        directory.chmod(0o755)


if __name__ == "__main__":
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "--check":
            runtime_files(Path(sys.argv[2]))
        else:
            stage(Path(sys.argv[1]), Path(sys.argv[2]))
    except (OSError, ValueError, KeyError) as error:
        raise SystemExit(f"ERROR: {error}") from error
