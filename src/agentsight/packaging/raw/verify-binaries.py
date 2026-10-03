#!/usr/bin/env python3
"""Verify that prebuilt AgentSight binaries match the requested target."""

from __future__ import annotations

import argparse
import hashlib
import struct
import tomllib
from pathlib import Path


ELF_MACHINES = {
    "x86_64": 62,
    "aarch64": 183,
}
MACHO_CPUS = {
    "x86_64": 0x01000007,
    "aarch64": 0x0100000C,
}


def verify_build_metadata(
    path: Path,
    version: str,
    os_name: str,
    arch: str,
    binaries: list[Path],
) -> None:
    """Verify build identity and bind it to the supplied binary bytes."""
    try:
        with path.open("rb") as stream:
            metadata = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"cannot read build metadata: {error}") from error

    expected_identity = {
        "version": version,
        "target_os": os_name,
        "target_arch": arch,
    }
    for key, expected in expected_identity.items():
        actual = metadata.get(key)
        if actual != expected:
            raise ValueError(f"{key} {actual!r} does not match {expected!r}")

    hashes = metadata.get("binaries")
    if not isinstance(hashes, dict):
        raise ValueError("missing [binaries] SHA-256 table")
    expected_names = {binary.name for binary in binaries}
    unexpected = sorted(set(hashes) - expected_names)
    if unexpected:
        raise ValueError(f"unexpected binaries in metadata: {', '.join(unexpected)}")
    for binary in binaries:
        expected_hash = hashes.get(binary.name)
        if not isinstance(expected_hash, str):
            raise ValueError(f"missing SHA-256 for {binary.name}")
        try:
            actual_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
        except OSError as error:
            raise ValueError(f"cannot hash {binary.name}: {error}") from error
        if expected_hash.lower() != actual_hash:
            raise ValueError(f"SHA-256 for {binary.name} does not match build metadata")


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
    parser.add_argument("--metadata", type=Path)
    parser.add_argument("--component-version")
    parser.add_argument("binaries", nargs="+", type=Path)
    args = parser.parse_args()
    if (args.metadata is None) != (args.component_version is None):
        parser.error("--metadata and --component-version must be supplied together")
    return args


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
    if args.metadata is not None:
        try:
            verify_build_metadata(
                args.metadata,
                args.component_version,
                args.os,
                args.arch,
                args.binaries,
            )
        except ValueError as error:
            raise SystemExit(f"ERROR: {args.metadata}: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
