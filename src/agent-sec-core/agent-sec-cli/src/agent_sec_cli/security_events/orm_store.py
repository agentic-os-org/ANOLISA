"""SQLAlchemy SQLite storage primitives for security event indexes."""

import re
import sqlite3
import sys
import threading
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import Any

from agent_sec_cli.security_events.orm_base import Base
from agent_sec_cli.security_events.schema_version import (
    SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
)
from sqlalchemy import create_engine, event, inspect, text
from sqlalchemy.engine import URL, Connection, Engine
from sqlalchemy.exc import DatabaseError, SQLAlchemyError
from sqlalchemy.orm import Session, sessionmaker
from sqlalchemy.schema import CreateIndex, CreateTable

_SCHEMA_VERSION = SECURITY_EVENTS_SQLITE_SCHEMA_VERSION
_SQLITE_PRIMARY_CODE_MASK = 0xFF
_SQLITE_CORRUPTION_CODES = {
    sqlite3.SQLITE_CORRUPT,
    sqlite3.SQLITE_NOTADB,
}
_SQLITE_BUSY_CODES = {
    sqlite3.SQLITE_BUSY,
    sqlite3.SQLITE_LOCKED,
}
_SQLITE_SCHEMA_ERROR_MARKERS = (
    "database schema has changed",
    "has no column named",
    "no such column",
    "no such table",
)
_IDENTIFIER_RE = re.compile(r"^[a-z_]+$")

OrmModel = type[Base]
SchemaMigration = Callable[[Connection, int, int, tuple[OrmModel, ...], str], None]
_DEFAULT_MODELS: tuple[OrmModel, ...] = ()


def normalize_sqlite_path(path: str | Path) -> Path:
    """Return a normalized filesystem path for SQLite state."""
    return Path(path).expanduser().resolve()


def create_sqlite_engine(path: Path, *, read_only: bool = False) -> Engine:
    """Create a pooled SQLAlchemy engine for a SQLite DB."""
    if read_only:
        url = URL.create(
            "sqlite+pysqlite",
            database=f"file:{path.as_posix()}",
            query={"mode": "ro", "uri": "true"},
        )
    else:
        url = URL.create("sqlite+pysqlite", database=str(path))

    engine = create_engine(
        url,
        connect_args={"check_same_thread": False},
        future=True,
    )

    @event.listens_for(engine, "connect")
    def _configure_connection(
        dbapi_connection: sqlite3.Connection,
        _connection_record: Any,
    ) -> None:
        cursor = dbapi_connection.cursor()
        try:
            cursor.execute("PRAGMA busy_timeout=200")
            cursor.execute("PRAGMA foreign_keys=ON")
            if read_only:
                cursor.execute("PRAGMA query_only=ON")
            else:
                cursor.execute("PRAGMA synchronous=NORMAL")
                cursor.execute("PRAGMA wal_autocheckpoint=100")
        finally:
            cursor.close()

    return engine


def _require_models(models: tuple[OrmModel, ...]) -> tuple[OrmModel, ...]:
    if models:
        return models
    raise ValueError(
        "No ORM models registered for SQLite schema initialization; "
        "import agent_sec_cli.security_events.models or pass models explicitly"
    )


def register_orm_models(models: Iterable[OrmModel]) -> None:
    """Register default ORM models for schema initialization."""
    global _DEFAULT_MODELS  # noqa: PLW0603
    _DEFAULT_MODELS = _require_models(tuple(models))


def _coerce_models(models: Iterable[OrmModel] | None) -> tuple[OrmModel, ...]:
    return _require_models(tuple(models) if models is not None else _DEFAULT_MODELS)


def _warn_newer_schema_version(
    version: int,
    supported_version: int,
    log_prefix: str = "[security_events]",
) -> None:
    print(
        f"{log_prefix} sqlite schema version {version} is newer than "
        f"this binary supports ({supported_version}); skipping schema migration",
        file=sys.stderr,
    )


def _schema_readiness(
    conn: Connection,
    models: tuple[OrmModel, ...],
    schema_version: int,
) -> tuple[int, list[str]]:
    version = int(conn.execute(text("PRAGMA user_version")).scalar_one())
    if version > schema_version:
        return version, []

    inspector = inspect(conn)
    missing_tables = [
        model.__table__.name
        for model in models
        if not inspector.has_table(model.__table__.name)
    ]
    return version, missing_tables


def _schema_version(conn: Connection) -> int:
    return int(conn.execute(text("PRAGMA user_version")).scalar_one())


def ensure_schema(
    engine: Engine,
    models: Iterable[OrmModel] | None = None,
    *,
    schema_version: int = _SCHEMA_VERSION,
    schema_migrations: SchemaMigration | None = None,
    log_prefix: str = "[security_events]",
) -> None:
    """Create model tables/indexes and apply convergent column migrations."""
    model_tuple = _coerce_models(models)
    with engine.connect() as conn:
        conn.execute(text("PRAGMA journal_mode=WAL"))
        version = conn.execute(text("PRAGMA user_version")).scalar_one()
        if version > schema_version:
            _warn_newer_schema_version(int(version), schema_version, log_prefix)
            conn.commit()
            return

        conn.execute(text("PRAGMA auto_vacuum = INCREMENTAL"))
        if version < schema_version and schema_migrations is not None:
            schema_migrations(
                conn,
                int(version),
                schema_version,
                model_tuple,
                log_prefix,
            )
        conn.commit()

    with engine.begin() as conn:
        version = conn.execute(text("PRAGMA user_version")).scalar_one()
        if version > schema_version:
            _warn_newer_schema_version(int(version), schema_version, log_prefix)
            return

        conn.execute(text("PRAGMA auto_vacuum = INCREMENTAL"))

        for model in model_tuple:
            table = model.__table__
            conn.execute(CreateTable(table, if_not_exists=True))

            extra_columns = getattr(model, "__schema_columns__", {})
            if extra_columns:
                existing = {
                    column["name"] for column in inspect(conn).get_columns(table.name)
                }
                for col, typedef in extra_columns.items():
                    if col not in existing:
                        if not _IDENTIFIER_RE.match(col):
                            raise ValueError(f"Invalid column name in schema: {col!r}")
                        conn.execute(
                            text(f"ALTER TABLE {table.name} ADD COLUMN {col} {typedef}")
                        )

            for index in table.indexes:
                conn.execute(CreateIndex(index, if_not_exists=True))

        if version < schema_version:
            conn.execute(text(f"PRAGMA user_version = {schema_version}"))


def ensure_schema_if_needed(
    engine: Engine,
    models: Iterable[OrmModel] | None = None,
    *,
    force: bool = False,
    schema_version: int = _SCHEMA_VERSION,
    schema_migrations: SchemaMigration | None = None,
    log_prefix: str = "[security_events]",
) -> None:
    """Run full schema convergence only when version changes or repair is forced."""
    model_tuple = _coerce_models(models)
    with engine.connect() as conn:
        version = _schema_version(conn)
        if version > schema_version:
            _warn_newer_schema_version(int(version), schema_version, log_prefix)
            return

        if version == schema_version and not force:
            return

    ensure_schema(
        engine,
        model_tuple,
        schema_version=schema_version,
        schema_migrations=schema_migrations,
        log_prefix=log_prefix,
    )


def warn_readonly_schema_readiness(
    engine: Engine,
    models: Iterable[OrmModel] | None = None,
    *,
    schema_version: int = _SCHEMA_VERSION,
    log_prefix: str = "[security_events]",
) -> None:
    """Warn about read-only schema drift without creating or migrating anything."""
    model_tuple = _coerce_models(models)
    with engine.connect() as conn:
        version, missing_tables = _schema_readiness(conn, model_tuple, schema_version)

    if version > schema_version:
        _warn_newer_schema_version(int(version), schema_version, log_prefix)
    elif version < schema_version and not missing_tables and version != 0:
        print(
            f"{log_prefix} sqlite schema is v{version}, "
            f"this binary expects v{schema_version}; "
            "run any write command (for example `agent-sec-cli scan-code ...`) "
            "to migrate. read-only queries may return empty results until then.",
            file=sys.stderr,
        )
    elif missing_tables or version == 0:
        print(
            f"{log_prefix} sqlite schema not ready for read-only access: "
            f"version={version}, expected={schema_version}, "
            f"missing_tables={missing_tables}",
            file=sys.stderr,
        )


def sqlite_database_files(path: Path) -> tuple[Path, Path, Path]:
    """Return the main DB path and SQLite sidecar paths."""
    return (
        path,
        Path(str(path) + "-wal"),
        Path(str(path) + "-shm"),
    )


def _sqlite_primary_error_code(exc: Exception) -> int | None:
    """Return the SQLite primary result code for DBAPI/SQLAlchemy exceptions."""
    for candidate in (getattr(exc, "orig", None), exc):
        code = getattr(candidate, "sqlite_errorcode", None)
        if isinstance(code, int):
            return code & _SQLITE_PRIMARY_CODE_MASK
    return None


def _is_sqlite_corruption_error(exc: Exception) -> bool:
    """Return True only for errors that indicate true DB corruption."""
    code = _sqlite_primary_error_code(exc)
    return code in _SQLITE_CORRUPTION_CODES


def _is_sqlite_busy_error(exc: Exception) -> bool:
    """Return True for SQLite busy/locked errors after the busy timeout expires."""
    code = _sqlite_primary_error_code(exc)
    return code in _SQLITE_BUSY_CODES


def _is_sqlite_schema_error(exc: Exception) -> bool:
    """Return True for errors that can be repaired by schema convergence."""
    code = _sqlite_primary_error_code(exc)
    if code == sqlite3.SQLITE_SCHEMA:
        return True
    message = str(getattr(exc, "orig", exc)).lower()
    return any(marker in message for marker in _SQLITE_SCHEMA_ERROR_MARKERS)


class SqliteStore:
    """Shared SQLite engine/session lifecycle for typed repositories."""

    def __init__(
        self,
        path: str | Path,
        *,
        read_only: bool = False,
        models: Iterable[OrmModel] | None = None,
        schema_version: int = _SCHEMA_VERSION,
        schema_migrations: SchemaMigration | None = None,
        log_prefix: str = "[security_events]",
    ) -> None:
        self.path = normalize_sqlite_path(path)
        self.read_only = read_only
        self.models = _coerce_models(models)
        self.schema_version = schema_version
        self.schema_migrations = schema_migrations
        self._log_prefix = log_prefix
        self._engine_lock = threading.RLock()
        self._engine: Engine | None = None
        self._session_factory: sessionmaker[Session] | None = None
        self._db_identity: tuple[int, int] | None = None
        self._disabled = False
        self._force_schema_convergence = False

    @property
    def engine(self) -> Engine | None:
        """Return the cached engine, if initialized."""
        return self._engine

    @property
    def cached_session_factory(self) -> sessionmaker[Session] | None:
        """Return the cached session factory, if initialized."""
        return self._session_factory

    @property
    def disabled(self) -> bool:
        """Return True when corruption cleanup failed and writes are disabled."""
        return self._disabled

    def session_factory(
        self,
        *,
        raise_on_error: bool = False,
    ) -> sessionmaker[Session] | None:
        """Return a lazily initialized session factory."""
        with self._engine_lock:
            if self._disabled:
                return None

            db_identity = None
            if self.read_only:
                db_identity = self._current_db_identity()
                if db_identity is None:
                    self.dispose()
                    return None
                session_factory = self._session_factory
                if session_factory is not None and self._db_identity == db_identity:
                    return session_factory
                self.dispose()
            else:
                session_factory = self._session_factory
                if session_factory is not None:
                    return session_factory

            try:
                self._open_session_factory(db_identity)
            except DatabaseError as exc:
                if raise_on_error:
                    raise
                if self.read_only or not _is_sqlite_corruption_error(exc):
                    print(
                        f"{self._log_prefix} schema init failure: {exc}",
                        file=sys.stderr,
                    )
                    return None
                self.handle_corruption(exc)
                if self._disabled:
                    return None
                try:
                    self._open_session_factory(None)
                except (SQLAlchemyError, OSError) as rebuild_exc:
                    if raise_on_error:
                        raise
                    print(
                        f"{self._log_prefix} corruption rebuild failed: {rebuild_exc}",
                        file=sys.stderr,
                    )
                    return None
            except (SQLAlchemyError, OSError) as exc:
                if raise_on_error:
                    raise
                print(
                    f"{self._log_prefix} schema init failure: {exc}",
                    file=sys.stderr,
                )
                return None

            session_factory = self._session_factory
            return session_factory

    def dispose(self) -> None:
        """Dispose SQLAlchemy engine state and clear cached session state."""
        with self._engine_lock:
            if self._engine is not None:
                try:
                    self._engine.dispose()
                except Exception:  # noqa: BLE001
                    pass
            self._engine = None
            self._session_factory = None
            self._db_identity = None

    def close(self) -> None:
        """Dispose cached SQLAlchemy connections."""
        self.dispose()

    def request_schema_repair(self) -> None:
        """Force full schema convergence the next time this store opens."""
        with self._engine_lock:
            self._force_schema_convergence = True
            self.dispose()

    def handle_corruption(self, exc: Exception) -> None:
        """Delete a corrupt expendable SQLite query index and clear state."""
        print(
            f"{self._log_prefix} corrupt DB detected, recreating: {exc}",
            file=sys.stderr,
        )
        self.dispose()
        try:
            for db_file in sqlite_database_files(self.path):
                db_file.unlink(missing_ok=True)
        except OSError as delete_exc:
            self._disabled = True
            print(
                f"{self._log_prefix} cannot delete corrupt db, "
                f"writer disabled: {delete_exc}",
                file=sys.stderr,
            )

    def _open_session_factory(self, db_identity: tuple[int, int] | None) -> None:
        force_schema = self._force_schema_convergence
        if not self.read_only:
            self._ensure_write_parent()

        engine = create_sqlite_engine(self.path, read_only=self.read_only)
        try:
            if self.read_only:
                warn_readonly_schema_readiness(
                    engine,
                    self.models,
                    schema_version=self.schema_version,
                    log_prefix=self._log_prefix,
                )
            else:
                ensure_schema_if_needed(
                    engine,
                    self.models,
                    force=force_schema,
                    schema_version=self.schema_version,
                    schema_migrations=self.schema_migrations,
                    log_prefix=self._log_prefix,
                )
            self._engine = engine
            self._db_identity = db_identity
            self._session_factory = sessionmaker(
                bind=engine,
                expire_on_commit=False,
                future=True,
            )
            if not self.read_only:
                try:
                    self.path.chmod(0o600)
                except OSError:
                    pass
            self._force_schema_convergence = False
        except Exception:
            engine.dispose()
            raise

    def _ensure_write_parent(self) -> None:
        parent = self.path.parent
        created_dirs: list[Path] = []
        current = parent
        while not current.exists() and current.parent != current:
            created_dirs.append(current)
            current = current.parent

        parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        for directory in created_dirs:
            try:
                directory.chmod(0o700)
            except OSError:
                pass

    def _current_db_identity(self) -> tuple[int, int] | None:
        try:
            stat_result = self.path.stat()
        except OSError:
            return None
        return (stat_result.st_dev, stat_result.st_ino)


__all__ = [
    "Base",
    "SqliteStore",
    "create_sqlite_engine",
    "ensure_schema",
    "ensure_schema_if_needed",
    "normalize_sqlite_path",
    "register_orm_models",
    "SchemaMigration",
    "sqlite_database_files",
    "warn_readonly_schema_readiness",
]
