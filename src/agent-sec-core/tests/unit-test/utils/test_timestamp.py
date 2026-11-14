"""Unit tests for shared timestamp helpers."""

import os
import time
from datetime import datetime, timezone

import pytest

from agent_sec_cli.utils.timestamp import (
    epoch_to_utc_iso,
    normalize_iso_to_utc_iso,
    ns_to_utc_iso,
    utc_iso_to_epoch,
)


def test_normalize_iso_to_utc_iso_converts_offset_timestamp() -> None:
    assert (
        normalize_iso_to_utc_iso("2026-05-20T12:00:00+08:00")
        == "2026-05-20T04:00:00+00:00"
    )


def test_normalize_iso_to_utc_iso_treats_naive_as_local(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    if not hasattr(time, "tzset"):
        pytest.skip("time.tzset is required to validate local timezone parsing")

    old_tz = os.environ.get("TZ")
    monkeypatch.setenv("TZ", "Asia/Shanghai")
    time.tzset()
    try:
        assert (
            normalize_iso_to_utc_iso("2026-05-20T12:00:00")
            == "2026-05-20T04:00:00+00:00"
        )
    finally:
        if old_tz is None:
            monkeypatch.delenv("TZ", raising=False)
        else:
            monkeypatch.setenv("TZ", old_tz)
        time.tzset()


def test_utc_iso_to_epoch_requires_utc_aware_input() -> None:
    epoch = datetime(2026, 5, 20, 4, 0, tzinfo=timezone.utc).timestamp()

    assert utc_iso_to_epoch("2026-05-20T04:00:00+00:00") == epoch
    with pytest.raises(ValueError, match="timezone information"):
        utc_iso_to_epoch("2026-05-20T12:00:00")
    with pytest.raises(ValueError, match="normalized to UTC"):
        utc_iso_to_epoch("2026-05-20T12:00:00+08:00")


def test_epoch_and_ns_helpers_emit_utc_iso() -> None:
    assert epoch_to_utc_iso(1_768_000_000.0).endswith("+00:00")
    assert ns_to_utc_iso(1_768_000_000_000_000_000).endswith("+00:00")


def test_boundary_offsets_raise_value_error_not_overflow() -> None:
    """Year-1 with a positive offset underflows UTC; must stay a ValueError.

    Request boundaries (e.g. daemon handlers) catch ValueError to answer
    with a client error; an escaping OverflowError became an internal
    error instead.
    """
    with pytest.raises(ValueError, match="out of range"):
        normalize_iso_to_utc_iso("0001-01-01T00:00:00+01:00")
    with pytest.raises(ValueError, match="out of range"):
        normalize_iso_to_utc_iso("9999-12-31T23:59:59-01:00")


def test_representable_boundary_neighbors_still_convert() -> None:
    # One step inside the edge in UTC terms: 0001-01-01T00:00Z exactly.
    assert normalize_iso_to_utc_iso("0001-01-01T01:00:00+01:00").endswith("+00:00")
    assert normalize_iso_to_utc_iso("0001-01-01T00:00:00+00:00").startswith(
        "0001-01-01"
    )
    assert normalize_iso_to_utc_iso("9999-12-31T23:59:59+00:00").startswith(
        "9999-12-31"
    )
