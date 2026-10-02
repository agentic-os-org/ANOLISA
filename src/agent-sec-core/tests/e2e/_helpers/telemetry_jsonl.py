"""Shared JSONL helpers for e2e telemetry assertions."""

import errno
import json
import os
import time
from pathlib import Path

TELEMETRY_DISABLED_SENTINEL = "/etc/anolisa/.telemetry_disabled"
TELEMETRY_LOG_PATH_ENV = "AGENT_SEC_TELEMETRY_LOG_PATH"


def is_l1_telemetry_allowed() -> bool:
    """Return whether the host permits L1 telemetry for this black-box test."""
    try:
        os.lstat(TELEMETRY_DISABLED_SENTINEL)
    except OSError as exc:
        return exc.errno == errno.ENOENT
    return False


def telemetry_file_offset(path: Path) -> int:
    """Return the current byte offset for records appended after this point."""
    try:
        return path.stat().st_size
    except FileNotFoundError:
        return 0


def read_jsonl_payloads(path: Path, *, start_offset: int = 0) -> list[dict]:
    """Read JSONL payloads from *start_offset*, ignoring malformed lines."""
    if not path.exists():
        return []

    payloads = []
    with path.open("rb") as file_obj:
        file_obj.seek(start_offset)
        content = file_obj.read().decode("utf-8", errors="replace")
    for line in content.splitlines():
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(payload, dict):
            payloads.append(payload)
    return payloads


def wait_for_telemetry_record(
    path: Path,
    *,
    event_type: str,
    start_offset: int = 0,
    timeout_seconds: float = 5,
) -> dict:
    """Return an appended telemetry record matching *event_type*."""
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        for payload in read_jsonl_payloads(path, start_offset=start_offset):
            if payload.get("seccore.event_type") == event_type:
                return payload
        time.sleep(0.1)
    raise AssertionError(
        f"telemetry record not written for event_type={event_type!r}; "
        f"payloads={read_jsonl_payloads(path, start_offset=start_offset)!r}"
    )
