"""Export real AcroForm widget values without changing fields or executing actions."""

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


class PdfFormFieldTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("pdf_form_reader", SCRIPT)
        cls.reader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.reader)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "application.pdf"
        with pymupdf.open() as document:
            for _ in range(3):
                page = document.new_page()
                page.insert_text((30, 30), "Application form")
            page = document[0]
            definitions = (
                ("Customer.Name", pymupdf.PDF_WIDGET_TYPE_TEXT, '李, "申请"\nsecond line', None, 3),
                ("Confirmed", pymupdf.PDF_WIDGET_TYPE_CHECKBOX, True, None, 0),
                ("NotConfirmed", pymupdf.PDF_WIDGET_TYPE_CHECKBOX, False, None, 0),
                ("Country", pymupdf.PDF_WIDGET_TYPE_COMBOBOX, "中国", ["中国", "Other"], 0),
                ("Category", pymupdf.PDF_WIDGET_TYPE_LISTBOX, "Two", ["One", "Two"], 0),
                ("Action", pymupdf.PDF_WIDGET_TYPE_BUTTON, "", None, 0),
                ("Signature", pymupdf.PDF_WIDGET_TYPE_SIGNATURE, "", None, 0),
                ("Choice", pymupdf.PDF_WIDGET_TYPE_RADIOBUTTON, False, None, 0),
            )
            for row, (name, kind, value, choices, flags) in enumerate(definitions):
                widget = pymupdf.Widget()
                widget.field_name = name
                widget.field_label = "Application " + name
                widget.field_type = kind
                widget.field_value = value
                widget.field_flags = flags
                widget.rect = pymupdf.Rect(40, 50 + row * 50, 250, 80 + row * 50)
                if choices is not None:
                    widget.choice_values = choices
                if name == "Customer.Name":
                    widget.script_calc = "throw new Error('should not run');"
                page.add_widget(widget)
            page = document[1]
            widget = pymupdf.Widget()
            widget.field_name = "Customer.Name"
            widget.field_type = pymupdf.PDF_WIDGET_TYPE_TEXT
            widget.rect = pymupdf.Rect(40, 50, 250, 80)
            widget.field_value = '李, "申请"\nsecond line'
            page.add_widget(widget)
            document.save(self.path)

    def run_cli(self, *arguments):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "-f", str(self.path), *arguments],
            env={**os.environ, "PYTHONUTF8": "1"},
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
        )

    def export(self, *arguments):
        result = self.run_cli("--format", "json", "--form-fields", *arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_actual_form_values_types_flags_and_geometry(self):
        report = self.export()
        fields = report["pages"][0]["form_fields"]
        self.assertEqual(len(fields), 8)
        self.assertEqual(
            [field["name"] for field in fields],
            [
                "Customer.Name",
                "Confirmed",
                "NotConfirmed",
                "Country",
                "Category",
                "Action",
                "Signature",
                "Choice",
            ],
        )
        text = fields[0]
        self.assertEqual(text["value"], '李, "申请"\nsecond line')
        self.assertEqual(text["type"], "Text")
        self.assertEqual(text["type_id"], pymupdf.PDF_WIDGET_TYPE_TEXT)
        self.assertEqual(text["flags"], 3)
        self.assertEqual(text["label"], "Application Customer.Name")
        self.assertEqual(text["rect"], [40.0, 50.0, 250.0, 80.0])
        self.assertGreater(text["xref"], 0)

    def test_checkbox_state_values_are_retained_without_coercion(self):
        fields = self.export()["pages"][0]["form_fields"]
        self.assertEqual(fields[1]["value"], "Yes")
        self.assertEqual(fields[2]["value"], "Off")
        self.assertIn("Yes", fields[1]["button_states"]["normal"])
        self.assertIn("Off", fields[1]["button_states"]["normal"])
        self.assertEqual(fields[7]["type"], "RadioButton")
        self.assertEqual(fields[7]["value"], "Off")
        self.assertIn("Off", fields[7]["button_states"]["normal"])

    def test_choices_and_unsigned_signature_status_are_detached(self):
        fields = self.export()["pages"][0]["form_fields"]
        self.assertEqual(fields[3]["choices"], ["中国", "Other"])
        self.assertEqual(fields[3]["value"], "中国")
        self.assertEqual(fields[4]["choices"], ["One", "Two"])
        self.assertFalse(fields[6]["signed"])

    def test_selected_pages_keep_repeated_field_appearances(self):
        report = self.export("-p", "2")
        self.assertEqual([page["page"] for page in report["pages"]], [2])
        self.assertEqual(
            [field["name"] for field in report["pages"][0]["form_fields"]], ["Customer.Name"]
        )
        self.assertEqual(report["pages"][0]["form_fields"][0]["value"], '李, "申请"\nsecond line')

    def test_empty_page_exports_an_empty_field_list(self):
        report = self.export("-p", "3")
        self.assertEqual(report["pages"][0]["form_fields"], [])

    def test_default_json_schema_and_text_remain_unchanged(self):
        result = self.run_cli("--format", "json")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(set(report), {"total_pages", "pages"})
        for page in report["pages"]:
            self.assertEqual(set(page), {"page", "text"})
        self.assertIn("Application form", report["pages"][0]["text"])

    def test_form_fields_requires_json_before_install_or_open(self):
        with mock.patch.object(
            sys, "argv", [str(SCRIPT), "-f", "absent.pdf", "--form-fields"]
        ), mock.patch.object(
            self.reader, "_install", side_effect=AssertionError("installed")
        ), contextlib.redirect_stderr(
            io.StringIO()
        ) as errors:
            with self.assertRaises(SystemExit) as raised:
                self.reader.main()
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("--format json", errors.getvalue())

    def test_default_mode_does_not_enumerate_widget_metadata(self):
        with mock.patch.object(
            self.reader, "_page_form_fields", side_effect=AssertionError("enumerated")
        ), mock.patch.object(
            sys, "argv", [str(SCRIPT), "-f", str(self.path), "--format", "json"]
        ), contextlib.redirect_stdout(
            io.StringIO()
        ) as output:
            self.reader.main()
        self.assertEqual(json.loads(output.getvalue())["total_pages"], 3)

    def test_metadata_and_page_ranges_compose_without_mutation(self):
        original = self.path.read_bytes()
        report = self.export("-p", "1-2", "--metadata")
        self.assertEqual([page["page"] for page in report["pages"]], [1, 2])
        self.assertEqual(self.path.read_bytes(), original)
        self.assertNotIn("should not run", json.dumps(report, ensure_ascii=False))

    def test_helper_copies_page_bound_values_before_document_close(self):
        with pymupdf.open(self.path) as document:
            page = document[0]
            fields = self.reader._page_form_fields(page)
        encoded = json.dumps(fields, ensure_ascii=False)
        self.assertIn("Customer.Name", encoded)
        self.assertIn("中国", encoded)
        self.assertEqual(fields[0]["rect"], [40.0, 50.0, 250.0, 80.0])


if __name__ == "__main__":
    unittest.main()
