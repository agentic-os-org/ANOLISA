#!/usr/bin/env python3
"""Validate prebuilt inputs and only the paths touched by a file installation."""

import argparse
import os
import re
import stat
import sys
from pathlib import Path


def reject_symlinks(root: Path, recursive: bool = False) -> None:
    """Check ancestors and, for owned trees, contents without following links."""
    for path in (root, *root.parents):
        try:
            mode = path.lstat().st_mode
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(mode):
            raise ValueError(f"symlink paths are not supported by installation: {path}")
        if path != root and not stat.S_ISDIR(mode):
            raise ValueError(f"installation ancestor is not a directory: {path}")
    if recursive:
        for current, directories, files in os.walk(root, followlinks=False):
            for name in (*directories, *files):
                path = Path(current) / name
                if path.is_symlink():
                    raise ValueError(f"symlink contents are not supported by installation: {path}")


def destination_root(destination: str) -> None:
    """Allow direct installation or an isolated temporary root."""
    if not destination:
        return
    root = Path(destination)
    if not root.is_absolute() or not destination.strip("/"):
        raise ValueError("DESTDIR must be empty or an absolute temporary root other than /")
    if any(part in (".", "..") for part in destination.split("/")):
        raise ValueError("DESTDIR must not contain . or .. path components")
    if re.fullmatch(r"/[A-Za-z0-9_./-]*", destination) is None:
        raise ValueError("DESTDIR must use letters, digits, _, ., / or -")
    reject_symlinks(root)
    if root.exists() and not root.is_dir():
        raise ValueError(f"DESTDIR is not a directory: {root}")


def validate(arguments: argparse.Namespace) -> None:
    """Preflight every source and destination before installers mutate files."""
    destination_root(arguments.destdir)
    for value in arguments.directory:
        source = Path(value)
        reject_symlinks(source, recursive=True)
        if not source.is_dir():
            raise ValueError(f"missing built resource directory: {source}")
    matches = []
    for value in arguments.glob:
        pattern = Path(value)
        found = list(pattern.parent.glob(pattern.name))
        if not found:
            raise ValueError(f"missing prebuilt inputs: {value}; run make build first")
        matches.extend(str(path) for path in found)
    for value in (*arguments.file, *arguments.executable, *matches):
        source = Path(value)
        reject_symlinks(source)
        if not source.is_file():
            raise ValueError(f"missing prebuilt input: {source}; run make build first")
        if value in arguments.executable and not source.stat().st_mode & 0o111:
            raise ValueError(f"prebuilt input is not executable: {source}")
    for directory, values in ((False, arguments.target_file), (True, arguments.target_directory)):
        for value in values:
            validate_target(arguments.destdir, value, directory)
    for source_value, target_value in arguments.tree:
        source = Path(source_value)
        for current, directories, files in os.walk(source, followlinks=False):
            relative = Path(current).relative_to(source)
            destination = Path(target_value) / relative
            validate_target(arguments.destdir, str(destination), True)
            for name in files:
                validate_target(arguments.destdir, str(destination / name), False)


def validate_target(destdir: str, value: str, directory: bool) -> None:
    """Check a written path without inspecting existing sibling resources."""
    if not Path(value).is_absolute():
        raise ValueError(f"installation target must be absolute: {value}")
    target = Path(destdir + value)
    if directory and target == Path("/"):
        raise ValueError("refusing installation into filesystem root")
    reject_symlinks(target)
    if target.exists():
        mode = target.stat().st_mode
        valid = stat.S_ISDIR(mode) if directory else stat.S_ISREG(mode)
        if not valid:
            raise ValueError(f"installation target has the wrong file type: {target}")


def main() -> None:
    """Parse the component-private install preflight interface."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destdir", default="")
    parser.add_argument("--tree", action="append", nargs=2, default=[])
    for name in ("directory", "file", "executable", "glob", "target-directory", "target-file"):
        parser.add_argument("--" + name, action="append", default=[])
    try:
        validate(parser.parse_args())
    except (OSError, ValueError) as error:
        sys.exit(f"cannot install component: {error}")


if __name__ == "__main__":
    main()
