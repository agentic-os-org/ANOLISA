#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Regression tests for naive and offset-aware documentation timestamps."""

import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

import check_docs


class FreshnessTimestampTests(unittest.TestCase):
    def check_stamps(self, stamps):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            for index in range(13):
                (folder / f"{index}.md").write_text(
                    "**爬取时间**: " + stamps[index % len(stamps)], encoding="utf-8")
            with patch.object(check_docs, "datetime", wraps=datetime) as clock:
                now = datetime(2026, 10, 2, 12, tzinfo=timezone.utc)
                clock.now.side_effect = lambda tz=None: (
                    now.astimezone(tz) if tz is not None
                    else now.astimezone().replace(tzinfo=None))
                return check_docs.check_freshness(folder)

    def test_recent_offset_timestamp(self):
        for stamp in ("2026-10-02T12:00:00+00:00", "2026-10-02T20:00:00+08:00"):
            with self.subTest(stamp=stamp):
                self.assertTrue(self.check_stamps([stamp])[0])

    def test_old_offset_timestamp(self):
        self.assertFalse(self.check_stamps(["2026-09-01T20:00:00+08:00"])[0])

    def test_mixed_legacy_and_offset_timestamps(self):
        legacy = datetime(2026, 9, 1, 12).strftime("%Y-%m-%d %H:%M:%S")
        self.assertTrue(self.check_stamps([legacy, "2026-10-02T12:00:00+00:00"])[0])

    def test_legacy_timestamps_keep_local_time_interpretation(self):
        recent = datetime(2026, 10, 2, 12, tzinfo=timezone.utc).astimezone()
        old = recent - timedelta(days=30)
        self.assertTrue(self.check_stamps([recent.strftime("%Y-%m-%d %H:%M:%S")])[0])
        self.assertFalse(self.check_stamps([old.strftime("%Y-%m-%d %H:%M:%S")])[0])


if __name__ == "__main__":
    unittest.main()
