"""Unit tests for observability.sqlite_reader."""

import json
from pathlib import Path
from typing import Any

from agent_sec_cli.observability.schema import validate_observability_record
from agent_sec_cli.observability.sqlite_reader import ObservabilityReader
from agent_sec_cli.observability.sqlite_writer import ObservabilitySqliteWriter


def _payload(
    *,
    hook: str = "before_agent_run",
    session_id: str = "session-A",
    run_id: str = "run-A",
    observed_at: str = "2026-05-16T12:00:00Z",
    metrics: dict[str, Any] | None = None,
    metadata_extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    metadata: dict[str, Any] = {"sessionId": session_id, "runId": run_id}
    if metadata_extra:
        metadata.update(metadata_extra)
    return {
        "hook": hook,
        "observedAt": observed_at,
        "metadata": metadata,
        "metrics": metrics or {"user_input": "inspect coverage"},
    }


def _seed(writer: ObservabilitySqliteWriter, **kwargs: Any) -> None:
    writer.write(validate_observability_record(_payload(**kwargs)))


def test_observability_reader_lists_sessions_runs_and_events(tmp_path: Path) -> None:
    db_path = tmp_path / "observability.db"
    writer = ObservabilitySqliteWriter(path=db_path, max_age_days=None)
    _seed(writer, observed_at="2026-05-16T12:00:00Z")
    _seed(
        writer,
        hook="before_tool_call",
        observed_at="2026-05-16T12:00:01Z",
        metrics={"tool_name": "pytest"},
        metadata_extra={"toolCallId": "tool-1"},
    )
    writer.close()

    reader = ObservabilityReader(path=db_path)
    try:
        sessions = reader.list_sessions()
        runs = reader.list_runs("session-A")
        events = reader.list_events("session-A", "run-A")
    finally:
        reader.close()

    assert [session.session_id for session in sessions] == ["session-A"]
    assert sessions[0].event_count == 2
    assert [run.run_id for run in runs] == ["run-A"]
    assert runs[0].user_input_preview == "inspect coverage"
    assert [event.hook for event in events] == ["before_agent_run", "before_tool_call"]
    assert json.loads(events[1].metrics_json)["tool_name"] == "pytest"


def test_observability_reader_counts_sessions_and_runs(tmp_path: Path) -> None:
    db_path = tmp_path / "observability.db"
    writer = ObservabilitySqliteWriter(path=db_path, max_age_days=None)
    _seed(
        writer,
        session_id="session-A",
        run_id="run-A",
        observed_at="2026-05-16T12:00:00Z",
    )
    _seed(
        writer,
        session_id="session-A",
        run_id="run-B",
        observed_at="2026-05-16T12:00:01Z",
    )
    _seed(
        writer,
        session_id="session-B",
        run_id="run-C",
        observed_at="2026-05-17T12:00:00Z",
    )
    writer.close()

    reader = ObservabilityReader(path=db_path)
    try:
        assert reader.count_sessions() == 2
        assert reader.count_runs("session-A") == 2
        assert (
            reader.count_sessions(
                start_epoch=1778932800.0,
                end_epoch=1779019200.0,
            )
            == 1
        )
        assert (
            reader.count_runs(
                "session-A",
                start_epoch=1778932800.0,
                end_epoch=1779019200.0,
            )
            == 2
        )
    finally:
        reader.close()


def test_observability_reader_close_disposes_store(tmp_path: Path) -> None:
    db_path = tmp_path / "observability.db"
    writer = ObservabilitySqliteWriter(path=db_path, max_age_days=None)
    _seed(writer)
    writer.close()

    reader = ObservabilityReader(path=db_path)
    assert reader.list_sessions()

    reader.close()

    assert reader._store.engine is None
