#!/usr/bin/env python3
"""Regression tests for crawl_docs.py exit status and success counting.

check_docs.py's ``run_crawl()`` treats returncode 0 as "docs updated".
``crawl_docs.py`` used to exit 0 unconditionally — even when every page
fetch failed or every extracted body came back empty — so the freshness
loop in check_docs.py kept serving a cache directory whose files were
never refreshed. These tests pin the contract: main() returns 0 only
when at least one doc carried non-empty content.
"""

import importlib.util
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "anolisa-guide"
    / "scripts"
    / "crawl_docs.py"
)


def load_crawl_module():
    # markdownify is an optional runtime dep of the crawl venv; stub it so
    # the module imports hermetically (the tests below never rely on it).
    if "markdownify" not in sys.modules:
        stub = types.ModuleType("markdownify")
        stub.markdownify = lambda html, **kwargs: "stub content"
        sys.modules["markdownify"] = stub
    spec = importlib.util.spec_from_file_location("crawl_docs_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CrawlExitStatusTest(unittest.TestCase):
    def setUp(self):
        self.module = load_crawl_module()
        self.tmp = tempfile.TemporaryDirectory()
        self.output_dir = Path(self.tmp.name) / "reference"
        self.argv = sys.argv

    def tearDown(self):
        sys.argv = self.argv
        self.tmp.cleanup()

    def run_main(self):
        sys.argv = ["crawl_docs.py", "--output-dir", str(self.output_dir)]
        return self.module.main()

    def test_all_fetches_failing_returns_1(self):
        with mock.patch.object(self.module, "get_page_content", return_value=None):
            code = self.run_main()
        self.assertEqual(code, 1, "a crawl that saved nothing must not exit 0")

    def test_empty_content_counts_as_failure(self):
        empty_page = {"url": "/x", "full_url": "u", "title": "t",
                      "last_modified": "", "content": "  \n"}
        with mock.patch.object(
            self.module, "extract_markdown", return_value=empty_page
        ):
            code = self.run_main()
        self.assertEqual(
            code, 1, "pages whose extracted body is empty must not count as success"
        )
        summary = __import__("json").loads(
            (self.output_dir / "crawl_summary.json").read_text(encoding="utf-8")
        )
        self.assertEqual(summary["success_count"], 0)
        self.assertEqual(summary["failed_count"], len(self.module.DOC_URLS))

    def test_nonempty_content_returns_0(self):
        good_page = {"url": "/x", "full_url": "u", "title": "t",
                     "last_modified": "", "content": "# real content"}
        with mock.patch.object(
            self.module, "extract_markdown", return_value=good_page
        ):
            code = self.run_main()
        self.assertEqual(code, 0)


if __name__ == "__main__":
    unittest.main()
