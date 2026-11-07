"""Report publication must preserve the input workbook and previous output."""

import contextlib
import importlib.util
import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/formula_check.py"
SPEC = importlib.util.spec_from_file_location("formula_check", SCRIPT)
formula_check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(formula_check)


class BrokenWriter:
    def __init__(self, stream: object) -> None:
        self.stream = stream

    def __getattr__(self, name: str) -> object:
        return getattr(self.stream, name)

    def __enter__(self) -> "BrokenWriter":
        return self

    def __exit__(self, *args: object) -> object:
        return self.stream.__exit__(*args)

    def write(self, content: str) -> None:
        self.stream.write(content[:8])
        self.stream.flush()
        raise OSError("fixture partial write failure")


class FormulaReportPublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.workbook = self.root / "input.xlsx"
        with zipfile.ZipFile(self.workbook, "w") as archive:
            archive.writestr(
                "xl/workbook.xml",
                '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
                'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
                '<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>',
            )
            archive.writestr(
                "xl/_rels/workbook.xml.rels",
                '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                '<Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>',
            )
            archive.writestr(
                "xl/worksheets/sheet1.xml",
                '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
                '<sheetData><row r="1"><c r="A1"><f>1+1</f><v>2</v></c></row></sheetData>'
                "</worksheet>",
            )
        self.original_workbook = self.workbook.read_bytes()

    def run_cli(self, output: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(self.workbook), "--report", "-o", str(output)],
            capture_output=True,
            text=True,
            timeout=10,
        )

    def run_main(self, output: Path) -> tuple[int, str]:
        stderr = io.StringIO()
        argv = [str(SCRIPT), str(self.workbook), "--report", "-o", str(output)]
        with patch.object(sys, "argv", argv), contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as exited:
                formula_check.main()
        return exited.exception.code, stderr.getvalue()

    def test_refuses_input_aliases(self) -> None:
        aliases = [self.workbook, self.root / "." / "input.xlsx"]
        hardlink = self.root / "hardlink.json"
        os.link(self.workbook, hardlink)
        aliases.append(hardlink)
        if os.name != "nt":
            symlink = self.root / "symlink.json"
            symlink.symlink_to(self.workbook.name)
            aliases.append(symlink)
        for alias in aliases:
            with self.subTest(alias=alias):
                self.workbook.write_bytes(self.original_workbook)
                result = self.run_cli(alias)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("report", result.stderr.lower())
                self.assertEqual(self.workbook.read_bytes(), self.original_workbook)

    def test_successful_report_preserves_input(self) -> None:
        output = self.root / "report.json"
        result = self.run_cli(output)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(report["status"], "success")
        self.assertEqual(report["total_formulas"], 1)
        self.assertEqual(self.workbook.read_bytes(), self.original_workbook)

    @unittest.skipIf(os.name == "nt", "POSIX modes and symlinks")
    def test_existing_symlink_and_target_mode_are_preserved(self) -> None:
        target = self.root / "report.json"
        target.write_text("previous", encoding="utf-8")
        target.chmod(0o640)
        link = self.root / "report-link.json"
        link.symlink_to(target.name)
        result = self.run_cli(link)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(link.is_symlink())
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o640)
        self.assertEqual(json.loads(target.read_text())["status"], "success")

    def test_partial_write_preserves_old_or_absent_output(self) -> None:
        real_open = open
        real_temporary = tempfile.NamedTemporaryFile

        def broken_open(*args: object, **kwargs: object) -> object:
            stream = real_open(*args, **kwargs)
            return BrokenWriter(stream) if "w" in str(args[1:2]) else stream

        def broken_temporary(*args: object, **kwargs: object) -> BrokenWriter:
            return BrokenWriter(real_temporary(*args, **kwargs))

        for existing in (False, True):
            with self.subTest(existing=existing):
                output = self.root / f"report-{existing}.json"
                if existing:
                    output.write_bytes(b"previous report")
                before = set(self.root.iterdir())
                with patch("builtins.open", side_effect=broken_open):
                    with patch("tempfile.NamedTemporaryFile", side_effect=broken_temporary):
                        code, error = self.run_main(output)
                self.assertEqual(code, 1)
                self.assertIn("fixture partial write failure", error)
                self.assertEqual(set(self.root.iterdir()), before)
                if existing:
                    self.assertEqual(output.read_bytes(), b"previous report")
                self.assertEqual(self.workbook.read_bytes(), self.original_workbook)

    def test_replace_failure_preserves_previous_report(self) -> None:
        output = self.root / "report.json"
        output.write_bytes(b"previous report")
        before = set(self.root.iterdir())
        with patch("os.replace", side_effect=OSError("fixture replace failure")):
            code, error = self.run_main(output)
        self.assertEqual(code, 1)
        self.assertIn("fixture replace failure", error)
        self.assertEqual(output.read_bytes(), b"previous report")
        self.assertEqual(set(self.root.iterdir()), before)

    def test_missing_parent_is_reported_without_creating_it(self) -> None:
        output = self.root / "missing" / "report.json"
        code, error = self.run_main(output)
        self.assertEqual(code, 1)
        self.assertIn("report", error.lower())
        self.assertFalse(output.parent.exists())


if __name__ == "__main__":
    unittest.main()
