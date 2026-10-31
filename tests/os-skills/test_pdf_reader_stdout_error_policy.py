"""Preserve inherited stdout errors through the complete real PDF reader CLI.

Run: python tests/os-skills/test_pdf_reader_stdout_error_policy.py -v
Reviewed-head baseline: 12 assertion failures and 6 UnicodeEncodeErrors across
three affected methods; two controls pass. Candidate: five methods pass, no skips.
Observed on Windows/Python 3.12.14/PyMuPDF 2.0; decoder-specific cases can skip
on versions that do not produce surrogate metadata. Manual regression outside CI.
"""

import contextlib
import io
import json
import os
import runpy
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import pymupdf

ROOT = Path(os.environ.get("PDF_READER_SOURCE_ROOT", Path(__file__).resolve().parents[2]))
SCRIPT = ROOT / "src/os-skills/others/pdf-reader/scripts/read_pdf.py"
PAGE_TEXT = "Real error-policy PDF"
POLICIES = ("surrogateescape", "backslashreplace", "replace", "strict")


class PDFStdoutErrorPolicyTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.normal_pdf = self.directory / "normal.pdf"
        self.malformed_pdf = self.directory / "malformed-title.pdf"
        self.write_pdf(self.normal_pdf, malformed=False)
        self.write_pdf(self.malformed_pdf, malformed=True)

    @staticmethod
    def write_pdf(path, malformed):
        # Keep a real page and valid PDF objects/xref; only the Info string's
        # UTF-16BE payload is malformed. No mocked metadata or parser.
        with pymupdf.open() as document:
            document.new_page().insert_text((72, 72), PAGE_TEXT)
            document.set_metadata({"title": "Reader \U0001f642", "author": "Policy test"})
            if malformed:
                kind, reference = document.xref_get_key(-1, "Info")
                if kind != "xref":
                    raise AssertionError("PyMuPDF did not create the Info dictionary")
                info_xref = int(reference.split()[0])
                document.xref_set_key(info_xref, "Title", "<FEFFDCFF>")
            document.save(path)
        with pymupdf.open(path) as document:
            if len(document) != 1 or PAGE_TEXT not in document[0].get_text():
                raise AssertionError("Fixture is not a readable real one-page PDF")

    def arguments(self, path, output_format):
        return [str(SCRIPT), "-f", str(path), "--metadata", "--format", output_format]

    def run_full_entrypoint(self, path, output_format, stream):
        original = path.read_bytes()
        with mock.patch.object(sys, "argv", self.arguments(path, output_format)):
            with contextlib.redirect_stdout(stream):
                try:
                    runpy.run_path(str(SCRIPT), run_name="__main__")
                finally:
                    self.assertIs(sys.stdout, stream)
                    self.assertEqual(path.read_bytes(), original)

    def reference_report(self, path, output_format):
        # The same complete CLI with an actual caller-owned non-reconfigurable
        # StringIO supplies the report before byte encoding.
        with io.StringIO() as output:
            self.run_full_entrypoint(path, output_format, output)
            return output.getvalue()

    def require_surrogate_metadata(self):
        with pymupdf.open(self.malformed_pdf) as document:
            title = document.metadata["title"]
            raw_kind, raw_value = document.xref_get_key(-1, "Info")
            self.assertEqual(raw_kind, "xref")
            self.assertTrue(raw_value.endswith(" R"))
        if not any(0xDC80 <= ord(character) <= 0xDCFF for character in title):
            self.skipTest(
                "Installed PyMuPDF "
                + str(pymupdf.VersionBind)
                + " does not decode <FEFFDCFF> to surrogateescape metadata; "
                + "real-metadata regression unavailable on this decoder"
            )
        # This specific case requires only byte-surrogates; unexpected decoder
        # behavior must not be hidden as a successful surrogateescape run.
        self.assertTrue(
            all(
                not 0xD800 <= ord(character) <= 0xDFFF or 0xDC80 <= ord(character) <= 0xDCFF
                for character in title
            )
        )
        return title

    def check_wrapper(self, path, output_format, errors):
        reference = self.reference_report(path, output_format)
        expected = reference.encode("utf-8", errors=errors)
        with io.BytesIO() as buffer:
            output = io.TextIOWrapper(buffer, encoding="gbk", errors=errors, newline="\n")
            try:
                self.run_full_entrypoint(path, output_format, output)
                output.flush()
                self.assertEqual(output.encoding, "utf-8")
                self.assertEqual(output.errors, errors)
                self.assertEqual(buffer.getvalue(), expected)
            finally:
                output.detach()

    def test_real_cli_preserves_all_four_error_policies(self):
        for errors in POLICIES:
            for output_format in ("json", "text"):
                with self.subTest(errors=errors, output_format=output_format):
                    self.check_wrapper(self.normal_pdf, output_format, errors)

    def test_real_malformed_metadata_preserves_non_strict_report_bytes(self):
        self.require_surrogate_metadata()
        for errors in POLICIES[:-1]:
            for output_format in ("json", "text"):
                with self.subTest(errors=errors, output_format=output_format):
                    self.check_wrapper(self.malformed_pdf, output_format, errors)

    def test_real_malformed_metadata_in_fresh_cli_processes(self):
        self.require_surrogate_metadata()
        original = self.malformed_pdf.read_bytes()
        for errors in POLICIES[:-1]:
            for output_format in ("json", "text"):
                with self.subTest(errors=errors, output_format=output_format):
                    environment = os.environ.copy()
                    environment["LC_ALL"] = "C"
                    environment["PYTHONIOENCODING"] = "gbk:" + errors
                    environment["PYTHONUTF8"] = "0"
                    reference = self.reference_report(self.malformed_pdf, output_format)
                    result = subprocess.run(
                        [sys.executable, "-X", "utf8=0"]
                        + self.arguments(self.malformed_pdf, output_format),
                        env=environment,
                        capture_output=True,
                        timeout=30,
                        check=False,
                    )
                    self.assertEqual(
                        result.returncode,
                        0,
                        result.stderr.decode("utf-8", errors="backslashreplace"),
                    )
                    expected = reference.replace("\n", os.linesep).encode("utf-8", errors)
                    self.assertEqual(result.stdout, expected)
                    self.assertEqual(self.malformed_pdf.read_bytes(), original)

    def test_strict_keeps_rejecting_real_surrogate_metadata(self):
        self.require_surrogate_metadata()
        for output_format in ("json", "text"):
            with self.subTest(output_format=output_format):
                with io.BytesIO() as buffer:
                    output = io.TextIOWrapper(buffer, encoding="gbk", errors="strict", newline="\n")
                    try:
                        with self.assertRaises(UnicodeEncodeError):
                            self.run_full_entrypoint(self.malformed_pdf, output_format, output)
                        self.assertEqual(output.errors, "strict")
                        self.assertEqual(output.encoding, "utf-8")
                        self.assertEqual(buffer.getvalue(), b"")
                    finally:
                        output.detach()

    def test_strict_unicode_metadata_remains_lossless_json(self):
        report = self.reference_report(self.normal_pdf, "json")
        self.assertEqual(json.loads(report)["metadata"]["title"], "Reader \U0001f642")
        self.check_wrapper(self.normal_pdf, "json", "strict")


if __name__ == "__main__":
    unittest.main()
