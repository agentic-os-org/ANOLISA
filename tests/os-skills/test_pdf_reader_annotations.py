"""--annotations must export page-local review markup in JSON reports."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/others/pdf-reader/scripts/read_pdf.py"
)

STICKY_CONTENT = "Sticky \u4e2d\u6587\nsecond line"
HIGHLIGHT_CONTENT = "Highlighted \u2603 passage"


def build_pdf(path):
    """Real three-page PDF: Unicode sticky note, rotated-page highlight."""
    import fitz

    doc = fitz.open()
    for _ in range(3):
        doc.new_page(width=612, height=792)
    p1, p2, p3 = doc[0], doc[1], doc[2]
    p1.insert_text((72, 720), "Body page 1")
    p2.insert_text((72, 720), "Body page 2")
    p2.set_rotation(90)
    p3.insert_text((72, 720), "Body page 3")

    sticky = p1.add_text_annot((100, 200), STICKY_CONTENT)
    sticky.set_info(title="Alice", content=STICKY_CONTENT)
    highlight = p2.add_highlight_annot(fitz.Rect(50, 300, 250, 320))
    highlight.set_info(title="Bob", content=HIGHLIGHT_CONTENT)

    doc.set_metadata({"title": "Annot fixture", "author": "fixture"})
    doc.save(path)
    doc.close()

    ref = fitz.open(path)
    rects = {}
    for i, page in enumerate(ref):
        for annot in page.annots():
            rects.setdefault(i + 1, []).append(
                [round(v, 2) for v in annot.rect]
            )
    ref.close()
    return rects


class PdfAnnotationsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory()
        cls.pdf = str(Path(cls._tmp.name) / "annotated.pdf")
        cls.library_rects = build_pdf(cls.pdf)

    @classmethod
    def tearDownClass(cls):
        cls._tmp.cleanup()

    def run_script(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            capture_output=True,
            text=True,
            timeout=120,
        )

    def json_report(self, *args):
        proc = self.run_script("-f", self.pdf, "--annotations", *args)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return self._payload(proc)

    @staticmethod
    def _payload(proc):
        """Parse the JSON report, tolerating engine import advisories.

        Current PyMuPDF versions print a ``fitz`` deprecation advisory onto
        stdout when the legacy alias is imported; the JSON payload always
        starts at the first opening brace (mirroring the ``--tables``
        sibling suite's convention).
        """
        return json.loads(proc.stdout[proc.stdout.index("{") :])

    def page_of(self, report, number):
        matches = [p for p in report["pages"] if p["page"] == number]
        self.assertEqual(len(matches), 1)
        return matches[0]

    def test_sticky_and_highlight_records(self):
        report = self.json_report("--format", "json")
        sticky = self.page_of(report, 1)["annotations"]
        self.assertEqual(len(sticky), 1)
        self.assertEqual(sticky[0]["type"], "Text")
        self.assertEqual(sticky[0]["author"], "Alice")
        self.assertEqual(
            sticky[0]["rect"], [100.0, 200.0, 116.0, 216.0]
        )
        highlight = self.page_of(report, 2)["annotations"]
        self.assertEqual(len(highlight), 1)
        self.assertEqual(highlight[0]["type"], "Highlight")
        self.assertEqual(highlight[0]["author"], "Bob")
        self.assertEqual(
            highlight[0]["rect"], self.library_rects[2][0]
        )

    def test_unicode_and_multiline_content_preserved(self):
        report = self.json_report("--format", "json")
        sticky = self.page_of(report, 1)["annotations"][0]
        self.assertEqual(sticky["content"], STICKY_CONTENT)
        highlight = self.page_of(report, 2)["annotations"][0]
        self.assertEqual(highlight["content"], HIGHLIGHT_CONTENT)

    def test_annotations_follow_page_selection(self):
        report = self.json_report("-p", "1,3", "--format", "json")
        self.assertEqual(
            [p["page"] for p in report["pages"]], [1, 3]
        )
        self.assertEqual(len(self.page_of(report, 1)["annotations"]), 1)
        self.assertEqual(self.page_of(report, 3)["annotations"], [])
        self.assertNotIn(2, [p["page"] for p in report["pages"]])

    def test_rotated_page_rect_stays_unrotated(self):
        report = self.json_report("-p", "2", "--format", "json")
        rect = self.page_of(report, 2)["annotations"][0]["rect"]
        self.assertEqual(rect, self.library_rects[2][0])
        x0, y0, x1, y1 = rect
        self.assertLess(x0, 300, "rect looks rotation-adjusted")
        self.assertLessEqual(x1, 612)
        self.assertLessEqual(y1, 792)

    def test_annotation_free_page_reports_empty_array(self):
        report = self.json_report("--format", "json")
        self.assertEqual(self.page_of(report, 3)["annotations"], [])

    def test_metadata_coexists_with_annotations(self):
        report = self.json_report("-d", "--format", "json")
        self.assertEqual(report["metadata"]["title"], "Annot fixture")
        self.assertEqual(len(self.page_of(report, 1)["annotations"]), 1)

    def test_text_mode_rejected_before_file_open(self):
        missing = os.path.join(self._tmp.name, "does-not-exist.pdf")
        proc = self.run_script("-f", missing, "--annotations")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("requires --format json", proc.stderr)
        self.assertNotIn("not found", proc.stderr)
        self.assertEqual(proc.stdout, "")

    def test_default_schema_omits_annotations(self):
        proc = self.run_script("-f", self.pdf, "--format", "json")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        report = self._payload(proc)
        for page in report["pages"]:
            self.assertNotIn("annotations", page)


if __name__ == "__main__":
    unittest.main()
