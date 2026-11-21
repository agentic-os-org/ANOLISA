#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Only complete, consistently fresh guide caches may be selected."""

import importlib.util
import sys
import tempfile
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/anolisa-guide/scripts/check_docs.py"
)
SPEC = importlib.util.spec_from_file_location("check_docs", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
check_docs = importlib.util.module_from_spec(SPEC)
with mock.patch.object(sys, "path", [str(SCRIPT.parent), *sys.path]):
    SPEC.loader.exec_module(check_docs)
DOCUMENTS = (
    "releasenotes",
    "agentic-os",
    "getting-started",
    "cosh-usage",
    "configuration",
    "extensibility",
    "agentsight",
    "agentseccore",
    "tokenless",
    "ws-ckpt",
    "deploy-openclaw",
    "resize-ecs",
    "faq",
)


class DocumentCacheTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.cache_dir = Path(self.temp_dir.name)
        for name in DOCUMENTS:
            self.write_document(name)

    def write_document(self, name: str, age_days: int = 0) -> None:
        timestamp = (datetime.now() - timedelta(days=age_days)).strftime("%Y-%m-%d %H:%M:%S")
        (self.cache_dir / f"{name}.md").write_text(
            f"# Guide\n\n> **爬取时间**: {timestamp}\n", encoding="utf-8"
        )

    def test_new_document_cannot_mask_expired_required_documents(self) -> None:
        for name in DOCUMENTS[1:]:
            self.write_document(name, 30)
        self.assertFalse(check_docs.check_freshness(self.cache_dir)[0])

    def test_required_document_without_timestamp_is_unknown(self) -> None:
        (self.cache_dir / "faq.md").write_text("# FAQ\n", encoding="utf-8")
        self.assertIsNone(check_docs.check_freshness(self.cache_dir)[0])

    def test_unrelated_markdown_cannot_replace_a_missing_document(self) -> None:
        (self.cache_dir / "faq.md").unlink()
        self.write_document("unrelated")
        self.assertIsNone(check_docs.check_freshness(self.cache_dir)[0])

    def test_every_expected_document_fresh_is_accepted(self) -> None:
        self.assertTrue(check_docs.check_freshness(self.cache_dir)[0])

    def test_unrelated_expired_files_do_not_invalidate_expected_documents(self) -> None:
        self.write_document("unrelated", 30)
        self.assertTrue(check_docs.check_freshness(self.cache_dir)[0])


if __name__ == "__main__":
    unittest.main()
