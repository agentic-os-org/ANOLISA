"""Pruned pending rows cannot become complete and must leave the refresh set."""

import sqlite3
from pathlib import Path

import pytest
from test_benchmark import validate_results


@pytest.mark.parametrize("count", [1, 501, 1001])
def test_deleted_pending_rows_leave_refresh_set(tmp_path: Path, count: int) -> None:
    path = tmp_path / "capture.db"
    with sqlite3.connect(path) as db:
        db.execute(
            "CREATE TABLE genai_events(id INTEGER PRIMARY KEY,call_id TEXT,status TEXT,total_tokens INTEGER)"
        )
        db.executemany(
            "INSERT INTO genai_events VALUES (?,?,?,?)",
            [(index, f"bench-{index}", "pending", 0) for index in range(1, count + 1)],
        )
    captured, last, rowid, pending = validate_results.load_captured_incremental(
        path, "bench-", None, 0, 0, set()
    )
    assert len(pending) == count
    with sqlite3.connect(path) as db:
        db.execute("DELETE FROM genai_events WHERE id <> 1")
        db.execute("UPDATE genai_events SET status='complete',total_tokens=20 WHERE id=1")
    changes, last, rowid, pending = validate_results.load_captured_incremental(
        path, "bench-", None, last, rowid, pending
    )
    validate_results.merge_captured(captured, changes)
    assert not pending, "deleted IDs will be queried forever on every future poll"
    assert ("complete", 20) in captured["bench-1"]
    assert len(captured) == count, "retention-safe accumulated evidence must survive pruning"
    _, _, _, pending = validate_results.load_captured_incremental(
        path, "bench-", None, last, rowid, pending
    )
    assert not pending


def test_existing_pending_row_is_still_refreshed(tmp_path: Path) -> None:
    path = tmp_path / "capture.db"
    with sqlite3.connect(path) as db:
        db.execute(
            "CREATE TABLE genai_events(id INTEGER PRIMARY KEY,call_id TEXT,status TEXT,total_tokens INTEGER)"
        )
        db.execute("INSERT INTO genai_events VALUES(1,'bench-1','pending',0)")
    _, last, rowid, pending = validate_results.load_captured_incremental(
        path, "bench-", None, 0, 0, set()
    )
    _, _, _, pending = validate_results.load_captured_incremental(
        path, "bench-", None, last, rowid, pending
    )
    assert pending == {1}
