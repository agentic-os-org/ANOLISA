#!/usr/bin/env python3
"""Validate ws-ckpt ELF architecture and hash-bound build metadata."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import struct
import tomllib


def verify(binary: Path, arch: str, metadata: Path | None, version: str) -> None:
    """Inspect binary bytes without executing a cross-compiled program."""
    blob = binary.read_bytes()
    if len(blob) < 64 or blob[:6] != b"\x7fELF\x02\x01":
        raise ValueError(f"not a 64-bit little-endian ELF executable: {binary}")
    if struct.unpack_from("<H", blob, 18)[0] != {"x86_64": 62, "aarch64": 183}[arch]:
        raise ValueError(f"ELF architecture does not match {arch}: {binary}")
    if metadata is not None:
        build = tomllib.loads(metadata.read_text())
        for key, expected in (("version", version), ("target_os", "linux"), ("target_arch", arch)):
            if build[key] != expected:
                raise ValueError(f"build metadata {key} does not match {expected}")
        if build["binaries"]["ws-ckpt"] != hashlib.sha256(blob).hexdigest():
            raise ValueError("ws-ckpt SHA-256 does not match build metadata")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--arch", required=True, choices=("x86_64", "aarch64"))
    parser.add_argument("--metadata", type=Path)
    parser.add_argument("--component-version", required=True)
    args = parser.parse_args()
    try:
        verify(args.binary, args.arch, args.metadata, args.component_version)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"ERROR: {error}\n")


if __name__ == "__main__":
    main()
