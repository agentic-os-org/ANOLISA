from __future__ import annotations

import sqlite3
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / "single_run"))

import validate_results


@pytest.mark.parametrize(
    ("observations", "complete", "correct", "captured_total"),
    [
        ([("pending", 37)], 0, 0, None),
        ([("error", 37)], 0, 0, None),
        ([("pending", 37), ("complete", 99)], 1, 0, 99),
        ([("complete", 99), ("pending", 37)], 1, 0, 99),
        ([("pending", 99), ("complete", 37)], 1, 1, 37),
        ([("pending", None), ("complete", 37)], 1, 1, 37),
        ([("pending", 37), ("complete", None)], 1, 0, None),
    ],
)
def test_token_accuracy_requires_completed_observations(
    observations: list[tuple[str, int | None]],
    complete: int,
    correct: int,
    captured_total: int | None,
) -> None:
    expected = {"bench-1"}
    report = validate_results.make_report(
        expected, expected, {"bench-1": observations}, 37
    )

    assert report["complete"] == complete
    assert report["captured_total_tokens"] == captured_total
    assert report["token_correct"] == correct
    assert report["token_accuracy"] == float(correct)


def test_streaming_pending_count_cannot_validate_incorrect_completion(
    tmp_path: Path,
) -> None:
    db_path = tmp_path / "agentsight.db"
    with sqlite3.connect(db_path) as connection:
        connection.executescript("""
            CREATE TABLE genai_events (
                id INTEGER PRIMARY KEY,
                call_id TEXT,
                status TEXT,
                total_tokens INTEGER
            );
            INSERT INTO genai_events VALUES (1, 'bench-1', 'pending', 37);
            """)
    captured, genai_id, token_rowid, pending = (
        validate_results.load_captured_incremental(db_path, "bench-", None, 0, 0, set())
    )
    assert pending == {1}

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE genai_events SET status = 'complete', total_tokens = 99 WHERE id = 1"
        )
    incremental, _, _, pending = validate_results.load_captured_incremental(
        db_path, "bench-", None, genai_id, token_rowid, pending
    )
    validate_results.merge_captured(captured, incremental)
    assert pending == set()
    assert captured == {"bench-1": [("pending", 37), ("complete", 99)]}

    report = validate_results.make_report({"bench-1"}, {"bench-1"}, captured, 37)
    assert report["completeness_ratio"] == 1.0
    assert report["captured_total_tokens"] == 99
    assert report["token_correct"] == 0
    assert report["token_accuracy"] == 0.0
