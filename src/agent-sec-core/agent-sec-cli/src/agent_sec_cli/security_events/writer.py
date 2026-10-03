"""Thread-safe, rotation-aware JSONL writer for security events."""

import fcntl
import json
import logging
import os
import re
import shutil
import stat
import threading
from collections.abc import Callable, Mapping
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from agent_sec_cli.security_events.config import get_log_path
from agent_sec_cli.security_events.schema import SecurityEvent

_logger = logging.getLogger("agent_sec_cli.security_events.writer")


def _log_security_events_write_failure(exc: Exception) -> None:
    """Surface a security-events JSONL write failure via the diagnostic stream.

    Goes through the ``agent_sec_cli`` logger tree, which is routed to
    ``cli.jsonl`` by ``JsonlCliLogHandler``. The handler's own writer is
    constructed *without* an ``on_error`` callback, so any failure to record
    this warning cannot loop back into another security-events write.
    """
    try:
        _logger.warning(
            "security events JSONL write failed",
            extra={
                "data": {
                    "error_type": type(exc).__name__,
                    "error": str(exc),
                }
            },
        )
    except Exception:  # noqa: BLE001
        pass


# Default maximum log file size before rotation (100 MB)
DEFAULT_MAX_BYTES = 100 * 1024 * 1024
# Default number of rotated files to keep
DEFAULT_BACKUP_COUNT = 10

# Local security-event, observability, and diagnostic streams can contain
# request/result evidence. Keep both data files and their advisory lock files
# owner-only, independently of the caller's umask.
_PRIVATE_FILE_MODE = 0o600

# Matches the timestamp suffix produced by _rotate():
#   YYYYMMDD-HHMMSS.fff          (normal)
#   YYYYMMDD-HHMMSS.fff.N        (collision-guard counter)
_BACKUP_SUFFIX_RE = re.compile(r"^\d{8}-\d{6}\.\d{3}(\.\d+)?$")


class JsonlEventWriter:
    """Append JSON-serializable records to a JSONL file.

    * **Thread-safe** — every ``write()`` is guarded by a ``threading.Lock``.
    * **Auto-rotation** — automatically rotates the log file when it exceeds
      ``max_bytes`` (default: 100 MB), keeping up to ``backup_count`` backup
      files (default: 10).
    * **Cross-process safe** — a dedicated advisory lock file serialises
      rotation *and* the subsequent write so that no two processes race.
      Inside the critical section the log file is opened **fresh by path**,
      which eliminates inode-reuse races.
    * **Fire-and-forget** — all internal errors are swallowed so that logging
      never disrupts the caller.
    """

    def __init__(
        self,
        path: str | Path,
        max_bytes: int = DEFAULT_MAX_BYTES,
        backup_count: int = DEFAULT_BACKUP_COUNT,
        *,
        error_prefix: str = "[security_events]",
        on_error: Callable[[Exception], None] | None = None,
    ) -> None:
        self._path: Path = Path(path).expanduser()
        self._max_bytes = max_bytes
        self._backup_count = backup_count
        self._error_prefix = error_prefix
        self._on_error = on_error
        self._lock = threading.Lock()
        self._dir_created = False
        self._retained_backups_secured = False

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _notify_error(self, exc: Exception) -> None:
        """Best-effort diagnostic callback for swallowed writer failures."""
        if self._on_error is None:
            return
        try:
            self._on_error(exc)
        except Exception:  # noqa: BLE001
            pass

    def _ensure_parent_dir(self) -> None:
        if self._dir_created:
            return
        self._path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        self._dir_created = True

    @staticmethod
    def _open_private_append_fd(path: Path) -> int:
        """Open a file for append and enforce mode ``0o600``.

        The creation mode prevents a new file from starting with broader
        permissions. ``fchmod`` tightens files created by older releases and
        restores owner access when the process umask is unusually restrictive.
        """
        fd = os.open(
            path,
            os.O_WRONLY | os.O_CREAT | os.O_APPEND,
            _PRIVATE_FILE_MODE,
        )
        try:
            metadata = os.fstat(fd)
            if stat.S_IMODE(metadata.st_mode) != _PRIVATE_FILE_MODE:
                os.fchmod(fd, _PRIVATE_FILE_MODE)
            return fd
        except BaseException:
            os.close(fd)
            raise

    def _needs_rotation(self, fd: int, additional_bytes: int = 0) -> bool:
        """Return whether appending to the opened log would cross its size limit."""
        return os.fstat(fd).st_size + additional_bytes >= self._max_bytes

    def _tighten_retained_backup(self, path: Path) -> None:
        """Best-effort tighten one recognized retained backup without following links."""
        try:
            if not stat.S_ISREG(path.lstat().st_mode):
                return

            fd = os.open(
                path,
                os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
            )
            try:
                metadata = os.fstat(fd)
                if not stat.S_ISREG(metadata.st_mode):
                    return
                if stat.S_IMODE(metadata.st_mode) != _PRIVATE_FILE_MODE:
                    os.fchmod(fd, _PRIVATE_FILE_MODE)
            finally:
                os.close(fd)
        except OSError as exc:
            self._notify_error(exc)

    def _secure_retained_backups_once(self) -> None:
        """Tighten backups left by older releases on this writer's first write."""
        if self._retained_backups_secured:
            return

        try:
            prefix = f"{self._path.name}."
            for entry in self._path.parent.iterdir():
                if not entry.name.startswith(prefix):
                    continue
                suffix = entry.name[len(prefix) :]
                if _BACKUP_SUFFIX_RE.match(suffix):
                    self._tighten_retained_backup(entry)
        except OSError as exc:
            self._notify_error(exc)
        finally:
            self._retained_backups_secured = True

    def _rotate(self) -> None:
        """Rotate the log file by renaming it with a timestamp suffix.

        Rotation scheme:
            security-events.jsonl                           -> current (will be rotated)
            security-events.jsonl.20260408-143022.123       -> rotated at 2026-04-08 14:30:22.123
            security-events.jsonl.20260408-120515.456       -> rotated at 2026-04-08 12:05:15.456

        After rotation, old backups exceeding ``backup_count`` are cleaned up.
        """
        # Generate timestamp-based backup filename with millisecond precision
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S.%f")[:-3]
        backup_path = self._path.parent / f"{self._path.name}.{timestamp}"

        # Guard against timestamp collisions: if the backup already exists,
        # append a counter to disambiguate.
        if backup_path.exists():
            for seq in range(1, 1000):
                candidate = self._path.parent / f"{self._path.name}.{timestamp}.{seq}"
                if not candidate.exists():
                    backup_path = candidate
                    break

        # Rotate current file to timestamp-named backup
        try:
            shutil.move(self._path, backup_path)
        except OSError as exc:
            self._notify_error(exc)
            return

        # Clean up old backups exceeding backup_count
        self._cleanup_old_backups()

    def _write_under_flock(self, line: str, line_bytes: int) -> None:
        """Acquire a cross-process flock, then rotate-if-needed + write.

        Following the "dedicated lock file" pattern, the flock serialises the
        **entire** write-with-potential-rotation sequence across OS processes.
        Inside the critical section the log file is opened **fresh by path**
        (not via a persistent fd), which eliminates inode-reuse races:
        no stale fd can reference a recycled inode because the fd is created
        and destroyed within a single lock acquisition.
        """
        lock_path = self._path.parent / (self._path.name + ".lock")
        lock_fd: int | None = None
        lock_acquired = False
        try:
            self._ensure_parent_dir()
            lock_fd = self._open_private_append_fd(lock_path)
            fcntl.flock(lock_fd, fcntl.LOCK_EX)
            lock_acquired = True
        except OSError:
            # open() or flock() failed — close the fd immediately if it
            # was opened, then fall through without flock protection.
            # Best-effort: still write, accept small race.
            if lock_fd is not None:
                try:
                    os.close(lock_fd)
                except OSError:
                    pass
                lock_fd = None

        try:
            self._secure_retained_backups_once()
            event_fd: int | None = self._open_private_append_fd(self._path)
            try:
                # Tighten an existing file before checking rotation so the
                # resulting backup cannot retain a legacy group/world-readable
                # mode. Reopen by path after rotation to avoid stale inodes.
                if self._needs_rotation(event_fd, line_bytes):
                    os.close(event_fd)
                    event_fd = None
                    self._rotate()
                    event_fd = self._open_private_append_fd(self._path)

                fh = os.fdopen(event_fd, "a", encoding="utf-8")
                event_fd = None
                with fh:
                    fh.write(line)
                    fh.flush()
            finally:
                if event_fd is not None:
                    os.close(event_fd)
        finally:
            if lock_fd is not None:
                try:
                    if lock_acquired:
                        fcntl.flock(lock_fd, fcntl.LOCK_UN)
                    os.close(lock_fd)
                except OSError:
                    pass

    def _cleanup_old_backups(self) -> None:
        """Remove oldest backup files if count exceeds backup_count.

        Backups are identified by the timestamp suffix pattern and sorted
        by modification time to determine which are oldest.
        """
        try:
            # Find all backup files matching the exact rotation pattern
            dir_path = self._path.parent
            base_name = self._path.name
            prefix = f"{base_name}."

            backup_files = []
            for entry in dir_path.iterdir():
                if not entry.name.startswith(prefix):
                    continue
                suffix = entry.name[len(prefix) :]
                if _BACKUP_SUFFIX_RE.match(suffix) and entry.is_file():
                    mtime = entry.stat().st_mtime
                    backup_files.append((entry, mtime))

            # Sort by modification time (oldest first)
            backup_files.sort(key=lambda x: x[1])

            # Remove oldest files if we exceed backup_count
            while len(backup_files) > self._backup_count:
                oldest_path, _ = backup_files.pop(0)
                try:
                    oldest_path.unlink()
                except OSError as exc:
                    self._notify_error(exc)
                    pass
        except OSError as exc:
            self._notify_error(exc)
            pass

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def _append_record(self, record: Mapping[str, Any]) -> None:
        line = json.dumps(record, ensure_ascii=False) + "\n"
        line_bytes = len(line.encode("utf-8"))
        self._write_under_flock(line, line_bytes)

    def write(self, record: Mapping[str, Any]) -> None:
        """Serialize *record* and append it as a single JSONL line.

        This method is safe to call from any thread and will never raise.
        Failures are forwarded to the ``on_error`` callback when configured;
        the callback itself is wrapped to ensure it never re-raises.
        """
        with self._lock:
            try:
                self._append_record(record)
            except Exception as exc:  # noqa: BLE001
                self._notify_error(exc)

    def write_or_raise(self, record: Mapping[str, Any]) -> None:
        """Serialize *record* and append it as a single JSONL line.

        Unlike ``write()``, this method surfaces serialization and persistence
        failures to callers that need a reliable ingestion contract.
        """
        with self._lock:
            self._append_record(record)


class SecurityEventWriter(JsonlEventWriter):
    """Append ``SecurityEvent`` records to the security-events JSONL file."""

    def __init__(
        self,
        path: str | Path | None = None,
        max_bytes: int = DEFAULT_MAX_BYTES,
        backup_count: int = DEFAULT_BACKUP_COUNT,
    ) -> None:
        super().__init__(
            path=path or get_log_path(),
            max_bytes=max_bytes,
            backup_count=backup_count,
            error_prefix="[security_events]",
            on_error=_log_security_events_write_failure,
        )

    def write(self, record: SecurityEvent | Mapping[str, Any]) -> None:
        """Serialize *record* and append it as a single JSONL line."""
        if isinstance(record, SecurityEvent):
            super().write(record.to_dict())
            return
        super().write(record)


__all__ = [
    "DEFAULT_BACKUP_COUNT",
    "DEFAULT_MAX_BYTES",
    "JsonlEventWriter",
    "SecurityEventWriter",
]
