#!/usr/bin/env python3
"""Regression tests for check_docs.py crawl attempt count.

With a stale cache whose crawl fails, main() used to fall from step 2
straight into step 3 and immediately re-run the same crawl — a second
ensure_venv + up-to-180s subprocess with no state change in between,
doubling the failure time before the static-docs fallback. One crawl
attempt per decision is enough.
"""

import importlib.util
import sys
import tempfile
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
    / "check_docs.py"
)


def load_module():
    spec = importlib.util.spec_from_file_location("check_docs_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def make_stale_cache(cache_dir: Path):
    """A cache directory that exists but reads as stale/incomplete."""
    cache_dir.mkdir(parents=True, exist_ok=True)
    (cache_dir / "only.md").write_text("# incomplete\n", encoding="utf-8")


class SingleCrawlTest(unittest.TestCase):
    def setUp(self):
        self.module = load_module()
        self.tmp = tempfile.TemporaryDirectory()
        self.tmp_path = Path(self.tmp.name)
        self.cache = self.tmp_path / "cache" / "reference"
        make_stale_cache(self.cache)
        self.static = self.tmp_path / "static" / "reference"
        make_stale_cache(self.static)

    def tearDown(self):
        self.tmp.cleanup()

    def run_main(self):
        with mock.patch.object(self.module, "CACHE_DIR", self.cache), \
                mock.patch.object(self.module, "STATIC_DIR", self.static):
            return self.module.main()

    def test_stale_cache_failed_crawl_runs_crawl_once(self):
        calls = []
        with mock.patch.object(
            self.module, "run_crawl", side_effect=lambda d: calls.append(d) or False
        ):
            code = self.run_main()
        # Static fallback still succeeds (step 4).
        self.assertEqual(code, 0)
        self.assertEqual(
            len(calls),
            1,
            "a failed crawl must not be retried back-to-back with no state change",
        )
        self.assertEqual(calls[0], self.cache)

    def test_missing_cache_crawls_once_then_falls_back(self):
        import shutil

        shutil.rmtree(self.cache)
        calls = []
        with mock.patch.object(
            self.module, "run_crawl", side_effect=lambda d: calls.append(d) or False
        ):
            code = self.run_main()
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 1)

    def test_successful_crawl_uses_cache(self):
        with mock.patch.object(self.module, "run_crawl", return_value=True) as crawl:
            code = self.run_main()
        self.assertEqual(code, 0)
        self.assertEqual(crawl.call_count, 1)


if __name__ == "__main__":
    unittest.main()
