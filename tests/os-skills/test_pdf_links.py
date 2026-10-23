"""Export embedded navigation targets without following them or changing the PDF."""

import contextlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import pymupdf

SCRIPT = Path(__file__).parents[2] / "src/os-skills/others/pdf-reader/scripts/read_pdf.py"


class PdfLinkTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("pdf_link_reader", SCRIPT)
        cls.reader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.reader)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "navigation.pdf"
        self.uri = 'https://example.invalid/报告?q=a,b&note="quoted"'
        with pymupdf.open() as document:
            for number in range(3):
                page = document.new_page()
                page.insert_text((50, 70), f"Page {number + 1} references and appendix")
            page = document[0]
            page.insert_link(
                {"kind": pymupdf.LINK_URI, "from": pymupdf.Rect(50, 50, 150, 80), "uri": self.uri}
            )
            page.insert_link(
                {
                    "kind": pymupdf.LINK_GOTO,
                    "from": pymupdf.Rect(50, 100, 150, 130),
                    "page": 2,
                    "to": pymupdf.Point(30, 50),
                }
            )
            page.insert_link(
                {
                    "kind": pymupdf.LINK_GOTOR,
                    "from": pymupdf.Rect(50, 150, 150, 180),
                    "file": "other.pdf",
                    "page": 1,
                    "to": pymupdf.Point(10, 20),
                }
            )
            page.insert_link(
                {
                    "kind": pymupdf.LINK_GOTOR,
                    "from": pymupdf.Rect(50, 200, 150, 230),
                    "file": "other.pdf",
                    "page": -1,
                    "to": "NamedTarget",
                }
            )
            document.set_metadata({"title": "Navigation fixture"})
            document.save(self.path)

    def read(self, *options):
        output, errors = io.StringIO(), io.StringIO()
        with (
            mock.patch.object(sys, "argv", [str(SCRIPT), "-f", str(self.path), *options]),
            mock.patch.object(self.reader, "_install", return_value=pymupdf),
            contextlib.redirect_stdout(output),
            contextlib.redirect_stderr(errors),
        ):
            try:
                self.reader.main()
            except SystemExit as error:
                return error.code, output.getvalue(), errors.getvalue()
        return 0, output.getvalue(), errors.getvalue()

    def report(self, *options):
        code, output, errors = self.read("--format", "json", *options)
        self.assertEqual(code, 0, errors)
        return json.loads(output)

    def test_uri_and_internal_targets_retain_order_and_geometry(self):
        links = self.report("--links")["pages"][0]["links"]
        self.assertEqual([link["kind"] for link in links[:3]], [2, 1, 5])
        self.assertIn(links[3]["kind"], (3, 5))
        self.assertEqual(links[0]["uri"], self.uri)
        self.assertEqual(links[0]["from"], [50, 50, 150, 80])
        self.assertEqual(links[1]["page"], 3)
        self.assertEqual(links[1]["to"], [30, 50])
        self.assertGreater(links[0]["xref"], 0)

    def test_remote_file_and_indirect_name_are_data_not_opened_files(self):
        links = self.report("--links")["pages"][0]["links"]
        self.assertEqual(links[2]["file"], "other.pdf")
        self.assertEqual(links[2]["page"], 2)
        self.assertEqual(len(links[2]["to"]), 2)
        if links[3]["kind"] == pymupdf.LINK_GOTOR:
            self.assertEqual(links[3]["file"], "other.pdf")
            self.assertEqual(links[3]["page"], -1)
            self.assertEqual(links[3]["to"], "NamedTarget")
        else:
            self.assertEqual(links[3]["file"], "other.pdf#nameddest=NamedTarget")
        self.assertFalse((self.path.parent / "other.pdf").exists())

    def test_documented_indirect_destination_shape_preserves_symbol_and_raw_record(self):
        raw = {
            "kind": 5,
            "from": pymupdf.Rect(1, 2, 3, 4),
            "page": -1,
            "to": "NamedTarget",
            "file": "remote.pdf",
        }
        page = mock.Mock()
        first_page = {
            "kind": 1,
            "from": pymupdf.Rect(5, 6, 7, 8),
            "page": 0,
            "to": pymupdf.Point(9, 10),
        }
        page.get_links.return_value = [raw, first_page]
        links = self.reader._page_links(page)
        self.assertEqual(links[0]["to"], "NamedTarget")
        self.assertEqual(links[0]["page"], -1)
        self.assertEqual(links[0]["from"], [1, 2, 3, 4])
        self.assertIsInstance(raw["from"], pymupdf.Rect)
        self.assertEqual(links[1]["page"], 1)
        self.assertEqual(links[1]["to"], [9, 10])
        self.assertIsInstance(first_page["to"], pymupdf.Point)

    def test_page_selection_scopes_link_arrays(self):
        report = self.report("--links", "--pages", "2")
        self.assertEqual([page["page"] for page in report["pages"]], [2])
        self.assertEqual(report["pages"][0]["links"], [])

    def test_pages_without_links_have_empty_arrays(self):
        report = self.report("--links")
        self.assertEqual(report["pages"][1]["links"], [])
        self.assertEqual(report["pages"][2]["links"], [])

    def test_link_report_keeps_metadata_and_selected_text(self):
        report = self.report("--links", "--metadata", "--pages", "1")
        self.assertEqual(report["metadata"]["title"], "Navigation fixture")
        self.assertIn("Page 1 references", report["pages"][0]["text"])
        self.assertEqual(len(report["pages"][0]["links"]), 4)

    def test_default_does_not_read_links_or_add_json_fields(self):
        with mock.patch.object(pymupdf.Page, "get_links", side_effect=AssertionError("unexpected")):
            report = self.report()
        self.assertEqual(set(report), {"total_pages", "pages"})
        self.assertTrue(all(set(page) == {"page", "text"} for page in report["pages"]))

    def test_links_require_json_before_engine_or_file_access(self):
        errors = io.StringIO()
        with (
            mock.patch.object(sys, "argv", [str(SCRIPT), "-f", "absent.pdf", "--links"]),
            mock.patch.object(self.reader, "_install") as installer,
            contextlib.redirect_stderr(errors),
            self.assertRaises(SystemExit) as raised,
        ):
            self.reader.main()
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("--links requires --format json", errors.getvalue())
        installer.assert_not_called()

    def test_export_leaves_source_bytes_unchanged(self):
        original = self.path.read_bytes()
        self.report("--links")
        self.assertEqual(self.path.read_bytes(), original)

    def test_actual_script_process_exports_json_safe_destinations(self):
        bootstrap = (
            "import pymupdf,sys,runpy;sys.modules['fitz']=pymupdf;"
            "sys.argv=[sys.argv[1],'-f',sys.argv[2],'--format','json','--links'];"
            "runpy.run_path(sys.argv[0],run_name='__main__')"
        )
        result = subprocess.run(
            [sys.executable, "-c", bootstrap, str(SCRIPT), str(self.path)],
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
            env={**os.environ, "PYTHONUTF8": "1"},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["pages"][0]["links"][1]["to"], [30, 50])


if __name__ == "__main__":
    unittest.main()
