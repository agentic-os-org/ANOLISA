#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for check_docs.py fallback ordering and failure reporting.

Regression tests for the double-crawl bug (a failed refresh fell through
into the create path and ran the identical crawl again — up to ~10 minutes
of guaranteed second failure before the static fallback), for the
"unusable cache" cases that were mislabeled as merely expired, and for the
empty failure diagnostics when the child reports on stdout.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest import mock

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SCRIPTS_DIR)

import check_docs  # noqa: E402


def write_docs(directory: Path, count=13, when=None):
    """Write a doc set; `when` controls the crawl timestamp of each file."""
    directory.mkdir(parents=True, exist_ok=True)
    if when is None:
        when = datetime.now()
    for i in range(count):
        (directory / f"doc{i:02d}.md").write_text(
            f"# doc{i}\n\n**爬取时间**: {when.strftime('%Y-%m-%d %H:%M:%S')}\n",
            encoding="utf-8",
        )


class CrawlRecorder:
    """Patches run_crawl and ensure_venv; records every crawl invocation."""

    def __init__(self, succeed=False):
        self.calls = []
        self.succeed = succeed

    def __enter__(self):
        self._patches = [
            mock.patch.object(check_docs, "run_crawl", self._run_crawl),
            mock.patch.object(check_docs, "ensure_venv",
                              return_value=Path("/fake/venv/bin/python")),
        ]
        for patch in self._patches:
            patch.start()
        return self

    def _run_crawl(self, output_dir):
        self.calls.append(output_dir)
        return self.succeed

    def __exit__(self, *exc):
        for patch in self._patches:
            patch.stop()
        return False


def run_main():
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        rc = check_docs.main()
    return rc, out.getvalue()


class CheckDocsFallback(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        base = Path(self._tmp.name)
        self.static = base / "static"
        self.cache = base / "cache"
        patches = [
            mock.patch.object(check_docs, "STATIC_DIR", self.static),
            mock.patch.object(check_docs, "CACHE_DIR", self.cache),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def tearDown(self):
        self._tmp.cleanup()

    def test_fresh_static_wins_without_crawl(self):
        write_docs(self.static)
        with CrawlRecorder(succeed=True) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0)
        self.assertIn("[使用静态]", out)
        self.assertIn(str(self.static), out)
        self.assertEqual(crawl.calls, [], "fresh static must not crawl")

    def test_fresh_cache_used_when_static_stale(self):
        write_docs(self.static, when=datetime.now() - timedelta(days=30))
        write_docs(self.cache)
        with CrawlRecorder(succeed=True) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0)
        self.assertIn("[使用缓存]", out)
        self.assertEqual(crawl.calls, [])

    def test_failed_crawl_falls_back_to_static_once(self):
        # The regression test for the double crawl: stale cache + failed
        # crawl must reach the fallback after exactly ONE attempt.
        write_docs(self.static, when=datetime.now() - timedelta(days=30))
        write_docs(self.cache, when=datetime.now() - timedelta(days=30))
        with CrawlRecorder(succeed=False) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0)
        self.assertIn("[兜底]", out)
        self.assertIn(str(self.static), out)
        self.assertEqual(len(crawl.calls), 1,
                         f"exactly one crawl attempt, got {crawl.calls}")

    def test_failed_crawl_without_static_exits_1(self):
        write_docs(self.cache, when=datetime.now() - timedelta(days=30))
        with CrawlRecorder(succeed=False) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 1)
        self.assertEqual(len(crawl.calls), 1)

    def test_missing_cache_crawls_once(self):
        write_docs(self.static, when=datetime.now() - timedelta(days=30))
        with CrawlRecorder(succeed=False) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0)
        self.assertEqual(len(crawl.calls), 1)
        self.assertTrue(self.cache.exists(), "cache dir created before crawl")

    def test_incomplete_cache_reason_is_surfaced(self):
        # 5 of 13 files: an unusable cache, not an expired one — the
        # message must say which.
        write_docs(self.cache, count=5)
        with CrawlRecorder(succeed=False):
            rc, out = run_main()
        self.assertEqual(rc, 1)
        self.assertIn("文档不完整（5/13）", out)
        self.assertIn("[缓存不可用]", out)
        self.assertNotIn("[缓存过期]", out)

    @unittest.skipIf(os.geteuid() == 0,
                     "root ignores the read-only parent directory mode")
    def test_unwritable_cache_dir_falls_back(self):
        write_docs(self.static, when=datetime.now() - timedelta(days=30))
        guard = self.cache.parent
        guard.mkdir(parents=True)
        guard.chmod(0o500)
        self.addCleanup(guard.chmod, 0o700)
        with CrawlRecorder(succeed=False) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0, out)
        self.assertIn("[兜底]", out)
        self.assertEqual(crawl.calls, [], "no crawl after mkdir failure")


class FailureReporting(unittest.TestCase):
    def test_run_crawl_failure_reports_stdout_tail(self):
        # crawl_docs.py reports per-page failures on stdout; with empty
        # stderr the old code printed "[失败] " with no reason at all.
        fake = mock.Mock(returncode=1, stderr="",
                         stdout="\n".join(f"line{i}" for i in range(6)))
        out = io.StringIO()
        with mock.patch.object(check_docs, "ensure_venv",
                               return_value=Path("/fake/venv/bin/python")), \
             mock.patch.object(check_docs, "subprocess") as sub, \
             contextlib.redirect_stdout(out):
            sub.run.return_value = fake
            sub.TimeoutExpired = Exception
            ok = check_docs.run_crawl(Path("/anywhere"))
        self.assertFalse(ok)
        # The last five stdout lines, not the first, and not a bare [失败].
        self.assertIn("line5", out.getvalue())
        self.assertIn("line1", out.getvalue())
        self.assertNotIn("line0", out.getvalue())
        self.assertIn("[失败]", out.getvalue())

    def test_run_crawl_failure_prefers_stderr_when_present(self):
        fake = mock.Mock(returncode=1, stderr="boom",
                         stdout="noise\n" * 6)
        out = io.StringIO()
        with mock.patch.object(check_docs, "ensure_venv",
                               return_value=Path("/fake/venv/bin/python")), \
             mock.patch.object(check_docs, "subprocess") as sub, \
             contextlib.redirect_stdout(out):
            sub.run.return_value = fake
            sub.TimeoutExpired = Exception
            ok = check_docs.run_crawl(Path("/anywhere"))
        self.assertFalse(ok)
        self.assertIn("boom", out.getvalue())


if __name__ == "__main__":
    unittest.main()
