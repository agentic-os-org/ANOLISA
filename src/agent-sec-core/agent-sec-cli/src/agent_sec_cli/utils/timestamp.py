"""Timestamp normalization helpers used at CLI/API boundaries."""

from datetime import datetime, timedelta, timezone
from typing import Literal

NaiveTimestampPolicy = Literal["local", "utc", "reject"]


def normalize_iso_to_utc_iso(
    value: str,
    *,
    field_name: str = "timestamp",
    naive: NaiveTimestampPolicy = "local",
) -> str:
    """Parse an ISO timestamp and return a UTC-aware ISO timestamp.

    Naive timestamps are interpreted according to *naive*. User-facing entry
    points use ``local`` so ``2026-01-01T09:00:00`` means local wall-clock time.
    Repository-facing checks use ``reject`` to keep storage/query layers from
    silently choosing a timezone.
    """
    return datetime_to_utc_iso(
        parse_iso_datetime(value, field_name=field_name, naive=naive),
        field_name=field_name,
        naive="reject",
    )


def optional_iso_to_utc_iso(
    value: str | None,
    *,
    field_name: str,
    naive: NaiveTimestampPolicy = "local",
) -> str | None:
    """Normalize an optional ISO timestamp to UTC."""
    if value is None:
        return None
    return normalize_iso_to_utc_iso(value, field_name=field_name, naive=naive)


def parse_iso_datetime(
    value: str,
    *,
    field_name: str = "timestamp",
    naive: NaiveTimestampPolicy = "local",
) -> datetime:
    """Parse an ISO-8601 timestamp and apply the requested naive-time policy."""
    try:
        parsed = datetime.fromisoformat(_normalize_z_suffix(value))
    except ValueError as exc:
        raise ValueError(
            f"Invalid time format for {field_name}: {value!r}. "
            "Expected ISO 8601 format."
        ) from exc
    return ensure_aware_datetime(parsed, field_name=field_name, naive=naive)


def ensure_aware_datetime(
    value: datetime,
    *,
    field_name: str = "timestamp",
    naive: NaiveTimestampPolicy = "local",
) -> datetime:
    """Return an aware datetime using the configured policy for naive values."""
    if value.tzinfo is not None and value.tzinfo.utcoffset(value) is not None:
        return value
    if naive == "local":
        # For user input, a naive timestamp is local wall-clock time.
        return value.astimezone()
    if naive == "utc":
        return value.replace(tzinfo=timezone.utc)
    raise ValueError(f"{field_name} must include timezone information.")


def datetime_to_utc_iso(
    value: datetime,
    *,
    field_name: str = "timestamp",
    naive: NaiveTimestampPolicy = "local",
) -> str:
    """Convert a datetime to a UTC-aware ISO timestamp."""
    return (
        ensure_aware_datetime(
            value,
            field_name=field_name,
            naive=naive,
        )
        .astimezone(timezone.utc)
        .isoformat()
    )


def epoch_to_utc_iso(epoch: float) -> str:
    """Convert epoch seconds to a UTC-aware ISO timestamp."""
    return datetime.fromtimestamp(epoch, tz=timezone.utc).isoformat()


def ns_to_utc_iso(value: int) -> str:
    """Convert nanoseconds since epoch to a UTC-aware ISO timestamp.

    Split into whole seconds and the sub-second remainder with integer
    arithmetic before building the datetime.  Dividing by 1e9 as a float
    loses precision for current-epoch values: e.g. 1700000000999999999 ns
    rounds up to 1700000001.0 s and lands on the next second.  Nanoseconds
    below one microsecond cannot be represented by ``datetime`` and are
    truncated (floor), which keeps the result on the same second.
    """
    seconds, nanos = divmod(value, 1_000_000_000)
    return (
        datetime.fromtimestamp(seconds, tz=timezone.utc)
        + timedelta(microseconds=nanos // 1000)
    ).isoformat()


def utc_iso_to_epoch(value: str, *, field_name: str = "timestamp") -> float:
    """Convert a UTC-aware ISO timestamp to epoch seconds.

    This is intentionally stricter than user-facing parsing: repository callers
    should normalize local/offset timestamps before crossing the repository
    boundary.
    """
    parsed = parse_iso_datetime(value, field_name=field_name, naive="reject")
    # Repository-facing timestamps must already be normalized to UTC.
    if parsed.utcoffset() != timedelta(0):
        raise ValueError(f"{field_name} must be normalized to UTC.")
    return parsed.timestamp()


def _normalize_z_suffix(value: str) -> str:
    if value.endswith("Z"):
        return f"{value[:-1]}+00:00"
    return value
