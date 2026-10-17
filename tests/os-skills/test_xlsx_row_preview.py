#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""A workbook edit preview must report real change counts without writing."""

import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_shift_rows.py"
)
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"


class RowPreviewTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.work = Path(self.temp_dir.name)
        fixtures = {
            "xl/worksheets/sheet1.xml": (
                f'<worksheet xmlns="{NS}"><dimension ref="A1:B9"/><sheetData>'
                '<row r="5"><c r="A5"><f>SUM(B5:B9)</f></c></row></sheetData>'
                '<mergeCells><mergeCell ref="A5:B5"/></mergeCells></worksheet>'
            ),
            "xl/charts/chart1.xml": '<c:chart xmlns:c="urn:chart"><c:f>Sheet1!$A$5:$A$9</c:f></c:chart>',
            "xl/tables/table1.xml": f'<table xmlns="{NS}" ref="A1:B9"/>',
            "xl/pivotCaches/pivotCacheDefinition1.xml": (
                f'<pivotCacheDefinition xmlns="{NS}"><cacheSource>'
                '<worksheetSource ref="A1:B9"/></cacheSource></pivotCacheDefinition>'
            ),
            "xl/worksheets/unchanged.xml": f'<worksheet xmlns="{NS}"><sheetData/></worksheet>',
            "notes.txt": "Do not modify unrelated files.\n",
        }
        for relative, content in fixtures.items():
            path = self.work / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")

    def snapshot(self) -> dict[str, bytes]:
        return {
            path.relative_to(self.work).as_posix(): path.read_bytes()
            for path in self.work.rglob("*")
            if path.is_file()
        }

    def run_shift(self, operation: str, dry_run: bool) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-X", "utf8", str(SCRIPT), str(self.work), operation, "5", "2"]
            + (["--dry-run"] if dry_run else []),
            capture_output=True,
            encoding="utf-8",
            check=False,
        )

    def verify_preview(self, operation: str) -> None:
        original = self.snapshot()
        preview = self.run_shift(operation, True)
        self.assertEqual(preview.returncode, 0, preview.stderr)
        self.assertEqual(self.snapshot(), original)
        self.assertIn("no files were written", preview.stdout)
        for part in ("sheet1.xml", "chart1.xml", "table1.xml", "pivotCacheDefinition1.xml"):
            self.assertIn(part, preview.stdout)
        applied = self.run_shift(operation, False)
        self.assertEqual(applied.returncode, 0, applied.stderr)
        self.assertNotEqual(self.snapshot(), original)
        for output in (preview.stdout, applied.stdout):
            self.assertRegex(output, r"Total changes: [1-9][0-9]*")

        def extract_changes(output: str) -> str:
            match = re.search(r"Total changes: ([0-9]+)", output)
            assert match is not None
            return match.group(1)

        self.assertEqual(extract_changes(preview.stdout), extract_changes(applied.stdout))
        self.assertEqual(
            [line for line in preview.stdout.splitlines() if "Updated" in line],
            [line for line in applied.stdout.splitlines() if "Updated" in line],
        )
        for relative in ("notes.txt", "xl/worksheets/unchanged.xml"):
            self.assertEqual(self.snapshot()[relative], original[relative])

    def test_insert_preview_matches_applied_counts_and_preserves_every_part(self) -> None:
        self.verify_preview("insert")

    def test_delete_preview_matches_applied_counts_and_preserves_every_part(self) -> None:
        self.verify_preview("delete")

    def test_preview_of_no_changes_leaves_workbook_identical(self) -> None:
        original = self.snapshot()
        result = subprocess.run(
            [
                sys.executable,
                "-X",
                "utf8",
                str(SCRIPT),
                str(self.work),
                "insert",
                "100",
                "1",
                "--dry-run",
            ],
            capture_output=True,
            encoding="utf-8",
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Total changes: 0", result.stdout)
        self.assertEqual(self.snapshot(), original)


if __name__ == "__main__":
    unittest.main()
