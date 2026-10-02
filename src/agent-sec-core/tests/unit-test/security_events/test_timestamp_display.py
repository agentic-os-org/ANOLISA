"""Unit tests for local-time display of stored UTC event timestamps."""

import os
import time

from agent_sec_cli.cli import _format_timestamp as format_cli_timestamp
from agent_sec_cli.security_events.summary_formatter import (
    _format_timestamp as format_summary_timestamp,
)


def _use_timezone(monkeypatch, timezone_name: str):
    old_tz = os.environ.get("TZ")
    monkeypatch.setenv("TZ", timezone_name)
    time.tzset()
    return old_tz


def _restore_timezone(monkeypatch, old_tz: str | None) -> None:
    if old_tz is None:
        monkeypatch.delenv("TZ", raising=False)
    else:
        monkeypatch.setenv("TZ", old_tz)
    time.tzset()


def test_cli_table_timestamp_uses_local_timezone(monkeypatch) -> None:
    old_tz = _use_timezone(monkeypatch, "Asia/Shanghai")
    try:
        assert (
            format_cli_timestamp("2026-05-09T06:01:07+00:00") == "2026-05-09 14:01:07"
        )
    finally:
        _restore_timezone(monkeypatch, old_tz)


def test_summary_timestamp_uses_local_timezone(monkeypatch) -> None:
    old_tz = _use_timezone(monkeypatch, "Asia/Shanghai")
    try:
        assert (
            format_summary_timestamp("2026-05-09T06:01:07+00:00")
            == "2026-05-09 14:01:07"
        )
    finally:
        _restore_timezone(monkeypatch, old_tz)


def test_naive_timestamp_is_treated_as_utc_for_display(monkeypatch) -> None:
    old_tz = _use_timezone(monkeypatch, "Asia/Shanghai")
    try:
        assert format_cli_timestamp("2026-05-09T06:01:07") == "2026-05-09 14:01:07"
    finally:
        _restore_timezone(monkeypatch, old_tz)
