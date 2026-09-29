#!/usr/bin/env python3
"""Validate a macOS release executable's architecture and deployment target."""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path


MH_MAGIC_64 = 0xFEEDFACF
CPU_TYPES = {"aarch64": 0x0100000C, "x86_64": 0x01000007}
MH_EXECUTE = 2
LC_LOAD_DYLIB = 0xC
LC_LOAD_WEAK_DYLIB = 0x80000018
LC_REEXPORT_DYLIB = 0x8000001F
LC_LOAD_UPWARD_DYLIB = 0x80000023
LC_VERSION_MIN_MACOSX = 0x24
LC_BUILD_VERSION = 0x32
PLATFORM_MACOS = 1


class MachOError(RuntimeError):
    """The input is not the expected deployable Mach-O executable."""


def decode_version(value: int) -> tuple[int, int, int]:
    """Decode Apple's packed X.Y.Z version integer."""
    return value >> 16, (value >> 8) & 0xFF, value & 0xFF


def parse_version(value: str) -> tuple[int, int, int]:
    """Parse an X[.Y[.Z]] version supplied by the release profile."""
    try:
        parts = tuple(int(part) for part in value.split("."))
    except ValueError as error:
        raise MachOError(f"invalid version: {value}") from error
    if not 1 <= len(parts) <= 3 or any(part < 0 or part > 255 for part in parts):
        raise MachOError(f"invalid version: {value}")
    return (*parts, *(0 for _ in range(3 - len(parts))))


def c_string(data: bytes, offset: int, end: int) -> str:
    """Decode a bounded Mach-O load-command string."""
    if offset < 0 or offset >= end:
        raise MachOError("invalid dylib name offset")
    terminator = data.find(b"\0", offset, end)
    if terminator < 0:
        raise MachOError("unterminated dylib name")
    try:
        return data[offset:terminator].decode("utf-8")
    except UnicodeDecodeError as error:
        raise MachOError("non-UTF-8 dylib name") from error


def validate(
    data: bytes, expected_minimum: tuple[int, int, int], arch: str = "aarch64"
) -> None:
    """Validate architecture, deployment target, and dynamic library paths."""
    if len(data) < 32:
        raise MachOError("file is too small for a Mach-O 64-bit header")
    (
        magic,
        cpu_type,
        _cpu_subtype,
        file_type,
        command_count,
        commands_size,
        _flags,
        _reserved,
    ) = struct.unpack_from("<IiiIIIII", data)
    if magic != MH_MAGIC_64:
        raise MachOError("expected a little-endian Mach-O 64-bit executable")
    if cpu_type != CPU_TYPES[arch]:
        raise MachOError(f"expected {arch} CPU type, got 0x{cpu_type:08x}")
    if file_type != MH_EXECUTE:
        raise MachOError(f"expected MH_EXECUTE, got file type {file_type}")
    if 32 + commands_size > len(data):
        raise MachOError("load command table extends beyond the file")

    minimums: list[tuple[int, int, int]] = []
    dylibs: list[str] = []
    cursor = 32
    commands_end = 32 + commands_size
    for _ in range(command_count):
        if cursor + 8 > commands_end:
            raise MachOError("truncated load command")
        command, size = struct.unpack_from("<II", data, cursor)
        if size < 8 or cursor + size > commands_end:
            raise MachOError("invalid load command size")
        if command == LC_BUILD_VERSION:
            if size < 24:
                raise MachOError("truncated LC_BUILD_VERSION")
            platform, minimum, _sdk, _tools = struct.unpack_from(
                "<IIII", data, cursor + 8
            )
            if platform != PLATFORM_MACOS:
                raise MachOError(f"expected macOS build platform, got {platform}")
            minimums.append(decode_version(minimum))
        elif command == LC_VERSION_MIN_MACOSX:
            if size < 16:
                raise MachOError("truncated LC_VERSION_MIN_MACOSX")
            minimum, _sdk = struct.unpack_from("<II", data, cursor + 8)
            minimums.append(decode_version(minimum))
        elif command in {
            LC_LOAD_DYLIB,
            LC_LOAD_WEAK_DYLIB,
            LC_REEXPORT_DYLIB,
            LC_LOAD_UPWARD_DYLIB,
        }:
            if size < 24:
                raise MachOError("truncated dylib load command")
            name_offset = struct.unpack_from("<I", data, cursor + 8)[0]
            dylibs.append(c_string(data, cursor + name_offset, cursor + size))
        cursor += size
    if cursor != commands_end:
        raise MachOError("load commands do not consume the declared command table")
    if set(minimums) != {expected_minimum}:
        rendered = ", ".join(
            ".".join(map(str, value)) for value in sorted(set(minimums))
        ) or "missing"
        expected = ".".join(map(str, expected_minimum))
        raise MachOError(f"deployment target is {rendered}, expected {expected}")
    allowed = ("/usr/lib/", "/System/Library/Frameworks/")
    unsupported = sorted(name for name in dylibs if not name.startswith(allowed))
    if unsupported:
        raise MachOError(
            "non-system dynamic library reference(s): " + ", ".join(unsupported)
        )


def main() -> int:
    """Validate one executable without running it."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--min", dest="minimum", required=True)
    parser.add_argument("--arch", choices=tuple(CPU_TYPES), default="aarch64")
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    try:
        validate(args.binary.read_bytes(), parse_version(args.minimum), args.arch)
    except (OSError, MachOError) as error:
        print(f"ERROR: {args.binary}: {error}", file=sys.stderr)
        return 1
    print(f"{args.binary}: arch={args.arch} min_macos={args.minimum}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
