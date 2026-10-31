"""Exercise number-format audits through the packed-workbook CLI."""

import json
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

from test_style_audit_sheets import NS_MAIN, RELS_TEMPLATE, WORKBOOK_XML

SCRIPT = Path(__file__).with_name("style_audit.py")


class NumberFormatAuditTests(unittest.TestCase):
    def audit(self, fmt_id=200, fmt="0", value=2024, count=1, custom=True,
              element_counts=True):
        count_attribute = f' count="{count}"' if count is not None else ""
        formats = (
            f'<numFmts{count_attribute}><numFmt numFmtId="{fmt_id}" '
            f'formatCode="{fmt}"/></numFmts>' if custom else ""
        )
        fonts_count = ' count="1"' if element_counts else ""
        fills_count = ' count="2"' if element_counts else ""
        xfs_count = ' count="1"' if element_counts else ""
        styles = f'''<styleSheet xmlns="{NS_MAIN}">{formats}
          <fonts{fonts_count}><font><color rgb="000000ff"/></font></fonts>
          <fills{fills_count}><fill><patternFill patternType="none"/></fill>
            <fill><patternFill patternType="gray125"/></fill></fills>
          <cellXfs{xfs_count}><xf numFmtId="{fmt_id}" fontId="0"/></cellXfs>
        </styleSheet>'''
        sheet = f'''<worksheet xmlns="{NS_MAIN}"><sheetData><row r="1">
          <c r="A1" s="0"><v>{value}</v></c>
        </row></sheetData></worksheet>'''
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "book.xlsx"
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("xl/styles.xml", styles)
                archive.writestr("xl/workbook.xml", WORKBOOK_XML)
                archive.writestr("xl/_rels/workbook.xml.rels", RELS_TEMPLATE.format(
                    target="worksheets/sheet1.xml"))
                archive.writestr("xl/worksheets/sheet1.xml", sheet)
            result = subprocess.run(
                [sys.executable, str(SCRIPT), str(path), "--json"],
                capture_output=True, text=True,
            )
        self.assertIn(result.returncode, (0, 1), result.stderr)
        return result.returncode, json.loads(result.stdout)

    def test_num_fmts_count_mismatch(self):
        code, report = self.audit(count=9)
        self.assertEqual(code, 1)
        mismatch = [v for v in report["violations"]
                    if v["type"] == "count_mismatch"]
        self.assertEqual(len(mismatch), 1)
        self.assertEqual(mismatch[0]["element"], "numFmts")
        self.assertEqual(mismatch[0]["declared"], 9)
        self.assertEqual(mismatch[0]["actual"], 1)

    def test_custom_ids_do_not_imply_formats(self):
        for fmt_id in (165, 167, 168, 170, 200):
            with self.subTest(fmt_id=fmt_id):
                code, report = self.audit(fmt_id=fmt_id)
                self.assertEqual(code, 0, report)
                self.assertEqual(report["warnings"], [])

    def test_omitted_num_fmts_count_is_valid(self):
        code, report = self.audit(count=None)
        self.assertEqual(code, 0, report)
        self.assertEqual(report["violations"], [])

    def test_explicit_zero_count_is_checked(self):
        code, report = self.audit(count=0)
        self.assertEqual(code, 1, report)
        self.assertEqual(report["violations"][0]["declared"], 0)

    def test_real_percentage_formats_still_warn(self):
        for fmt_id, custom in ((9, False), (10, False), (165, True), (200, True)):
            with self.subTest(fmt_id=fmt_id):
                _, report = self.audit(fmt_id=fmt_id, fmt="0%", value=8, custom=custom)
                self.assertTrue(report["warnings"], report)

    def test_real_comma_formats_still_fail(self):
        for fmt_id, custom in ((3, False), (4, False), (167, True), (200, True)):
            with self.subTest(fmt_id=fmt_id):
                code, report = self.audit(fmt_id=fmt_id, fmt="#,##0", custom=custom)
                self.assertEqual(code, 1, report)
                self.assertTrue(report["violations"], report)

    def test_absent_custom_formats_are_valid(self):
        code, report = self.audit(fmt_id=0, custom=False)
        self.assertEqual(code, 0, report)
        self.assertEqual(report["violations"], [])
        self.assertEqual(report["warnings"], [])

    def test_undeclared_custom_id_is_invalid(self):
        code, report = self.audit(fmt_id=167, custom=False, value=2024)
        self.assertEqual(code, 1, report)
        types = [v["type"] for v in report["violations"]]
        self.assertIn("undefined_num_fmt", types)

    def test_omitted_element_counts_are_valid(self):
        code, report = self.audit(fmt_id=0, custom=False, element_counts=False)
        self.assertEqual(code, 0, report)
        self.assertEqual(report["violations"], [])


if __name__ == "__main__":
    unittest.main()
