"""--outline must export PDF bookmark hierarchy and page destinations."""

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

NESTED_TOC = [
    [1, "Intro", 1],
    [1, "\u4e2d\u6587\u7ae0\u8282", 2],
    [2, "Nested \u00e9\u00e0\u00e7", 2],
    [3, "Deep \u2603 snow", 3],
    [1, "External link", 1],
]
NESTED_OUTLINE = [
    [1, "Intro", 1],
    [1, "\u4e2d\u6587\u7ae0\u8282", 2],
    [2, "Nested \u00e9\u00e0\u00e7", 2],
    [3, "Deep \u2603 snow", 3],
    [1, "External link", -1],
]


def _load_json(result):
    """Parse the CLI's JSON report, tolerating engine import advisories.

    Current PyMuPDF versions print a ``fitz`` deprecation advisory onto
    stdout when the legacy alias is imported; the JSON payload always
    starts at the first opening brace. This keeps the suite isolated from
    the separately pending legacy-import stdout fix (#4514), mirroring the
    ``--tables`` sibling suite's convention.
    """
    return json.loads(result.stdout[result.stdout.index("{") :])


def build_pdf(path, toc=None, destless=(), metadata=None):
    """Real three-page PDF; titles in `destless` become URI-action bookmarks."""
    import fitz

    doc = fitz.open()
    for i in range(3):
        page = doc.new_page()
        page.insert_text((72, 720), f"Body page {i + 1}")
    if metadata:
        doc.set_metadata(metadata)
    if toc:
        doc.set_toc(toc)
        for item in doc.get_toc(simple=False):
            if item[1] in destless:
                doc.xref_set_key(item[3]["xref"], "Dest", "null")
                doc.xref_set_key(
                    item[3]["xref"], "A", "<</S/URI/URI(http://example.com/)>>"
                )
    doc.save(path)
    doc.close()


class PdfOutlineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory()
        cls.nested_pdf = str(Path(cls._tmp.name) / "nested.pdf")
        build_pdf(
            cls.nested_pdf,
            toc=NESTED_TOC,
            destless=("External link",),
            metadata={"title": "Outline fixture", "author": "fixture"},
        )
        cls.destless_pdf = str(Path(cls._tmp.name) / "destless.pdf")
        build_pdf(
            cls.destless_pdf,
            toc=[[1, "Go web", 1], [1, "Home", 2]],
            destless=("Go web",),
        )
        cls.flat_pdf = str(Path(cls._tmp.name) / "flat.pdf")
        build_pdf(cls.flat_pdf)

    @classmethod
    def tearDownClass(cls):
        cls._tmp.cleanup()

    def read(self, *args, pdf=None, env_extra=None):
        env = {**os.environ, **(env_extra or {})}
        return subprocess.run(
            [sys.executable, str(SCRIPT), "-f", pdf or self.nested_pdf, *args],
            capture_output=True,
            text=True,
            env=env,
            timeout=60,
        )

    def test_outline_json_preserves_nesting_unicode_and_order(self):
        result = self.read("--outline", "--format", "json")
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = _load_json(result)
        self.assertEqual(doc["outline"], NESTED_OUTLINE)

    def test_outline_covers_full_document_with_selected_pages(self):
        result = self.read("--outline", "-p", "2", "--format", "json")
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = _load_json(result)
        self.assertEqual([p["page"] for p in doc["pages"]], [2])
        self.assertIn("Body page 2", doc["pages"][0]["text"])
        self.assertEqual(doc["outline"], NESTED_OUTLINE)

    def test_outline_empty_when_no_bookmarks(self):
        result = self.read("--outline", "--format", "json", pdf=self.flat_pdf)
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = _load_json(result)
        self.assertEqual(doc["outline"], [])

    def test_outline_marks_destinationless_bookmarks(self):
        result = self.read("--outline", "--format", "json", pdf=self.destless_pdf)
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = _load_json(result)
        self.assertEqual(doc["outline"], [[1, "Go web", -1], [1, "Home", 2]])

    def test_outline_with_metadata(self):
        result = self.read("--outline", "-d", "--format", "json")
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = _load_json(result)
        self.assertEqual(doc["metadata"]["title"], "Outline fixture")
        self.assertEqual(doc["metadata"]["author"], "fixture")
        self.assertEqual(doc["outline"], NESTED_OUTLINE)
        self.assertEqual(doc["total_pages"], 3)

    def test_outline_rejected_in_text_mode_before_open(self):
        with tempfile.TemporaryDirectory() as stub_dir:
            (Path(stub_dir) / "fitz.py").write_text(
                'VersionFitz = "1.27.2"\n'
                "\n"
                "def open(*args, **kwargs):\n"
                '    raise AssertionError("engine opened the document")\n',
                encoding="utf-8",
            )
            missing = str(Path(stub_dir) / "missing.pdf")
            result = self.read(
                "--outline", pdf=missing, env_extra={"PYTHONPATH": stub_dir}
            )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        combined = result.stdout + result.stderr
        self.assertIn("--format json", combined)
        self.assertNotIn("engine opened", combined)
        self.assertNotIn("not found", combined)

    def test_default_json_has_no_outline(self):
        result = self.read("--format", "json")
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = _load_json(result)
        self.assertEqual(sorted(doc.keys()), ["pages", "total_pages"])
        self.assertEqual(len(doc["pages"]), 3)


if __name__ == "__main__":
    unittest.main()
