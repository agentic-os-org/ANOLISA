#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Discover installed Windows LibreOffice without requiring a PATH change."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/libreoffice_recalc.py"
)
SPEC = importlib.util.spec_from_file_location("libreoffice_recalc", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
recalc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(recalc)


def launcher(root: Path, name: str) -> Path:
    binary = root / "LibreOffice/program" / name
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text("fake launcher", encoding="utf-8")
    binary.chmod(0o700)
    return binary


class WindowsDiscoveryTests(unittest.TestCase):
    def discover(self, environment: dict[str, str], platform: str = "win32") -> str | None:
        with (
            mock.patch.dict(os.environ, environment, clear=True),
            mock.patch.object(recalc.sys, "platform", platform),
            mock.patch.object(recalc.shutil, "which", return_value=None),
        ):
            return recalc.find_soffice()

    def test_console_launcher_is_preferred_to_gui_executable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            launcher(root, "soffice.exe")
            console = launcher(root, "soffice.com")
            self.assertEqual(self.discover({"ProgramFiles": str(root)}), str(console))

    def test_gui_executable_is_a_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = launcher(root, "soffice.exe")
            self.assertEqual(self.discover({"ProgramFiles": str(root)}), str(binary))

    def test_program_files_variants_are_supported(self) -> None:
        for variable in ("ProgramW6432", "ProgramFiles(x86)"):
            with self.subTest(variable=variable), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                console = launcher(root, "soffice.com")
                self.assertEqual(self.discover({variable: str(root)}), str(console))

    def test_native_64_bit_installation_precedes_redirected_program_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            native = Path(directory) / "native"
            redirected = Path(directory) / "redirected"
            console = launcher(native, "soffice.com")
            launcher(redirected, "soffice.com")
            self.assertEqual(
                self.discover({"ProgramFiles": str(redirected), "ProgramW6432": str(native)}),
                str(console),
            )

    def test_existing_path_installation_keeps_priority(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            launcher(root, "soffice.com")
            with (
                mock.patch.dict(os.environ, {"ProgramFiles": str(root)}, clear=True),
                mock.patch.object(recalc.sys, "platform", "win32"),
                mock.patch.object(
                    recalc.shutil,
                    "which",
                    side_effect=lambda name: "custom-office" if name == "soffice" else None,
                ),
            ):
                self.assertEqual(recalc.find_soffice(), "custom-office")

    def test_non_windows_discovery_ignores_program_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            launcher(root, "soffice.com")
            self.assertIsNone(self.discover({"ProgramFiles": str(root)}, platform="linux"))

    def test_missing_installation_remains_unavailable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            self.assertIsNone(self.discover({"ProgramFiles": directory}))


if __name__ == "__main__":
    unittest.main()
