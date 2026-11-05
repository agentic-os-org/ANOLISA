"""Capture readiness and final validation must open the exact database path."""

from __future__ import annotations

import json
import sqlite3
import subprocess
import sys
from contextlib import closing
from pathlib import Path
from types import SimpleNamespace

import pytest
from test_resource_report import artifacts, resource_report

sys.path.insert(0, str(Path(__file__).parents[1] / "single_run"))
import resource_run
import validate_results


def database(path: Path, request_id: str = "bench-one", total: int = 13) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with closing(sqlite3.connect(path)) as connection:
        connection.execute(
            "CREATE TABLE genai_events (id INTEGER PRIMARY KEY, call_id TEXT, "
            "status TEXT, total_tokens INTEGER)"
        )
        connection.execute(
            "INSERT INTO genai_events VALUES (1, ?, 'complete', ?)", (request_id, total)
        )
        connection.commit()


@pytest.mark.parametrize(
    "name", ["normal.db", "capture#one.db", "capture?one.db", "capture%23one.db"]
)
@pytest.mark.parametrize("streaming", [False, True])
def test_final_cli_reconciles_exact_database_and_reaches_resource_report(
    tmp_path: Path, name: str, streaming: bool
) -> None:
    directory = artifacts(tmp_path, "run", 25, 250)
    path = directory / name
    database(path)
    log = directory / "requests.jsonl"
    log.write_text('{"request_id":"bench-one","http_status":200}\n', encoding="utf-8")
    command = [
        sys.executable,
        str(Path(validate_results.__file__)),
        "--db",
        str(path),
        "--load-results",
        str(log),
        "--request-log",
        str(log),
        "--expected-total-tokens",
        "13",
        "--output",
        str(directory / "report.json"),
    ]
    if streaming:
        marker = directory / "done"
        marker.touch()
        command.extend(["--completion-marker", str(marker)])
    result = subprocess.run(command, capture_output=True, text=True, timeout=10)
    assert result.returncode == 0, result.stderr
    capture = json.loads((directory / "report.json").read_text())
    assert capture["complete"] == capture["token_correct"] == capture["sent"] == 1
    assert resource_report.report(tmp_path, {"run": {}})
    assert json.loads((tmp_path / "resource-summary.json").read_text())["valid"]


@pytest.mark.parametrize("directory_name", ["run#one", "run?one", "run%23one", "run space 世界"])
def test_resource_preflight_releases_workers_in_literal_directory(
    tmp_path: Path, directory_name: str
) -> None:
    directory = tmp_path / directory_name
    database(directory / "db/genai_events.db", "preflight-one")
    (directory / "agents").mkdir()
    (directory / "agents/preflight-0.json").write_text("{}")
    alive = SimpleNamespace(poll=lambda: None)
    resource_run.wait_for_capture(directory, {"request_ids": ["preflight-one"]}, alive, alive, 0)
    result = json.loads((directory / "capture-ready.json").read_text())
    assert result["ready"] and result["token_correct"] == 1


@pytest.mark.parametrize("incremental", [False, True])
def test_uri_syntax_cannot_select_a_neighbor_database(tmp_path: Path, incremental: bool) -> None:
    neighbor = tmp_path / "capture.db"
    requested = tmp_path / "capture.db#literal"
    database(neighbor, "bench-wrong", 99)
    database(requested, "bench-right", 13)
    if incremental:
        captured = validate_results.load_captured_incremental(
            requested, "bench-", None, 0, 0, set()
        )[0]
    else:
        captured = validate_results.load_captured(requested)
    assert captured == {"bench-right": [("complete", 13)]}


def test_relative_percent_encoded_name_is_not_decoded(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    database(Path("capture%23literal.db"))
    assert validate_results.load_captured(Path("capture%23literal.db")) == {
        "bench-one": [("complete", 13)]
    }
    assert not Path("capture#literal.db").exists()


@pytest.mark.parametrize("incremental", [False, True])
def test_missing_literal_database_is_not_created(tmp_path: Path, incremental: bool) -> None:
    path = tmp_path / "missing?mode=rwc.db"
    with pytest.raises(sqlite3.OperationalError):
        if incremental:
            validate_results.load_captured_incremental(path, "", None, 0, 0, set())
        else:
            validate_results.load_captured(path)
    assert list(tmp_path.iterdir()) == []


def test_connection_remains_read_only(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = tmp_path / "capture#literal.db"
    database(path)
    original = sqlite3.connect
    connections = []

    def inspect(*args: object, **kwargs: object) -> sqlite3.Connection:
        connection = original(*args, **kwargs)
        with pytest.raises(sqlite3.OperationalError, match="readonly"):
            connection.execute("DELETE FROM genai_events")
        connections.append(connection)
        return connection

    monkeypatch.setattr(validate_results.sqlite3, "connect", inspect)
    assert validate_results.load_captured(path)
    assert len(connections) == 1
    with pytest.raises(sqlite3.ProgrammingError, match="closed"):
        connections[0].execute("SELECT 1")
