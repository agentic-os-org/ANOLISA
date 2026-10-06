#!/usr/bin/env python3
"""Regression tests for read_pdf.py handling of corrupt input files.

A non-PDF file that merely *exists* used to fall through to fitz.open and
die with a raw pymupdf.FileDataError traceback instead of the script's own
ERROR-line + exit-1 convention.
"""

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


class TestCorruptFile(unittest.TestCase):
    def test_garbage_file_reports_clean_error(self):
        with tempfile.NamedTemporaryFile(suffix=".pdf", delete=False) as fh:
            fh.write(b"this is definitely not a pdf")
            path = fh.name
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "-f", path],
            capture_output=True,
            text=True,
        )
        self.assertEqual(proc.returncode, 1, proc.stderr)
        self.assertTrue(
            proc.stderr.startswith("ERROR:"),
            f"stderr should start with ERROR:, got: {proc.stderr[:200]}",
        )
        self.assertNotIn("Traceback", proc.stderr)

    def test_garbage_file_json_mode_reports_clean_error(self):
        with tempfile.NamedTemporaryFile(suffix=".pdf", delete=False) as fh:
            fh.write(b"\x00\x01not a pdf")
            path = fh.name
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "-f", path, "--format", "json"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(proc.returncode, 1, proc.stderr)
        self.assertTrue(proc.stderr.startswith("ERROR:"), proc.stderr)
        self.assertNotIn("Traceback", proc.stderr)


if __name__ == "__main__":
    unittest.main()
