#!/usr/bin/env python3
"""Tests for crawl_docs.post_process_markdown fenced-code handling."""
import sys
import types
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

# crawl_docs imports third-party modules at import time; stub them so the
# test runs with stdlib only.
for _name, _attrs in (
    ("requests", {}),
    ("bs4", {"BeautifulSoup": object}),
    ("markdownify", {}),
):
    _mod = types.ModuleType(_name)
    for _k, _v in _attrs.items():
        setattr(_mod, _k, _v)
    sys.modules[_name] = _mod

import crawl_docs


class FencedCodeBlockTest(unittest.TestCase):
    def test_backtick_fence_comment_is_untouched(self):
        content = "```bash\n# set **BOLD** flag here\nexport X=1\n```\n"
        self.assertEqual(crawl_docs.post_process_markdown(content), content)

    def test_tilde_fence_comment_is_untouched(self):
        content = "~~~python\n# keep **markers**   and   spacing\n~~~\n"
        self.assertEqual(crawl_docs.post_process_markdown(content), content)

    def test_headings_around_fence_are_still_processed(self):
        content = "## **快速开始**\n\n```bash\n# comment\n```\n\n#### Real **Heading**\n"
        self.assertEqual(
            crawl_docs.post_process_markdown(content),
            "## 快速开始\n\n```bash\n# comment\n```\n\n#### Real Heading\n",
        )

    def test_processing_resumes_after_fence_closes(self):
        content = "```\n# in fence\n```\n### Spaced   **Heading**\n"
        self.assertEqual(
            crawl_docs.post_process_markdown(content),
            "```\n# in fence\n```\n### Spaced Heading\n",
        )


if __name__ == "__main__":
    unittest.main()
