"""Typed, read-only access to Tokenless compression statistics."""

from __future__ import annotations

import json
import os
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from pathlib import Path
from types import MappingProxyType
from typing import Any, Literal, TypeVar

from anolisa_tokenless._native import _StatsQuery

_T = TypeVar("_T")


class StatsNotFoundError(LookupError):
    """Raised when a requested statistics record or session does not exist."""


class StatsOperation(StrEnum):
    """Operation recorded by Tokenless statistics."""

    COMPRESS_SCHEMA = "compress-schema"
    COMPRESS_RESPONSE = "compress-response"
    REWRITE_COMMAND = "rewrite-command"
    COMPRESS_TOON = "compress-toon"


class StatsMode(StrEnum):
    """Whether recorded savings were applied or predicted."""

    ACTIVE = "active"
    DRY_RUN = "dry-run"


class StatsDiffSort(StrEnum):
    """Ordering for chains in a session diff."""

    SAVED = "saved"
    TIME = "time"


@dataclass(frozen=True)
class StatsStatus:
    """Availability and location of one statistics database."""

    data_dir: str
    database_path: str
    available: bool
    error: str | None
    records: int | None


@dataclass(frozen=True)
class StatsSavings:
    """Aggregate character and estimated-token savings."""

    records: int
    before_chars: int
    after_chars: int
    chars_saved: int
    chars_saved_percent: float
    before_tokens: int
    after_tokens: int
    tokens_saved: int
    tokens_saved_percent: float


@dataclass(frozen=True)
class StatsAttribution:
    """Net-savings attribution over the active records of a summary.

    The retrieve fields are ``None`` when retrieve totals were unavailable
    (the summary then reports gross savings only).
    """

    gross_savings_tokens: int
    compressions_with_unrecoverable_truncations: int
    unrecoverable_truncation_events: int
    retrieved_tokens: int | None
    net_savings_tokens: int | None
    retrieve_hits: int | None
    retrieve_misses: int | None
    retrieve_errors: int | None


@dataclass(frozen=True)
class StatsSummary:
    """Overall savings with a breakdown by operation.

    Aggregates active records only; ``dry_run_records_excluded`` counts the
    dry-run rows the summary left out.
    """

    schema_version: str
    total: StatsSavings
    by_operation: Mapping[StatsOperation, StatsSavings]
    dry_run_records_excluded: int
    attribution: StatsAttribution


@dataclass(frozen=True)
class StatsRecord:
    """One recorded operation; content fields are populated only by ``show``."""

    id: int
    timestamp: datetime
    operation: StatsOperation
    agent_id: str
    source_pid: int | None
    session_id: str | None
    tool_use_id: str | None
    before_chars: int
    before_tokens: int
    after_chars: int
    after_tokens: int
    before_text: str | None
    after_text: str | None
    before_output: str | None
    after_output: str | None
    mode: StatsMode
    stash_writes: int | None
    stash_errors: int | None
    stash_size: int | None
    content_type: str | None
    content_origin: str | None
    applied_operations: tuple[str, ...] | None
    recoverability: str | None
    tokenizer_id: str | None
    unrecoverable_truncations: int | None

    @property
    def chars_saved(self) -> int:
        """Return characters removed by this operation."""
        return max(self.before_chars - self.after_chars, 0)

    @property
    def tokens_saved(self) -> int:
        """Return estimated tokens removed by this operation."""
        return max(self.before_tokens - self.after_tokens, 0)

    @property
    def chars_saved_percent(self) -> float:
        """Return the percentage of input characters removed."""
        if self.before_chars == 0:
            return 0.0
        return self.chars_saved / self.before_chars * 100.0

    @property
    def tokens_saved_percent(self) -> float:
        """Return the percentage of estimated input tokens removed."""
        if self.before_tokens == 0:
            return 0.0
        return self.tokens_saved / self.before_tokens * 100.0


@dataclass(frozen=True)
class StatsComparison:
    """Estimated-token comparison between baseline and Tokenless sessions."""

    schema_version: str
    baseline_tokens: int
    tokenless_tokens: int
    saved_tokens: int
    saved_percent: float
    baseline_by_operation: Mapping[StatsOperation, int]
    tokenless_by_operation: Mapping[StatsOperation, int]


@dataclass(frozen=True)
class StatsDiffScope:
    """Record, session, or tool-use scope represented by a diff."""

    kind: Literal["record", "session", "tool-use"]
    record_id: int | None
    session_id: str | None
    tool_use_id: str | None


@dataclass(frozen=True)
class StatsStashMetrics:
    """Stash metrics captured for one compression stage."""

    writes: int | None
    errors: int | None
    size: int | None


@dataclass(frozen=True)
class StatsDiffStage:
    """One recorded stage in a linked compression chain."""

    record_id: int
    timestamp: datetime
    operation: StatsOperation
    agent_id: str
    mode: StatsMode
    before_bytes: int
    after_bytes: int
    before_tokens: int
    after_tokens: int
    emitted_tokens: int
    saved_tokens: int
    saved_percent: float
    stash: StatsStashMetrics | None


@dataclass(frozen=True)
class StatsDiffLine:
    """One context, deletion, or insertion line in a content diff."""

    kind: Literal["context", "delete", "insert"]
    old_line: int | None
    new_line: int | None
    text: str


@dataclass(frozen=True)
class StatsDiffHunk:
    """One bounded unified-diff hunk."""

    old_start: int
    old_len: int
    new_start: int
    new_len: int
    lines: tuple[StatsDiffLine, ...]


@dataclass(frozen=True)
class StatsContentDiff:
    """Structured content diff, including omission and truncation state."""

    available: bool
    normalization: Literal["none", "json"]
    truncated: bool
    omitted_reason: Literal["missing-content", "content-too-large"] | None
    hunks: tuple[StatsDiffHunk, ...]


@dataclass(frozen=True)
class StatsDiffChain:
    """Standalone or linked sequence of token-saving stages."""

    status: Literal["standalone", "linked"]
    mode: StatsMode
    agent_id: str
    session_id: str | None
    tool_use_id: str | None
    started_at: datetime
    before_bytes: int
    after_bytes: int
    before_tokens: int
    after_tokens: int
    emitted_tokens: int
    saved_tokens: int
    saved_percent: float
    stages: tuple[StatsDiffStage, ...]
    diff: StatsContentDiff | None


@dataclass(frozen=True)
class StatsDiff:
    """Structured record, session, or tool-use savings report."""

    schema_version: str
    scope: StatsDiffScope
    saving_records_only: bool
    split_chains: bool
    chains: tuple[StatsDiffChain, ...]


class TokenlessStats:
    """Query one Tokenless ``stats.db`` without clearing data or changing settings.

    Opening the client follows CLI initialization and may create or migrate ``stats.db``, so the
    data directory must be writable.
    """

    def __init__(self, data_dir: str | os.PathLike[str] | None = None) -> None:
        if data_dir is None:
            native_data_dir = None
        else:
            path = Path(data_dir).expanduser()
            if not path.is_absolute():
                raise ValueError("data_dir must be an absolute path")
            native_data_dir = os.fspath(path)
        self._native = _StatsQuery(native_data_dir)

    @property
    def status(self) -> StatsStatus:
        """Return database availability, location, and record count."""
        value = _load_json(self._native.status_json())
        return StatsStatus(
            data_dir=value["data_dir"],
            database_path=value["database_path"],
            available=value["available"],
            error=value["error"],
            records=value["records"],
        )

    def summary(self, *, limit: int | None = None) -> StatsSummary:
        """Return aggregate savings for the newest records within ``limit``.

        ``None`` uses the recorder's default cap of 10,000 newest records.
        """
        _validate_limit(limit, optional=True)
        value = _load_json(self._native.summary_json(limit))
        attribution = value["attribution"]
        return StatsSummary(
            schema_version=value["schema_version"],
            total=_parse_savings(value["total"]),
            by_operation=_operation_mapping(value["by_operation"], _parse_savings),
            dry_run_records_excluded=value["dry_run_records_excluded"],
            attribution=StatsAttribution(
                gross_savings_tokens=attribution["gross_savings_tokens"],
                compressions_with_unrecoverable_truncations=attribution[
                    "compressions_with_unrecoverable_truncations"
                ],
                unrecoverable_truncation_events=attribution[
                    "unrecoverable_truncation_events"
                ],
                retrieved_tokens=attribution.get("retrieved_tokens"),
                net_savings_tokens=attribution.get("net_savings_tokens"),
                retrieve_hits=attribution.get("retrieve_hits"),
                retrieve_misses=attribution.get("retrieve_misses"),
                retrieve_errors=attribution.get("retrieve_errors"),
            ),
        )

    def list(self, *, limit: int = 20) -> tuple[StatsRecord, ...]:
        """List newest records without returning their stored content."""
        _validate_limit(limit)
        return tuple(
            _parse_record(value) for value in _load_json(self._native.list_json(limit))
        )

    def show(self, record_id: int) -> StatsRecord:
        """Return one record, including its potentially sensitive stored content."""
        _validate_record_id(record_id)
        value = self._native.show_json(record_id)
        if value is None:
            raise StatsNotFoundError(f"statistics record {record_id} was not found")
        return _parse_record(_load_json(value))

    def diff(
        self,
        *,
        record_id: int | None = None,
        session_id: str | None = None,
        tool_use_id: str | None = None,
        limit: int = 20,
        sort: StatsDiffSort | Literal["saved", "time"] = StatsDiffSort.SAVED,
        context: int = 3,
    ) -> StatsDiff:
        """Explain estimated savings, ordered by ``"saved"`` or ``"time"`` for sessions.

        Session and tool-use queries inspect at most the newest 10,000 matching records.
        """
        if (record_id is None) == (session_id is None):
            raise ValueError("specify exactly one of record_id or session_id")
        if tool_use_id is not None and session_id is None:
            raise ValueError("tool_use_id requires session_id")
        if record_id is not None:
            _validate_record_id(record_id)
        if session_id is not None and not session_id:
            raise ValueError("session_id must not be empty")
        if tool_use_id is not None and not tool_use_id:
            raise ValueError("tool_use_id must not be empty")
        _validate_limit(limit)
        _validate_context(context)
        sort = StatsDiffSort(sort)
        value = self._native.diff_json(
            record_id=record_id,
            session_id=session_id,
            tool_use_id=tool_use_id,
            limit=limit,
            sort=sort.value,
            context=context,
        )
        if value is None:
            scope = (
                f"record {record_id}"
                if record_id is not None
                else f"session {session_id!r}"
            )
            if tool_use_id is not None:
                scope = f"tool use {tool_use_id!r} in {scope}"
            raise StatsNotFoundError(f"no statistics found for {scope}")
        return _parse_diff(_load_json(value))

    def compare(
        self,
        baseline_session_id: str,
        tokenless_session_id: str,
        *,
        limit: int | None = None,
    ) -> StatsComparison:
        """Compare baseline input tokens with Tokenless emitted tokens.

        The baseline should be a dry-run session and the Tokenless session should be active.
        ``None`` uses the recorder's default cap of 10,000 newest records per session.
        """
        if not baseline_session_id or not tokenless_session_id:
            raise ValueError("session identifiers must not be empty")
        _validate_limit(limit, optional=True)
        parsed = _load_json(
            self._native.compare_json(
                baseline_session_id,
                tokenless_session_id,
                limit,
            )
        )
        if missing_sessions := parsed.get("missing_sessions"):
            labels = {
                "baseline": f"baseline session {baseline_session_id!r}",
                "tokenless": f"Tokenless session {tokenless_session_id!r}",
            }
            missing = " and ".join(labels[role] for role in missing_sessions)
            raise StatsNotFoundError(f"no statistics records for {missing}")
        return StatsComparison(
            schema_version=parsed["schema_version"],
            baseline_tokens=parsed["baseline_tokens"],
            tokenless_tokens=parsed["tokenless_tokens"],
            saved_tokens=parsed["saved_tokens"],
            saved_percent=parsed["saved_percent"],
            baseline_by_operation=_operation_mapping(
                parsed["baseline_by_operation"], int
            ),
            tokenless_by_operation=_operation_mapping(
                parsed["tokenless_by_operation"], int
            ),
        )


def _load_json(value: str) -> Any:
    return json.loads(value)


def _validate_limit(limit: int | None, *, optional: bool = False) -> None:
    if limit is None and optional:
        return
    if not isinstance(limit, int) or isinstance(limit, bool) or limit <= 0:
        raise ValueError("limit must be a positive integer")


def _validate_record_id(record_id: int) -> None:
    if not isinstance(record_id, int) or isinstance(record_id, bool) or record_id <= 0:
        raise ValueError("record_id must be a positive integer")


def _validate_context(context: int) -> None:
    if not isinstance(context, int) or isinstance(context, bool) or context < 0:
        raise ValueError("context must be a non-negative integer")


def _parse_savings(value: Mapping[str, Any]) -> StatsSavings:
    return StatsSavings(
        records=value["records"],
        before_chars=value["before_chars"],
        after_chars=value["after_chars"],
        chars_saved=value["chars_saved"],
        chars_saved_percent=value["chars_saved_percent"],
        before_tokens=value["before_tokens"],
        after_tokens=value["after_tokens"],
        tokens_saved=value["tokens_saved"],
        tokens_saved_percent=value["tokens_saved_percent"],
    )


def _operation_mapping(
    values: Mapping[str, Any], parser: Callable[[Any], _T]
) -> Mapping[StatsOperation, _T]:
    return MappingProxyType(
        {StatsOperation(operation): parser(value) for operation, value in values.items()}
    )


def _parse_record(value: Mapping[str, Any]) -> StatsRecord:
    applied_operations = value.get("applied_operations")
    return StatsRecord(
        id=value["id"],
        timestamp=datetime.fromisoformat(value["timestamp"]),
        operation=StatsOperation(value["operation"]),
        agent_id=value["agent_id"],
        source_pid=value["source_pid"],
        session_id=value["session_id"],
        tool_use_id=value["tool_use_id"],
        before_chars=value["before_chars"],
        before_tokens=value["before_tokens"],
        after_chars=value["after_chars"],
        after_tokens=value["after_tokens"],
        before_text=value.get("before_text"),
        after_text=value.get("after_text"),
        before_output=value.get("before_output"),
        after_output=value.get("after_output"),
        mode=StatsMode(value["mode"]),
        stash_writes=value["stash_writes"],
        stash_errors=value["stash_errors"],
        stash_size=value["stash_size"],
        content_type=value.get("content_type"),
        content_origin=value.get("content_origin"),
        applied_operations=(
            None if applied_operations is None else tuple(applied_operations)
        ),
        recoverability=value.get("recoverability"),
        tokenizer_id=value.get("tokenizer_id"),
        unrecoverable_truncations=value.get("unrecoverable_truncations"),
    )


def _parse_diff(value: Mapping[str, Any]) -> StatsDiff:
    scope = value["scope"]
    return StatsDiff(
        schema_version=value["schema_version"],
        scope=StatsDiffScope(
            kind=scope["kind"],
            record_id=scope.get("record_id"),
            session_id=scope.get("session_id"),
            tool_use_id=scope.get("tool_use_id"),
        ),
        saving_records_only=value["saving_records_only"],
        split_chains=value["split_chains"],
        chains=tuple(_parse_diff_chain(chain) for chain in value["chains"]),
    )


def _parse_diff_chain(value: Mapping[str, Any]) -> StatsDiffChain:
    content_diff = value.get("diff")
    return StatsDiffChain(
        status=value["status"],
        mode=StatsMode(value["mode"]),
        agent_id=value["agent_id"],
        session_id=value.get("session_id"),
        tool_use_id=value.get("tool_use_id"),
        started_at=datetime.fromisoformat(value["started_at"]),
        before_bytes=value["before_bytes"],
        after_bytes=value["after_bytes"],
        before_tokens=value["before_tokens"],
        after_tokens=value["after_tokens"],
        emitted_tokens=value["emitted_tokens"],
        saved_tokens=value["saved_tokens"],
        saved_percent=value["saved_percent"],
        stages=tuple(_parse_diff_stage(stage) for stage in value["stages"]),
        diff=_parse_content_diff(content_diff) if content_diff is not None else None,
    )


def _parse_diff_stage(value: Mapping[str, Any]) -> StatsDiffStage:
    stash = value.get("stash")
    return StatsDiffStage(
        record_id=value["record_id"],
        timestamp=datetime.fromisoformat(value["timestamp"]),
        operation=StatsOperation(value["operation"]),
        agent_id=value["agent_id"],
        mode=StatsMode(value["mode"]),
        before_bytes=value["before_bytes"],
        after_bytes=value["after_bytes"],
        before_tokens=value["before_tokens"],
        after_tokens=value["after_tokens"],
        emitted_tokens=value["emitted_tokens"],
        saved_tokens=value["saved_tokens"],
        saved_percent=value["saved_percent"],
        stash=(
            StatsStashMetrics(
                writes=stash.get("writes"),
                errors=stash.get("errors"),
                size=stash.get("size"),
            )
            if stash is not None
            else None
        ),
    )


def _parse_content_diff(value: Mapping[str, Any]) -> StatsContentDiff:
    return StatsContentDiff(
        available=value["available"],
        normalization=value["normalization"],
        truncated=value["truncated"],
        omitted_reason=value.get("omitted_reason"),
        hunks=tuple(
            StatsDiffHunk(
                old_start=hunk["old_start"],
                old_len=hunk["old_len"],
                new_start=hunk["new_start"],
                new_len=hunk["new_len"],
                lines=tuple(
                    StatsDiffLine(
                        kind=line["kind"],
                        old_line=line.get("old_line"),
                        new_line=line.get("new_line"),
                        text=line["text"],
                    )
                    for line in hunk["lines"]
                ),
            )
            for hunk in value["hunks"]
        ),
    )
