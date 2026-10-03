"""Unit tests for ObservabilityEventRepository read methods.

Mirrors the writer→reader roundtrip pattern in
``tests/unit-test/security_events/test_sqlite_reader.py``: the writer seeds
records, then the read methods (``list_sessions`` / ``list_runs`` /
``list_events``) are exercised directly on a fresh ``SqliteStore``.
"""

import json
import sqlite3
import threading
from pathlib import Path
from typing import Any

import pytest
from agent_sec_cli.observability.models import (
    OBSERVABILITY_SQLITE_SCHEMA_VERSION,
    ORM_MODELS,
    ObservabilityEventRecord,
)
from agent_sec_cli.observability.repositories import (
    ObservabilityEventRepository,
    SessionSummary,
)
from agent_sec_cli.observability.schema import validate_observability_record
from agent_sec_cli.observability.sqlite_writer import ObservabilitySqliteWriter
from agent_sec_cli.security_events.orm_store import SqliteStore
from sqlalchemy.exc import SQLAlchemyError


def _payload(
    *,
    hook: str = "before_agent_run",
    session_id: str = "session-A",
    run_id: str = "run-1",
    observed_at: str = "2026-05-16T12:00:00Z",
    metrics: dict[str, Any] | None = None,
    metadata_extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    base_metadata: dict[str, Any] = {"sessionId": session_id, "runId": run_id}
    if metadata_extra:
        base_metadata.update(metadata_extra)
    return {
        "hook": hook,
        "observedAt": observed_at,
        "metadata": base_metadata,
        "metrics": metrics or {"prompt": "default-prompt"},
    }


def _seed(writer: ObservabilitySqliteWriter, **kwargs: Any) -> None:
    record = validate_observability_record(_payload(**kwargs))
    writer.write(record)


@pytest.fixture()
def db_path(tmp_path: Path) -> str:
    return str(tmp_path / "observability.db")


@pytest.fixture()
def writer(db_path: str) -> ObservabilitySqliteWriter:
    # max_age_days=None disables retention prune so hardcoded-timestamp tests
    # don't depend on the wall clock at run time.
    w = ObservabilitySqliteWriter(path=db_path, max_age_days=None)
    yield w
    w.close()


def _open_repository(db_path: str) -> tuple[SqliteStore, ObservabilityEventRepository]:
    store = SqliteStore(
        db_path,
        read_only=True,
        models=ORM_MODELS,
        schema_version=OBSERVABILITY_SQLITE_SCHEMA_VERSION,
        log_prefix="[observability-test]",
    )
    return store, ObservabilityEventRepository(store)


class _FakeResult:
    def all(self):
        return []


class _FakeSession:
    def __init__(self) -> None:
        self.statements = []

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        return False

    def execute(self, statement):
        self.statements.append(statement)
        return _FakeResult()


class _FakeStore:
    def __init__(self) -> None:
        self.session = _FakeSession()

    def session_factory(self):
        return lambda: self.session

    def dispose(self) -> None:
        pass


def _compiled_sql(statement) -> str:
    return str(statement.compile(compile_kwargs={"literal_binds": True})).lower()


# ---------------------------------------------------------------------------
# Assertion 1: empty DB returns empty lists
# ---------------------------------------------------------------------------


def test_list_sessions_empty_db_returns_empty(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    # writer fixture creates the schema by initializing the store, but inserts nothing.
    writer.close()  # flush schema
    store, repo = _open_repository(db_path)
    try:
        assert repo.list_sessions() == []
    finally:
        store.close()


# ---------------------------------------------------------------------------
# Assertion 2: multi-session ordering + turn_count + event_count
# ---------------------------------------------------------------------------


def test_list_sessions_orders_by_last_seen_desc(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    # session-OLD: 1 run, 1 event, last seen 2026-05-15T12:00:00Z
    _seed(
        writer,
        session_id="session-OLD",
        run_id="run-old-1",
        observed_at="2026-05-15T12:00:00Z",
    )
    # session-NEW: 2 distinct runs, 3 events total, last seen 2026-05-16T15:00:00Z
    _seed(
        writer,
        session_id="session-NEW",
        run_id="run-new-1",
        observed_at="2026-05-16T10:00:00Z",
    )
    _seed(
        writer,
        session_id="session-NEW",
        run_id="run-new-1",
        hook="before_llm_call",
        observed_at="2026-05-16T10:00:01Z",
        metrics={"prompt": "p"},
        metadata_extra={"callId": "call-1"},
    )
    _seed(
        writer,
        session_id="session-NEW",
        run_id="run-new-2",
        observed_at="2026-05-16T15:00:00Z",
    )
    writer.close()

    store, repo = _open_repository(db_path)
    try:
        sessions = repo.list_sessions()
    finally:
        store.close()

    assert [s.session_id for s in sessions] == ["session-NEW", "session-OLD"]
    new = sessions[0]
    assert new.turn_count == 2
    assert new.event_count == 3
    assert new.first_seen_epoch < new.last_seen_epoch
    old = sessions[1]
    assert old.turn_count == 1
    assert old.event_count == 1


# ---------------------------------------------------------------------------
# Assertion 3: list_runs preview fallback chain + 80-char truncation
# ---------------------------------------------------------------------------


def test_list_runs_preview_fallback_and_truncation(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    long_text = "x" * 200

    # run-A: before_agent_run with user_input — picks user_input
    _seed(
        writer,
        session_id="session-S",
        run_id="run-A",
        hook="before_agent_run",
        observed_at="2026-05-16T10:00:00Z",
        metrics={"user_input": "first user input", "prompt": "prompt-fallback"},
    )
    # run-B: before_agent_run without user_input but with prompt — falls back
    _seed(
        writer,
        session_id="session-S",
        run_id="run-B",
        hook="before_agent_run",
        observed_at="2026-05-16T10:01:00Z",
        metrics={"prompt": long_text},
    )
    # run-C: only a before_llm_call (no before_agent_run) — preview is None
    _seed(
        writer,
        session_id="session-S",
        run_id="run-C",
        hook="before_llm_call",
        observed_at="2026-05-16T10:02:00Z",
        metrics={"prompt": "should-not-appear"},
        metadata_extra={"callId": "call-c"},
    )
    writer.close()

    store, repo = _open_repository(db_path)
    try:
        runs = repo.list_runs("session-S")
    finally:
        store.close()

    by_id = {r.run_id: r for r in runs}
    assert by_id["run-A"].user_input_preview == "first user input"
    assert by_id["run-B"].user_input_preview == "x" * 80
    assert by_id["run-C"].user_input_preview is None
    # ordering: chronological by started_at (run-A < run-B < run-C)
    assert [r.run_id for r in runs] == ["run-A", "run-B", "run-C"]


def test_list_runs_preview_query_selects_first_before_agent_run_per_run_in_sql() -> (
    None
):
    store = _FakeStore()
    repo = ObservabilityEventRepository(store)  # type: ignore[arg-type]

    repo.list_runs("session-S")

    assert len(store.session.statements) == 2
    before_run_sql = _compiled_sql(store.session.statements[1])
    assert "row_number()" in before_run_sql
    assert "partition by observability_events.run_id" in before_run_sql
    assert "where" in before_run_sql
    assert "rn = 1" in before_run_sql


def test_list_runs_preview_tolerates_malformed_metrics_json(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    _seed(writer, session_id="session-schema")
    writer.close()
    conn = sqlite3.connect(db_path)
    try:
        rows = [
            ("run-invalid-json", "not-json"),
            ("run-non-object", json.dumps(["not", "an", "object"])),
            ("run-no-preview-field", json.dumps({"duration_ms": 10})),
        ]
        for index, (run_id, metrics_json) in enumerate(rows):
            conn.execute(
                "INSERT INTO observability_events "
                "(hook, observed_at, observed_at_epoch, session_id, run_id, "
                "metrics_json, metadata_json, call_id, tool_call_id) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    "before_agent_run",
                    f"2026-05-16T10:03:0{index}Z",
                    1778925780.0 + index,
                    "session-M",
                    run_id,
                    metrics_json,
                    json.dumps({"sessionId": "session-M", "runId": run_id}),
                    None,
                    None,
                ),
            )
        conn.commit()
    finally:
        conn.close()

    store, repo = _open_repository(db_path)
    try:
        runs = repo.list_runs("session-M")
    finally:
        store.close()

    assert [run.run_id for run in runs] == [
        "run-invalid-json",
        "run-non-object",
        "run-no-preview-field",
    ]
    assert [run.user_input_preview for run in runs] == [None, None, None]


# ---------------------------------------------------------------------------
# Assertion 4: nonexistent session returns empty
# ---------------------------------------------------------------------------


def test_list_runs_nonexistent_session_returns_empty(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    _seed(writer, session_id="session-real")
    writer.close()

    store, repo = _open_repository(db_path)
    try:
        assert repo.list_runs("session-does-not-exist") == []
    finally:
        store.close()


# ---------------------------------------------------------------------------
# Assertion 5: list_events ordering + field preservation
# ---------------------------------------------------------------------------


def test_list_events_ordered_with_fields_preserved(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    # Seed in non-chronological order; reader must sort.
    _seed(
        writer,
        session_id="session-E",
        run_id="run-E",
        hook="after_tool_call",
        observed_at="2026-05-16T10:00:05Z",
        metrics={"result": "ok", "duration_ms": 12},
        metadata_extra={"toolCallId": "tc-1", "callId": "call-after"},
    )
    _seed(
        writer,
        session_id="session-E",
        run_id="run-E",
        hook="before_agent_run",
        observed_at="2026-05-16T10:00:00Z",
        metrics={"user_input": "hi"},
    )
    _seed(
        writer,
        session_id="session-E",
        run_id="run-E",
        hook="before_tool_call",
        observed_at="2026-05-16T10:00:04Z",
        metrics={"tool_name": "grep", "parameters": {"q": "x"}},
        metadata_extra={"toolCallId": "tc-1", "callId": "call-before"},
    )
    writer.close()

    store, repo = _open_repository(db_path)
    try:
        events = repo.list_events("session-E", "run-E")
    finally:
        store.close()

    assert [e.hook for e in events] == [
        "before_agent_run",
        "before_tool_call",
        "after_tool_call",
    ]
    # field preservation
    assert isinstance(events[0], ObservabilityEventRecord)
    assert events[0].session_id == "session-E"
    assert events[0].run_id == "run-E"
    assert json.loads(events[0].metrics_json)["user_input"] == "hi"
    assert json.loads(events[1].metadata_json)["toolCallId"] == "tc-1"
    assert events[2].tool_call_id == "tc-1"
    assert events[2].call_id == "call-after"


def test_list_events_is_scoped_to_session_when_run_ids_collide(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    _seed(
        writer,
        session_id="session-A",
        run_id="run-collision",
        observed_at="2026-05-16T10:00:00Z",
        metrics={"user_input": "session A input"},
    )
    _seed(
        writer,
        session_id="session-B",
        run_id="run-collision",
        observed_at="2026-05-16T10:00:01Z",
        metrics={"user_input": "session B input"},
    )
    writer.close()

    store, repo = _open_repository(db_path)
    try:
        events = repo.list_events("session-A", "run-collision")
    finally:
        store.close()

    assert [event.session_id for event in events] == ["session-A"]
    assert json.loads(events[0].metrics_json)["user_input"] == "session A input"


# ---------------------------------------------------------------------------
# Assertion 6: nonexistent run returns empty
# ---------------------------------------------------------------------------


def test_list_events_nonexistent_run_returns_empty(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    _seed(writer, run_id="run-real")
    writer.close()

    store, repo = _open_repository(db_path)
    try:
        assert repo.list_events("session-real", "run-does-not-exist") == []
    finally:
        store.close()


# ---------------------------------------------------------------------------
# Assertion 7: reader can be reopened — read-only store doesn't hold a lock
# ---------------------------------------------------------------------------


def test_reader_can_be_reopened(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    _seed(writer)
    writer.close()

    # First reader
    store1, repo1 = _open_repository(db_path)
    sessions1 = repo1.list_sessions()
    store1.close()

    # Second reader on the same file — must not be blocked by the first.
    store2, repo2 = _open_repository(db_path)
    try:
        sessions2 = repo2.list_sessions()
    finally:
        store2.close()

    assert sessions1 == sessions2
    assert len(sessions1) == 1


# ---------------------------------------------------------------------------
# Assertion 8: concurrent read while writer is active does not raise
# ---------------------------------------------------------------------------


def test_read_during_concurrent_write(
    writer: ObservabilitySqliteWriter, db_path: str
) -> None:
    """A read-only ObservabilityEventRepository should serve queries while the
    writer is alive (WAL mode + ``mode=ro`` URI engine). The PRAGMA
    busy_timeout=200 set in ``create_sqlite_engine`` covers any short window."""
    # Seed a baseline so list_sessions has work to do.
    _seed(writer, session_id="session-X", run_id="run-X-1")

    write_errors: list[BaseException] = []
    read_errors: list[BaseException] = []
    read_results: list[list[SessionSummary]] = []

    def writer_worker() -> None:
        try:
            for i in range(20):
                _seed(
                    writer,
                    session_id="session-X",
                    run_id=f"run-X-{i + 2}",
                    observed_at=f"2026-05-16T12:00:{i:02d}Z",
                )
        except BaseException as exc:  # noqa: BLE001
            write_errors.append(exc)

    def reader_worker() -> None:
        try:
            store, repo = _open_repository(db_path)
            try:
                for _ in range(20):
                    read_results.append(repo.list_sessions())
            finally:
                store.close()
        except BaseException as exc:  # noqa: BLE001
            read_errors.append(exc)

    t_w = threading.Thread(target=writer_worker)
    t_r = threading.Thread(target=reader_worker)
    t_w.start()
    t_r.start()
    t_w.join(timeout=5)
    t_r.join(timeout=5)

    assert not write_errors, f"writer raised: {write_errors}"
    assert not read_errors, f"reader raised: {read_errors}"
    assert all(isinstance(r, list) for r in read_results)
    # Final state: session-X exists with all writes flushed.
    writer.close()
    store, repo = _open_repository(db_path)
    try:
        final = repo.list_sessions()
    finally:
        store.close()
    assert len(final) == 1
    assert final[0].session_id == "session-X"
    assert final[0].event_count >= 1


class _NullSessionStore:
    engine = None

    def session_factory(self) -> None:
        return None

    def dispose(self) -> None:
        raise AssertionError("store should not be disposed for missing session factory")


class _RaisingSession:
    def __enter__(self) -> "_RaisingSession":
        return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> bool:
        return False

    def execute(self, statement: object) -> object:
        raise SQLAlchemyError("database unavailable")


class _RaisingSessionFactory:
    def __call__(self) -> _RaisingSession:
        return _RaisingSession()


class _RaisingStore:
    engine = None

    def __init__(self) -> None:
        self.disposed = False

    def session_factory(self) -> _RaisingSessionFactory:
        return _RaisingSessionFactory()

    def dispose(self) -> None:
        self.disposed = True


def test_repository_read_methods_return_empty_without_session_factory() -> None:
    repo = ObservabilityEventRepository(_NullSessionStore())  # type: ignore[arg-type]

    assert repo.list_runs("session-A") == []
    assert repo.list_events("session-A", "run-A") == []


@pytest.mark.parametrize("method_name", ["list_sessions", "list_runs", "list_events"])
def test_repository_read_methods_dispose_and_return_empty_on_sqlalchemy_error(
    method_name: str,
) -> None:
    store = _RaisingStore()
    repo = ObservabilityEventRepository(store)  # type: ignore[arg-type]

    if method_name == "list_sessions":
        result = repo.list_sessions()
    elif method_name == "list_runs":
        result = repo.list_runs("session-A")
    else:
        result = repo.list_events("session-A", "run-A")

    assert result == []
    assert store.disposed is True
