#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""check_docs.py must judge a crawl by the cache it produced, not by exit code.

`crawl_docs.py` exits 0 even when every fetch failed, so a run that left the
cache empty (or still holding the pre-crawl pages) used to be reported as
`[成功] 文档已更新` and the unusable cache directory was recommended.
"""

import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest import mock

SCRIPTS_DIR = Path(__file__).resolve().parents[1] / "others" / "anolisa-guide" / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import check_docs

sys.dont_write_bytecode = True

PAGE_COUNT = 13


def write_cache(directory, stamp):
    directory.mkdir(parents=True, exist_ok=True)
    for index in range(PAGE_COUNT):
        page = directory / f"page{index}.md"
        page.write_text(
            f"# Page {index}\n\n**爬取时间**: {stamp:%Y-%m-%d %H:%M:%S}\n",
            encoding="utf-8",
        )


class TestRunCrawlVerdict(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmpdir.cleanup)
        self.target = Path(self.tmpdir.name) / "reference"
        self.completed = subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr="")

    def _run_crawl(self, completed=None):
        with (
            mock.patch.object(check_docs, "ensure_venv", return_value=Path(sys.executable)),
            mock.patch.object(check_docs.subprocess, "run", return_value=completed or self.completed),
        ):
            return check_docs.run_crawl(self.target)

    def test_silent_crawl_that_wrote_nothing_is_a_failure(self):
        self.target.mkdir()

        self.assertFalse(self._run_crawl())

    def test_crawl_that_left_the_cache_stale_is_a_failure(self):
        write_cache(self.target, datetime.now() - timedelta(days=30))

        self.assertFalse(self._run_crawl())

    def test_crawl_that_left_a_fresh_cache_is_success(self):
        write_cache(self.target, datetime.now())

        self.assertTrue(self._run_crawl())

    def test_non_zero_exit_is_still_a_failure(self):
        write_cache(self.target, datetime.now())
        failed = subprocess.CompletedProcess(args=[], returncode=1, stdout="", stderr="boom")

        self.assertFalse(self._run_crawl(failed))


class TestCrawlTimeoutBudget(unittest.TestCase):
    def test_timeout_covers_the_crawler_worst_case(self):
        worst_case = PAGE_COUNT * (30 + 0.5)

        self.assertGreater(check_docs.TIMEOUT, worst_case)


if __name__ == "__main__":
    unittest.main()
