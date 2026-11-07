#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Recalculation must use a fresh, temporary LibreOffice profile per call."""

import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock
from urllib.parse import urlsplit
from urllib.request import url2pathname

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/libreoffice_recalc.py"
)
SPEC = importlib.util.spec_from_file_location("libreoffice_recalc", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
recalc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(recalc)


class ProfileIsolationTests(unittest.TestCase):
    def conversion(self, command: list[str], **options: object) -> subprocess.CompletedProcess:
        workspace = Path(command[command.index("--outdir") + 1])
        profile_arguments = [item for item in command if item.startswith("-env:UserInstallation=")]
        self.assertEqual(len(profile_arguments), 1)
        uri = profile_arguments[0].split("=", 1)[1]
        profile = Path(url2pathname(urlsplit(uri).path))
        self.assertEqual(uri, profile.resolve().as_uri())
        workspace_root = Path(os.path.commonpath([workspace, Path(command[-1]).parent]))
        self.assertEqual(profile.parent, workspace_root)
        self.assertEqual(options["timeout"], 17)
        profile.mkdir(exist_ok=True)
        (profile / "settings").write_text("temporary settings", encoding="utf-8")
        self.profiles.append(profile)
        (workspace / (Path(command[-1]).stem + ".xlsx")).write_bytes(b"recalculated")
        return subprocess.CompletedProcess(command, 0, stdout=b"converted", stderr=b"")

    def setUp(self) -> None:
        self.profiles: list[Path] = []

    def test_calls_use_unique_profiles_and_preserve_the_source(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "input.xlsx"
            source.write_bytes(b"original")
            with (
                mock.patch.object(recalc, "find_soffice", return_value="fake-office"),
                mock.patch.object(recalc, "get_libreoffice_version", return_value="test"),
                mock.patch.object(recalc.subprocess, "run", side_effect=self.conversion),
            ):
                for index in range(2):
                    output = root / f"output{index}.xlsx"
                    success, message = recalc.recalculate(str(source), str(output), timeout=17)
                    self.assertTrue(success, message)
                    self.assertEqual(output.read_bytes(), b"recalculated")
            self.assertEqual(source.read_bytes(), b"original")
        self.assertEqual(len(set(self.profiles)), 2)
        self.assertTrue(all(not profile.exists() for profile in self.profiles))

    def test_profile_uri_encodes_spaces_and_non_ascii_paths(self) -> None:
        temporary_directory = tempfile.TemporaryDirectory
        with temporary_directory() as directory:
            root = Path(directory)
            workspace_parent = root / "office space # unicode \u7a7a\u95f4"
            workspace_parent.mkdir()
            source = root / "input.xlsx"
            source.write_bytes(b"original")
            with (
                mock.patch.object(recalc, "find_soffice", return_value="fake-office"),
                mock.patch.object(recalc, "get_libreoffice_version", return_value="test"),
                mock.patch.object(recalc.subprocess, "run", side_effect=self.conversion),
                mock.patch.object(
                    recalc.tempfile,
                    "TemporaryDirectory",
                    side_effect=lambda **kwargs: temporary_directory(
                        dir=workspace_parent, **kwargs
                    ),
                ),
            ):
                success, message = recalc.recalculate(str(source), str(root / "output.xlsx"), 17)
                self.assertTrue(success, message)
            self.assertIn("%20", self.profiles[0].as_uri())
            self.assertIn("%23", self.profiles[0].as_uri())
            self.assertIn("%E7", self.profiles[0].as_uri())
            self.assertFalse(self.profiles[0].exists())

    def test_failed_processes_clean_up_the_temporary_profile(self) -> None:
        for failure in ("timeout", "crash"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source = root / "input.xlsx"
                source.write_bytes(b"original")
                output = root / "output.xlsx"
                output.write_bytes(b"existing output")

                def fail(command: list[str], **options: object) -> subprocess.CompletedProcess:
                    self.conversion(command, **options)
                    if failure == "timeout":
                        raise subprocess.TimeoutExpired(command, 17)
                    return subprocess.CompletedProcess(command, 1, stdout=b"", stderr=b"failure")

                with (
                    mock.patch.object(recalc, "find_soffice", return_value="fake-office"),
                    mock.patch.object(recalc, "get_libreoffice_version", return_value="test"),
                    mock.patch.object(recalc.subprocess, "run", side_effect=fail),
                ):
                    success, _ = recalc.recalculate(str(source), str(output), timeout=17)
                    self.assertFalse(success)
                self.assertEqual(source.read_bytes(), b"original")
                self.assertEqual(output.read_bytes(), b"existing output")
                self.assertFalse(self.profiles[-1].exists())

    def test_missing_libreoffice_does_not_create_a_profile(self) -> None:
        with (
            mock.patch.object(recalc, "find_soffice", return_value=None),
            mock.patch.object(recalc.tempfile, "TemporaryDirectory") as workspace,
        ):
            success, message = recalc.recalculate("input.xlsx", "output.xlsx")
            self.assertFalse(success)
            self.assertIn("LibreOffice not found", message)
            workspace.assert_not_called()


if __name__ == "__main__":
    unittest.main()
