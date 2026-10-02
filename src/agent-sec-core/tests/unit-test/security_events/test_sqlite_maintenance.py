"""Unit tests for cross-process SQLite maintenance gating."""

import fcntl
import os
from pathlib import Path

from agent_sec_cli.security_events.sqlite_maintenance import (
    run_sqlite_maintenance_if_due,
)


def test_sqlite_maintenance_runs_once_per_interval(tmp_path: Path) -> None:
    db_path = tmp_path / "events.db"
    calls: list[str] = []

    def maintenance() -> None:
        calls.append("run")

    assert run_sqlite_maintenance_if_due(
        db_path,
        maintenance,
        interval_seconds=10,
        now=100.0,
    )
    assert not run_sqlite_maintenance_if_due(
        db_path,
        maintenance,
        interval_seconds=10,
        now=109.0,
    )
    assert run_sqlite_maintenance_if_due(
        db_path,
        maintenance,
        interval_seconds=10,
        now=110.0,
    )

    assert calls == ["run", "run"]
    marker_path = Path(f"{db_path}.maintenance")
    assert float(marker_path.read_text(encoding="utf-8").strip()) == 110.0


def test_sqlite_maintenance_skips_when_another_process_holds_lock(
    tmp_path: Path,
) -> None:
    db_path = tmp_path / "events.db"
    lock_path = Path(f"{db_path}.maintenance.lock")
    calls: list[str] = []

    def maintenance() -> None:
        calls.append("run")

    fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)

        assert not run_sqlite_maintenance_if_due(
            db_path,
            maintenance,
            interval_seconds=10,
            now=100.0,
        )
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)

    assert calls == []
    assert not Path(f"{db_path}.maintenance").exists()


def test_sqlite_maintenance_retries_after_callback_failure(tmp_path: Path) -> None:
    db_path = tmp_path / "events.db"
    calls: list[str] = []

    def failing_maintenance() -> None:
        calls.append("fail")
        raise RuntimeError("maintenance failed")

    assert not run_sqlite_maintenance_if_due(
        db_path,
        failing_maintenance,
        interval_seconds=10,
        now=100.0,
    )

    assert calls == ["fail"]
    assert not Path(f"{db_path}.maintenance").exists()


def test_sqlite_maintenance_treats_invalid_marker_as_due(tmp_path: Path) -> None:
    db_path = tmp_path / "events.db"
    marker_path = Path(f"{db_path}.maintenance")
    marker_path.write_text("not-a-timestamp\n", encoding="utf-8")
    calls: list[str] = []

    def maintenance() -> None:
        calls.append("run")

    assert run_sqlite_maintenance_if_due(
        db_path,
        maintenance,
        interval_seconds=10,
        now=100.0,
    )

    assert calls == ["run"]
    assert float(marker_path.read_text(encoding="utf-8").strip()) == 100.0
