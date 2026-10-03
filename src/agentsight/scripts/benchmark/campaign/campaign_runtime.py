"""Capture per-run AgentSight process and runtime-log evidence."""

from __future__ import annotations

import os
import re
from dataclasses import dataclass
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


@dataclass(frozen=True)
class LogPosition:
    """Identity and bounded pre-run tail used to verify log continuity."""

    offset: int
    device: int
    inode: int
    anchor: bytes


def log_position(path: Path | None) -> LogPosition | None:
    """Snapshot the opened runtime log, including a bounded continuity anchor."""
    if path is None:
        return None
    try:
        with path.open("rb") as handle:
            metadata = os.fstat(handle.fileno())
            handle.seek(max(0, metadata.st_size - 4096))
            anchor = handle.read(min(metadata.st_size, 4096))
            if len(anchor) != min(metadata.st_size, 4096):
                return None
            return LogPosition(
                metadata.st_size, metadata.st_dev, metadata.st_ino, anchor
            )
    except OSError:
        return None


def capture_runtime_log(
    source: Path | None, start: LogPosition | None, destination: Path
) -> tuple[bool | None, list[str]]:
    """Capture available diagnostics; only continuous evidence can certify clean."""
    if source is None or start is None:
        return None, []
    try:
        with source.open("rb") as handle:
            metadata = os.fstat(handle.fileno())
            continuous = (
                metadata.st_dev == start.device
                and metadata.st_ino == start.inode
                and metadata.st_size >= start.offset
            )
            if continuous:
                # Size and inode alone miss copytruncate followed by regrowth.
                handle.seek(start.offset - len(start.anchor))
                continuous = handle.read(len(start.anchor)) == start.anchor
            handle.seek(start.offset if continuous else 0)
            payload = handle.read()
            if continuous:
                handle.seek(start.offset - len(start.anchor))
                continuous = (
                    os.fstat(handle.fileno()).st_size >= start.offset
                    and handle.read(len(start.anchor)) == start.anchor
                )
                if not continuous:
                    handle.seek(0)
                    payload += b"\n" + handle.read()
            try:
                current = source.stat()
            except OSError:
                continuous = False
            else:
                if not os.path.samestat(metadata, current):
                    continuous = False
                    # Rotation can race with open/read. Preserve diagnostics
                    # from one replacement without chasing an unbounded stream.
                    try:
                        payload += b"\n" + source.read_bytes()
                    except OSError:
                        pass
    except OSError:
        return None, []
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(payload)
    text = payload.decode("utf-8", errors="replace")
    errors = [
        name for name, pattern in RUNTIME_ERROR_PATTERNS.items() if pattern.search(text)
    ]
    if errors:
        return False, errors
    # A replaced or truncated log may have lost failures from this interval.
    return (True if continuous else None), []


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
