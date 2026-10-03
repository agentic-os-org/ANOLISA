#!/usr/bin/env python3
"""Regression tests for read_pdf.py output capping (stdlib unittest).

Run from this directory:
    python3 -m unittest test_read_pdf -v
"""
import io
import json
import contextlib
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import read_pdf as rp  # noqa: E402


def _pages(*texts):
    return [{"page": i + 1, "text": t} for i, t in enumerate(texts)]


class CapPagesTest(unittest.TestCase):
    def test_zero_limit_disables_cap(self):
        pages = _pages("abc", "def")
        out, truncated = rp._cap_pages(pages, 0)
        self.assertEqual(out, pages)
        self.assertFalse(truncated)

    def test_exact_fit_is_not_truncated(self):
        out, truncated = rp._cap_pages(_pages("abc", "def"), 6)
        self.assertEqual([p["text"] for p in out], ["abc", "def"])
        self.assertFalse(truncated)

    def test_crossing_page_is_cut_and_marked(self):
        out, truncated = rp._cap_pages(_pages("abc", "defgh", "xyz"), 5)
        self.assertEqual([p["page"] for p in out], [1, 2])
        self.assertEqual(out[1]["text"], "de\n...[truncated]")
        self.assertTrue(truncated)

    def test_no_room_drops_page_entirely(self):
        out, truncated = rp._cap_pages(_pages("abc", "def"), 3)
        self.assertEqual([p["page"] for p in out], [1])
        self.assertTrue(truncated)

    def test_empty_pages_list(self):
        out, truncated = rp._cap_pages([], 10)
        self.assertEqual(out, [])
        self.assertFalse(truncated)


class JsonOutputTest(unittest.TestCase):
    """End-to-end runs, gated on PyMuPDF availability."""

    @classmethod
    def setUpClass(cls):
        mod = None
        try:
            import pymupdf as mod
        except ImportError:
            try:
                import fitz as mod
            except ImportError:
                raise unittest.SkipTest("PyMuPDF not installed")
        cls.tmp = tempfile.TemporaryDirectory()

        cls.pdf = os.path.join(cls.tmp.name, "small.pdf")
        doc = mod.open()
        for text in ("A" * 200, "B" * 200):
            page = doc.new_page()
            page.insert_text((72, 72), "\n".join(
                text[i:i + 40] for i in range(0, len(text), 40)))
        doc.save(cls.pdf)
        doc.close()

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def _run(self, *extra):
        script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "read_pdf.py")
        import subprocess
        proc = subprocess.run(
            [sys.executable, script, "-f", self.pdf, "--format", "json", *extra],
            capture_output=True, text=True,
        )
        return proc

    def test_json_parses_without_cap(self):
        proc = self._run()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        doc = json.loads(proc.stdout)
        self.assertEqual(doc["total_pages"], 2)
        self.assertNotIn("truncated", doc)

    def test_json_parses_with_cap_and_flags_truncation(self):
        proc = self._run("-m", "120")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        doc = json.loads(proc.stdout)
        self.assertTrue(doc.get("truncated"))
        joined = sum(len(p["text"]) for p in doc["pages"])
        self.assertLessEqual(joined, 120 + len("\n...[truncated]"))
        # Earlier pages are kept first.
        self.assertEqual(doc["pages"][0]["page"], 1)

    def test_json_has_no_import_warning_prefix(self):
        proc = self._run()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(proc.stdout.lstrip().startswith("{"),
                        f"stdout polluted: {proc.stdout[:80]!r}")


if __name__ == "__main__":
    unittest.main()
