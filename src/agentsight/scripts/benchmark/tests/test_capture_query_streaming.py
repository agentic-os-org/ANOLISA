"""Large unrelated capture histories must not be materialized while validating."""

from __future__ import annotations

import json
import sqlite3
import tracemalloc
from pathlib import Path
from typing import Any

import pytest
from test_validate_results import validate_results


def database(path: Path, with_id: bool = True) -> None:
    with sqlite3.connect(path) as connection:
        identity = "id INTEGER PRIMARY KEY," if with_id else ""
        connection.execute(
            f"CREATE TABLE genai_events ({identity} call_id TEXT, status TEXT, total_tokens INTEGER, event_json TEXT)"
        )
        payload = json.dumps({"response": {"content": "x" * 16384}})
        connection.executemany(
            "INSERT INTO genai_events (call_id,status,total_tokens,event_json) VALUES (?, 'complete', 30, ?)",
            ((f"other-{i}", payload) for i in range(1600)),
        )
        connection.execute(
            "INSERT INTO genai_events (call_id,status,total_tokens,event_json) VALUES ('run-1','complete',30,'{}')"
        )
        connection.execute(
            "CREATE TABLE token_records (request_id TEXT, input_tokens INTEGER, output_tokens INTEGER)"
        )
        connection.execute("INSERT INTO token_records VALUES ('run-1',10,20)")


@pytest.mark.parametrize("mode", ["full", "incremental"])
@pytest.mark.parametrize("with_id", [False, True])
def test_database_backlog_has_bounded_python_memory_and_identical_verdict(
    tmp_path: Path, mode: str, with_id: bool
) -> None:
    path = tmp_path / "capture.db"
    database(path, with_id)
    tracemalloc.start()
    try:
        if mode == "full":
            captured = validate_results.load_captured(path, "run-", {"run-1"})
        else:
            captured, _, _, _ = validate_results.load_captured_incremental(
                path, "run-", None, 0, 0, set()
            )
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    report = validate_results.make_report({"run-1"}, {"run-1"}, captured, 30)
    assert report["completeness_ratio"] == report["token_accuracy"] == 1
    assert set(captured) == {"run-1"}
    # 25 MiB of persisted payloads are deliberate; per-row parsing plus the
    # returned evidence comfortably fits under this platform-independent bound.
    assert peak < 4 * 1024 * 1024, f"capture backlog materialized {peak} bytes"


def test_pending_refresh_crosses_parameter_batches_and_retires_deleted_rows(tmp_path: Path) -> None:
    path = tmp_path / "capture.db"
    with sqlite3.connect(path) as connection:
        connection.execute(
            "CREATE TABLE genai_events (id INTEGER PRIMARY KEY, call_id TEXT, status TEXT, total_tokens INTEGER, event_json TEXT)"
        )
        connection.executemany(
            "INSERT INTO genai_events VALUES (?, ?, 'pending', NULL, '{}')",
            ((i, f"run-{i}") for i in range(1, 1003)),
        )
    initial, genai, token, pending = validate_results.load_captured_incremental(
        path, "run-", None, 0, 0, set(), 30
    )
    assert len(initial) == len(pending) == 1002
    assert genai == 1002
    with sqlite3.connect(path) as connection:
        connection.execute("UPDATE genai_events SET status='complete', total_tokens=30")
        connection.execute("DELETE FROM genai_events WHERE id IN (1,500,1002)")
        connection.execute("INSERT INTO genai_events VALUES (1003,'run-1003','complete',30,'{}')")
    refreshed, after, token_after, pending_after = validate_results.load_captured_incremental(
        path, "run-", None, genai, token, pending, 30
    )
    assert pending_after == set()
    assert after == 1003 and token_after == token
    expected = {f"run-{i}" for i in range(1, 1004)} - {"run-1", "run-500", "run-1002"}
    report = validate_results.make_report(expected, expected, refreshed, 30)
    assert report["completeness_ratio"] == report["token_accuracy"] == 1
    assert set(refreshed) == expected
    again, final, _, _ = validate_results.load_captured_incremental(
        path, "run-", None, after, token_after, pending_after, 30
    )
    assert again == {} and final == after


def test_filtering_and_late_token_enrichment_survive_streamed_rows(tmp_path: Path) -> None:
    path = tmp_path / "capture.db"
    with sqlite3.connect(path) as connection:
        connection.execute(
            "CREATE TABLE genai_events (id INTEGER PRIMARY KEY, call_id TEXT, status TEXT, total_tokens INTEGER, event_json TEXT, start_timestamp_ns INTEGER)"
        )
        connection.executemany(
            "INSERT INTO genai_events VALUES (?,?,?,?,?,?)",
            [
                (1, "run-old", "complete", 30, "{}", 1),
                (
                    2,
                    "opaque",
                    "complete",
                    None,
                    json.dumps({"request": {"raw_body": json.dumps({"request_id": "run-new"})}}),
                    10,
                ),
            ],
        )
    captured, genai, token, pending = validate_results.load_captured_incremental(
        path, "run-", 5, 0, 0, set(), 30
    )
    assert set(captured) == {"run-new"} and pending == {2}
    with sqlite3.connect(path) as connection:
        connection.execute("UPDATE genai_events SET total_tokens=30 WHERE id=2")
    update, _, _, pending = validate_results.load_captured_incremental(
        path, "run-", 5, genai, token, pending, 30
    )
    validate_results.merge_captured(captured, update)
    report = validate_results.make_report({"run-new"}, {"run-new"}, captured, 30)
    assert report["token_accuracy"] == 1 and not pending
