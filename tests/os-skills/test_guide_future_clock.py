"""A future crawl timestamp cannot establish a fresh local guide cache."""

import contextlib
import importlib.util
import io
import sys
import tempfile
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/anolisa-guide/scripts/check_docs.py"
)
NOW = datetime(2026, 10, 5, 10, 0, 0)


class FrozenDateTime(datetime):
    @classmethod
    def now(cls, tz=None):
        return NOW if tz is None else NOW.astimezone(tz)


class GuideFutureClockTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("guide_future", SCRIPT)
        cls.guide = importlib.util.module_from_spec(spec)
        before = sys.path[:]
        sys.path.insert(0, str(SCRIPT.parent))
        try:
            spec.loader.exec_module(cls.guide)
        finally:
            sys.path[:] = before

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.clock = mock.patch.object(self.guide, "datetime", FrozenDateTime)
        self.clock.start()
        self.addCleanup(self.clock.stop)

    def cache(self, name, timestamp):
        path = self.root / name
        path.mkdir()
        names = (
            [name for name, _ in self.guide.DOC_URLS]
            if hasattr(self.guide, "DOC_URLS")
            else [f"{i:02}" for i in range(13)]
        )
        for name in names:
            (path / f"{name}.md").write_text(
                f"# Guide\n**爬取时间**: {timestamp:%Y-%m-%d %H:%M:%S}\n\nValid content\n",
                encoding="utf-8",
            )
        return path

    def test_future_directory_is_not_fresh(self):
        for distance in (timedelta(seconds=1), timedelta(days=1), timedelta(days=365)):
            with self.subTest(distance=distance):
                path = self.cache(str(distance.total_seconds()), NOW + distance)
                fresh, message = self.guide.check_freshness(path)
                self.assertFalse(fresh, message)

    def test_single_future_page_cannot_make_stale_collection_fresh(self):
        path = self.cache("mixed", NOW - timedelta(days=30))
        next(path.glob("*.md")).write_text(
            "# Guide\n**爬取时间**: 2099-01-01 00:00:00\n", encoding="utf-8"
        )
        self.assertFalse(self.guide.check_freshness(path)[0])

    def test_exact_present_and_recent_past_are_fresh(self):
        for days in (0, 1, self.guide.MAX_DAYS):
            with self.subTest(days=days):
                path = self.cache(f"past-{days}", NOW - timedelta(days=days))
                self.assertTrue(self.guide.check_freshness(path)[0])

    def test_stale_past_still_fails(self):
        path = self.cache("old", NOW - timedelta(days=self.guide.MAX_DAYS + 1))
        self.assertFalse(self.guide.check_freshness(path)[0])

    def test_future_static_directory_does_not_override_valid_user_cache(self):
        static = self.cache("static", NOW + timedelta(days=365))
        cache = self.cache("cache", NOW - timedelta(days=1))
        output = io.StringIO()
        with (
            mock.patch.object(self.guide, "STATIC_DIR", static),
            mock.patch.object(self.guide, "CACHE_DIR", cache),
            mock.patch.object(self.guide, "run_crawl") as crawl,
            contextlib.redirect_stdout(output),
        ):
            self.assertEqual(self.guide.main(), 0)
        self.assertEqual(output.getvalue().splitlines()[-1], str(cache))
        crawl.assert_not_called()


if __name__ == "__main__":
    unittest.main()
