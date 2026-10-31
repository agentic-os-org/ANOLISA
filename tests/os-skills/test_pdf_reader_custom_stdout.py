"""Manual full-entrypoint PDF regression with real PDFs and custom stdout.

Run: python3 tests/os-skills/test_pdf_reader_custom_stdout.py -v
Baseline before the stdout guard: 2 entrypoint errors.
This file is not currently discovered by repository CI.
"""

import contextlib
import io
import json
import runpy
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any, Dict, Optional, TextIO
from unittest import mock

import pymupdf

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/others/pdf-reader/scripts/read_pdf.py"
)
TITLE = "Reader \U0001f642"
PAGE_TEXT = "Real PDF entrypoint"


class PDFCustomStdoutTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.pdf = Path(temporary.name) / "stream.pdf"
        with pymupdf.open() as document:
            document.new_page().insert_text((72, 72), PAGE_TEXT)
            document.set_metadata({"title": TITLE, "author": "Hanabi21"})
            document.save(self.pdf)
        self.original_pdf = self.pdf.read_bytes()

    def run_main(self, stream: Optional[TextIO]) -> Dict[str, Any]:
        arguments = [
            str(SCRIPT),
            "-f",
            str(self.pdf),
            "--metadata",
            "--format",
            "json",
        ]
        with mock.patch.object(sys, "argv", arguments), contextlib.redirect_stdout(
            stream
        ):
            namespace = runpy.run_path(str(SCRIPT), run_name="__main__")
            self.assertIs(sys.stdout, stream)
        self.assertEqual(namespace["__name__"], "__main__")
        self.assertEqual(self.pdf.read_bytes(), self.original_pdf)
        return namespace

    def test_stringio_keeps_its_identity_and_receives_complete_report(self) -> None:
        with io.StringIO() as stream:
            self.assertFalse(hasattr(stream, "reconfigure"))
            self.run_main(stream)
            # This fixture uses the real installed pymupdf import path, not the
            # separate legacy-fitz advisory behavior covered by other work.
            report = json.loads(stream.getvalue())
        self.assertEqual(report["total_pages"], 1)
        self.assertEqual(report["pages"], [{"page": 1, "text": PAGE_TEXT}])
        self.assertEqual(report["metadata"]["title"], TITLE)
        self.assertEqual(report["metadata"]["author"], "Hanabi21")

    def test_unavailable_stdout_does_not_break_the_full_entrypoint(self) -> None:
        namespace = self.run_main(None)
        self.assertTrue(callable(namespace["main"]))


if __name__ == "__main__":
    unittest.main()
