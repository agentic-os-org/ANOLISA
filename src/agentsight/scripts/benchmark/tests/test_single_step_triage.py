"""A short capture can contain a complete tool turn or substantive answer."""

import json
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest
from test_calibrate_triage import calibrate


@pytest.mark.parametrize(
    ("message", "calls", "expected"),
    [
        ("", [{"function_name": "Read"}], "substantive"),
        ("x" * 2000, [], "substantive"),
        ("x" * 1999, [], "useless"),
        ("", [], "empty"),
    ],
    ids=["tool-only", "substantive-answer", "short-answer", "empty"],
)
def test_single_step_uses_content_not_step_count(
    message: str, calls: list[dict[str, str]], expected: str
) -> None:
    doc = {"steps": [{"source": "agent", "message": message, "tool_calls": calls}]}
    assert calibrate.classify(calibrate.measure(json.dumps(doc)), 2000) == expected


def test_cli_does_not_exclude_a_single_tool_turn(tmp_path: Path) -> None:
    path = tmp_path / "trajectories.db"
    doc = {
        "steps": [
            {
                "source": "agent",
                "message": "",
                "tool_calls": [{"function_name": "Read"}],
            }
        ]
    }
    with sqlite3.connect(path) as connection:
        connection.execute(
            "CREATE TABLE collected_trajectories (session_id TEXT, first_user_message TEXT, atif_json TEXT)"
        )
        connection.execute(
            "INSERT INTO collected_trajectories VALUES (?, ?, ?)",
            ("single-tool", None, json.dumps(doc)),
        )
    before = path.read_bytes()
    script = Path(__file__).parents[2] / "calibrate-triage.py"
    result = subprocess.run(
        [sys.executable, str(script), str(path)],
        capture_output=True,
        text=True,
        check=True,
    )
    assert "substantive" in result.stdout
    assert "[empty" not in result.stdout
    assert path.read_bytes() == before
