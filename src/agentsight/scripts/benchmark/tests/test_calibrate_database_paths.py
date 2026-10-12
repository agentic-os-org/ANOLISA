"""Exercise calibration against literal SQLite filenames through the CLI."""

from __future__ import annotations

import json
import sqlite3
import subprocess
import sys
from contextlib import closing
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[2] / "calibrate-triage.py"


def create_database(path: Path, rows: int = 1) -> None:
    document = json.dumps(
        {
            "steps": [
                {"source": "user", "message": "hello"},
                {"source": "agent", "message": "answer"},
            ]
        }
    )
    with closing(sqlite3.connect(path)) as connection:
        connection.execute(
            "CREATE TABLE collected_trajectories "
            "(session_id TEXT, first_user_message TEXT, atif_json TEXT)"
        )
        connection.executemany(
            "INSERT INTO collected_trajectories VALUES (?, ?, ?)",
            [(f"fixture-{index}", "hello", document) for index in range(rows)],
        )
        connection.commit()


@pytest.mark.parametrize("relative", [False, True])
@pytest.mark.parametrize(
    "filename", ["ordinary.db", "capture#1.db", "capture%231.db", "轨迹 data.db"]
)
def test_calibration_reads_the_literal_database(
    tmp_path: Path, filename: str, relative: bool
) -> None:
    database = tmp_path / filename
    create_database(database)
    # A decoded URI must not silently select another valid database.
    if filename == "capture%231.db":
        create_database(tmp_path / "capture#1.db", rows=2)
    before = {path.name: path.read_bytes() for path in tmp_path.iterdir()}

    result = subprocess.run(
        [sys.executable, "-X", "utf8", str(SCRIPT), filename if relative else str(database)],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=10,
    )

    assert result.returncode == 0, result.stderr
    assert "total=1 unparsable=0" in result.stdout
    assert {path.name: path.read_bytes() for path in tmp_path.iterdir()} == before


@pytest.mark.parametrize("filename", ["missing.db", "missing#1.db", "missing%231.db"])
def test_missing_database_is_not_created(tmp_path: Path, filename: str) -> None:
    result = subprocess.run(
        [sys.executable, "-X", "utf8", str(SCRIPT), filename],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=10,
    )

    assert result.returncode == 1
    assert "cannot read" in result.stderr
    assert list(tmp_path.iterdir()) == []
