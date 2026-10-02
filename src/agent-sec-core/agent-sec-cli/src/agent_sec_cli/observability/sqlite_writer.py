"""SQLAlchemy-backed writer for observability records."""

from pathlib import Path

from agent_sec_cli.observability.config import (
    DEFAULT_OBSERVABILITY_RETENTION_DAYS,
    OBSERVABILITY_LOG_PREFIX,
    get_observability_db_path,
)
from agent_sec_cli.observability.models import (
    OBSERVABILITY_SQLITE_SCHEMA_VERSION,
    ORM_MODELS,
)
from agent_sec_cli.observability.repositories import (
    ObservabilityEventRepository,
)
from agent_sec_cli.observability.schema import ObservabilityRecord
from agent_sec_cli.security_events.orm_store import (
    SqliteStore,
    _is_sqlite_busy_error,
    _is_sqlite_corruption_error,
    _is_sqlite_schema_error,
)
from agent_sec_cli.security_events.sqlite_maintenance import (
    run_sqlite_maintenance_if_due,
)
from sqlalchemy.engine import Engine
from sqlalchemy.exc import DatabaseError, SQLAlchemyError
from sqlalchemy.orm import Session, sessionmaker


class ObservabilitySqliteWriter:
    """SQLite index writer for observability records."""

    def __init__(
        self,
        path: str | Path | None = None,
        max_age_days: int | None = DEFAULT_OBSERVABILITY_RETENTION_DAYS,
    ) -> None:
        self._store = SqliteStore(
            path or get_observability_db_path(),
            models=ORM_MODELS,
            schema_version=OBSERVABILITY_SQLITE_SCHEMA_VERSION,
            log_prefix=OBSERVABILITY_LOG_PREFIX,
        )
        self._repository = ObservabilityEventRepository(self._store)
        self._max_age_days = max_age_days

    @property
    def _engine(self) -> Engine | None:
        return self._store.engine

    @property
    def _session_factory(self) -> sessionmaker[Session] | None:
        return self._store.cached_session_factory

    @property
    def _disabled(self) -> bool:
        return self._store.disabled

    def write(self, record: ObservabilityRecord) -> None:
        """Insert *record* into SQLite. Fire-and-forget index writes never raise."""
        try:
            self.write_or_raise(record)
        except Exception:  # noqa: BLE001
            pass

    def write_or_raise(self, record: ObservabilityRecord) -> None:
        """Insert *record* into SQLite and raise on foreground ingestion failure.

        Distinguishes three failure classes so the engine pool only gets torn
        down for actual I/O / DB faults:

        * ``ValueError`` / ``TypeError`` — caller-supplied record is malformed.
          The repository never touched the engine; just propagate.
        * ``DatabaseError`` — corruption is rebuilt; schema drift requests a
          repair; everything else surfaces.
        * ``SQLAlchemyError`` / ``OSError`` — real I/O or driver fault; the
          pooled engine is potentially stale, so dispose before propagating.
        """
        if self._store.disabled:
            raise OSError("observability SQLite store is disabled")

        try:
            inserted = self._repository.insert_or_raise(record)
        except (ValueError, TypeError):
            raise
        except DatabaseError as exc:
            if _is_sqlite_busy_error(exc):
                raise OSError("observability SQLite database is busy") from exc
            if not _is_sqlite_corruption_error(exc):
                if _is_sqlite_schema_error(exc):
                    self._store.request_schema_repair()
                raise
            self._store.handle_corruption(exc)
            if self._store.disabled:
                raise OSError("observability SQLite store is disabled") from exc
            try:
                inserted = self._repository.insert_or_raise(record)
            except (ValueError, TypeError):
                raise
            except DatabaseError as retry_exc:
                if _is_sqlite_busy_error(retry_exc):
                    raise OSError(
                        "observability SQLite database is busy"
                    ) from retry_exc
                self._store.dispose()
                raise retry_exc from exc
            except Exception as retry_exc:  # noqa: BLE001
                self._store.dispose()
                raise retry_exc from exc
        except (SQLAlchemyError, OSError):
            self._store.dispose()
            raise
        if not inserted:
            raise OSError("observability SQLite write was skipped")

    def close(self) -> None:
        """Best-effort gated prune/WAL checkpoint and dispose pooled connections."""
        if self._store.engine is None:
            return
        try:
            run_sqlite_maintenance_if_due(self._store.path, self._run_maintenance)
        finally:
            self._store.close()

    def _run_maintenance(self) -> None:
        """Run low-frequency SQLite maintenance for this writer."""
        if self._max_age_days is not None:
            self._repository.prune(self._max_age_days)
        self._repository.checkpoint()


__all__ = ["ObservabilitySqliteWriter"]
