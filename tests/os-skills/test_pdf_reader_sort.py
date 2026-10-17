"""--sort must expose PyMuPDF spatial reading order for text and JSON."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/others/pdf-reader/scripts/read_pdf.py"
)


def build_reversed_pdf(path, blank_second_page=False):
    """Two-page PDF whose content objects are written bottom-paragraph-first."""
    import fitz

    doc = fitz.open()
    doc.set_metadata({"title": "Reversed object order", "author": "fixture"})
    for pg in (1, 2):
        page = doc.new_page()
        if blank_second_page and pg == 2:
            continue
        page.insert_text((72, 700), f"Bottom-pg{pg} written first in object order")
        page.insert_text((72, 120), f"Top-pg{pg} written second in object order")
    doc.save(path)
    doc.close()


class PdfSortTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory()
        cls.reversed_pdf = str(Path(cls._tmp.name) / "reversed.pdf")
        build_reversed_pdf(cls.reversed_pdf)
        cls.blank_tail_pdf = str(Path(cls._tmp.name) / "blank_tail.pdf")
        build_reversed_pdf(cls.blank_tail_pdf, blank_second_page=True)

    @classmethod
    def tearDownClass(cls):
        cls._tmp.cleanup()

    def read(self, *args, pdf=None, env_extra=None):
        env = {**os.environ, **(env_extra or {})}
        return subprocess.run(
            [os.sys.executable, str(SCRIPT), "-f", pdf or self.reversed_pdf, *args],
            capture_output=True,
            text=True,
            env=env,
            timeout=60,
        )

    def test_default_order_control_preserves_object_order(self):
        result = self.read()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Bottom-pg1 written first in object order", result.stdout)
        self.assertLess(
            result.stdout.index("Bottom-pg1"),
            result.stdout.index("Top-pg1"),
            "default extraction must keep content-object order",
        )

    def test_sort_outputs_text_in_spatial_reading_order(self):
        result = self.read("--sort")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLess(
            result.stdout.index("Top-pg1"),
            result.stdout.index("Bottom-pg1"),
            "--sort must emit the top paragraph first",
        )
        self.assertIn("Top-pg2 written second in object order", result.stdout)

    def test_sort_outputs_json_in_spatial_reading_order(self):
        result = self.read("--sort", "--format", "json")
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads(result.stdout)
        self.assertEqual(doc["total_pages"], 2)
        self.assertEqual([p["page"] for p in doc["pages"]], [1, 2])
        self.assertTrue(doc["pages"][0]["text"].startswith("Top-pg1"))

    def test_sort_selected_pages_and_metadata(self):
        result = self.read("--sort", "-p", "1", "-d", "--format", "json")
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads(result.stdout)
        self.assertEqual([p["page"] for p in doc["pages"]], [1])
        self.assertEqual(doc["metadata"]["title"], "Reversed object order")
        self.assertTrue(doc["pages"][0]["text"].startswith("Top-pg1"))

    def test_sort_respects_max_length(self):
        result = self.read("--sort", "-m", "60")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLessEqual(
            len(result.stdout), 60 + len("\n...[truncated]") + 1
        )  # +1: print appends a newline
        self.assertTrue(result.stdout.rstrip("\n").endswith("...[truncated]"))
        self.assertIn("Top-pg1", result.stdout[:60])

    def test_sort_reports_blank_pages(self):
        result = self.read("--sort", "--format", "json", pdf=self.blank_tail_pdf)
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads(result.stdout)
        self.assertEqual(len(doc["pages"]), 2)
        self.assertEqual(doc["pages"][1]["text"], "")
        self.assertTrue(doc["pages"][0]["text"].startswith("Top-pg1"))

    def test_sort_rejected_on_old_engine_before_open(self):
        with tempfile.TemporaryDirectory() as stub_dir:
            (Path(stub_dir) / "pymupdf.py").write_text(
                'VersionFitz = "1.18.7"\n'
                "\n"
                "def open(*args, **kwargs):\n"
                '    raise AssertionError("engine opened the document")\n',
                encoding="utf-8",
            )
            missing = str(Path(stub_dir) / "missing.pdf")
            result = self.read(
                "--sort", pdf=missing, env_extra={"PYTHONPATH": stub_dir}
            )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        combined = result.stdout + result.stderr
        self.assertIn("1.19.1", combined)
        self.assertNotIn("engine opened", combined)
        self.assertNotIn("not found", combined)


if __name__ == "__main__":
    unittest.main()
