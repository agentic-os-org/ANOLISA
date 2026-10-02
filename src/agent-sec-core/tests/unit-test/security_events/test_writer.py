"""Unit tests for security_events.writer."""

import json
import multiprocessing
import os
import re
import stat
import threading
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

import pytest
from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.writer import (
    JsonlEventWriter,
    SecurityEventWriter,
)


def _make_event(**overrides: Any) -> SecurityEvent:
    defaults: dict[str, Any] = {
        "event_type": "test",
        "category": "test_cat",
        "details": {"k": "v"},
    }
    defaults.update(overrides)
    return SecurityEvent(**defaults)


def _backup_files(log_path: Path) -> list[Path]:
    prefix = f"{log_path.name}."
    return sorted(
        path
        for path in log_path.parent.iterdir()
        if path.name.startswith(prefix)
        and not path.name.endswith(".lock")
        and path.is_file()
    )


def _all_event_files(log_path: Path) -> list[Path]:
    return [log_path, *_backup_files(log_path)]


def _read_event_lines(path: Path) -> list[str]:
    if not path.exists():
        return []
    return path.read_text(encoding="utf-8").splitlines()


def _event_timestamps(path: Path) -> list[str]:
    timestamps = []
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                timestamps.append(json.loads(line)["timestamp"])
    return timestamps


def _max_event_timestamp(path: Path) -> str | None:
    timestamps = _event_timestamps(path)
    return max(timestamps) if timestamps else None


def _min_event_timestamp(path: Path) -> str | None:
    timestamps = _event_timestamps(path)
    return min(timestamps) if timestamps else None


class TestWriterBasic:
    def test_write_appends_security_event_jsonl_line(self, tmp_path: Path) -> None:
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path)

        writer.write(_make_event())

        lines = _read_event_lines(path)
        assert len(lines) == 1
        assert json.loads(lines[0])["event_type"] == "test"

    def test_write_appends_multiple_security_events(self, tmp_path: Path) -> None:
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path)

        for i in range(3):
            writer.write(_make_event(event_type=f"evt_{i}"))

        lines = _read_event_lines(path)
        assert len(lines) == 3
        for i, line in enumerate(lines):
            assert json.loads(line)["event_type"] == f"evt_{i}"

    def test_write_keeps_generic_jsonl_writer_contract(self, tmp_path: Path) -> None:
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path)

        writer.write({"event_type": "raw", "category": "test", "details": {"k": "v"}})

        lines = _read_event_lines(path)
        assert len(lines) == 1
        assert json.loads(lines[0])["event_type"] == "raw"


class TestJsonlEventWriter:
    def test_generic_writer_appends_json_serializable_records(
        self, tmp_path: Path
    ) -> None:
        path = tmp_path / "observability.jsonl"
        writer = JsonlEventWriter(path=path)

        writer.write({"hook": "before_tool_call", "metrics": {"tool_name": "exec"}})

        lines = _read_event_lines(path)
        assert len(lines) == 1
        assert json.loads(lines[0])["hook"] == "before_tool_call"

    def test_parent_directory_created_only_once_per_writer(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        path = tmp_path / "nested" / "observability.jsonl"
        original_mkdir = Path.mkdir
        mkdir_calls: list[Path] = []

        def count_mkdir(self: Path, *args: Any, **kwargs: Any) -> None:
            mkdir_calls.append(self)
            original_mkdir(self, *args, **kwargs)

        monkeypatch.setattr(Path, "mkdir", count_mkdir)
        writer = JsonlEventWriter(path=path)

        writer.write({"seq": 1})
        writer.write({"seq": 2})

        assert mkdir_calls == [path.parent]

    def test_streams_use_independent_lock_files(self, tmp_path: Path) -> None:
        security_path = tmp_path / "security-events.jsonl"
        observability_path = tmp_path / "observability.jsonl"

        JsonlEventWriter(path=security_path).write({"stream": "security"})
        JsonlEventWriter(path=observability_path).write({"stream": "observability"})

        security_lock = Path(f"{security_path}.lock")
        observability_lock = Path(f"{observability_path}.lock")
        assert security_lock.exists()
        assert observability_lock.exists()
        assert security_lock.resolve() != observability_lock.resolve()

    def test_data_and_lock_files_are_created_owner_only(self, tmp_path: Path) -> None:
        path = tmp_path / "security-events.jsonl"

        JsonlEventWriter(path=path).write({"stream": "security"})

        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        assert stat.S_IMODE(Path(f"{path}.lock").stat().st_mode) == 0o600

    @pytest.mark.parametrize("process_umask", [0o000, 0o777])
    def test_owner_only_mode_does_not_depend_on_process_umask(
        self, tmp_path: Path, process_umask: int
    ) -> None:
        path = tmp_path / "security-events.jsonl"
        previous_umask = os.umask(process_umask)
        try:
            JsonlEventWriter(path=path).write_or_raise({"stream": "security"})
        finally:
            os.umask(previous_umask)

        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        assert stat.S_IMODE(Path(f"{path}.lock").stat().st_mode) == 0o600

    def test_existing_owned_data_and_lock_files_are_tightened(
        self, tmp_path: Path
    ) -> None:
        path = tmp_path / "security-events.jsonl"
        lock_path = Path(f"{path}.lock")
        path.write_text("", encoding="utf-8")
        lock_path.write_text("", encoding="utf-8")
        path.chmod(0o644)
        lock_path.chmod(0o666)

        JsonlEventWriter(path=path).write({"stream": "security"})

        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        assert stat.S_IMODE(lock_path.stat().st_mode) == 0o600

    def test_existing_retained_backups_are_tightened_without_rotation(
        self, tmp_path: Path
    ) -> None:
        path = tmp_path / "security-events.jsonl"
        backups = [
            tmp_path / "security-events.jsonl.20260101-120000.100",
            tmp_path / "security-events.jsonl.20260101-120000.100.1",
        ]
        for backup in backups:
            backup.write_text("legacy\n", encoding="utf-8")
            backup.chmod(0o644)

        JsonlEventWriter(path=path, max_bytes=10_000).write_or_raise(
            {"stream": "security"}
        )

        assert all(stat.S_IMODE(backup.stat().st_mode) == 0o600 for backup in backups)
        assert len(_backup_files(path)) == 2

    def test_retained_backup_migration_does_not_follow_symlinks(
        self, tmp_path: Path
    ) -> None:
        path = tmp_path / "security-events.jsonl"
        target = tmp_path / "unrelated.txt"
        target.write_text("unrelated\n", encoding="utf-8")
        target.chmod(0o644)
        backup_link = tmp_path / "security-events.jsonl.20260101-120000.100"
        backup_link.symlink_to(target)

        JsonlEventWriter(path=path).write_or_raise({"stream": "security"})

        assert backup_link.is_symlink()
        assert stat.S_IMODE(target.stat().st_mode) == 0o644

    def test_retained_backup_migration_ignores_non_backup_files(
        self, tmp_path: Path
    ) -> None:
        path = tmp_path / "security-events.jsonl"
        unrelated = [
            tmp_path / "security-events.jsonl.old",
            tmp_path / "security-events.jsonl.bak",
            tmp_path / "security-events.jsonl.notes",
        ]
        for candidate in unrelated:
            candidate.write_text("unrelated\n", encoding="utf-8")
            candidate.chmod(0o644)

        JsonlEventWriter(path=path).write_or_raise({"stream": "security"})

        assert all(
            stat.S_IMODE(candidate.stat().st_mode) == 0o644 for candidate in unrelated
        )

    def test_generic_writer_uses_stream_specific_rotation_state(
        self, tmp_path: Path
    ) -> None:
        security_path = tmp_path / "security-events.jsonl"
        observability_path = tmp_path / "observability.jsonl"
        security_writer = JsonlEventWriter(
            path=security_path, max_bytes=250, backup_count=1
        )
        observability_writer = JsonlEventWriter(
            path=observability_path, max_bytes=10_000, backup_count=1
        )

        for i in range(10):
            security_writer.write({"stream": "security", "seq": i, "pad": "x" * 50})
        observability_writer.write({"stream": "observability", "seq": 1})

        assert len(_backup_files(security_path)) >= 1
        assert not _backup_files(observability_path)


class TestWriterRotation:
    def test_rotation_detection(self, tmp_path: Path) -> None:
        path = tmp_path / "security-events.jsonl"
        path.touch()
        writer = SecurityEventWriter(path=path)

        writer.write(_make_event(event_type="before_rotate"))

        path.unlink()
        path.write_text("", encoding="utf-8")

        writer.write(_make_event(event_type="after_rotate"))

        lines = _read_event_lines(path)
        assert len(lines) >= 1
        assert json.loads(lines[-1])["event_type"] == "after_rotate"


class TestWriterAutoRotation:
    """Test automatic file size-based rotation."""

    def test_auto_rotation_on_size_limit(self, tmp_path: Path) -> None:
        """Test that log file is rotated when it exceeds max_bytes."""
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path, max_bytes=500, backup_count=3)

        for i in range(20):
            writer.write(_make_event(event_type=f"evt_{i}", details={"data": "x" * 50}))

        assert _backup_files(path), "At least one rotated backup file should exist"
        assert path.exists()
        assert path.stat().st_size < 600

    def test_rotation_tightens_legacy_file_before_moving_it(
        self, tmp_path: Path
    ) -> None:
        path = tmp_path / "security-events.jsonl"
        path.write_text("legacy\n", encoding="utf-8")
        path.chmod(0o644)
        writer = JsonlEventWriter(path=path, max_bytes=1, backup_count=3)

        writer.write({"stream": "security"})

        backups = _backup_files(path)
        assert len(backups) == 1
        assert stat.S_IMODE(backups[0].stat().st_mode) == 0o600
        assert stat.S_IMODE(path.stat().st_mode) == 0o600

    def test_backup_count_limit(self, tmp_path: Path) -> None:
        """Test that old backups are deleted when backup_count is exceeded."""
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path, max_bytes=300, backup_count=3)

        for i in range(50):
            writer.write(_make_event(event_type=f"evt_{i}", details={"data": "y" * 50}))

        backup_files = _backup_files(path)
        assert len(backup_files) <= 4, (
            f"Should have at most 4 backup files, but found "
            f"{len(backup_files)}: {backup_files}"
        )

    def test_rotation_preserves_events(self, tmp_path: Path) -> None:
        """Test that events are not lost during rotation."""
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path, max_bytes=1000, backup_count=5)

        total_events = 15
        for i in range(total_events):
            writer.write(
                _make_event(event_id=f"event-{i}", details={"payload": "z" * 40})
            )
            time.sleep(0.01)

        total_count = sum(
            len(_read_event_lines(file_path)) for file_path in _all_event_files(path)
        )
        assert (
            total_count == total_events
        ), f"Should have {total_events} total events across all files"

    def test_timestamp_format_in_backup_filename(self, tmp_path: Path) -> None:
        """Test that backup files use timestamp format with millisecond precision."""
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path, max_bytes=400, backup_count=5)

        for i in range(20):
            writer.write(_make_event(event_type=f"evt_{i}", details={"data": "x" * 50}))

        timestamp_pattern = re.compile(r"^\d{8}-\d{6}\.\d{3}(\.\d+)?$")

        for backup_file in _backup_files(path):
            suffix = backup_file.name[len(path.name) + 1 :]
            assert timestamp_pattern.match(suffix), (
                f"Backup file '{backup_file.name}' should have timestamp format "
                f"YYYYMMDD-HHMMSS.fff[.N], got suffix: {suffix}"
            )

    def test_oldest_backups_are_deleted(self, tmp_path: Path) -> None:
        """Test that oldest backup files are deleted when exceeding backup_count."""
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path, max_bytes=300, backup_count=3)

        for i in range(60):
            writer.write(_make_event(event_type=f"evt_{i}", details={"data": "y" * 50}))
            time.sleep(0.01)

        backup_files = _backup_files(path)
        assert len(backup_files) <= 4, (
            f"Should have at most 4 backup files after cleanup, but found "
            f"{len(backup_files)}: {backup_files}"
        )

        current_time = time.time()
        for backup_file in backup_files:
            assert current_time - backup_file.stat().st_mtime < 10, (
                "Backup files should be recent, not old ones that should have "
                "been deleted"
            )

        assert path.exists()
        assert path.stat().st_size < 600

    def test_cleanup_preserves_most_recent_backups(self, tmp_path: Path) -> None:
        """Test that cleanup keeps the most recent backups, not random ones."""
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path, max_bytes=250, backup_count=2)

        for batch in range(5):
            for i in range(10):
                writer.write(
                    _make_event(
                        event_type=f"batch{batch}_evt{i}", details={"data": "z" * 50}
                    )
                )
            time.sleep(0.05)

        backup_files = _backup_files(path)
        assert (
            len(backup_files) <= 3
        ), f"Should have at most 3 backup files, but found {len(backup_files)}"
        assert len(backup_files) >= 2

        ts1 = backup_files[0].name[len(path.name) + 1 :]
        ts2 = backup_files[1].name[len(path.name) + 1 :]
        assert ts2 > ts1, f"Backups should be ordered by timestamp: {ts1} < {ts2}"

    def test_cleanup_detailed_verification(self, tmp_path: Path) -> None:
        """Comprehensive test of cleanup mechanism with detailed verification."""
        path = tmp_path / "security-events.jsonl"
        max_bytes = 1000
        writer = SecurityEventWriter(path=path, max_bytes=max_bytes, backup_count=3)

        for i in range(100):
            writer.write(_make_event(event_type=f"evt_{i}", details={"data": "x" * 50}))
            time.sleep(0.01)

        backup_files = _backup_files(path)
        assert len(backup_files) <= 4, (
            f"Should have at most 4 backup files, but found "
            f"{len(backup_files)}: {backup_files}"
        )

        for backup_file in backup_files:
            assert backup_file.exists()
            assert backup_file.stat().st_size >= 0
            assert backup_file.stat().st_mtime > 0

        current_time = time.time()
        for backup_file in backup_files:
            age = current_time - backup_file.stat().st_mtime
            assert age < 5, (
                f"Backup {backup_file.name} should be recent (< 5s old), "
                f"but is {age:.1f}s old"
            )

        assert path.exists(), "Current log file should exist after rotation"
        current_size = path.stat().st_size
        assert current_size < max_bytes + 300, (
            f"Current file ({current_size} bytes) should be reasonably small "
            f"(< {max_bytes + 300})"
        )

        mtimes = [backup_file.stat().st_mtime for backup_file in backup_files]
        for i in range(len(mtimes) - 1):
            assert (
                mtimes[i] <= mtimes[i + 1]
            ), f"Backups should be ordered by time: backup[{i}] <= backup[{i + 1}]"


class TestCleanupBackupMatching:
    """Verify _cleanup_old_backups correctly identifies backup files."""

    def _create_file(self, tmp_path: Path, name: str, age_offset: int = 0) -> Path:
        """Create a file in tmp_path and set its mtime to now - age_offset."""
        path = tmp_path / name
        path.write_text("data\n", encoding="utf-8")
        mtime = time.time() - age_offset
        os.utime(path, (mtime, mtime))
        return path

    def _list_files(self, tmp_path: Path, log_path: Path) -> set[str]:
        """Return filenames in tmp_path, excluding the active log."""
        return {path.name for path in tmp_path.iterdir() if path.name != log_path.name}

    def test_collision_guard_backups_are_recognized(self, tmp_path: Path) -> None:
        """Backups with .N collision-guard suffix must be counted and cleaned."""
        log_path = tmp_path / "security-events.jsonl"
        log_path.write_text('{"event_type": "current"}\n', encoding="utf-8")
        self._create_file(tmp_path, "security-events.jsonl.20260101-120000.100", 40)
        self._create_file(tmp_path, "security-events.jsonl.20260101-120000.100.1", 30)
        self._create_file(tmp_path, "security-events.jsonl.20260101-120001.200", 20)
        self._create_file(tmp_path, "security-events.jsonl.20260101-120001.200.1", 10)

        writer = SecurityEventWriter(path=log_path, backup_count=2)
        writer._cleanup_old_backups()

        remaining = self._list_files(tmp_path, log_path)
        assert len(remaining) <= 2, f"Expected <= 2 backups, got: {remaining}"
        assert "security-events.jsonl.20260101-120000.100" not in remaining
        assert "security-events.jsonl.20260101-120000.100.1" not in remaining

    def test_non_backup_files_are_not_deleted(self, tmp_path: Path) -> None:
        """Files that share the prefix but lack the timestamp pattern must survive."""
        log_path = tmp_path / "security-events.jsonl"
        log_path.write_text('{"event_type": "current"}\n', encoding="utf-8")
        self._create_file(tmp_path, "security-events.jsonl.old", 100)
        self._create_file(tmp_path, "security-events.jsonl.bak", 100)
        self._create_file(tmp_path, "security-events.jsonl.lock", 100)
        self._create_file(tmp_path, "security-events.jsonl.tmp", 100)
        self._create_file(tmp_path, "security-events.jsonl.schema", 100)
        self._create_file(tmp_path, "security-events.jsonl.20260101-120000.100", 5)

        writer = SecurityEventWriter(path=log_path, backup_count=5)
        writer._cleanup_old_backups()

        remaining = self._list_files(tmp_path, log_path)
        for name in [
            "security-events.jsonl.old",
            "security-events.jsonl.bak",
            "security-events.jsonl.lock",
            "security-events.jsonl.tmp",
            "security-events.jsonl.schema",
        ]:
            assert name in remaining, f"{name} should NOT have been deleted"
        assert "security-events.jsonl.20260101-120000.100" in remaining

    def test_mixed_cleanup_respects_backup_count(self, tmp_path: Path) -> None:
        """With mixed real and non-backup files, only real backups are counted."""
        log_path = tmp_path / "security-events.jsonl"
        log_path.write_text('{"event_type": "current"}\n', encoding="utf-8")
        self._create_file(tmp_path, "security-events.jsonl.20260101-100000.000", 50)
        self._create_file(tmp_path, "security-events.jsonl.20260101-100000.000.1", 40)
        self._create_file(tmp_path, "security-events.jsonl.20260101-110000.000", 30)
        self._create_file(tmp_path, "security-events.jsonl.20260101-120000.000", 20)
        self._create_file(tmp_path, "security-events.jsonl.20260101-130000.000", 10)
        self._create_file(tmp_path, "security-events.jsonl.old", 200)
        self._create_file(tmp_path, "security-events.jsonl.notes", 200)

        writer = SecurityEventWriter(path=log_path, backup_count=3)
        writer._cleanup_old_backups()

        remaining = self._list_files(tmp_path, log_path)
        assert "security-events.jsonl.old" in remaining
        assert "security-events.jsonl.notes" in remaining
        assert "security-events.jsonl.20260101-100000.000" not in remaining
        assert "security-events.jsonl.20260101-100000.000.1" not in remaining
        assert "security-events.jsonl.20260101-110000.000" in remaining
        assert "security-events.jsonl.20260101-120000.000" in remaining
        assert "security-events.jsonl.20260101-130000.000" in remaining


def _child_writer(
    path: str,
    proc_id: int,
    event_count: int,
    max_bytes: int,
    backup_count: int,
) -> list[str]:
    """Entry point executed inside each child process."""
    kwargs: dict[str, Any] = {"path": path}
    if max_bytes:
        kwargs["max_bytes"] = max_bytes
        kwargs["backup_count"] = backup_count
    writer = SecurityEventWriter(**kwargs)
    written_events = []
    for i in range(event_count):
        evt = SecurityEvent(
            event_type=f"p{proc_id}_e{i}",
            category="mp_test",
            details={"proc": proc_id, "seq": i, "pad": "x" * 30},
        )
        writer.write(evt)
        written_events.append(f"p{proc_id}_e{i}")
    return written_events


class TestWriterMultiProcessSafety:
    """Cross-process flock contention tests."""

    _REQUIRED_FIELDS = {
        "event_id",
        "event_type",
        "category",
        "result",
        "timestamp",
        "trace_id",
        "pid",
        "uid",
        "session_id",
        "details",
    }

    def _spawn_and_wait(
        self,
        path: Path,
        n_procs: int,
        events_per_proc: int,
        max_bytes: int = 0,
        backup_count: int = 0,
    ) -> list[multiprocessing.Process]:
        """Fork n_procs children, wait, and assert clean exit."""
        procs = [
            multiprocessing.Process(
                target=_child_writer,
                args=(str(path), pid, events_per_proc, max_bytes, backup_count),
            )
            for pid in range(n_procs)
        ]
        for process in procs:
            process.start()
        for process in procs:
            process.join(timeout=30)
        for i, process in enumerate(procs):
            assert (
                process.exitcode == 0
            ), f"Child {i} exited with code {process.exitcode}"
        return procs

    def _collect_all_events(self, path: Path) -> list[dict[str, Any]]:
        """Read every JSONL line from the main file and all rotated backups."""
        events = []
        for file_path in _all_event_files(path):
            if not file_path.exists() or file_path.stat().st_size == 0:
                continue
            with file_path.open(encoding="utf-8") as fh:
                for line in fh:
                    line = line.strip()
                    if line:
                        events.append(json.loads(line))
        return events

    def _assert_valid_event(self, record: dict[str, Any], context: str = "") -> None:
        """Assert record has the full SecurityEvent schema."""
        missing = self._REQUIRED_FIELDS - record.keys()
        assert not missing, f"Event missing fields {missing}: {record!r} {context}"
        assert isinstance(record["event_type"], str)
        assert isinstance(record["pid"], int)
        assert isinstance(record["details"], dict)

    def test_cross_process_concurrent_writes_no_rotation(self, tmp_path: Path) -> None:
        """Multiple processes appending to the same file must not lose events."""
        path = tmp_path / "security-events.jsonl"
        n_procs = 4
        events_per_proc = 25
        self._spawn_and_wait(path, n_procs, events_per_proc)

        lines = _read_event_lines(path)
        expected = n_procs * events_per_proc
        assert len(lines) == expected, f"Expected {expected} lines, got {len(lines)}"
        for i, line in enumerate(lines):
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                pytest.fail(f"Line {i} is not valid JSON: {line!r}")
            self._assert_valid_event(record, context=f"(line {i})")

    def test_cross_process_rotation_under_contention(self, tmp_path: Path) -> None:
        """Flock contention during rotation must not lose or corrupt events."""
        path = tmp_path / "security-events.jsonl"
        n_procs = 4
        events_per_proc = 30
        self._spawn_and_wait(
            path, n_procs, events_per_proc, max_bytes=5000, backup_count=200
        )

        events = self._collect_all_events(path)
        expected = n_procs * events_per_proc
        assert (
            len(events) == expected
        ), f"Expected {expected} total events across all files, got {len(events)}"

        tags = {event["event_type"] for event in events}
        for pid in range(n_procs):
            for seq in range(events_per_proc):
                tag = f"p{pid}_e{seq}"
                assert tag in tags, f"Missing event {tag}"

        for event in events:
            self._assert_valid_event(event)

    def test_new_events_land_in_current_file_after_rotation(
        self, tmp_path: Path
    ) -> None:
        """After rotation, new writes must go to the current file, not a backup."""
        path = tmp_path / "security-events.jsonl"
        n_procs = 4
        events_per_proc = 30
        self._spawn_and_wait(
            path, n_procs, events_per_proc, max_bytes=5000, backup_count=200
        )

        backup_files = _backup_files(path)
        if not backup_files:
            return

        current_min = _min_event_timestamp(path)
        latest_backup_max = max(
            timestamp
            for backup_file in backup_files
            if (timestamp := _max_event_timestamp(backup_file)) is not None
        )
        assert current_min is not None

        current_min_dt = datetime.fromisoformat(current_min)
        backup_max_dt = datetime.fromisoformat(latest_backup_max)
        tolerance = timedelta(seconds=1)

        assert current_min_dt + tolerance >= backup_max_dt, (
            f"Current file min ts ({current_min}) is >1 s older than "
            f"latest backup max ts ({latest_backup_max}) - "
            "a process is likely still writing to a rotated file"
        )

    def test_flock_loser_reopens_and_writes(self, tmp_path: Path) -> None:
        """Processes that lose the flock race must still write all events."""
        path = tmp_path / "security-events.jsonl"
        n_procs = 6
        events_per_proc = 20

        self._spawn_and_wait(
            path, n_procs, events_per_proc, max_bytes=5000, backup_count=200
        )

        events = self._collect_all_events(path)
        expected = n_procs * events_per_proc
        assert len(events) == expected, (
            f"Expected {expected} events, got {len(events)} "
            "(flock losers may have lost events)"
        )

        pids_seen = {event["event_type"].split("_")[0] for event in events}
        for pid in range(n_procs):
            assert (
                f"p{pid}" in pids_seen
            ), f"Process p{pid} has zero events - flock loser path likely broken"

    def test_cross_process_events_carry_distinct_pids(self, tmp_path: Path) -> None:
        """Each child process should stamp its real OS PID in the event."""
        path = tmp_path / "security-events.jsonl"
        n_procs = 3
        events_per_proc = 5
        self._spawn_and_wait(path, n_procs, events_per_proc)

        events = self._collect_all_events(path)
        pids = {event["pid"] for event in events}
        assert len(pids) >= n_procs, f"Expected >= {n_procs} distinct PIDs, got {pids}"


class TestWriterFireAndForget:
    def test_write_with_no_fd_does_not_raise(self) -> None:
        writer = SecurityEventWriter(path="/nonexistent/path/events.jsonl")
        writer.write(_make_event())

    def test_write_serialization_failure_does_not_emit_stderr(
        self, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        path = tmp_path / "events.jsonl"
        writer = JsonlEventWriter(path=path)

        writer.write({"bad": object()})

        captured = capsys.readouterr()
        assert captured.err == ""
        assert not path.exists()

    def test_write_or_raise_surfaces_serialization_failure_without_stderr(
        self, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        path = tmp_path / "events.jsonl"
        writer = JsonlEventWriter(path=path)

        with pytest.raises(TypeError):
            writer.write_or_raise({"path": Path("/tmp/x")})

        captured = capsys.readouterr()
        assert captured.err == ""

    def test_write_invokes_on_error_for_io_failure(self, tmp_path: Path) -> None:
        """When the underlying append fails (e.g. ENOSPC simulated via patched
        ``_append_record``), the configured ``on_error`` callback is invoked
        with the exception. See PR #651 review #1.
        """
        captured: list[Exception] = []

        def _capture(exc: Exception) -> None:
            captured.append(exc)

        path = tmp_path / "events.jsonl"
        writer = JsonlEventWriter(path=path, on_error=_capture)

        boom = OSError("no space left on device")

        def _raising(_record: object) -> None:
            raise boom

        writer._append_record = _raising  # type: ignore[assignment]
        writer.write({"k": "v"})

        assert captured == [boom]

    def test_write_invokes_on_error_for_rotation_failure(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        captured: list[Exception] = []
        path = tmp_path / "events.jsonl"
        path.write_text("x" * 20, encoding="utf-8")
        writer = JsonlEventWriter(path=path, max_bytes=1, on_error=captured.append)
        boom = OSError("cannot rotate")

        def _raise_move(_src: object, _dst: object) -> None:
            raise boom

        monkeypatch.setattr(
            "agent_sec_cli.security_events.writer.shutil.move",
            _raise_move,
        )

        writer.write({"k": "v"})

        assert captured == [boom]
        assert path.exists()

    def test_cleanup_invokes_on_error_for_unlink_failure(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        captured: list[Exception] = []
        path = tmp_path / "events.jsonl"
        backup = tmp_path / "events.jsonl.20260101-120000.000"
        backup.write_text("old\n", encoding="utf-8")
        writer = JsonlEventWriter(path=path, backup_count=0, on_error=captured.append)
        boom = OSError("cannot unlink")

        def _raise_unlink(_path: Path) -> None:
            raise boom

        monkeypatch.setattr(Path, "unlink", _raise_unlink)

        writer._cleanup_old_backups()

        assert captured == [boom]

    def test_write_swallows_on_error_callback_failure(self, tmp_path: Path) -> None:
        """A misbehaving ``on_error`` (e.g. raises) must not surface to the
        caller — the writer's fire-and-forget contract is preserved.
        """

        def _bad_callback(_exc: Exception) -> None:
            raise RuntimeError("on_error itself blew up")

        path = tmp_path / "events.jsonl"
        writer = JsonlEventWriter(path=path, on_error=_bad_callback)
        writer._append_record = lambda _r: (_ for _ in ()).throw(  # type: ignore[assignment]
            OSError("disk full")
        )
        writer.write({"k": "v"})  # must not raise

    def test_write_without_on_error_remains_silent(self, tmp_path: Path) -> None:
        """No callback configured (e.g. cli.jsonl handler) → silent drop, no recursion."""
        path = tmp_path / "events.jsonl"
        writer = JsonlEventWriter(path=path)
        writer._append_record = lambda _r: (_ for _ in ()).throw(  # type: ignore[assignment]
            OSError("disk full")
        )
        writer.write({"k": "v"})  # must not raise


class TestWriterThreadSafety:
    def test_concurrent_writes(self, tmp_path: Path) -> None:
        path = tmp_path / "security-events.jsonl"
        writer = SecurityEventWriter(path=path)

        n_threads = 10
        events_per_thread = 5
        errors: list[Exception] = []

        def _write_events(tid: int) -> None:
            try:
                for i in range(events_per_thread):
                    writer.write(_make_event(event_type=f"t{tid}_{i}"))
            except Exception as exc:
                errors.append(exc)

        threads = [
            threading.Thread(target=_write_events, args=(thread_id,))
            for thread_id in range(n_threads)
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

        assert errors == []
