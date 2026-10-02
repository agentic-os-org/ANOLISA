"""Correlate observability records to security events for review UI.

Entry points:

* ``SecurityCorrelationService.find_correlated(record)`` — correlate one
  observability record.
* ``SecurityCorrelationService.find_correlated_many(records)`` — correlate a
  batch of records while sharing candidate reads. This is a read optimization
  for the review event list; it must return the same per-record correlations as
  calling ``find_correlated`` for each record independently.

Preconditions
-------------
* Hook must be ``before_tool_call``, ``before_agent_run``, or
  ``after_tool_call``; anything else returns ``[]``.
* ``session_id`` must be present; otherwise no query is issued.
* Candidate categories are constrained by ``SUPPORTED_SECURITY_EVENT_CATEGORIES``:

  - ``before_tool_call`` → ``(code_scan, skill_ledger, pii_scan)``
  - ``before_agent_run`` → ``(prompt_scan, pii_scan)``
  - ``after_tool_call`` → ``(pii_scan,)``

* At most one event per category is returned, in the order listed above.

Matching modes (highest priority first)
---------------------------------------
1. **Exact (``tool_call_id``)** — triggered when the record has non-empty,
   non-zero ``session_id + run_id + tool_call_id``. Requires the event's
   ``session_id / run_id / tool_call_id`` to all match. No time window.
   Yields ``match_rank=0``, ``match_reason="tool_call_id"``.

2. **Run (``run_id``)** — only for hook ``before_agent_run`` with a valid
   ``run_id``. Requires ``session_id + run_id`` to match. No time window.
   Yields ``match_rank=0``, ``match_reason="run_id"``.

3. **Fallback (``field+time``)** — when the prior two don't apply (typically
   because the obs record's ``tool_call_id`` is missing or the all-zero UUID).
   Two filters then per-category field matching:

   a. Time window: ``|event_ts − obs_ts| ≤ FALLBACK_TIME_WINDOW_SECONDS`` (10s).
   b. Soft ``run_id`` constraint: if the record has a real ``run_id`` the event
      must match; if not (missing / zero), the reader is asked for events with
      ``run_id=None``.
   c. ``_field_match_rank`` dispatches by category:

      ====================  ======================================================
      Category              Strategy
      ====================  ======================================================
      ``skill_ledger``      **Skipped** — event stores a resolved absolute
                            ``skill_dir``, obs stores the unresolved logical name;
                            the two live at different abstraction layers, so
                            fallback similarity matching would be misleading.
      ``pii_scan``          Hash-only — ``sha256(obs prompt text)`` or
                            ``obs.metrics.pii_scan_input_sha256`` equals
                            ``event.request.text_sha256``.
      ``prompt_scan``       String similarity between obs
                            ``metrics.{prompt,user_input,text,input}`` and event
                            ``request.{text,prompt,user_input,input}``.
      ``code_scan``         String similarity between obs
                            ``metrics.parameters.{command,cmd,code,script,input}``
                            and event ``request.{code,command,cmd,script}``.
      ====================  ======================================================

   ``_string_match_rank`` normalizes both sides with ``" ".join(s.split())``
   then ranks the comparison:

   - exact equality → ``match_rank=0``
   - either side is a suffix of the other → ``match_rank=1``
   - either side is a prefix of the other → ``match_rank=2``
   - otherwise → no match

   Yields ``match_reason="field+time"``.

Batch candidate reads
---------------------
``find_correlated_many`` does not change the matching modes above. It groups
records only to reduce SQLite round-trips before applying the same per-record
selection logic:

* Exact mode groups records by ``session_id + run_id + categories`` and reads
  candidates with ``tool_call_id IN (...)``.
* Run mode groups records by ``session_id + run_id + categories`` and reads
  candidates once for that run.
* Fallback mode keeps each record's ``observed_at ± 10s`` semantics. It merges
  only overlapping windows whose total span stays within
  ``MAX_FALLBACK_BATCH_WINDOW_SECONDS`` so long runs are not prefetched as one
  large time range.

Per-category selection
----------------------
When multiple candidates of one category pass matching, the smallest
``(match_rank, |time_delta|, security_timestamp_epoch, event_id)`` wins.
Match quality dominates, then proximity in time, then a stable tiebreak.

Example: in fallback, an event whose text exactly equals the obs prompt but
sits 5 s away beats an event that's only a suffix of the prompt at 0.1 s away:
``(0, 5)`` < ``(1, 0.1)``.
"""

import hashlib
from dataclasses import dataclass
from typing import Any, Iterable, Literal, Mapping, Protocol, Sequence

from agent_sec_cli.security_events.schema import SecurityEvent

ZERO_RUN_ID = "00000000-0000-0000-0000-000000000000"
FALLBACK_TIME_WINDOW_SECONDS = 10.0
MAX_FALLBACK_BATCH_WINDOW_SECONDS = 60.0

SUPPORTED_SECURITY_EVENT_CATEGORIES: dict[str, tuple[str, ...]] = {
    "before_tool_call": ("code_scan", "skill_ledger", "pii_scan"),
    "before_agent_run": ("prompt_scan", "pii_scan"),
    "after_tool_call": ("pii_scan",),
}
EXPECTED_PII_SOURCE_BY_HOOK: dict[str, str] = {
    "before_tool_call": "tool_input",
    "after_tool_call": "tool_output",
}

MatchReason = Literal["tool_call_id", "run_id", "field+time"]


@dataclass(frozen=True)
class ObservabilityRecordFields:
    """Plain fields required to correlate one observability record."""

    hook: str
    session_id: str | None
    run_id: str | None
    tool_call_id: str | None
    observed_at_epoch: float
    metrics: Mapping[str, Any] | None = None


@dataclass(frozen=True)
class CorrelatedSecurityEvent:
    """Security event plus correlation metadata computed by the service."""

    event: SecurityEvent
    match_reason: MatchReason
    time_delta_seconds: float
    security_timestamp_epoch: float
    match_rank: int = 0


class _SecurityEventCandidate(Protocol):
    event: SecurityEvent
    timestamp_epoch: float


class _CorrelationReader(Protocol):
    def query_correlation_candidates(
        self,
        *,
        session_id: str,
        categories: tuple[str, ...],
        run_id: str | None,
        tool_call_id: str | None,
        tool_call_ids: Sequence[str] | None = None,
        since_epoch: float | None = None,
        until_epoch: float | None = None,
    ) -> list[_SecurityEventCandidate]:
        pass


@dataclass(frozen=True)
class _FallbackBatchItem:
    index: int
    record: ObservabilityRecordFields
    since_epoch: float
    until_epoch: float


class SecurityCorrelationService:
    """Find security events correlated to one observability record."""

    def __init__(self, reader: _CorrelationReader) -> None:
        self._reader = reader

    def find_correlated(
        self, record: ObservabilityRecordFields
    ) -> list[CorrelatedSecurityEvent]:
        """Return sorted, category-deduplicated security-event correlations."""
        categories = SUPPORTED_SECURITY_EVENT_CATEGORIES.get(record.hook)
        if categories is None or _missing(record.session_id):
            return []

        if _has_tool_call_correlation(record):
            candidates = self._reader.query_correlation_candidates(
                session_id=str(record.session_id),
                categories=categories,
                run_id=record.run_id,
                tool_call_id=record.tool_call_id,
                since_epoch=None,
                until_epoch=None,
            )
            return self._select_by_category(
                record,
                candidates,
                categories,
                "tool_call_id",
            )

        if _has_run_correlation(record):
            candidates = self._reader.query_correlation_candidates(
                session_id=str(record.session_id),
                categories=categories,
                run_id=record.run_id,
                tool_call_id=None,
                since_epoch=None,
                until_epoch=None,
            )
            return self._select_by_category(
                record,
                candidates,
                categories,
                "run_id",
            )

        run_id = None if _missing_run_id(record.run_id) else record.run_id
        candidates = self._reader.query_correlation_candidates(
            session_id=str(record.session_id),
            categories=categories,
            run_id=run_id,
            tool_call_id=None,
            since_epoch=record.observed_at_epoch - FALLBACK_TIME_WINDOW_SECONDS,
            until_epoch=record.observed_at_epoch + FALLBACK_TIME_WINDOW_SECONDS,
        )
        return self._select_by_category(record, candidates, categories, "field+time")

    def find_correlated_many(
        self, records: Sequence[ObservabilityRecordFields]
    ) -> list[list[CorrelatedSecurityEvent]]:
        """Return correlations for many records while sharing candidate queries."""
        results: list[list[CorrelatedSecurityEvent]] = [[] for _ in records]
        exact_groups: dict[
            tuple[str, tuple[str, ...], str],
            list[tuple[int, ObservabilityRecordFields]],
        ] = {}
        run_groups: dict[
            tuple[str, tuple[str, ...], str],
            list[tuple[int, ObservabilityRecordFields]],
        ] = {}
        fallback_groups: dict[
            tuple[str, tuple[str, ...], str | None],
            list[_FallbackBatchItem],
        ] = {}

        for index, record in enumerate(records):
            categories = SUPPORTED_SECURITY_EVENT_CATEGORIES.get(record.hook)
            if categories is None or _missing(record.session_id):
                continue

            session_id = str(record.session_id)
            if _has_tool_call_correlation(record):
                exact_groups.setdefault(
                    (session_id, categories, str(record.run_id)),
                    [],
                ).append((index, record))
                continue

            if _has_run_correlation(record):
                run_groups.setdefault(
                    (session_id, categories, str(record.run_id)),
                    [],
                ).append((index, record))
                continue

            run_id = None if _missing_run_id(record.run_id) else record.run_id
            fallback_groups.setdefault((session_id, categories, run_id), []).append(
                _FallbackBatchItem(
                    index=index,
                    record=record,
                    since_epoch=record.observed_at_epoch - FALLBACK_TIME_WINDOW_SECONDS,
                    until_epoch=record.observed_at_epoch + FALLBACK_TIME_WINDOW_SECONDS,
                )
            )

        for (session_id, categories, run_id), items in exact_groups.items():
            tool_call_ids = tuple(
                sorted(
                    {
                        str(record.tool_call_id)
                        for _, record in items
                        if not _missing(record.tool_call_id)
                    }
                )
            )
            if not tool_call_ids:
                continue
            candidates = self._reader.query_correlation_candidates(
                session_id=session_id,
                categories=categories,
                run_id=run_id,
                tool_call_id=None,
                tool_call_ids=tool_call_ids,
                since_epoch=None,
                until_epoch=None,
            )
            for index, record in items:
                results[index] = self._select_by_category(
                    record,
                    candidates,
                    categories,
                    "tool_call_id",
                )

        for (session_id, categories, run_id), items in run_groups.items():
            candidates = self._reader.query_correlation_candidates(
                session_id=session_id,
                categories=categories,
                run_id=run_id,
                tool_call_id=None,
                since_epoch=None,
                until_epoch=None,
            )
            for index, record in items:
                results[index] = self._select_by_category(
                    record,
                    candidates,
                    categories,
                    "run_id",
                )

        for (session_id, categories, run_id), items in fallback_groups.items():
            candidates_by_index: dict[int, list[_SecurityEventCandidate]] = {
                item.index: [] for item in items
            }
            for window_items in _merge_fallback_batch_items(items):
                since_epoch = min(item.since_epoch for item in window_items)
                until_epoch = max(item.until_epoch for item in window_items)
                candidates = self._reader.query_correlation_candidates(
                    session_id=session_id,
                    categories=categories,
                    run_id=run_id,
                    tool_call_id=None,
                    since_epoch=since_epoch,
                    until_epoch=until_epoch,
                )
                for item in window_items:
                    candidates_by_index[item.index].extend(candidates)

            for item in items:
                results[item.index] = self._select_by_category(
                    item.record,
                    candidates_by_index[item.index],
                    categories,
                    "field+time",
                )

        return results

    def _select_by_category(
        self,
        record: ObservabilityRecordFields,
        candidates: list[_SecurityEventCandidate],
        categories: tuple[str, ...],
        match_reason: MatchReason,
    ) -> list[CorrelatedSecurityEvent]:
        selected: dict[str, CorrelatedSecurityEvent] = {}
        for candidate in candidates:
            match_rank = _candidate_match_rank(
                record,
                candidate,
                categories,
                match_reason,
            )
            if match_rank is None:
                continue
            correlated = CorrelatedSecurityEvent(
                event=candidate.event,
                match_reason=match_reason,
                time_delta_seconds=candidate.timestamp_epoch - record.observed_at_epoch,
                security_timestamp_epoch=candidate.timestamp_epoch,
                match_rank=match_rank,
            )
            current = selected.get(candidate.event.category)
            if current is None or _rank(correlated) < _rank(current):
                selected[candidate.event.category] = correlated

        return [selected[category] for category in categories if category in selected]


def _candidate_match_rank(
    record: ObservabilityRecordFields,
    candidate: _SecurityEventCandidate,
    categories: tuple[str, ...],
    match_reason: MatchReason,
) -> int | None:
    event = candidate.event
    if event.category not in categories:
        return None
    if _missing(event.session_id) or event.session_id != record.session_id:
        return None

    if match_reason == "tool_call_id":
        if (
            not _missing_run_id(event.run_id)
            and event.run_id == record.run_id
            and not _missing(event.tool_call_id)
            and event.tool_call_id == record.tool_call_id
        ):
            return _exact_match_rank(record, event)
        return None

    if match_reason == "run_id":
        if not _missing_run_id(event.run_id) and event.run_id == record.run_id:
            return _exact_match_rank(record, event)
        return None

    if not _missing_run_id(record.run_id) and event.run_id != record.run_id:
        return None
    if (
        abs(candidate.timestamp_epoch - record.observed_at_epoch)
        > FALLBACK_TIME_WINDOW_SECONDS
    ):
        return None
    return _field_match_rank(record, event)


def _exact_match_rank(
    record: ObservabilityRecordFields,
    event: SecurityEvent,
) -> int | None:
    if event.category != "pii_scan":
        return 0
    return _pii_exact_match_rank(record, event)


def _pii_exact_match_rank(
    record: ObservabilityRecordFields,
    event: SecurityEvent,
) -> int | None:
    expected_source = EXPECTED_PII_SOURCE_BY_HOOK.get(record.hook)
    if expected_source is None:
        return 0

    request = event.details.get("request")
    if not isinstance(request, Mapping):
        return None

    source = request.get("source")
    if isinstance(source, str) and source.strip():
        return 0 if source == expected_source else None

    record_values = _observability_match_values(record, event.category)
    return _pii_hash_match_rank(record_values, event)


def _has_tool_call_correlation(record: ObservabilityRecordFields) -> bool:
    return (
        not _missing(record.session_id)
        and not _missing_run_id(record.run_id)
        and not _missing(record.tool_call_id)
    )


def _has_run_correlation(record: ObservabilityRecordFields) -> bool:
    return record.hook == "before_agent_run" and not _missing_run_id(record.run_id)


def _missing(value: str | None) -> bool:
    return value is None or not value.strip()


def _missing_run_id(value: str | None) -> bool:
    return _missing(value) or value == ZERO_RUN_ID


def _merge_fallback_batch_items(
    items: list[_FallbackBatchItem],
) -> list[list[_FallbackBatchItem]]:
    merged: list[list[_FallbackBatchItem]] = []
    current: list[_FallbackBatchItem] = []
    current_since = 0.0
    current_until = 0.0

    for item in sorted(items, key=lambda value: (value.since_epoch, value.until_epoch)):
        if not current:
            current = [item]
            current_since = item.since_epoch
            current_until = item.until_epoch
            continue

        next_until = max(current_until, item.until_epoch)
        if (
            item.since_epoch <= current_until
            and next_until - current_since <= MAX_FALLBACK_BATCH_WINDOW_SECONDS
        ):
            current.append(item)
            current_until = next_until
            continue

        merged.append(current)
        current = [item]
        current_since = item.since_epoch
        current_until = item.until_epoch

    if current:
        merged.append(current)
    return merged


def _field_match_rank(
    record: ObservabilityRecordFields,
    event: SecurityEvent,
) -> int | None:
    # skill_ledger events store a resolved skill_dir (absolute path) while
    # observability records carry the unresolved logical name. The two live
    # at different abstraction layers, so fallback string-similarity matching
    # would be misleading — only the tool_call_id exact mode can relate them.
    if event.category == "skill_ledger":
        return None

    record_values = _observability_match_values(record, event.category)
    if event.category == "pii_scan":
        return _pii_hash_match_rank(record_values, event)

    event_values = _security_event_match_values(event)
    ranks = [
        rank
        for left in record_values
        for right in event_values
        if (rank := _string_match_rank(left, right)) is not None
    ]

    if not ranks:
        return None
    return min(ranks)


def _observability_match_values(
    record: ObservabilityRecordFields,
    category: str,
) -> list[str]:
    metrics = record.metrics
    if not isinstance(metrics, Mapping):
        return []

    if record.hook == "before_agent_run":
        return _strings_from_mapping(
            metrics,
            ("pii_scan_input_sha256", "prompt", "user_input", "text", "input"),
        )

    if (
        record.hook in {"before_tool_call", "after_tool_call"}
        and category == "pii_scan"
    ):
        return _strings_from_mapping(metrics, ("pii_scan_input_sha256",))

    if record.hook != "before_tool_call":
        return []

    parameters = metrics.get("parameters")
    if category == "code_scan":
        return _strings_from_tool_parameters(
            parameters,
            ("command", "cmd", "code", "script", "input"),
        )
    return []


def _security_event_match_values(event: SecurityEvent) -> list[str]:
    request = event.details.get("request")
    if not isinstance(request, Mapping):
        return []

    if event.category == "prompt_scan":
        return _strings_from_mapping(
            request,
            ("text", "prompt", "user_input", "input"),
        )
    if event.category == "code_scan":
        return _strings_from_mapping(request, ("code", "command", "cmd", "script"))
    return []


def _strings_from_tool_parameters(
    value: Any,
    keys: tuple[str, ...],
) -> list[str]:
    if isinstance(value, str):
        return _non_empty_strings((value,))
    if isinstance(value, Mapping):
        return _strings_from_mapping(value, keys)
    return []


def _strings_from_mapping(
    values: Mapping[str, Any],
    keys: tuple[str, ...],
) -> list[str]:
    return _non_empty_strings(values.get(key) for key in keys)


def _non_empty_strings(values: Iterable[Any]) -> list[str]:
    result: list[str] = []
    for value in values:
        if isinstance(value, str) and value.strip():
            result.append(value)
    return result


def _string_match_rank(left: str, right: str) -> int | None:
    normalized_left = _normalize_match_text(left)
    normalized_right = _normalize_match_text(right)
    if not normalized_left or not normalized_right:
        return None
    if normalized_left == normalized_right:
        return 0
    if normalized_left.endswith(normalized_right) or normalized_right.endswith(
        normalized_left
    ):
        return 1
    if normalized_left.startswith(normalized_right) or normalized_right.startswith(
        normalized_left
    ):
        return 2
    return None


def _normalize_match_text(value: str) -> str:
    return " ".join(value.split())


def _pii_hash_match_rank(record_values: list[str], event: SecurityEvent) -> int | None:
    request = event.details.get("request")
    if not isinstance(request, Mapping):
        return None
    expected_hash = request.get("text_sha256")
    if not isinstance(expected_hash, str) or not expected_hash:
        return None

    for value in record_values:
        if _is_sha256_hex(value) and value == expected_hash:
            return 0
        actual_hash = hashlib.sha256(value.encode("utf-8")).hexdigest()
        if actual_hash == expected_hash:
            return 0
    return None


def _is_sha256_hex(value: str) -> bool:
    return len(value) == 64 and all(char in "0123456789abcdef" for char in value)


def _rank(correlation: CorrelatedSecurityEvent) -> tuple[int, float, float, str]:
    return (
        correlation.match_rank,
        abs(correlation.time_delta_seconds),
        correlation.security_timestamp_epoch,
        correlation.event.event_id,
    )


__all__ = [
    "CorrelatedSecurityEvent",
    "FALLBACK_TIME_WINDOW_SECONDS",
    "ObservabilityRecordFields",
    "SUPPORTED_SECURITY_EVENT_CATEGORIES",
    "SecurityCorrelationService",
    "ZERO_RUN_ID",
]
