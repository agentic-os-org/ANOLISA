"""Recalculation must publish a complete workbook or preserve the previous one."""

import errno
import importlib.util
import io
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SOURCE = Path(
    os.environ.get(
        "RECALC_SOURCE",
        ROOT / "src/os-skills/others/xlsx/scripts/libreoffice_recalc.py",
    )
)
SPEC = importlib.util.spec_from_file_location("libreoffice_recalc", SOURCE)
assert SPEC is not None and SPEC.loader is not None
recalc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(recalc)


def workbook_bytes(value: str) -> bytes:
    """Produce distinct, readable ZIP fixtures for source and converted workbooks."""
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w") as archive:
        archive.writestr("xl/worksheets/sheet1.xml", f"<worksheet><v>{value}</v></worksheet>")
    return stream.getvalue()


class AtomicOutputTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workspace = tempfile.TemporaryDirectory()
        self.addCleanup(self.workspace.cleanup)
        self.directory = Path(self.workspace.name)
        self.source = self.directory / "source workbook.xlsx"
        self.output = self.directory / "output.xlsx"
        self.original = workbook_bytes("original")
        self.converted = workbook_bytes("recalculated")
        self.previous = workbook_bytes("previous output")
        self.source.write_bytes(self.original)
        self.conversion_finished = False
        self.addCleanup(patch.stopall)
        patch.object(recalc, "find_soffice", return_value="isolated-soffice").start()
        patch.object(recalc, "get_libreoffice_version", return_value="test version").start()
        patch.object(recalc.subprocess, "run", side_effect=self.convert).start()

    def convert(self, command: list[str], **kwargs: object) -> subprocess.CompletedProcess:
        output_dir = Path(command[command.index("--outdir") + 1])
        output_dir.mkdir(parents=True, exist_ok=True)
        converted = output_dir / f"{Path(command[-1]).stem}.xlsx"
        converted.write_bytes(self.converted)
        shutil.copymode(self.source, converted)
        self.conversion_finished = True
        return subprocess.CompletedProcess(command, 0, stdout=b"converted", stderr=b"")

    def call(self, output: Path) -> tuple[bool, str]:
        try:
            return recalc.recalculate(str(self.source), str(output))
        except OSError as error:
            return False, f"uncaught: {error}"

    def assert_failure(self, result: tuple[bool, str]) -> None:
        success, message = result
        self.assertFalse(success)
        self.assertNotIn("uncaught:", message, "I/O failures must use the result contract")
        self.assertIn("publish", message.lower())
        self.assertIn(str(self.output), message)

    def assert_clean(self, directory: Path | None = None) -> None:
        self.assertEqual(list((directory or self.directory).glob(".xlsx-recalc-*")), [])

    def inject_partial_copy(self) -> None:
        original_copy = shutil.copy

        def copy(source: str, destination: str, **kwargs: object) -> str:
            if self.conversion_finished:
                target = Path(destination)
                if target.is_dir():
                    target /= Path(source).name
                target.write_bytes(self.converted[:17])
                raise OSError(errno.ENOSPC, "injected disk full")
            return original_copy(source, destination, **kwargs)

        patch.object(recalc.shutil, "copy", side_effect=copy).start()

    def test_partial_copy_preserves_existing_output(self) -> None:
        self.output.write_bytes(self.previous)
        self.inject_partial_copy()
        result = self.call(self.output)
        self.assertEqual(self.output.read_bytes(), self.previous)
        self.assertEqual(self.source.read_bytes(), self.original)
        self.assert_failure(result)
        self.assert_clean()

    def test_partial_copy_does_not_publish_new_output(self) -> None:
        self.inject_partial_copy()
        result = self.call(self.output)
        self.assertFalse(self.output.exists())
        self.assert_failure(result)
        self.assert_clean()

    def test_partial_copy_preserves_in_place_source(self) -> None:
        self.inject_partial_copy()
        self.output = self.source
        result = self.call(self.output)
        self.assertEqual(self.source.read_bytes(), self.original)
        self.assert_failure(result)
        self.assert_clean()

    def test_failed_replace_preserves_output_and_cleans_staging(self) -> None:
        self.output.write_bytes(self.previous)
        with patch.object(recalc.os, "replace", side_effect=PermissionError("replace denied")):
            result = self.call(self.output)
        self.assertEqual(self.output.read_bytes(), self.previous)
        self.assert_failure(result)
        self.assert_clean()

    def test_failed_staging_creation_uses_result_contract(self) -> None:
        self.output.write_bytes(self.previous)
        with patch.object(
            recalc.tempfile, "mkstemp", side_effect=PermissionError("cannot create staging file")
        ):
            result = self.call(self.output)
        self.assertEqual(self.output.read_bytes(), self.previous)
        self.assert_failure(result)
        self.assert_clean()

    def test_failed_mode_copy_preserves_existing_output(self) -> None:
        self.output.write_bytes(self.previous)
        original_mode_copy = shutil.copymode

        def copy_mode(source: str, destination: str, **kwargs: object) -> None:
            if Path(source) == self.output:
                raise PermissionError("cannot retain output permissions")
            original_mode_copy(source, destination, **kwargs)

        with patch.object(recalc.shutil, "copymode", side_effect=copy_mode):
            result = self.call(self.output)
        self.assertEqual(self.output.read_bytes(), self.previous)
        self.assert_failure(result)
        self.assert_clean()

    def test_read_only_staging_is_cleaned_after_failed_replace(self) -> None:
        self.output.write_bytes(self.previous)
        self.output.chmod(0o444)
        try:
            with patch.object(recalc.os, "replace", side_effect=PermissionError("replace denied")):
                result = self.call(self.output)
            self.assertEqual(self.output.read_bytes(), self.previous)
            self.assert_failure(result)
            self.assert_clean()
        finally:
            self.output.chmod(0o600)

    def test_failed_parent_creation_uses_result_contract(self) -> None:
        blocker = self.directory / "file parent"
        blocker.write_bytes(b"parent remains a file")
        self.output = blocker / "result.xlsx"
        result = self.call(self.output)
        self.assertEqual(blocker.read_bytes(), b"parent remains a file")
        self.assertEqual(self.source.read_bytes(), self.original)
        self.assert_failure(result)

    def test_success_publishes_complete_workbook(self) -> None:
        self.output.write_bytes(self.previous)
        self.assertTrue(self.call(self.output)[0])
        self.assertEqual(self.output.read_bytes(), self.converted)
        self.assertEqual(self.source.read_bytes(), self.original)
        with zipfile.ZipFile(self.output) as archive:
            self.assertIsNone(archive.testzip())
        self.assert_clean()

    def test_success_supports_in_place_output(self) -> None:
        self.assertTrue(self.call(self.source)[0])
        self.assertEqual(self.source.read_bytes(), self.converted)
        self.assert_clean()

    def test_success_creates_output_parent(self) -> None:
        destination = self.directory / "new parent" / "result.xlsx"
        self.assertTrue(self.call(destination)[0])
        self.assertEqual(destination.read_bytes(), self.converted)
        self.assert_clean(destination.parent)

    def test_success_keeps_directory_destination_behavior(self) -> None:
        destination = self.directory / "directory output"
        destination.mkdir()
        self.assertTrue(self.call(destination)[0])
        self.assertEqual((destination / self.source.name).read_bytes(), self.converted)
        self.assert_clean(destination)

    @unittest.skipIf(os.name == "nt", "POSIX mode bits are not available on Windows")
    def test_success_preserves_existing_output_mode(self) -> None:
        self.output.write_bytes(self.previous)
        self.output.chmod(0o640)
        self.source.chmod(0o644)
        self.assertTrue(self.call(self.output)[0])
        self.assertEqual(stat.S_IMODE(self.output.stat().st_mode), 0o640)

    @unittest.skipIf(os.name == "nt", "POSIX mode bits are not available on Windows")
    def test_new_output_keeps_converted_file_mode(self) -> None:
        self.source.chmod(0o640)
        self.assertTrue(self.call(self.output)[0])
        self.assertEqual(stat.S_IMODE(self.output.stat().st_mode), 0o640)

    def create_output_link(self) -> Path:
        target_dir = self.directory / "target directory"
        target_dir.mkdir()
        target = target_dir / "target.xlsx"
        target.write_bytes(self.previous)
        try:
            self.output.symlink_to(target)
        except OSError as error:
            self.skipTest(f"symlink creation unavailable: {error}")
        return target

    def test_success_follows_symlink_and_stages_beside_target(self) -> None:
        target = self.create_output_link()
        real_replace = os.replace
        with patch.object(recalc.os, "replace", wraps=real_replace) as replace:
            result = self.call(self.output)
        self.assertTrue(result[0])
        self.assertTrue(self.output.is_symlink())
        self.assertEqual(target.read_bytes(), self.converted)
        staging, destination = replace.call_args.args
        self.assertEqual(Path(staging).parent, target.parent)
        self.assertEqual(Path(destination), target)
        self.assert_clean(target.parent)

    def test_partial_copy_preserves_symlink_target(self) -> None:
        target = self.create_output_link()
        self.inject_partial_copy()
        result = self.call(self.output)
        self.assertTrue(self.output.is_symlink())
        self.assertEqual(target.read_bytes(), self.previous)
        self.assert_failure(result)
        self.assert_clean(target.parent)


@unittest.skipIf(os.name == "nt", "real POSIX executable stub")
class PosixCliTests(unittest.TestCase):
    def test_cli_conversion_and_publication_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            source = directory / "source workbook.xlsx"
            converted = directory / "converted fixture.xlsx"
            source.write_bytes(workbook_bytes("original"))
            converted.write_bytes(workbook_bytes("converted"))
            executable = directory / "soffice"
            executable.write_text(
                "#!/bin/sh\n"
                'if [ "$1" = --version ]; then echo "LibreOffice test"; exit 0; fi\n'
                'while [ "$#" -gt 0 ]; do\n'
                '  case "$1" in --outdir) shift; output="$1";; esac\n'
                '  input="$1"; shift\n'
                "done\n"
                'cp "$RECALC_FIXTURE" "$output/$(basename "$input" .xlsx).xlsx"\n',
                encoding="utf-8",
            )
            executable.chmod(0o755)
            environment = dict(os.environ, RECALC_FIXTURE=str(converted))
            environment["PATH"] = str(directory) + os.pathsep + environment.get("PATH", "")
            output = directory / "success.xlsx"
            success = subprocess.run(
                [sys.executable, str(SOURCE), str(source), str(output)],
                capture_output=True,
                text=True,
                env=environment,
                timeout=20,
            )
            self.assertEqual(success.returncode, 0, success.stdout + success.stderr)
            self.assertEqual(output.read_bytes(), converted.read_bytes())
            blocker = directory / "blocked parent"
            blocker.write_bytes(b"unchanged")
            failure = subprocess.run(
                [sys.executable, str(SOURCE), str(source), str(blocker / "result.xlsx")],
                capture_output=True,
                text=True,
                env=environment,
                timeout=20,
            )
            self.assertEqual(failure.returncode, 1)
            self.assertIn("publish", failure.stdout.lower())
            self.assertNotIn("Traceback", failure.stderr)
            self.assertEqual(blocker.read_bytes(), b"unchanged")


if __name__ == "__main__":
    unittest.main()
