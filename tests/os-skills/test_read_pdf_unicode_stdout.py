"""Manual full-CLI PDF regression with real metadata and stdout codecs.

Run: python3 tests/os-skills/test_read_pdf_unicode_stdout.py -v
Baseline on unchanged main: 2 failures / 2 passing controls.
This file is not currently discovered by repository CI.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import pymupdf

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/others/pdf-reader/scripts/read_pdf.py"
)
UNICODE_TITLE = "Reader \U0001f642"
PAGE_TEXT = "Ordinary PDF page"


class PDFUnicodeOutputTests(unittest.TestCase):
    @staticmethod
    def write_pdf(path: Path, title: str) -> None:
        with pymupdf.open() as document:
            document.new_page().insert_text((72, 72), PAGE_TEXT)
            document.set_metadata({"title": title, "author": "Hanabi21"})
            document.save(path)

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.unicode_pdf = Path(temporary.name) / "unicode.pdf"
        self.ascii_pdf = Path(temporary.name) / "ascii.pdf"
        self.write_pdf(self.unicode_pdf, UNICODE_TITLE)
        self.write_pdf(self.ascii_pdf, "Reader ASCII")

    def read_pdf(self, path: Path, output_format: str, codec: str) -> str:
        original = path.read_bytes()
        environment = os.environ.copy()
        environment["PYTHONIOENCODING"] = f"{codec}:strict"
        environment["PYTHONUTF8"] = "0"
        result = subprocess.run(
            [
                sys.executable,
                "-X",
                "utf8=0",
                str(SCRIPT),
                "-f",
                str(path),
                "--metadata",
                "--format",
                output_format,
            ],
            env=environment,
            capture_output=True,
            timeout=30,
            check=False,
        )
        self.assertEqual(path.read_bytes(), original)
        self.assertEqual(
            result.returncode, 0, result.stderr.decode("ascii", errors="replace")
        )
        self.assertNotIn(b"Traceback", result.stderr)
        return result.stdout.decode("utf-8")

    def assert_json_output(self, output: str, title: str) -> None:
        data = json.loads(output)
        self.assertEqual(data["total_pages"], 1)
        self.assertEqual(data["pages"], [{"page": 1, "text": PAGE_TEXT}])
        self.assertEqual(data["metadata"]["title"], title)
        self.assertEqual(data["metadata"]["author"], "Hanabi21")

    def assert_text_output(self, output: str, title: str) -> None:
        self.assertIn("=== Metadata ===", output)
        self.assertIn(f"title: {title}", output)
        self.assertIn("author: Hanabi21", output)
        self.assertIn("--- Page 1 ---", output)
        self.assertIn(PAGE_TEXT, output)

    def test_unicode_metadata_json_with_gbk_stdout(self) -> None:
        output = self.read_pdf(self.unicode_pdf, "json", "gbk")
        self.assert_json_output(output, UNICODE_TITLE)

    def test_unicode_metadata_text_with_gbk_stdout(self) -> None:
        output = self.read_pdf(self.unicode_pdf, "text", "gbk")
        self.assert_text_output(output, UNICODE_TITLE)

    def test_ascii_metadata_with_gbk_stdout(self) -> None:
        for output_format in ("json", "text"):
            with self.subTest(output_format=output_format):
                output = self.read_pdf(self.ascii_pdf, output_format, "gbk")
                if output_format == "json":
                    self.assert_json_output(output, "Reader ASCII")
                else:
                    self.assert_text_output(output, "Reader ASCII")

    def test_unicode_metadata_with_utf8_stdout(self) -> None:
        for output_format in ("json", "text"):
            with self.subTest(output_format=output_format):
                output = self.read_pdf(self.unicode_pdf, output_format, "utf-8")
                if output_format == "json":
                    self.assert_json_output(output, UNICODE_TITLE)
                else:
                    self.assert_text_output(output, UNICODE_TITLE)


if __name__ == "__main__":
    unittest.main()
