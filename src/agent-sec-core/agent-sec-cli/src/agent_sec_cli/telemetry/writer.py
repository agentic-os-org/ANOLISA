"""Best-effort telemetry JSONL writer."""

import errno
import fcntl
import json
import logging
import os
import threading
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.telemetry.config import (
    get_telemetry_log_path,
    is_l1_telemetry_allowed,
)
from agent_sec_cli.telemetry.schema import (
    TelemetryContext,
    build_telemetry_security_event,
)

_logger = logging.getLogger("agent_sec_cli.telemetry.writer")
_writer: "TelemetryWriter | None" = None
_writer_lock = threading.Lock()


def _log_telemetry_write_failure(exc: Exception) -> None:
    """Best-effort diagnostic logging for swallowed telemetry write failures."""
    try:
        _logger.warning(
            "telemetry JSONL write failed",
            extra={
                "data": {
                    "error_type": type(exc).__name__,
                    "error": str(exc),
                }
            },
        )
    except Exception:  # noqa: BLE001
        pass


def _log_telemetry_write_skipped(
    record: Mapping[str, Any],
    *,
    reason: str,
    path: Path,
) -> None:
    """Best-effort diagnostic logging for intentionally skipped telemetry writes."""
    try:
        _logger.warning(
            "telemetry JSONL write skipped",
            extra={
                "data": {
                    "reason": reason,
                    "path": str(path),
                    "event_type": record.get("seccore.event_type"),
                    "category": record.get("seccore.category"),
                    "agent_name": record.get("component.agent_name"),
                }
            },
        )
    except Exception:  # noqa: BLE001
        pass


class TelemetryWriter:
    """Append telemetry records to an existing JSONL file.

    The Agentic OS component file is pre-created.
    This writer deliberately does not create directories, files, lock
    files, rotation backups, or temporary files.
    """

    def __init__(self, path: str | Path | None = None) -> None:
        self._path = (
            Path(path).expanduser() if path is not None else get_telemetry_log_path()
        )
        self._lock = threading.Lock()

    @property
    def path(self) -> Path:
        """Return the writer target path."""
        return self._path

    def exists(self) -> bool:
        """Return whether the target telemetry file currently exists."""
        return self._path.is_file()

    def write(self, record: Mapping[str, Any]) -> None:
        """Best-effort append of a telemetry record as one JSONL line."""
        try:
            payload = (json.dumps(record, ensure_ascii=False) + "\n").encode("utf-8")
        except Exception as exc:  # noqa: BLE001
            _log_telemetry_write_failure(exc)
            return

        if not self._lock.acquire(blocking=False):
            return
        try:
            self._append_line(payload)
        except FileNotFoundError:
            pass
        except BlockingIOError:
            _log_telemetry_write_skipped(
                record,
                reason="target_flock_busy",
                path=self._path,
            )
        except Exception as exc:  # noqa: BLE001
            _log_telemetry_write_failure(exc)
        finally:
            self._lock.release()

    def _append_line(self, payload: bytes) -> None:
        """Open, lock, append, unlock, then close the target path for this write."""
        flags = os.O_WRONLY | os.O_APPEND | getattr(os, "O_CLOEXEC", 0)
        fd = os.open(self._path, flags)
        lock_acquired = False
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as exc:
                if exc.errno in {errno.EACCES, errno.EAGAIN}:
                    raise BlockingIOError from exc
                raise
            lock_acquired = True
            self._write_all(fd, payload)
        finally:
            if lock_acquired:
                try:
                    fcntl.flock(fd, fcntl.LOCK_UN)
                except OSError:
                    pass
            os.close(fd)

    def _write_all(self, fd: int, payload: bytes) -> None:
        """Write the complete payload, handling short writes."""
        remaining = payload
        while remaining:
            written = os.write(fd, remaining)
            if written <= 0:
                raise OSError("os.write returned no bytes")
            remaining = remaining[written:]


def get_writer() -> TelemetryWriter:
    """Return the module-level telemetry writer singleton."""
    global _writer  # noqa: PLW0603
    if _writer is None:
        with _writer_lock:
            if _writer is None:
                _writer = TelemetryWriter()
    return _writer


def record_security_event_telemetry(
    event: SecurityEvent,
    ctx: TelemetryContext,
) -> None:
    """Best-effort write of telemetry mapped from a SecurityEvent."""
    try:
        if not is_l1_telemetry_allowed():
            return
        writer = get_writer()
        if not writer.exists():
            return
        writer.write(build_telemetry_security_event(event, ctx))
    except Exception as exc:  # noqa: BLE001
        _log_telemetry_write_failure(exc)
