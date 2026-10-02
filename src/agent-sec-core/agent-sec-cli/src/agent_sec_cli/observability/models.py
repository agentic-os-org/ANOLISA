"""SQLAlchemy ORM models for observability indexes."""

from agent_sec_cli.security_events.orm_base import Base
from sqlalchemy import Float, Index, Integer, Text
from sqlalchemy.orm import Mapped, mapped_column

OBSERVABILITY_SQLITE_SCHEMA_VERSION = 1


class ObservabilityEventRecord(Base):
    """ORM mapping for the queryable observability event index."""

    __tablename__ = "observability_events"
    __table_args__ = (
        Index("idx_observability_observed_at_epoch", "observed_at_epoch"),
        Index("idx_observability_hook_observed_at_epoch", "hook", "observed_at_epoch"),
        Index(
            "idx_observability_session_observed_at_epoch",
            "session_id",
            "observed_at_epoch",
        ),
        Index(
            "idx_observability_session_run_observed_at_epoch",
            "session_id",
            "run_id",
            "observed_at_epoch",
        ),
    )

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    hook: Mapped[str] = mapped_column(Text, nullable=False)
    observed_at: Mapped[str] = mapped_column(Text, nullable=False)
    observed_at_epoch: Mapped[float] = mapped_column(Float, nullable=False)
    session_id: Mapped[str] = mapped_column(Text, nullable=False)
    run_id: Mapped[str] = mapped_column(Text, nullable=False)
    metrics_json: Mapped[str] = mapped_column(Text, nullable=False)
    metadata_json: Mapped[str] = mapped_column(Text, nullable=False)
    call_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    tool_call_id: Mapped[str | None] = mapped_column(Text, nullable=True)


ORM_MODELS = (ObservabilityEventRecord,)


__all__ = [
    "OBSERVABILITY_SQLITE_SCHEMA_VERSION",
    "ORM_MODELS",
    "ObservabilityEventRecord",
]
