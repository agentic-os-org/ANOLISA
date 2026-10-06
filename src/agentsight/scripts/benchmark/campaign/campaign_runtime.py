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
    """Bounded identity and boundary evidence for an append-only log interval."""

    device: int
    inode: int
    size: int
    tail: bytes


def log_position(path: Path | None) -> LogPosition | None:
    """Snapshot the opened log identity, size, and at most 64 boundary bytes."""
    if path is None:
        return None
    try:
        with path.open("rb") as handle:
            stat = os.fstat(handle.fileno())
            offset = max(0, stat.st_size - 64)
            handle.seek(offset)
            tail = handle.read(stat.st_size - offset)
            if len(tail) != stat.st_size - offset:
                return None
            return LogPosition(stat.st_dev, stat.st_ino, stat.st_size, tail)
    except OSError:
        return None


def capture_runtime_log(
    source: Path | None, start: LogPosition | int | None, destination: Path
) -> tuple[bool | None, list[str]]:
    """Copy observed messages; lost log intervals cannot prove a clean run."""
    if source is None or start is None:
        return None, []
    complete = True
    try:
        with source.open("rb") as handle:
            stat = os.fstat(handle.fileno())
            if isinstance(start, LogPosition):
                complete = (stat.st_dev, stat.st_ino) == (
                    start.device,
                    start.inode,
                ) and stat.st_size >= start.size
                if complete:
                    handle.seek(start.size - len(start.tail))
                    complete = handle.read(len(start.tail)) == start.tail
                handle.seek(start.size if complete else 0)
            elif stat.st_size >= start:
                handle.seek(start)
            else:
                complete = False
            payload = handle.read()
            if complete:
                after = os.fstat(handle.fileno())
                boundary = start.size if isinstance(start, LogPosition) else start
                complete = after.st_size >= max(boundary, handle.tell())
                if complete and isinstance(start, LogPosition):
                    handle.seek(start.size - len(start.tail))
                    complete = handle.read(len(start.tail)) == start.tail
                try:
                    current = source.stat()
                    complete = complete and (current.st_dev, current.st_ino) == (
                        stat.st_dev,
                        stat.st_ino,
                    )
                except OSError:
                    complete = False
    except OSError:
        return None, []
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(payload)
    text = payload.decode("utf-8", errors="replace")
    errors = [
        name for name, pattern in RUNTIME_ERROR_PATTERNS.items() if pattern.search(text)
    ]
    return (False if errors else True if complete else None), errors


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
