#!/usr/bin/env python3
"""Verify that prebuilt raw-package binaries match the requested target."""

from __future__ import annotations

import argparse
import struct
from pathlib import Path


ELF_MACHINES = {
    "x86_64": 62,
    "aarch64": 183,
}
MACHO_CPUS = {
    "x86_64": 0x01000007,
    "aarch64": 0x0100000C,
}


def verify_elf(path: Path, arch: str) -> None:
    """Verify one 64-bit little-endian ELF binary's architecture."""
    with path.open("rb") as stream:
        header = stream.read(64)
    if len(header) < 20 or header[:4] != b"\x7fELF":
        raise ValueError("not an ELF binary")
    if header[4] != 2:
        raise ValueError("not a 64-bit ELF binary")
    if header[5] != 1:
        raise ValueError("not a little-endian ELF binary")
    machine = struct.unpack_from("<H", header, 18)[0]
    if machine != ELF_MACHINES[arch]:
        raise ValueError(f"ELF machine {machine} does not match {arch}")


def verify_macho(path: Path, arch: str) -> None:
    """Verify one thin 64-bit Mach-O binary's architecture."""
    with path.open("rb") as stream:
        header = stream.read(32)
    if len(header) < 8:
        raise ValueError("Mach-O header is truncated")
    if header[:4] == b"\xcf\xfa\xed\xfe":
        byte_order = "<"
    elif header[:4] == b"\xfe\xed\xfa\xcf":
        byte_order = ">"
    else:
        raise ValueError("not a thin 64-bit Mach-O binary")
    cpu = struct.unpack_from(f"{byte_order}I", header, 4)[0]
    if cpu != MACHO_CPUS[arch]:
        raise ValueError(f"Mach-O CPU {cpu:#x} does not match {arch}")


def parse_args() -> argparse.Namespace:
    """Parse target identity and binary paths."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--os", choices=("linux", "macos"), required=True)
    parser.add_argument("--arch", choices=tuple(ELF_MACHINES), required=True)
    parser.add_argument("binaries", nargs="+", type=Path)
    return parser.parse_args()


def main() -> int:
    """Validate all binaries without executing cross-target code."""
    args = parse_args()
    verifier = verify_elf if args.os == "linux" else verify_macho
    for path in args.binaries:
        if not path.is_file():
            raise SystemExit(f"ERROR: missing binary: {path}")
        try:
            verifier(path, args.arch)
        except (OSError, ValueError) as error:
            raise SystemExit(f"ERROR: {path}: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
