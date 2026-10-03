#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for crawl_docs.py HTTP status and empty-content handling.

Regression tests for 404/5xx pages flowing through as success: the crawler
never called raise_for_status(), so an error page replaced the previous good
reference/<name>.md while crawl_summary.json reported success.
"""

import importlib.util
import os
import sys
import tempfile
import types
import unittest
from pathlib import Path

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "crawl_docs.py")


class FakeResponse:
    def __init__(self, status_code, text):
        self.status_code = status_code
        self.text = text
        self.encoding = None

    def raise_for_status(self):
        if self.status_code >= 400:
            raise RuntimeError(f"{self.status_code} Client Error")


class FakeTag:
    def __init__(self, text):
        self._text = text

    def get_text(self, strip=False):
        return self._text

    def find_all(self, names):
        return []

    def get(self, key, default=None):
        return default


class FakeSoup:
    """Only knows a title; markdown-body container is configurable."""

    def __init__(self, html, parser=None, body_text=None):
        self._title = FakeTag("文档标题 - Alibaba Cloud Linux(Alinux)-阿里云帮助中心")
        self._body = FakeTag(body_text) if body_text is not None else None

    def find(self, name, attrs=None, **kw):
        if name == "title":
            return self._title
        if name == "h1":
            return None
        if name == "meta":
            return None
        if name == "div":
            return self._body
        return None


def install_stubs(body_present):
    """Replace requests/bs4/markdownify in sys.modules (audit technique)."""
    requests = types.ModuleType("requests")

    def _get(url, headers=None, timeout=None):
        return FakeResponse(200, "<html>ok</html>")

    requests.get = _get
    sys.modules["requests"] = requests

    bs4 = types.ModuleType("bs4")
    bs4.BeautifulSoup = lambda html, parser=None: FakeSoup(
        html, parser, body_text="正文内容" if body_present else None)
    sys.modules["bs4"] = bs4

    markdownify = types.ModuleType("markdownify")
    markdownify.markdownify = lambda s, **kw: "正文内容" if body_present else ""
    sys.modules["markdownify"] = markdownify


def load_crawl_docs():
    spec = importlib.util.spec_from_file_location("crawl_docs_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.DOC_URLS = [("faq", "/zh/alinux/faq")]
    module.time.sleep = lambda s: None
    return module


class TestCrawlHttpHandling(unittest.TestCase):
    def test_404_page_marks_failed_and_keeps_old_file(self):
        install_stubs(body_present=True)
        sys.modules["requests"].get = lambda url, headers=None, timeout=None: \
            FakeResponse(404, "<html><title>404 Not Found</title></html>")
        module = load_crawl_docs()
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            old = "# FAQ\n\n<old good content>\n"
            (out / "faq.md").write_text(old, encoding="utf-8")
            summary = module.crawl_all_docs(out)
            self.assertEqual(summary["failed_count"], 1)
            self.assertEqual(summary["success_count"], 0)
            self.assertEqual(summary["results"][0]["status"], "failed")
            self.assertEqual((out / "faq.md").read_text(encoding="utf-8"), old)

    def test_empty_content_marks_failed_and_keeps_old_file(self):
        install_stubs(body_present=False)
        module = load_crawl_docs()
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            old = "# FAQ\n\n<old good content>\n"
            (out / "faq.md").write_text(old, encoding="utf-8")
            summary = module.crawl_all_docs(out)
            self.assertEqual(summary["failed_count"], 1)
            self.assertEqual(summary["results"][0]["status"], "failed")
            self.assertEqual((out / "faq.md").read_text(encoding="utf-8"), old)

    def test_good_page_still_saved(self):
        install_stubs(body_present=True)
        module = load_crawl_docs()
        with tempfile.TemporaryDirectory() as root:
            out = Path(root)
            summary = module.crawl_all_docs(out)
            self.assertEqual(summary["success_count"], 1)
            self.assertEqual(summary["results"][0]["status"], "success")
            saved = (out / "faq.md").read_text(encoding="utf-8")
            self.assertIn("正文内容", saved)


if __name__ == "__main__":
    unittest.main()
