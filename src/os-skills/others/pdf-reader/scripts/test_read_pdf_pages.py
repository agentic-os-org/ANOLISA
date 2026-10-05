#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for read_pdf.py --pages spec handling.

Regression tests for malformed page specs (like "1,abc" or "2-") which used
to escape as raw ValueError tracebacks, and for specs whose pages all fall
outside the document, which used to print nothing and exit 0 — the agent
calling this skill cannot distinguish that from an empty PDF.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "read_pdf.py")

try:
    import fitz  # noqa: F401
    HAS_FITZ = True
except ImportError:
    HAS_FITZ = False


def build_pdf(path: str, page_count: int = 3) -> str:
    import fitz
    doc = fitz.open()
    for i in range(page_count):
        page = doc.new_page()
        page.insert_text((72, 72), f"Page {i + 1} content here")
    doc.save(path)
    doc.close()
    return path


def run_reader(path: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT, "-f", path, *args],
                          capture_output=True, text=True)


@unittest.skipUnless(HAS_FITZ, "PyMuPDF not installed")
class TestReadPdfPageSpec(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.pdf = build_pdf(os.path.join(self._tmp.name, "doc.pdf"), 3)

    def tearDown(self):
        self._tmp.cleanup()

    def test_malformed_page_number_is_clean_error(self):
        """修复前：-p "1,abc" 的 int("abc") 抛 ValueError 裸堆栈。
        修复后：stderr 一行 ERROR、exit 1，无 Traceback。
        """
        result = run_reader(self.pdf, "-p", "1,abc")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ERROR", result.stderr)
        self.assertIn("abc", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_open_ended_range_is_clean_error(self):
        """修复前：-p "2-" 的 int("") 抛 ValueError 裸堆栈。"""
        result = run_reader(self.pdf, "-p", "2-")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ERROR", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_valid_range_extracts_requested_pages(self):
        """正常链路保护：1-2 取前两页，不含第三页。"""
        result = run_reader(self.pdf, "-p", "1-2")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--- Page 1 ---", result.stdout)
        self.assertIn("--- Page 2 ---", result.stdout)
        self.assertNotIn("--- Page 3 ---", result.stdout)

    def test_single_page_and_json_output(self):
        """正常链路保护：单页 + JSON 结构化输出。"""
        result = run_reader(self.pdf, "-p", "2", "--format", "json")
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["total_pages"], 3)
        self.assertEqual([p["page"] for p in data["pages"]], [2])

    def test_mixed_spec_tolerates_out_of_range(self):
        """既有容忍行为保持：1,99 仍取出第 1 页（混合规格不整体失败）。"""
        result = run_reader(self.pdf, "-p", "1,99")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--- Page 1 ---", result.stdout)
        self.assertNotIn("--- Page 3 ---", result.stdout)

    def test_all_out_of_range_is_clean_error(self):
        """修复前：-p "9" 在 3 页文档上输出为空且 exit 0——调用方无法
        与"PDF 没有文本"区分。修复后：明确报错 exit 1。
        """
        result = run_reader(self.pdf, "-p", "9")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ERROR", result.stderr)
        self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
