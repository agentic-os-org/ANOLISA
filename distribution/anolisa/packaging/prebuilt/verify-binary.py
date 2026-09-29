#!/usr/bin/env python3
"""Bind one ANOLISA CLI binary to prebuilt build metadata."""

from __future__ import annotations

import argparse
import hashlib
import tomllib
from pathlib import Path


def sha256_file(path: Path) -> str:
    """Return a file's SHA-256 digest."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    """Validate the binary checksum and target identity."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--os", choices=("linux", "macos"), required=True)
    parser.add_argument("--arch", choices=("x86_64", "aarch64"), required=True)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()

    try:
        with args.metadata.open("rb") as stream:
            metadata = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise SystemExit(f"ERROR: cannot read {args.metadata}: {error}") from error
    if set(metadata) != {"version", "target_os", "target_arch", "binaries"}:
        raise SystemExit(f"ERROR: unexpected fields in {args.metadata}")
    binaries = metadata.get("binaries")
    if not isinstance(binaries, dict) or set(binaries) != {"anolisa"}:
        raise SystemExit(f"ERROR: {args.metadata} must identify only the anolisa binary")
    if (
        metadata.get("version") != args.version
        or metadata.get("target_os") != args.os
        or metadata.get("target_arch") != args.arch
    ):
        raise SystemExit(f"ERROR: {args.metadata} target identity does not match the package")
    try:
        digest = sha256_file(args.binary)
    except OSError as error:
        raise SystemExit(f"ERROR: cannot read {args.binary}: {error}") from error
    if binaries.get("anolisa") != digest:
        raise SystemExit(f"ERROR: {args.binary} checksum does not match {args.metadata}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
