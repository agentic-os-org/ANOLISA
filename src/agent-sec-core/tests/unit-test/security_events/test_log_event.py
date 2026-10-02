"""Unit tests for security_events — module-level log_event() and get_writer()."""

import json
import subprocess
import sys
import unittest
from unittest.mock import MagicMock, patch

import agent_sec_cli.security_events as security_events
from agent_sec_cli.security_events import log_event
from agent_sec_cli.security_events.schema import SecurityEvent


def test_security_events_package_import_does_not_load_sqlalchemy():
    probe = """
import json
import sys

import agent_sec_cli.security_events  # noqa: F401

heavy_modules = [
    "agent_sec_cli.security_events.sqlite_reader",
    "agent_sec_cli.security_events.sqlite_writer",
    "agent_sec_cli.security_events.orm_store",
    "sqlalchemy",
]
print(json.dumps([name for name in heavy_modules if name in sys.modules]))
"""

    result = subprocess.run(
        [sys.executable, "-c", probe],
        text=True,
        capture_output=True,
        check=True,
    )

    assert json.loads(result.stdout) == []


class TestGetWriter(unittest.TestCase):
    def test_singleton_returns_same_instance(self):
        w1 = security_events.get_writer()
        w2 = security_events.get_writer()
        self.assertIs(w1, w2)


class TestLogEvent(unittest.TestCase):
    @patch("agent_sec_cli.security_events.get_writer")
    def test_log_event_delegates_to_writer(self, mock_get_writer):
        mock_writer = MagicMock()
        mock_get_writer.return_value = mock_writer

        evt = SecurityEvent(event_type="t", category="c", details={})
        log_event(evt)

        mock_writer.write.assert_called_once_with(evt)

    @patch("agent_sec_cli.security_events.get_writer")
    def test_log_event_swallows_exceptions(self, mock_get_writer):
        mock_writer = MagicMock()
        mock_writer.write.side_effect = RuntimeError("disk full")
        mock_get_writer.return_value = mock_writer

        evt = SecurityEvent(event_type="t", category="c", details={})
        # Should not raise
        log_event(evt)


class TestGetSqliteWriter(unittest.TestCase):
    def test_singleton_returns_same_instance(self):
        w1 = security_events.get_sqlite_writer()
        w2 = security_events.get_sqlite_writer()
        self.assertIs(w1, w2)


class TestDualWrite(unittest.TestCase):
    @patch("agent_sec_cli.security_events.get_sqlite_writer")
    @patch("agent_sec_cli.security_events.get_writer")
    def test_log_event_writes_to_both(self, mock_get_writer, mock_get_sqlite_writer):
        mock_jsonl = MagicMock()
        mock_sqlite = MagicMock()
        mock_get_writer.return_value = mock_jsonl
        mock_get_sqlite_writer.return_value = mock_sqlite

        evt = SecurityEvent(event_type="t", category="c", details={})
        log_event(evt)
        mock_jsonl.write.assert_called_once_with(evt)
        mock_sqlite.write.assert_called_once_with(evt)

    @patch("agent_sec_cli.security_events.get_sqlite_writer")
    @patch("agent_sec_cli.security_events.get_writer")
    def test_jsonl_failure_does_not_block_sqlite(
        self, mock_get_writer, mock_get_sqlite_writer
    ):
        mock_jsonl = MagicMock()
        mock_jsonl.write.side_effect = RuntimeError("disk full")
        mock_sqlite = MagicMock()
        mock_get_writer.return_value = mock_jsonl
        mock_get_sqlite_writer.return_value = mock_sqlite

        evt = SecurityEvent(event_type="t", category="c", details={})
        log_event(evt)
        # SQLite write should still be called even though JSONL failed
        mock_sqlite.write.assert_called_once_with(evt)

    @patch("agent_sec_cli.security_events.get_sqlite_writer")
    @patch("agent_sec_cli.security_events.get_writer")
    def test_sqlite_failure_does_not_block_jsonl(
        self, mock_get_writer, mock_get_sqlite_writer
    ):
        mock_jsonl = MagicMock()
        mock_sqlite = MagicMock()
        mock_sqlite.write.side_effect = RuntimeError("corruption")
        mock_get_writer.return_value = mock_jsonl
        mock_get_sqlite_writer.return_value = mock_sqlite

        evt = SecurityEvent(event_type="t", category="c", details={})
        log_event(evt)
        mock_jsonl.write.assert_called_once_with(evt)


if __name__ == "__main__":
    unittest.main()
