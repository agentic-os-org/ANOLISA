"""Unit tests for security_events.orm_store helpers."""

import sqlite3
import stat
import subprocess
import sys
import threading
from pathlib import Path

import agent_sec_cli.security_events.orm_store as orm_store
import pytest
from agent_sec_cli.security_events.models import SecurityEventRecord
from agent_sec_cli.security_events.orm_store import (
    Base,
    SqliteStore,
    _is_sqlite_busy_error,
    _is_sqlite_corruption_error,
    _is_sqlite_schema_error,
    create_sqlite_engine,
    ensure_schema,
    ensure_schema_if_needed,
    normalize_sqlite_path,
    sqlite_database_files,
)
from agent_sec_cli.security_events.repositories import SecurityEventRepository
from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.schema_version import (
    SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS,
    SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
    SECURITY_EVENTS_VERDICT_SCHEMA_VERSION,
)
from sqlalchemy import Index, Integer, Text, inspect, text
from sqlalchemy.exc import DatabaseError, SQLAlchemyError
from sqlalchemy.orm import Mapped, mapped_column, sessionmaker


class _SessionFactoryRaceStore(SqliteStore):
    def enable_second_session_factory_read_race(self) -> None:
        self._race_second_session_factory_read = True
        self._session_factory_read_count = 0

    def __getattribute__(self, name: str) -> object:
        if name != "_session_factory":
            return object.__getattribute__(self, name)

        try:
            race_enabled = object.__getattribute__(
                self,
                "_race_second_session_factory_read",
            )
        except AttributeError:
            return object.__getattribute__(self, name)
        if not race_enabled:
            return object.__getattribute__(self, name)

        read_count = object.__getattribute__(self, "_session_factory_read_count") + 1
        object.__setattr__(self, "_session_factory_read_count", read_count)
        if read_count == 2:
            object.__setattr__(self, "_race_second_session_factory_read", False)
            object.__setattr__(self, "_session_factory", None)
        return object.__getattribute__(self, name)


class _SchemaRepairRaceStore(SqliteStore):
    def __init__(
        self,
        path: Path,
        first_open_started: threading.Event,
        allow_first_open_finish: threading.Event,
        repair_flag_set: threading.Event,
    ) -> None:
        super().__init__(path)
        self.first_open_started = first_open_started
        self.allow_first_open_finish = allow_first_open_finish
        self.repair_flag_set = repair_flag_set
        self.open_force_values: list[bool] = []

    def __setattr__(self, name: str, value: object) -> None:
        object.__setattr__(self, name, value)
        if name == "_force_schema_convergence" and value is True:
            try:
                object.__getattribute__(self, "repair_flag_set").set()
            except AttributeError:
                pass

    def _open_session_factory(self, db_identity: tuple[int, int] | None) -> None:
        force_schema = self._force_schema_convergence
        self.open_force_values.append(force_schema)
        if len(self.open_force_values) == 1:
            self.first_open_started.set()
            assert self.allow_first_open_finish.wait(timeout=2)
        self._db_identity = db_identity
        self._session_factory = sessionmaker(expire_on_commit=False, future=True)
        self._force_schema_convergence = False


def test_schema_version_tracks_revision_history() -> None:
    revisions = SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS

    assert SECURITY_EVENTS_SQLITE_SCHEMA_VERSION == max(revisions)
    assert sorted(revisions) == list(
        range(1, SECURITY_EVENTS_SQLITE_SCHEMA_VERSION + 1)
    )
    assert orm_store._SCHEMA_VERSION == SECURITY_EVENTS_SQLITE_SCHEMA_VERSION
    assert SECURITY_EVENTS_VERDICT_SCHEMA_VERSION in revisions
    assert "verdict" in revisions[SECURITY_EVENTS_VERDICT_SCHEMA_VERSION]


def test_sqlite_corruption_classification_uses_result_code(tmp_path: Path) -> None:
    db_path = tmp_path / "corrupt.db"
    db_path.write_bytes(b"CORRUPT_GARBAGE" * 100)

    with pytest.raises(sqlite3.DatabaseError) as exc_info:
        conn = sqlite3.connect(db_path)
        try:
            conn.execute("SELECT * FROM sqlite_master").fetchall()
        finally:
            conn.close()

    assert _is_sqlite_corruption_error(exc_info.value)


def test_sqlite_busy_classification_requires_result_code() -> None:
    exc = DatabaseError(
        "INSERT",
        {},
        RuntimeError("connection error: database is locked-out by admin"),
    )

    assert not _is_sqlite_busy_error(exc)


def test_write_engine_preserves_sqlite_pragmas(tmp_path: Path) -> None:
    engine = create_sqlite_engine(tmp_path / "events.db")
    try:
        ensure_schema(engine)
        with engine.connect() as conn:
            assert conn.execute(text("PRAGMA busy_timeout")).scalar_one() == 200
            assert conn.execute(text("PRAGMA foreign_keys")).scalar_one() == 1
            assert conn.execute(text("PRAGMA journal_mode")).scalar_one() == "wal"
            assert conn.execute(text("PRAGMA synchronous")).scalar_one() == 1
            assert conn.execute(text("PRAGMA wal_autocheckpoint")).scalar_one() == 100
    finally:
        engine.dispose()


def test_readonly_engine_uses_sqlite_readonly_uri(tmp_path: Path) -> None:
    missing_db = tmp_path / "missing.db"
    engine = create_sqlite_engine(missing_db, read_only=True)
    try:
        with pytest.raises(SQLAlchemyError):
            with engine.connect():
                pass
        assert not missing_db.exists()
    finally:
        engine.dispose()


def test_normalize_sqlite_path_expands_user(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))

    assert normalize_sqlite_path("~/events.db") == tmp_path / "events.db"


def test_split_model_modules_import_in_either_order() -> None:
    snippets = (
        "import agent_sec_cli.security_events.models; "
        "import agent_sec_cli.security_events.orm_store; "
        "import agent_sec_cli.security_events.repositories",
        "import agent_sec_cli.security_events.orm_store; "
        "import agent_sec_cli.security_events.models; "
        "import agent_sec_cli.security_events.repositories",
    )

    for snippet in snippets:
        result = subprocess.run(
            [sys.executable, "-c", snippet],
            capture_output=True,
            text=True,
            check=False,
        )
        assert result.returncode == 0, result.stderr


def test_ensure_schema_creates_registered_model_tables_and_indexes(
    tmp_path: Path,
) -> None:
    class AuxiliaryRecord(Base):
        __tablename__ = "auxiliary_events"
        __table_args__ = (Index("idx_auxiliary_value", "value"),)

        id: Mapped[int] = mapped_column(Integer, primary_key=True)
        value: Mapped[str] = mapped_column(Text, nullable=False)

    engine = create_sqlite_engine(tmp_path / "events.db")
    try:
        ensure_schema(engine, models=(SecurityEventRecord, AuxiliaryRecord))
        with engine.connect() as conn:
            tables = {
                row[0]
                for row in conn.execute(
                    text("SELECT name FROM sqlite_master WHERE type = 'table'")
                )
            }
            aux_indexes = {
                row[1]
                for row in conn.execute(text("PRAGMA index_list(auxiliary_events)"))
            }

        assert {"security_events", "auxiliary_events"}.issubset(tables)
        assert "idx_auxiliary_value" in aux_indexes
    finally:
        engine.dispose()


def test_ensure_schema_rejects_empty_default_models(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(orm_store, "_DEFAULT_MODELS", ())
    engine = create_sqlite_engine(tmp_path / "events.db")
    try:
        with pytest.raises(ValueError, match="No ORM models registered"):
            ensure_schema(engine)
    finally:
        engine.dispose()


def test_sqlite_store_rejects_explicit_empty_models(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="No ORM models registered"):
        SqliteStore(tmp_path / "events.db", models=())


def test_ensure_schema_does_not_downgrade_newer_schema(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    db_path = tmp_path / "events.db"
    newer_version = orm_store._SCHEMA_VERSION + 1
    engine = create_sqlite_engine(db_path)
    try:
        with engine.begin() as conn:
            conn.execute(text(f"PRAGMA user_version = {newer_version}"))

        ensure_schema(engine)

        with engine.connect() as conn:
            assert conn.execute(text("PRAGMA user_version")).scalar_one() == (
                newer_version
            )
            assert not inspect(conn).has_table(SecurityEventRecord.__tablename__)
    finally:
        engine.dispose()

    assert "newer than this binary supports" in capsys.readouterr().err


def test_ensure_schema_if_needed_does_not_downgrade_newer_schema(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    db_path = tmp_path / "events.db"
    newer_version = orm_store._SCHEMA_VERSION + 1
    engine = create_sqlite_engine(db_path)
    try:
        with engine.begin() as conn:
            conn.execute(text(f"PRAGMA user_version = {newer_version}"))
    finally:
        engine.dispose()

    def fail_full_schema(_engine, _models=None):  # type: ignore[no-untyped-def]
        raise AssertionError("old schema should not run against newer DB")

    monkeypatch.setattr(orm_store, "ensure_schema", fail_full_schema)

    engine = create_sqlite_engine(db_path)
    try:
        ensure_schema_if_needed(engine)
        with engine.connect() as conn:
            assert conn.execute(text("PRAGMA user_version")).scalar_one() == (
                newer_version
            )
    finally:
        engine.dispose()

    assert "newer than this binary supports" in capsys.readouterr().err


def test_ensure_schema_if_needed_skips_full_schema_when_current(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    engine = create_sqlite_engine(tmp_path / "events.db")
    try:
        ensure_schema(engine)
    finally:
        engine.dispose()

    def fail_full_schema(_engine, _models=None):  # type: ignore[no-untyped-def]
        raise AssertionError("full ensure_schema should not run")

    monkeypatch.setattr(orm_store, "ensure_schema", fail_full_schema)

    engine = create_sqlite_engine(tmp_path / "events.db")
    try:
        ensure_schema_if_needed(engine)
    finally:
        engine.dispose()


def test_ensure_schema_if_needed_trusts_current_version_fast_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    db_path = tmp_path / "events.db"
    engine = create_sqlite_engine(db_path)
    try:
        with engine.begin() as conn:
            conn.execute(text(f"PRAGMA user_version = {orm_store._SCHEMA_VERSION}"))
    finally:
        engine.dispose()

    def fail_full_schema(_engine, _models=None):  # type: ignore[no-untyped-def]
        raise AssertionError("current schema version should use the fast path")

    def fail_inspect(_conn):  # type: ignore[no-untyped-def]
        raise AssertionError("current schema version should not inspect tables")

    monkeypatch.setattr(orm_store, "ensure_schema", fail_full_schema)
    monkeypatch.setattr(orm_store, "inspect", fail_inspect)

    engine = create_sqlite_engine(db_path)
    try:
        ensure_schema_if_needed(engine)
        with engine.connect() as conn:
            assert conn.execute(text("PRAGMA user_version")).scalar_one() == (
                orm_store._SCHEMA_VERSION
            )
    finally:
        engine.dispose()


def test_ensure_schema_if_needed_force_repairs_current_version_schema(
    tmp_path: Path,
) -> None:
    db_path = tmp_path / "events.db"
    engine = create_sqlite_engine(db_path)
    try:
        with engine.begin() as conn:
            conn.execute(text(f"PRAGMA user_version = {orm_store._SCHEMA_VERSION}"))

        ensure_schema_if_needed(engine, force=True)

        with engine.connect() as conn:
            assert inspect(conn).has_table(SecurityEventRecord.__tablename__)
    finally:
        engine.dispose()


def test_ensure_schema_if_needed_runs_full_schema_when_version_mismatch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    db_path = tmp_path / "events.db"
    engine = create_sqlite_engine(db_path)
    try:
        ensure_schema(engine)
        with engine.begin() as conn:
            conn.execute(text("PRAGMA user_version = 0"))
    finally:
        engine.dispose()

    called = False
    original_ensure_schema = orm_store.ensure_schema

    def wrapped_ensure_schema(
        engine_arg,
        models=None,
        *,
        schema_version=orm_store._SCHEMA_VERSION,
        schema_migrations=None,
        log_prefix="[security_events]",
    ):  # type: ignore[no-untyped-def]
        nonlocal called
        called = True
        original_ensure_schema(
            engine_arg,
            models,
            schema_version=schema_version,
            schema_migrations=schema_migrations,
            log_prefix=log_prefix,
        )

    monkeypatch.setattr(orm_store, "ensure_schema", wrapped_ensure_schema)

    engine = create_sqlite_engine(db_path)
    try:
        ensure_schema_if_needed(engine)
        with engine.connect() as conn:
            assert conn.execute(text("PRAGMA user_version")).scalar_one() == (
                orm_store._SCHEMA_VERSION
            )
    finally:
        engine.dispose()

    assert called


def test_sqlite_schema_error_classification_uses_message() -> None:
    class SchemaError(Exception):
        sqlite_errorcode = sqlite3.SQLITE_ERROR

    assert _is_sqlite_schema_error(SchemaError("no such table: security_events"))


def test_sqlite_store_reuses_session_factory_across_repositories(
    tmp_path: Path,
) -> None:
    store = SqliteStore(tmp_path / "events.db")
    repo_one = SecurityEventRepository(store)
    repo_two = SecurityEventRepository(store)
    try:
        assert repo_one.insert(
            SecurityEvent(
                event_id="store-reuse-one",
                event_type="alpha",
                category="test",
                details={},
            )
        )
        first_session_factory = store.cached_session_factory

        assert repo_two.insert(
            SecurityEvent(
                event_id="store-reuse-two",
                event_type="beta",
                category="test",
                details={},
            )
        )

        assert first_session_factory is not None
        assert store.cached_session_factory is first_session_factory
        assert repo_one.count() == 2
    finally:
        store.close()


def test_security_event_prune_disposes_store_on_sqlalchemy_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    class FailingSessionFactory:
        def begin(self):
            raise SQLAlchemyError("prune failed")

    store = SqliteStore(tmp_path / "events.db")
    repository = SecurityEventRepository(store)
    disposed = False

    def fake_dispose() -> None:
        nonlocal disposed
        disposed = True

    monkeypatch.setattr(store, "session_factory", lambda: FailingSessionFactory())
    monkeypatch.setattr(store, "dispose", fake_dispose)

    repository.prune(30)

    assert disposed


def test_write_store_returns_checked_session_factory_if_cache_is_cleared(
    tmp_path: Path,
) -> None:
    store = _SessionFactoryRaceStore(tmp_path / "events.db")
    try:
        cached_session_factory = store.session_factory()
        assert cached_session_factory is not None

        store.enable_second_session_factory_read_race()

        assert store.session_factory() is cached_session_factory
    finally:
        store.close()


def test_request_schema_repair_is_preserved_during_concurrent_open(
    tmp_path: Path,
) -> None:
    first_open_started = threading.Event()
    allow_first_open_finish = threading.Event()
    repair_flag_set = threading.Event()
    store = _SchemaRepairRaceStore(
        tmp_path / "events.db",
        first_open_started,
        allow_first_open_finish,
        repair_flag_set,
    )
    open_thread = threading.Thread(target=store.session_factory)
    repair_thread = threading.Thread(target=store.request_schema_repair)
    try:
        open_thread.start()
        assert first_open_started.wait(timeout=2)

        repair_thread.start()
        repair_flag_set.wait(timeout=0.2)
        allow_first_open_finish.set()

        open_thread.join(timeout=2)
        repair_thread.join(timeout=2)
        assert not open_thread.is_alive()
        assert not repair_thread.is_alive()

        assert store.session_factory() is not None
        assert store.open_force_values == [False, True]
    finally:
        allow_first_open_finish.set()
        open_thread.join(timeout=2)
        repair_thread.join(timeout=2)
        store.close()


def test_readonly_store_does_not_create_missing_db(tmp_path: Path) -> None:
    missing_db = tmp_path / "missing.db"
    store = SqliteStore(missing_db, read_only=True)

    assert store.session_factory() is None
    assert not missing_db.exists()


def test_write_store_chmods_only_created_parent_dirs(tmp_path: Path) -> None:
    parent = tmp_path / "shared"
    parent.mkdir()
    parent.chmod(0o750)
    db_dir = parent / "new" / "nested"

    store = SqliteStore(db_dir / "events.db")
    try:
        assert store.session_factory() is not None
    finally:
        store.close()

    assert stat.S_IMODE(parent.stat().st_mode) == 0o750
    assert stat.S_IMODE((parent / "new").stat().st_mode) == 0o700
    assert stat.S_IMODE(db_dir.stat().st_mode) == 0o700


def test_readonly_store_warns_without_migrating_unready_schema(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    db_path = tmp_path / "events.db"
    conn = sqlite3.connect(db_path)
    conn.close()

    store = SqliteStore(db_path, read_only=True)
    try:
        assert store.session_factory() is not None
    finally:
        store.close()

    assert "sqlite schema not ready for read-only access" in capsys.readouterr().err

    conn = sqlite3.connect(db_path)
    try:
        assert not {
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
    finally:
        conn.close()


def test_sqlite_store_uses_custom_error_prefix(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    db_path = tmp_path / "observability.db"
    conn = sqlite3.connect(db_path)
    conn.close()

    store = SqliteStore(
        db_path,
        read_only=True,
        models=(SecurityEventRecord,),
        log_prefix="[observability]",
    )
    try:
        assert store.session_factory() is not None
    finally:
        store.close()

    assert "[observability] sqlite schema not ready" in capsys.readouterr().err


def test_store_corruption_cleanup_resets_state_and_allows_reinit(
    tmp_path: Path,
) -> None:
    db_path = tmp_path / "events.db"
    store = SqliteStore(db_path)
    repo = SecurityEventRepository(store)
    repo.insert(
        SecurityEvent(
            event_id="before-corruption",
            event_type="before",
            category="test",
            details={},
        )
    )

    for db_file in sqlite_database_files(db_path):
        db_file.write_bytes(b"stale")

    store.handle_corruption(Exception("corrupt"))

    assert store.engine is None
    assert store.cached_session_factory is None
    assert all(not db_file.exists() for db_file in sqlite_database_files(db_path))

    assert repo.insert(
        SecurityEvent(
            event_id="after-corruption",
            event_type="after",
            category="test",
            details={},
        )
    )
    assert repo.count() == 1
    store.close()
