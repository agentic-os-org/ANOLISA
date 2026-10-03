"""Record-boundary recovery for the shared JSONL writer."""

import json
import multiprocessing
import os
from pathlib import Path

import pytest

from agent_sec_cli.security_events.writer import JsonlEventWriter


def _write_records(path: str, worker: int) -> None:
    writer = JsonlEventWriter(path)
    for index in range(20):
        writer.write_or_raise({"worker": worker, "index": index})


def _valid_records(data: bytes) -> list[dict[str, object]]:
    records = []
    for line in data.splitlines():
        try:
            records.append(json.loads(line))
        except (ValueError, UnicodeDecodeError):
            continue
    return records


@pytest.mark.parametrize("tail", [b'{"broken":', b'{"text":"\xe4\xb8', b'{"complete": true}'])
def test_append_after_unterminated_tail(tmp_path: Path, tail: bytes) -> None:
    path = tmp_path / "events.jsonl"
    prefix = b'{"prior": 1}\n'
    path.write_bytes(prefix + tail)
    writer = JsonlEventWriter(path)
    writer.write_or_raise({"new": 1})
    writer.write_or_raise({"new": 2})
    data = path.read_bytes()
    assert data.startswith(prefix + tail + b"\n")
    assert _valid_records(data)[-2:] == [{"new": 1}, {"new": 2}]
    assert data.count(b"\n") == 4
    if tail == b'{"complete": true}':
        assert _valid_records(data)[1] == {"complete": True}
    assert path.stat().st_mode & 0o777 == 0o600


@pytest.mark.parametrize("initial", [b"", b'{"prior": 1}\n'])
def test_complete_log_keeps_exact_newline_format(tmp_path: Path, initial: bytes) -> None:
    path = tmp_path / "events.jsonl"
    path.write_bytes(initial)
    writer = JsonlEventWriter(path)
    writer.write_or_raise({"new": 1})
    assert path.read_bytes() == initial + b'{"new": 1}\n'


def test_recovery_cooperates_with_rotation(tmp_path: Path) -> None:
    path = tmp_path / "events.jsonl"
    original = b'{"prior": 1}\n{"broken":'
    path.write_bytes(original)
    writer = JsonlEventWriter(path, max_bytes=len(original) + 5)
    writer.write_or_raise({"new": 1})
    backups = [entry for entry in tmp_path.iterdir() if entry.name.startswith("events.jsonl.20")]
    assert len(backups) == 1
    assert backups[0].read_bytes() == original
    assert path.read_bytes() == b'{"new": 1}\n'


def test_concurrent_writers_recover_the_tail_once(tmp_path: Path) -> None:
    path = tmp_path / "events.jsonl"
    prefix = b'{"prior": 1}\n{"broken":'
    path.write_bytes(prefix)
    context = multiprocessing.get_context("spawn")
    workers = [
        context.Process(target=_write_records, args=(str(path), index)) for index in range(4)
    ]
    try:
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join(timeout=15)
            assert worker.exitcode == 0
    finally:
        for worker in workers:
            if worker.is_alive():
                worker.terminate()
                worker.join(timeout=5)
    data = path.read_bytes()
    assert data.startswith(prefix + b"\n")
    records = _valid_records(data)
    assert records[0] == {"prior": 1}
    assert {(record["worker"], record["index"]) for record in records[1:]} == {
        (worker, index) for worker in range(4) for index in range(20)
    }
    assert len(data.splitlines()) == 82


@pytest.mark.skipif(os.geteuid() == 0, reason="root bypasses owner read permissions")
def test_write_only_legacy_file_remains_writable(tmp_path: Path) -> None:
    path = tmp_path / "events.jsonl"
    path.write_bytes(b'{"broken":')
    path.chmod(0o200)
    JsonlEventWriter(path).write_or_raise({"new": 1})
    assert path.stat().st_mode & 0o777 == 0o600
    assert _valid_records(path.read_bytes()) == [{"new": 1}]
