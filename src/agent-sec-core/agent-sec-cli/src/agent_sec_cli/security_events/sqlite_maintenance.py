"""Cross-process gate for low-frequency SQLite maintenance."""

import fcntl
import os
import time
from collections.abc import Callable
from pathlib import Path

DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS = 24 * 60 * 60


def run_sqlite_maintenance_if_due(
    db_path: str | Path,
    maintenance: Callable[[], None],
    *,
    interval_seconds: float = DEFAULT_SQLITE_MAINTENANCE_INTERVAL_SECONDS,
    now: float | None = None,
) -> bool:
    """Run maintenance once per DB path when its marker is older than interval."""
    path = Path(db_path)
    marker_path = _maintenance_marker_path(path)
    lock_path = _maintenance_lock_path(path)
    current_time = _current_time(now)

    if not _maintenance_due(marker_path, interval_seconds, current_time):
        return False

    lock_fd = _try_acquire_lock(lock_path)
    if lock_fd is None:
        return False

    try:
        current_time = _current_time(now)
        if not _maintenance_due(marker_path, interval_seconds, current_time):
            return False

        try:
            maintenance()
        except Exception:  # noqa: BLE001
            return False

        # Only a durable marker write advances the gate. If this fails, the
        # idempotent maintenance may run again on the next short-lived CLI exit.
        _mark_maintenance_complete(marker_path, current_time)
        return True
    except OSError:
        return False
    finally:
        _release_lock(lock_fd)


def _maintenance_marker_path(db_path: Path) -> Path:
    return Path(f"{db_path}.maintenance")


def _maintenance_lock_path(db_path: Path) -> Path:
    return Path(f"{db_path}.maintenance.lock")


def _current_time(now: float | None) -> float:
    if now is not None:
        return now
    return time.time()


def _maintenance_due(marker_path: Path, interval_seconds: float, now: float) -> bool:
    if interval_seconds <= 0:
        return True

    last_run = _read_last_maintenance(marker_path)
    if last_run is None or last_run > now:
        return True
    return now - last_run >= interval_seconds


def _read_last_maintenance(marker_path: Path) -> float | None:
    try:
        return float(marker_path.read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return None


def _mark_maintenance_complete(marker_path: Path, now: float) -> None:
    tmp_path = marker_path.with_name(f"{marker_path.name}.{os.getpid()}.tmp")
    tmp_path.write_text(f"{now:.6f}\n", encoding="utf-8")
    try:
        tmp_path.chmod(0o600)
    except OSError:
        pass
    tmp_path.replace(marker_path)


def _try_acquire_lock(lock_path: Path) -> int | None:
    try:
        # Keep the lock file on disk; flock state belongs to the open file
        # descriptor, and unlinking lock files can create cross-process races.
        fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_CLOEXEC, 0o600)
    except OSError:
        return None

    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        os.close(fd)
        return None
    return fd


def _release_lock(lock_fd: int) -> None:
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_UN)
    except OSError:
        pass
    os.close(lock_fd)
