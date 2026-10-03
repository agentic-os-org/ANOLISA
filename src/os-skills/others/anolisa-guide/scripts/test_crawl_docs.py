#!/usr/bin/env python3
"""Tests for crawl_docs.post_process_markdown heading bold cleanup."""
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


class BoldHeadingCleanupTest(unittest.TestCase):
    def test_split_bold_heading_keeps_text(self):
        self.assertEqual(
            crawl_docs.post_process_markdown("#### **什么** **是** Agent OS\n"),
            "#### 什么是 Agent OS\n",
        )

    def test_single_bold_heading_with_trailing_space_keeps_text(self):
        self.assertEqual(
            crawl_docs.post_process_markdown("## **快速开始** \n"),
            "## 快速开始\n",
        )

    def test_split_bold_only_heading_matches_documented_output(self):
        self.assertEqual(
            crawl_docs.post_process_markdown("## **什么** **是**\n"),
            "## 什么是\n",
        )

    def test_single_bold_heading_is_unwrapped(self):
        self.assertEqual(
            crawl_docs.post_process_markdown("#### **标题**\n"),
            "#### 标题\n",
        )

    def test_bold_segment_with_inner_spaces_is_preserved(self):
        self.assertEqual(
            crawl_docs.post_process_markdown("## **Quick Start** Guide\n"),
            "## Quick Start Guide\n",
        )

    def test_plain_heading_is_unchanged(self):
        self.assertEqual(
            crawl_docs.post_process_markdown("# Plain heading\n"),
            "# Plain heading\n",
        )


if __name__ == "__main__":
    unittest.main()
