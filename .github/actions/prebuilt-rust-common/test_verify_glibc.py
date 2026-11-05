#!/usr/bin/env python3
"""Exercise release linkage checks without compiling or executing Linux binaries."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "verify_glibc", Path(__file__).with_name("verify-glibc.py")
)
glibc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(glibc)

HEADER = """ELF Header:
  Class:                             ELF64
  Type:                              {kind}
  Machine:                           {machine}
"""
PROGRAM_HEADERS = """Program Headers:
  Type           Offset   VirtAddr           PhysAddr           FileSiz  MemSiz   Flg Align
  LOAD           0x000000 0x0000000000000000 0x0000000000000000 0x001000 0x001000 R E 0x1000
"""
STATIC_PIE_DYNAMIC = """Dynamic section at offset 0x1000 contains 3 entries:
  Tag                Type                 Name/Value
 0x0000000000000007 (RELA)               0x2000
 0x000000006ffffffb (FLAGS_1)            Flags: PIE
 0x0000000000000000 (NULL)               0x0
"""


class GlibcVerifierTests(unittest.TestCase):
    def run_verifier(
        self,
        *options: str,
        actual_arch: str = "x86_64",
        expected_arch: str = "x86_64",
        kind: str = "EXEC (Executable file)",
        program_headers: str = PROGRAM_HEADERS,
        dynamic: str = "There is no dynamic section in this file.\n",
        symbols: str = "No version information found in this file.\n",
    ) -> tuple[int, str, list[tuple[str, ...]]]:
        outputs = {
            ("-h",): HEADER.format(kind=kind, machine=glibc.MACHINES[actual_arch]),
            ("--program-headers", "--wide"): program_headers,
            ("--dynamic", "--wide"): dynamic,
            ("--version-info",): symbols,
        }
        output = io.StringIO()
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "binary"
            binary.touch()
            with (
                patch("sys.argv", ["verify-glibc.py", "--arch", expected_arch, *options, str(binary)]),
                patch.object(glibc, "readelf", side_effect=lambda _, *args: outputs[args]) as reader,
                contextlib.redirect_stdout(output),
                contextlib.redirect_stderr(output),
            ):
                try:
                    result = glibc.main()
                except SystemExit as error:
                    result = error.code
                calls = [call.args[1:] for call in reader.call_args_list]
        return result, output.getvalue(), calls

    def test_static_executable_and_static_pie_on_both_architectures(self) -> None:
        for arch in glibc.MACHINES:
            for kind, dynamic in (
                ("EXEC (Executable file)", "There is no dynamic section in this file.\n"),
                ("DYN (Position-Independent Executable file)", STATIC_PIE_DYNAMIC),
            ):
                with self.subTest(arch=arch, kind=kind):
                    result, output, calls = self.run_verifier(
                        "--require-static",
                        actual_arch=arch,
                        expected_arch=arch,
                        kind=kind,
                        dynamic=dynamic,
                    )
                    self.assertEqual(result, 0, output)
                    self.assertIn("static=true", output)
                    self.assertNotIn(("--version-info",), calls)

    def test_static_mode_rejects_interpreter_or_needed_independently(self) -> None:
        cases = (
            {"program_headers": PROGRAM_HEADERS + "  INTERP         0x001000 0x0\n"},
            {"dynamic": STATIC_PIE_DYNAMIC + " 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]\n"},
        )
        for fixture, expected in zip(cases, ("PT_INTERP", "DT_NEEDED")):
            with self.subTest(expected=expected):
                result, output, _ = self.run_verifier("--require-static", **fixture)
                self.assertEqual(result, 1)
                self.assertIn(expected, output)

    def test_static_mode_rejects_wrong_architecture_and_relocatable_objects(self) -> None:
        for fixture, expected in (
            ({"actual_arch": "aarch64"}, "expected 'Advanced Micro Devices X86-64'"),
            ({"kind": "REL (Relocatable file)"}, "not an ELF executable"),
        ):
            with self.subTest(fixture=fixture):
                result, output, _ = self.run_verifier("--require-static", **fixture)
                self.assertEqual(result, 1)
                self.assertIn(expected, output)

    def test_glibc_mode_preserves_symbol_requirements_and_ceiling(self) -> None:
        for symbols, status, expected in (
            ("GLIBC_2.17 GLIBC_2.28", 0, "max_glibc=GLIBC_2.28"),
            ("GLIBC_2.29", 1, "above GLIBC_2.28"),
            ("No version information found in this file.", 1, "no GLIBC symbol versions"),
        ):
            with self.subTest(symbols=symbols):
                result, output, calls = self.run_verifier("--max", "2.28", symbols=symbols)
                self.assertEqual(result, status, output)
                self.assertIn(expected, output)
                self.assertEqual(calls, [("-h",), ("--version-info",)])

    def test_linkage_mode_must_be_explicit_and_unique(self) -> None:
        for options in ((), ("--require-static", "--max", "2.28")):
            with self.subTest(options=options):
                result, _, calls = self.run_verifier(*options)
                self.assertEqual(result, 2)
                self.assertEqual(calls, [])

    def test_only_sandbox_release_check_opts_into_static(self) -> None:
        build = Path(__file__).resolve().parent.parent / "build-sec-core-prebuilt" / "build.sh"
        source = build.read_text()
        self.assertEqual(source.count("--require-static"), 1)
        self.assertIn('--require-static \\\n    "$BUILD_DIR/linux-sandbox"', source)
        self.assertIn('--max 2.28 \\\n        "$BUILD_DIR/v2/bin/$binary"', source)
        self.assertIn('--max 2.28 \\\n    "$BUILD_DIR/python-runtime/bin/python3.11"', source)


if __name__ == "__main__":
    unittest.main()
