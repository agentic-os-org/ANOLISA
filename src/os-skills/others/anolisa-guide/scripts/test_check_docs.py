#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for check_docs.py fallback ordering and failure reporting.

Regression tests for the double-crawl bug (a failed refresh fell through
into the create path and ran the identical crawl again — up to ~10 minutes
of guaranteed second failure before the static fallback), for the
"unusable cache" cases that were mislabeled as merely expired, for the
false-success bug (crawl_docs.py exits 0 even when every page failed, so
the zero exit code is re-validated with check_freshness() instead of
being trusted), and for the empty failure diagnostics when the child
reports on stdout.

Stdlib only, no network, no venv, no root privileges required.
Requires Python 3.10+ (check_docs.py uses PEP 604 annotations that are
evaluated at import time).

Run from the repository root:

    python3 -m unittest discover -s src/os-skills/others/anolisa-guide/scripts -p 'test_check_docs.py'
"""

import contextlib
import io
import sys
import tempfile
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest import mock

SCRIPTS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS_DIR))

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
    """Patches run_crawl and ensure_venv; records every crawl invocation.

    `files` controls what the simulated child leaves behind:
    crawl_docs.py exits 0 no matter how many pages actually succeeded,
    so (succeed=True, files=0) reproduces a total fetch failure that
    still reports success, and files=5 a partial crawl.
    """

    def __init__(self, succeed=False, files=0):
        self.calls = []
        self.succeed = succeed
        self.files = files

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
        if self.succeed and self.files:
            write_docs(output_dir, count=self.files)
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

    def test_validated_crawl_uses_cache(self):
        # Only a crawl that exits 0 AND leaves a complete fresh doc set
        # may print [成功] and hand out the cache directory.
        write_docs(self.static, when=datetime.now() - timedelta(days=30))
        with CrawlRecorder(succeed=True, files=13) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0, out)
        self.assertIn("[成功] 文档已更新", out)
        self.assertEqual(out.splitlines()[-1], str(self.cache))
        self.assertNotIn("[兜底]", out)
        self.assertEqual(len(crawl.calls), 1)

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
        # 无静态兜底时不得宣告“转入静态文档兜底”。
        self.assertNotIn("[兜底]", out)
        self.assertNotIn("转入静态文档兜底", out)

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

    def test_exit_zero_crawl_with_no_docs_falls_back(self):
        # crawl_docs.py exits 0 even when all 13 pages fail (it only
        # writes crawl_summary.json); the exit code must not be trusted.
        write_docs(self.static, when=datetime.now() - timedelta(days=30))
        with CrawlRecorder(succeed=True) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0, out)
        self.assertIn("[缓存不可用] 爬取后校验未通过", out)
        self.assertIn("文档不完整（0/13）", out)
        self.assertIn("[兜底]", out)
        self.assertEqual(out.splitlines()[-1], str(self.static))
        self.assertNotIn("[成功]", out)
        self.assertEqual(len(crawl.calls), 1)

    def test_partial_crawl_rejected(self):
        # 5 of 13 pages: exit code 0, but an incomplete doc set must not
        # be served as a success.
        write_docs(self.static, when=datetime.now() - timedelta(days=30))
        with CrawlRecorder(succeed=True, files=5) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0, out)
        self.assertIn("文档不完整（5/13）", out)
        self.assertIn("[兜底]", out)
        self.assertEqual(out.splitlines()[-1], str(self.static))
        self.assertNotIn("[成功]", out)
        self.assertEqual(len(crawl.calls), 1)

    def test_unwritable_cache_dir_falls_back(self):
        # Deterministic and root-proof: a read-only filesystem fails
        # CACHE_DIR.mkdir(), the crawl must never start, and the static
        # fallback still answers.
        write_docs(self.static, when=datetime.now() - timedelta(days=30))
        with mock.patch.object(Path, "mkdir",
                               side_effect=OSError(30, "Read-only file system")), \
             CrawlRecorder(succeed=False) as crawl:
            rc, out = run_main()
        self.assertEqual(rc, 0, out)
        self.assertIn("[兜底]", out)
        self.assertEqual(crawl.calls, [], "no crawl after mkdir failure")


class FailureReporting(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        scripts = Path(self._tmp.name)
        # run_crawl() requires SCRIPT_DIR/crawl_docs.py to exist; a stub
        # keeps the tests independent of where the real scripts live.
        (scripts / "crawl_docs.py").write_text("# stub\n", encoding="utf-8")
        patch = mock.patch.object(check_docs, "SCRIPT_DIR", scripts)
        patch.start()
        self.addCleanup(patch.stop)
        self.addCleanup(self._tmp.cleanup)

    def _run_crawl_with(self, fake_result):
        out = io.StringIO()
        with mock.patch.object(check_docs, "ensure_venv",
                               return_value=Path("/fake/venv/bin/python")), \
             mock.patch.object(check_docs.subprocess, "run",
                               return_value=fake_result), \
             contextlib.redirect_stdout(out):
            ok = check_docs.run_crawl(Path("/anywhere"))
        return ok, out.getvalue()

    def test_run_crawl_failure_reports_stdout_tail(self):
        # crawl_docs.py reports per-page failures on stdout; with empty
        # stderr the old code printed "[失败] " with no reason at all.
        fake = mock.Mock(returncode=1, stderr="",
                         stdout="\n".join(f"line{i}" for i in range(6)))
        ok, out = self._run_crawl_with(fake)
        self.assertFalse(ok)
        # The last five stdout lines, not the first, and not a bare [失败] —
        # joined into ONE line so the child output cannot compete with the
        # directory answer on the final line.
        detail = [ln for ln in out.splitlines() if ln.startswith("[失败]")][0]
        self.assertEqual(detail, "[失败] line1 | line2 | line3 | line4 | line5")
        self.assertNotIn("line0", out)

    def test_run_crawl_failure_prefers_stderr_when_present(self):
        fake = mock.Mock(returncode=1, stderr="boom",
                         stdout="noise\n" * 6)
        ok, out = self._run_crawl_with(fake)
        self.assertFalse(ok)
        self.assertIn("boom", out)

    def test_run_crawl_failure_reports_no_output(self):
        # Both streams empty: the reason must still be non-empty instead
        # of a bare "[失败] ".
        fake = mock.Mock(returncode=1, stderr="", stdout="")
        ok, out = self._run_crawl_with(fake)
        self.assertFalse(ok)
        self.assertIn("[失败] 无输出", out)


if __name__ == "__main__":
    unittest.main()
