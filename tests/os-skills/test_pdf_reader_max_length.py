#!/usr/bin/env python3
"""Regression tests for the read_pdf.py --max-length / stdout contract (#3598).

Two layers, both stdlib only:

* unit tests for the ``_cap_text`` budget helper and the legacy ``fitz``
  import guard -- no PDF and no PyMuPDF needed;
* end-to-end CLI runs, skipped when PyMuPDF is unavailable, asserting the
  whole stdout still parses as JSON with and without ``-m``.

Baseline on unchanged main: the end-to-end cap tests fail because the limit
sliced the already-serialized document (``json.loads`` raises "Invalid control
character"), and a legacy ``fitz`` advisory printed during import would prefix
-- hence corrupt -- every report on stdout.
"""

import builtins
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import types
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

MARKER = "...[truncated]"

PAGE_TEXTS = [
    "First page prose about widget pricing, repeated to be long enough. " * 4,
    "Second page prose about kernel tuning, repeated to be long enough. " * 4,
    "Third page prose about storage resizing, repeated to be long enough. " * 4,
]


def load_read_pdf():
    """Load read_pdf.py as a module (its directory name is not importable)."""
    spec = importlib.util.spec_from_file_location("read_pdf_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


read_pdf = load_read_pdf()


def pymupdf_available():
    try:
        import pymupdf  # noqa: F401
    except ImportError:
        return False
    return True


def build_fixture_pdf(path, texts):
    import pymupdf

    doc = pymupdf.open()
    for text in texts:
        page = doc.new_page()
        page.insert_textbox(pymupdf.Rect(72, 72, 540, 500), text, fontsize=11)
    doc.save(str(path))
    doc.close()


class CapTextTestCase(unittest.TestCase):
    """The cap applies to extracted text, never to the rendered document."""

    @staticmethod
    def pages(*texts):
        return [{"page": i + 1, "text": t} for i, t in enumerate(texts)]

    def test_zero_or_negative_limit_is_no_cap(self):
        for limit in (0, -5):
            pages = self.pages("abc", "def")
            self.assertFalse(read_pdf._cap_text(pages, limit), limit)
            self.assertEqual([p["text"] for p in pages], ["abc", "def"])

    def test_budget_is_spent_on_earlier_pages_first(self):
        pages = self.pages("abc", "defgh", "ij")
        self.assertTrue(read_pdf._cap_text(pages, 6))
        self.assertEqual(pages[0]["text"], "abc")
        self.assertEqual(pages[1]["text"], "def" + MARKER)
        self.assertEqual(pages[2]["text"], "")

    def test_text_that_fits_is_not_truncation(self):
        pages = self.pages("abc", "de")
        self.assertFalse(read_pdf._cap_text(pages, 5))
        self.assertEqual([p["text"] for p in pages], ["abc", "de"])

    def test_marker_matches_the_documented_spelling(self):
        self.assertEqual(read_pdf.TRUNCATION_MARKER, MARKER)
        pages = self.pages("abcdef")
        read_pdf._cap_text(pages, 2)
        self.assertTrue(pages[0]["text"].endswith(MARKER))

    def test_sibling_keys_such_as_tables_survive(self):
        pages = [
            {
                "page": 1,
                "text": "abcdef",
                "tables": [{"bbox": [0.0, 0.0, 1.0, 1.0], "rows": [["x"]]}],
            }
        ]
        self.assertTrue(read_pdf._cap_text(pages, 3))
        self.assertEqual(pages[0]["text"], "abc" + MARKER)
        self.assertEqual(pages[0]["tables"][0]["rows"], [["x"]])

    def test_empty_pages_never_report_truncation(self):
        pages = self.pages("", "")
        self.assertFalse(read_pdf._cap_text(pages, 1))
        self.assertEqual([p["text"] for p in pages], ["", ""])


class LegacyImportGuardTestCase(unittest.TestCase):
    """An advisory printed by ``import fitz`` must not reach stdout."""

    def test_advisory_is_redirected_off_stdout(self):
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "fitz.py").write_text(
                'print("PyMuPDF: the fitz alias is deprecated, use pymupdf")\n'
                'VERSION = "1.23.0"\n'
            )
            saved_path = list(sys.path)
            saved_module = sys.modules.pop("fitz", None)
            real_out, real_err = sys.stdout, sys.stderr
            buf_out, buf_err = io.StringIO(), io.StringIO()
            sys.path.insert(0, tmp)
            try:
                sys.stdout, sys.stderr = buf_out, buf_err
                module = read_pdf._import_legacy_fitz()
            finally:
                sys.stdout, sys.stderr = real_out, real_err
                sys.path[:] = saved_path
                sys.modules.pop("fitz", None)
                if saved_module is not None:
                    sys.modules["fitz"] = saved_module
        self.assertEqual(module.VERSION, "1.23.0")
        self.assertEqual(buf_out.getvalue(), "", "stdout stayed clean")
        self.assertIn("deprecated", buf_err.getvalue())

    def test_install_prefers_the_modern_name(self):
        if not pymupdf_available():
            self.skipTest("PyMuPDF is not installed")
        import pymupdf

        self.assertIs(read_pdf._install(), pymupdf)

    def test_install_falls_back_to_the_legacy_alias(self):
        fake_fitz = types.ModuleType("fitz")
        fake_fitz.VERSION = "1.23.0"
        real_import = builtins.__import__
        stashed = {k: sys.modules.pop(k) for k in ("pymupdf", "fitz")
                   if k in sys.modules}

        def guarded(name, *args, **kwargs):
            if name == "pymupdf":
                raise ImportError("blocked for this test")
            if name == "fitz":
                return fake_fitz
            return real_import(name, *args, **kwargs)

        builtins.__import__ = guarded
        try:
            self.assertIs(read_pdf._install(), fake_fitz)
        finally:
            builtins.__import__ = real_import
            sys.modules.pop("fitz", None)
            sys.modules.update(stashed)


@unittest.skipUnless(pymupdf_available(), "PyMuPDF is not installed")
class MaxLengthCliTestCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.pdf = Path(cls.tmp.name) / "prose.pdf"
        build_fixture_pdf(cls.pdf, PAGE_TEXTS)

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

    def whole_stdout_json(self, proc):
        """Parse stdout as one JSON document -- no prefix, no sliced tail."""
        self.assertEqual(proc.returncode, 0, f"cli failed: {proc.stderr[-400:]}")
        return json.loads(proc.stdout)

    def test_json_without_cap_parses_and_reports_no_flag(self):
        doc = self.whole_stdout_json(self.run_cli("--format", "json"))
        self.assertEqual(doc["total_pages"], 3)
        self.assertNotIn("truncated", doc)
        self.assertTrue(all(p["text"] for p in doc["pages"]))

    def test_json_with_cap_parses_and_sets_truncated(self):
        doc = self.whole_stdout_json(self.run_cli("--format", "json", "-m", "120"))
        self.assertIs(doc["truncated"], True)
        self.assertTrue(doc["pages"][0]["text"].endswith(MARKER))
        self.assertLessEqual(len(doc["pages"][0]["text"]), 120 + len(MARKER))

    def test_cap_buys_earlier_pages_first(self):
        uncapped = self.whole_stdout_json(self.run_cli("--format", "json"))
        budget = len(uncapped["pages"][0]["text"]) + 10
        doc = self.whole_stdout_json(
            self.run_cli("--format", "json", "-m", str(budget))
        )
        self.assertIs(doc["truncated"], True)
        self.assertEqual([p["page"] for p in doc["pages"]], [1, 2, 3])
        self.assertTrue(doc["pages"][0]["text"])
        self.assertEqual(doc["pages"][2]["text"], "")
        extracted = sum(len(p["text"]) - len(MARKER)
                        for p in doc["pages"] if p["text"].endswith(MARKER))
        extracted += sum(len(p["text"]) for p in doc["pages"]
                         if not p["text"].endswith(MARKER))
        self.assertLessEqual(extracted, budget)

    def test_cap_generous_enough_leaves_everything(self):
        uncapped = self.whole_stdout_json(self.run_cli("--format", "json"))
        total = sum(len(p["text"]) for p in uncapped["pages"])
        doc = self.whole_stdout_json(
            self.run_cli("--format", "json", "-m", str(total))
        )
        self.assertNotIn("truncated", doc)
        self.assertEqual([p["text"] for p in doc["pages"]],
                         [p["text"] for p in uncapped["pages"]])

    def test_tables_payload_survives_a_cap(self):
        doc = self.whole_stdout_json(
            self.run_cli("--format", "json", "--tables", "-m", "40")
        )
        self.assertIs(doc["truncated"], True)
        for page in doc["pages"]:
            self.assertIn("tables", page)
            self.assertIsInstance(page["tables"], list)

    def test_text_mode_marks_the_cut_page(self):
        proc = self.run_cli("-m", "40")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("--- Page 1 ---", proc.stdout)
        self.assertIn(MARKER, proc.stdout)

    def test_help_documents_the_contract(self):
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "--help"],
            capture_output=True, text=True, timeout=60,
        )
        self.assertEqual(proc.returncode, 0)
        self.assertIn("extracted text", proc.stdout)
        self.assertIn(MARKER, proc.stdout)
        self.assertIn("truncated", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
