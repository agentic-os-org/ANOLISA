"""Unit tests for security_middleware.backends.summary — SummaryBackend."""

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.sqlite_reader import SqliteEventReader
from agent_sec_cli.security_events.sqlite_writer import SqliteEventWriter
from agent_sec_cli.security_middleware.backends.summary import SummaryBackend
from agent_sec_cli.security_middleware.context import RequestContext


def _make_event(event_type="test_event", category="sandbox", **kwargs):
    return SecurityEvent(
        event_type=event_type,
        category=category,
        details=kwargs.get("details", {"key": "value"}),
    )


class TestSummaryBackend(unittest.TestCase):
    def setUp(self):
        self.tmp_dir = tempfile.mkdtemp()
        self.db_path = str(Path(self.tmp_dir) / "test.db")
        self.writer = SqliteEventWriter(path=self.db_path)
        self.reader = SqliteEventReader(path=self.db_path)

    def tearDown(self):
        self.writer.close()

    @patch("agent_sec_cli.security_middleware.backends.summary.get_reader")
    def test_summary_with_events(self, mock_get_reader):
        # Write test events
        for i in range(5):
            self.writer.write(_make_event(category="sandbox"))
        for i in range(3):
            self.writer.write(
                _make_event(category="hardening", event_type="hardening_scan")
            )

        mock_get_reader.return_value = self.reader

        backend = SummaryBackend()
        ctx = RequestContext(action="summary")
        result = backend.execute(ctx, hours=24)

        self.assertTrue(result.success)
        self.assertEqual(result.data["total_events"], 8)
        self.assertIn("sandbox", result.data["by_category"])
        self.assertEqual(result.data["by_category"]["sandbox"], 5)
        self.assertEqual(result.data["by_category"]["hardening"], 3)
        self.assertIn("Total events: 8", result.stdout)

    @patch("agent_sec_cli.security_middleware.backends.summary.get_reader")
    def test_summary_breakdowns_respect_filters(self, mock_get_reader):
        self.writer.write(_make_event(category="sandbox", event_type="alpha"))
        self.writer.write(_make_event(category="sandbox", event_type="alpha"))
        self.writer.write(_make_event(category="hardening", event_type="alpha"))
        self.writer.write(_make_event(category="sandbox", event_type="beta"))

        mock_get_reader.return_value = self.reader

        backend = SummaryBackend()
        ctx = RequestContext(action="summary")
        result = backend.execute(ctx, hours=24, category="sandbox", event_type="alpha")

        self.assertTrue(result.success)
        self.assertEqual(result.data["total_events"], 2)
        self.assertEqual(result.data["by_category"], {"sandbox": 2})
        self.assertEqual(result.data["by_event_type"], {"alpha": 2})

    @patch("agent_sec_cli.security_middleware.backends.summary.get_reader")
    def test_summary_empty_db(self, mock_get_reader):
        # Non-existent DB path
        reader = SqliteEventReader(path=str(Path(self.tmp_dir) / "nonexistent.db"))
        mock_get_reader.return_value = reader

        backend = SummaryBackend()
        ctx = RequestContext(action="summary")
        result = backend.execute(ctx)

        self.assertTrue(result.success)
        self.assertEqual(result.data["total_events"], 0)
        self.assertEqual(result.data["by_category"], {})
        self.assertEqual(result.data["by_event_type"], {})


if __name__ == "__main__":
    unittest.main()
