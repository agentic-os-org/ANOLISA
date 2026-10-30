"""Explicit token expectations must affect validation polling and CLI verdicts."""

import json
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest
from test_benchmark import validate_results


def capture_fixture(root: Path, total: int = 3) -> tuple[Path, Path, Path]:
    db = root / "capture.db"
    with sqlite3.connect(db) as connection:
        connection.execute(
            "CREATE TABLE genai_events (id INTEGER PRIMARY KEY, call_id TEXT, status TEXT, total_tokens INTEGER)"
        )
        connection.execute(
            "INSERT INTO genai_events VALUES (1, 'bench-one', 'complete', ?)", (total,)
        )
    ledger = root / "requests.jsonl"
    ledger.write_text(
        json.dumps({"request_id": "bench-one", "http_status": 200}) + "\n", encoding="utf-8"
    )
    marker = root / "complete"
    marker.touch()
    return db, ledger, marker


def poll(
    db: Path,
    ledger: Path,
    marker: Path,
    streaming: bool,
    expected: int | None,
    wait: float = 1,
    interval: float = 0.001,
) -> dict:
    if streaming:
        return validate_results.wait_for_streaming_report(
            db, ledger, marker, "bench-", expected, 1.0, wait, interval, request_log=ledger
        )
    return validate_results.wait_for_report(
        db, "bench-", {"bench-one"}, {"bench-one"}, expected, 1.0, wait, interval
    )


@pytest.mark.parametrize("streaming", [False, True])
def test_polling_waits_for_async_token_correction(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, streaming: bool
) -> None:
    db, ledger, marker = capture_fixture(tmp_path)
    sleeps = []

    def correct(_: float) -> None:
        sleeps.append(1)
        with sqlite3.connect(db) as connection:
            connection.execute("UPDATE genai_events SET total_tokens=20 WHERE id=1")

    monkeypatch.setattr(validate_results.time, "sleep", correct)
    report = poll(db, ledger, marker, streaming, 20)
    assert sleeps
    assert report["completeness_ratio"] == report["token_accuracy"] == 1


@pytest.mark.parametrize("streaming", [False, True])
def test_persistent_wrong_tokens_reach_the_existing_deadline(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, streaming: bool
) -> None:
    db, ledger, marker = capture_fixture(tmp_path)
    now = [0.0]
    sleeps = []
    monkeypatch.setattr(validate_results.time, "monotonic", lambda: now[0])

    def advance(seconds: float) -> None:
        sleeps.append(seconds)
        now[0] += seconds

    monkeypatch.setattr(validate_results.time, "sleep", advance)
    report = poll(db, ledger, marker, streaming, 20, wait=0.2, interval=10)
    assert sleeps and now[0] == pytest.approx(0.2)
    assert report["completeness_ratio"] == 1
    assert report["token_accuracy"] == 0


@pytest.mark.parametrize("streaming", [False, True])
def test_completion_only_mode_does_not_wait_for_token_counts(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, streaming: bool
) -> None:
    db, ledger, marker = capture_fixture(tmp_path)
    monkeypatch.setattr(validate_results.time, "sleep", lambda _: pytest.fail("unexpected wait"))
    report = poll(db, ledger, marker, streaming, None)
    assert report["completeness_ratio"] == 1 and report["token_accuracy"] is None


@pytest.mark.parametrize("streaming", [False, True])
def test_correct_tokens_return_without_waiting(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, streaming: bool
) -> None:
    db, ledger, marker = capture_fixture(tmp_path, 20)
    monkeypatch.setattr(validate_results.time, "sleep", lambda _: pytest.fail("unexpected wait"))
    assert poll(db, ledger, marker, streaming, 20)["token_accuracy"] == 1


@pytest.mark.parametrize("expected,total,exit_code", [(20, 3, 1), (20, 20, 0), (None, 3, 0)])
@pytest.mark.parametrize("streaming", [False, True])
def test_real_cli_exit_matches_requested_token_expectation(
    tmp_path: Path, expected: int | None, total: int, exit_code: int, streaming: bool
) -> None:
    db, ledger, marker = capture_fixture(tmp_path, total)
    output = tmp_path / "result.json"
    command = [
        sys.executable,
        str(validate_results.__file__),
        "--db",
        str(db),
        "--load-results",
        str(ledger),
        "--request-log",
        str(ledger),
        "--output",
        str(output),
        "--wait-seconds",
        "0",
    ]
    if expected is not None:
        command += ["--expected-total-tokens", str(expected)]
    if streaming:
        command += ["--completion-marker", str(marker)]
    result = subprocess.run(command, text=True, capture_output=True)
    assert result.returncode == exit_code, result.stdout + result.stderr
    report = json.loads(output.read_text())
    assert report["completeness_ratio"] == 1
    assert report["token_accuracy"] == (None if expected is None else float(total == expected))


def test_token_refresh_does_not_retain_unrelated_run_rows(tmp_path: Path) -> None:
    db, _, _ = capture_fixture(tmp_path, 20)
    with sqlite3.connect(db) as connection:
        connection.execute("INSERT INTO genai_events VALUES (2, 'other-run', 'complete', 3)")
    captured, _, _, pending = validate_results.load_captured_incremental(
        db, "bench-", None, 0, 0, set(), expected_total_tokens=20
    )
    assert set(captured) == {"bench-one"}
    assert not pending


def test_incremental_token_refresh_preserves_earlier_pruned_evidence(tmp_path: Path) -> None:
    db, _, _ = capture_fixture(tmp_path, 20)
    with sqlite3.connect(db) as connection:
        connection.execute("INSERT INTO genai_events VALUES (2, 'bench-two', 'complete', 3)")
    first, cursor, token_cursor, pending = validate_results.load_captured_incremental(
        db, "bench-", None, 0, 0, set(), expected_total_tokens=20
    )
    with sqlite3.connect(db) as connection:
        connection.execute("DELETE FROM genai_events WHERE id=1")
        connection.execute("UPDATE genai_events SET total_tokens=20 WHERE id=2")
    second, _, _, pending = validate_results.load_captured_incremental(
        db, "bench-", None, cursor, token_cursor, pending, expected_total_tokens=20
    )
    validate_results.merge_captured(first, second)
    report = validate_results.make_report(
        {"bench-one", "bench-two"}, {"bench-one", "bench-two"}, first, 20
    )
    assert report["token_accuracy"] == 1
    assert not pending
