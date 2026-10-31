#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Cached Markdown must retain the source document's link and image targets."""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/anolisa-guide/scripts/crawl_docs.py"
)
SPEC = importlib.util.spec_from_file_location("crawl_docs", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
crawl = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(crawl)


def convert(article: str) -> str:
    return crawl.extract_markdown(
        f'<h1>Guide</h1><div class="markdown-body">{article}</div>',
        "/zh/alinux/agentic-os",
    )["content"]


class SourceUrlTests(unittest.TestCase):
    def test_document_and_root_relative_links_keep_the_remote_target(self) -> None:
        content = convert('<p><a href="../faq">FAQ</a> <a href="/zh/start">Start</a></p>')
        self.assertIn("[FAQ](https://help.aliyun.com/zh/faq)", content)
        self.assertIn("[Start](https://help.aliyun.com/zh/start)", content)

    def test_images_are_resolved_against_the_same_source_page(self) -> None:
        for path, expected in (
            ("../assets/chart.png", "https://help.aliyun.com/zh/assets/chart.png"),
            ("/images/chart.png", "https://help.aliyun.com/images/chart.png"),
            ("//cdn.example.com/chart.png", "https://cdn.example.com/chart.png"),
        ):
            with self.subTest(path=path):
                self.assertIn(f"![Chart]({expected})", convert(f'<img src="{path}" alt="Chart">'))

    def test_query_and_fragment_links_refer_to_the_source_page(self) -> None:
        content = convert('<a href="?lang=en#section">Query</a> <a href="#section">Section</a>')
        self.assertIn(
            "[Query](https://help.aliyun.com/zh/alinux/agentic-os?lang=en#section)", content
        )
        self.assertIn("[Section](https://help.aliyun.com/zh/alinux/agentic-os#section)", content)

    def test_absolute_urls_and_explicit_schemes_are_preserved(self) -> None:
        content = convert(
            '<a href="https://example.com/a?x=1&amp;y=2#part">External</a>'
            '<a href="mailto:team@example.com">Email</a>'
            '<img src="data:image/png;base64,abc" alt="Inline">'
        )
        self.assertIn("[External](https://example.com/a?x=1&y=2#part)", content)
        self.assertIn("[Email](mailto:team@example.com)", content)
        self.assertIn("![Inline](data:image/png;base64,abc)", content)

    def test_malformed_source_urls_do_not_abort_other_content(self) -> None:
        content = convert(
            '<a href="http://[invalid">Malformed</a><a href="/valid">Valid</a>'
            '<img alt="No source"><a>No target</a>'
        )
        self.assertIn("http://[invalid", content)
        self.assertIn("[Valid](https://help.aliyun.com/valid)", content)
        self.assertIn("No target", content)

    def test_content_outside_the_article_is_not_included(self) -> None:
        data = crawl.extract_markdown(
            '<nav><a href="/not-article">Navigation</a></nav><h1>Guide</h1>'
            '<div class="article-content"><a href="/inside">Inside</a>'
            "<script>ignored()</script></div>",
            "/zh/alinux/agentic-os",
        )
        self.assertNotIn("Navigation", data["content"])
        self.assertNotIn("ignored", data["content"])
        self.assertIn("[Inside](https://help.aliyun.com/inside)", data["content"])
        self.assertEqual(data["full_url"], "https://help.aliyun.com/zh/alinux/agentic-os")


if __name__ == "__main__":
    unittest.main()
