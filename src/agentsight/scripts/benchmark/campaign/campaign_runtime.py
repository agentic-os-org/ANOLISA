"""Capture per-run AgentSight process and runtime-log evidence."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

RUNTIME_ERROR_PATTERNS = {
    "panic": re.compile(r"\bpanicked at\b", re.IGNORECASE),
    "oom": re.compile(r"\b(?:out of memory|oom[-_ ]kill(?:er|ed)?)\b", re.IGNORECASE),
    "database_write": re.compile(
        r"\bdatabase\b.*\b(?:write|insert|commit)\b.*\b(?:error|failed)\b"
        r"|\bFailed to (?:store|insert|persist|complete) "
        r"(?:GenAI event in batch flush|analysis result|(?:deferred )?pending call|"
        r"(?:tool_failure )?interruption(?: event)?|Agent resource samples)\b"
        r"|\bFailed to (?:record (?:exit status|(?:OOM )?agent_crash)|"
        r"clear stale exit status|mark pending(?: calls as)? interrupted)\b"
        r"|\[DrainCheck\] FAIL (?:persist|update session_id)\b",
        re.IGNORECASE,
    ),
}


def log_position(path: Path | None) -> int | None:
    """Return the current log size when a readable runtime log is configured."""
    if path is None:
        return None
    try:
        return path.stat().st_size
    except OSError:
        return None


def capture_runtime_log(
    source: Path | None, start: int | None, destination: Path
) -> tuple[bool | None, list[str]]:
    """Copy the measured log segment and classify fatal runtime messages."""
    if source is None or start is None:
        return None, []
    try:
        with source.open("rb") as handle:
            if source.stat().st_size >= start:
                handle.seek(start)
            payload = handle.read()
    except OSError:
        return None, []
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(payload)
    text = payload.decode("utf-8", errors="replace")
    errors = [
        name for name, pattern in RUNTIME_ERROR_PATTERNS.items() if pattern.search(text)
    ]
    return not errors, errors


def process_metadata(pid: int) -> dict[str, Any]:
    """Snapshot the measured process identity and cgroup/limit context."""
    proc = Path(f"/proc/{pid}")
    try:
        command = (
            proc.joinpath("cmdline")
            .read_bytes()
            .replace(b"\0", b" ")
            .decode("utf-8", errors="replace")
            .strip()
        )
    except OSError:
        command = None
    values: dict[str, Any] = {"pid": pid, "command": command}
    for name in ("cgroup", "limits"):
        try:
            values[name] = (
                proc.joinpath(name)
                .read_text(encoding="utf-8", errors="replace")
                .strip()
            )
        except OSError:
            values[name] = None
    return values
