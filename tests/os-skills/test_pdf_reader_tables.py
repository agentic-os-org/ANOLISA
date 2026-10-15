#!/usr/bin/env python3
"""Regression tests for PDF table extraction in read_pdf.py (#5626).

Builds a real three-page ruled PDF with PyMuPDF (drawn ruling plus cell
text) and drives the actual CLI via subprocess. No network.

Baseline on unchanged main: 4 failures (no ``--tables`` option) /
2 passing controls (text-format rejection stays an error, defaults
unchanged).
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "pdf-reader"
    / "scripts"
    / "read_pdf.py"
)

P1_LABELS = [["Name", "Qty", "Price"], ["apple", "3", "2.50"], ["pear", "5", "1.20"]]
P2_LABELS = [["Key", "Value"], ["cpu", "8"], ["mem", "32G"]]


def build_fixture_pdf(path):
    """Three-page ruled PDF: 3x3 grid, 2x2 grid, plain text page."""
    import pymupdf

    doc = pymupdf.open()
    for labels, rows, cols, box in (
        (P1_LABELS, 3, 3, (72, 72, 432, 252)),
        (P2_LABELS, 3, 2, (90, 100, 450, 280)),
    ):
        page = doc.new_page()
        x0, y0, x1, y1 = box
        cw, ch = (x1 - x0) / cols, (y1 - y0) / rows
        for r in range(rows + 1):
            y = y0 + r * ch
            page.draw_line(pymupdf.Point(x0, y), pymupdf.Point(x1, y))
        for c in range(cols + 1):
            x = x0 + c * cw
            page.draw_line(pymupdf.Point(x, y0), pymupdf.Point(x, y1))
        for r in range(rows):
            for c in range(cols):
                cell = pymupdf.Rect(
                    x0 + c * cw, y0 + r * ch, x0 + (c + 1) * cw, y0 + (r + 1) * ch
                )
                page.insert_textbox(cell, " " + labels[r][c], fontsize=12)
    page = doc.new_page()
    page.insert_textbox(
        pymupdf.Rect(72, 72, 400, 150), "No tables here, just prose.", fontsize=12
    )
    doc.save(str(path))
    doc.close()


def bbox_close(actual, expected, tol=1.0):
    return len(actual) == 4 and all(
        abs(float(a) - e) <= tol for a, e in zip(actual, expected)
    )


class PdfReaderTablesTestCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.pdf = Path(cls.tmp.name) / "ruled.pdf"
        build_fixture_pdf(cls.pdf)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "-f", str(self.pdf), *args],
            capture_output=True,
            text=True,
            timeout=60,
        )

    def json_ok(self, proc):
        self.assertEqual(
            proc.returncode, 0, f"cli failed: {proc.stderr[-400:]}"
        )
        # Old PyMuPDF versions print an import advisory onto stdout; the
        # JSON payload always starts at the first opening brace.
        return json.loads(proc.stdout[proc.stdout.index("{") :])


class TestTableExtraction(PdfReaderTablesTestCase):
    def test_exact_grid_cells_and_bbox(self):
        out = self.json_ok(self.run_cli("-p", "1", "--tables", "--format", "json"))
        page = out["pages"][0]
        self.assertEqual(page["page"], 1)
        tables = page["tables"]
        self.assertEqual(len(tables), 1)
        self.assertTrue(bbox_close(tables[0]["bbox"], [72.0, 72.0, 432.0, 252.0]))
        self.assertEqual(tables[0]["rows"], P1_LABELS)
        self.assertTrue(page["text"].strip(), "ordinary page text preserved")

    def test_page_selection_scopes_tables(self):
        out = self.json_ok(self.run_cli("-p", "2-3", "--tables", "--format", "json"))
        self.assertEqual([p["page"] for p in out["pages"]], [2, 3])
        self.assertEqual(out["pages"][0]["tables"][0]["rows"], P2_LABELS)
        self.assertTrue(bbox_close(out["pages"][0]["tables"][0]["bbox"],
                                   [90.0, 100.0, 450.0, 280.0]))
        self.assertEqual(out["pages"][1]["tables"], [])

    def test_metadata_coexists_with_tables(self):
        out = self.json_ok(
            self.run_cli("-p", "1", "--tables", "--format", "json", "--metadata")
        )
        self.assertIn("metadata", out)
        self.assertEqual(out["total_pages"], 3)
        self.assertEqual(out["pages"][0]["tables"][0]["rows"][0], P1_LABELS[0])

    def test_no_table_pages_report_empty_arrays(self):
        out = self.json_ok(self.run_cli("-p", "3", "--tables", "--format", "json"))
        self.assertEqual(out["pages"][0]["tables"], [])
        self.assertIn("No tables here", out["pages"][0]["text"])


class TestDefaultContract(PdfReaderTablesTestCase):
    def test_tables_requires_json_format(self):
        proc = self.run_cli("-p", "1", "--tables", "--format", "text")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("json", (proc.stderr + proc.stdout).lower())

    def test_defaults_unchanged_without_flag(self):
        out = self.json_ok(self.run_cli("-p", "1", "--format", "json"))
        self.assertNotIn("tables", json.dumps(out))
        text = self.run_cli("-p", "1")
        self.assertEqual(text.returncode, 0)
        self.assertIn("--- Page 1 ---", text.stdout)
        self.assertIn("Name", text.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
