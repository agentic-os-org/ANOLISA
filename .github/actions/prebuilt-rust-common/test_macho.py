#!/usr/bin/env python3
"""Check macOS release CPU, deployment, and system-library constraints."""

from __future__ import annotations

import importlib.util
import struct
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location(
    "verify_macho", Path(__file__).with_name("verify-macho.py")
)
macho = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(macho)


def executable(
    arch: str, minimum: int = 11 << 16, dylib: str = "/usr/lib/libSystem.B.dylib"
) -> bytes:
    """Create a thin executable fixture with version and dylib load commands."""
    name = dylib.encode() + b"\0"
    name += b"\0" * (-len(name) % 8)
    commands = struct.pack("<6I", 0x32, 24, 1, minimum, 0, 0)
    commands += struct.pack("<6I", 0xC, 24 + len(name), 24, 0, 0, 0) + name
    return struct.pack(
        "<IiiIIIII", 0xFEEDFACF, macho.CPU_TYPES[arch], 0, 2, 2, len(commands), 0, 0
    ) + commands


class MachOTests(unittest.TestCase):
    def test_both_architectures(self) -> None:
        for arch in macho.CPU_TYPES:
            with self.subTest(arch=arch):
                macho.validate(executable(arch), (11, 0, 0), arch)

    def test_default_stays_arm64(self) -> None:
        macho.validate(executable("aarch64"), (11, 0, 0))
        with self.assertRaisesRegex(macho.MachOError, "expected aarch64 CPU"):
            macho.validate(executable("x86_64"), (11, 0, 0))

    def test_wrong_architecture(self) -> None:
        for actual, expected in (("aarch64", "x86_64"), ("x86_64", "aarch64")):
            with self.subTest(actual=actual), self.assertRaisesRegex(
                macho.MachOError, "CPU type"
            ):
                macho.validate(executable(actual), (11, 0, 0), expected)

    def test_wrong_minimum(self) -> None:
        for arch in macho.CPU_TYPES:
            with self.subTest(arch=arch), self.assertRaisesRegex(
                macho.MachOError, "deployment target"
            ):
                macho.validate(executable(arch, 12 << 16), (11, 0, 0), arch)

    def test_non_system_library(self) -> None:
        for arch in macho.CPU_TYPES:
            with self.subTest(arch=arch), self.assertRaisesRegex(macho.MachOError, "non-system"):
                macho.validate(
                    executable(arch, dylib="/opt/homebrew/lib/libssl.dylib"), (11, 0, 0), arch
                )


if __name__ == "__main__":
    unittest.main()
