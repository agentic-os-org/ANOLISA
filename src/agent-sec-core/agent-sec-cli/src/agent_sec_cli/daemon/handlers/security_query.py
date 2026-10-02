"""Read-only daemon handlers for security and observability SQLite data."""

import json
from datetime import datetime
from pathlib import PurePath
from typing import Any

from agent_sec_cli.daemon.errors import BadRequestError
from agent_sec_cli.daemon.protocol import DaemonRequest
from agent_sec_cli.daemon.registry import (
    HandlerResult,
    MethodRegistry,
    MethodSpec,
)
from agent_sec_cli.daemon.runtime import DaemonRuntime
from agent_sec_cli.observability.correlation import (
    ObservabilityRecordFields,
    SecurityCorrelationService,
)
from agent_sec_cli.observability.models import ObservabilityEventRecord
from agent_sec_cli.observability.sqlite_reader import ObservabilityReader
from agent_sec_cli.security_events.schema import SecurityEvent, extract_verdict
from agent_sec_cli.security_events.sqlite_reader import SqliteEventReader
from agent_sec_cli.utils.timestamp import (
    normalize_iso_to_utc_iso,
    ns_to_utc_iso,
    utc_iso_to_epoch,
)

_DEFAULT_LIMIT = 100
_MAX_LIMIT = 1000
_MAX_OFFSET = (1 << 63) - 1
_SUMMARY_LATEST_LIMIT = 5
_EVENT_RESULTS = frozenset({"failed", "succeeded"})
_EVENT_GROUP_FIELDS = {
    "category",
    "event_type",
    "result",
    "trace_id",
    "session_id",
    "run_id",
    "call_id",
    "tool_call_id",
    "verdict",
}


def register_security_query_methods(registry: MethodRegistry) -> None:
    """Register dashboard-oriented read-only query methods."""
    for method, handler in (
        ("sec.summary", security_summary_handler),
        ("sec.events.list", security_events_list_handler),
        ("sec.events.get", security_events_get_handler),
        ("sec.events.count_by", security_events_count_by_handler),
        ("obs.sessions.list", observability_sessions_list_handler),
        ("obs.runs.list", observability_runs_list_handler),
        ("obs.timeline.get", observability_timeline_get_handler),
    ):
        registry.register(
            MethodSpec(
                method=method,
                handler=handler,
                lifecycle="dashboard query",
                queue="dashboard",
                timeout_ms=5000,
                access_log=True,
            )
        )


def security_summary_handler(
    request: DaemonRequest, _runtime: DaemonRuntime
) -> HandlerResult:
    """Return aggregate security event data for dashboard summary cards."""
    filters = _security_filters(request.params)
    latest_limit = _limit_param(
        request.params,
        "latest_limit",
        default=_SUMMARY_LATEST_LIMIT,
        max_value=50,
    )
    reader = SqliteEventReader()
    try:
        summary = reader.summary(**filters, latest_limit=latest_limit)
    finally:
        reader.close()

    return HandlerResult(
        data={
            "total": summary.total,
            "by_category": _count_map(summary.by_category),
            "by_event_type": _count_map(summary.by_event_type),
            "by_result": _count_map(summary.by_result),
            "affected_sessions": _non_empty_group_count(summary.by_session),
            "affected_runs": _non_empty_group_count(summary.by_run),
            "latest_events": [
                _event_payload(event, include_details=False)
                for event in summary.latest_events
            ],
        }
    )


def security_events_list_handler(
    request: DaemonRequest, _runtime: DaemonRuntime
) -> HandlerResult:
    """Return paginated security event rows from SQLite."""
    params = request.params
    filters = _security_filters(params)
    limit = _limit_param(params, "limit", default=_DEFAULT_LIMIT)
    offset = _offset_param(params)
    include_details = _bool_param(params, "include_details", default=False)

    reader = SqliteEventReader()
    try:
        items = reader.query(**filters, limit=limit, offset=offset)
        total = reader.count(**filters)
    finally:
        reader.close()

    next_offset = offset + limit if offset + len(items) < total else None
    return HandlerResult(
        data={
            "items": [
                _event_payload(event, include_details=include_details)
                for event in items
            ],
            "total": total,
            "limit": limit,
            "offset": offset,
            "next_offset": next_offset,
        }
    )


def security_events_get_handler(
    request: DaemonRequest, _runtime: DaemonRuntime
) -> HandlerResult:
    """Return one security event by event_id."""
    event_id = _required_string_param(request.params, "event_id")
    reader = SqliteEventReader()
    try:
        event = reader.get(event_id)
    finally:
        reader.close()

    return HandlerResult(
        data={
            "found": event is not None,
            "event": (
                None if event is None else _event_payload(event, include_details=True)
            ),
        }
    )


def security_events_count_by_handler(
    request: DaemonRequest, _runtime: DaemonRuntime
) -> HandlerResult:
    """Return grouped security event counts from SQLite."""
    params = request.params
    group_by = _required_string_param(params, "group_by")
    if group_by not in _EVENT_GROUP_FIELDS:
        allowed = ", ".join(sorted(_EVENT_GROUP_FIELDS))
        raise BadRequestError(f"group_by must be one of: {allowed}")
    _reject_params(params, ("limit", "offset"), "sec.events.count_by")

    filters = _security_filters(params)
    reader = SqliteEventReader()
    try:
        groups = reader.count_by(group_by, **filters)
    finally:
        reader.close()

    return HandlerResult(
        data={
            "group_by": group_by,
            "items": _count_items(groups),
        }
    )


def observability_sessions_list_handler(
    request: DaemonRequest, _runtime: DaemonRuntime
) -> HandlerResult:
    """Return observability session summaries from SQLite."""
    params = request.params
    start_epoch, end_epoch = _epoch_range(params)
    since, until = _iso_range(params)
    limit = _limit_param(params, "limit", default=_DEFAULT_LIMIT)
    offset = _offset_param(params)

    obs_reader = ObservabilityReader()
    sec_reader = SqliteEventReader()
    try:
        sessions = obs_reader.list_sessions(
            start_epoch=start_epoch,
            end_epoch=end_epoch,
            limit=limit,
            offset=offset,
        )
        total = obs_reader.count_sessions(
            start_epoch=start_epoch,
            end_epoch=end_epoch,
        )
        security_counts = sec_reader.count_by(
            "session_id",
            since=since,
            until=until,
        )
    finally:
        obs_reader.close()
        sec_reader.close()

    return HandlerResult(
        data={
            "items": [
                {
                    "session_id": session.session_id,
                    "first_seen_epoch": session.first_seen_epoch,
                    "last_seen_epoch": session.last_seen_epoch,
                    "turn_count": session.turn_count,
                    "observability_event_count": session.event_count,
                    "security_event_count": int(
                        security_counts.get(session.session_id, 0)
                    ),
                }
                for session in sessions
            ],
            "total": total,
            "limit": limit,
            "offset": offset,
            "next_offset": offset + limit if offset + len(sessions) < total else None,
        }
    )


def observability_runs_list_handler(
    request: DaemonRequest, _runtime: DaemonRuntime
) -> HandlerResult:
    """Return observability run summaries for one session."""
    params = request.params
    session_id = _required_string_param(params, "session_id")
    start_epoch, end_epoch = _epoch_range(params)
    since, until = _iso_range(params)
    limit = _limit_param(params, "limit", default=_DEFAULT_LIMIT)
    offset = _offset_param(params)

    obs_reader = ObservabilityReader()
    sec_reader = SqliteEventReader()
    try:
        runs = obs_reader.list_runs(
            session_id,
            start_epoch=start_epoch,
            end_epoch=end_epoch,
            limit=limit,
            offset=offset,
        )
        total = obs_reader.count_runs(
            session_id,
            start_epoch=start_epoch,
            end_epoch=end_epoch,
        )
        security_counts = sec_reader.count_by(
            "run_id",
            session_id=session_id,
            since=since,
            until=until,
        )
    finally:
        obs_reader.close()
        sec_reader.close()

    return HandlerResult(
        data={
            "session_id": session_id,
            "items": [
                {
                    "run_id": run.run_id,
                    "started_at_epoch": run.started_at_epoch,
                    "ended_at_epoch": run.ended_at_epoch,
                    "user_input_preview": run.user_input_preview,
                    "observability_event_count": run.event_count,
                    "security_event_count": int(security_counts.get(run.run_id, 0)),
                }
                for run in runs
            ],
            "total": total,
            "limit": limit,
            "offset": offset,
            "next_offset": offset + limit if offset + len(runs) < total else None,
        }
    )


def observability_timeline_get_handler(
    request: DaemonRequest, _runtime: DaemonRuntime
) -> HandlerResult:
    """Return one run's observability timeline plus correlated security events."""
    params = request.params
    session_id = _required_string_param(params, "session_id")
    run_id = _required_string_param(params, "run_id")
    start_epoch, end_epoch = _epoch_range(params)
    limit = _limit_param(params, "limit", default=_MAX_LIMIT)
    offset = _offset_param(params)
    include_security = _bool_param(params, "include_security", default=True)

    obs_reader = ObservabilityReader()
    sec_reader = SqliteEventReader()
    try:
        obs_rows = obs_reader.list_events(
            session_id,
            run_id,
            start_epoch=start_epoch,
            end_epoch=end_epoch,
            limit=limit,
            offset=offset,
        )
        items = [_observability_item(row) for row in obs_rows]
        if include_security:
            items.extend(_correlated_security_items(obs_rows, sec_reader))
    finally:
        obs_reader.close()
        sec_reader.close()

    items.sort(key=lambda item: (item["timestamp_epoch"], item["kind"]))
    return HandlerResult(
        data={
            "session_id": session_id,
            "run_id": run_id,
            "limit": limit,
            "offset": offset,
            "items": items,
        }
    )


def _security_filters(params: dict[str, Any]) -> dict[str, str | None]:
    since, until = _iso_range(params)
    return {
        "event_type": _optional_string_param(params, "event_type"),
        "category": _optional_string_param(params, "category"),
        "result": _optional_choice_param(params, "result", _EVENT_RESULTS),
        "trace_id": _optional_string_param(params, "trace_id"),
        "session_id": _optional_string_param(params, "session_id"),
        "run_id": _optional_string_param(params, "run_id"),
        "call_id": _optional_string_param(params, "call_id"),
        "tool_call_id": _optional_string_param(params, "tool_call_id"),
        "verdict": _optional_string_param(params, "verdict"),
        "since": since,
        "until": until,
    }


def _event_payload(event: SecurityEvent, *, include_details: bool) -> dict[str, Any]:
    payload = event.to_dict()
    payload.update(_dashboard_event_fields(event))
    if not include_details:
        payload.pop("details", None)
    return payload


def _dashboard_event_fields(event: SecurityEvent) -> dict[str, str]:
    """Project optional fields needed by dashboard event rows."""
    fields: dict[str, str] = {}
    verdict = extract_verdict(event.details)
    if verdict is not None:
        fields["verdict"] = verdict

    if event.category != "skill_ledger":
        return fields

    result = event.details.get("result")
    result_data = result if isinstance(result, dict) else {}
    request = event.details.get("request")
    request_data = request if isinstance(request, dict) else {}

    command = _first_non_empty_string(
        result_data.get("command"), request_data.get("command")
    )
    if command is not None:
        fields["command"] = command

    if isinstance(result_data.get("results"), list):
        return fields

    skill_name = _first_non_empty_string(result_data.get("skill_name"))
    if skill_name is None:
        skill_dir = _first_non_empty_string(request_data.get("skill_dir"))
        if skill_dir is not None:
            skill_name = PurePath(skill_dir).name or None
    if skill_name is not None:
        fields["skill_name"] = skill_name
    return fields


def _first_non_empty_string(*values: Any) -> str | None:
    for value in values:
        if isinstance(value, str) and value:
            return value
    return None


def _observability_item(row: ObservabilityEventRecord) -> dict[str, Any]:
    return {
        "kind": "observability",
        "id": row.id,
        "hook": row.hook,
        "timestamp": row.observed_at,
        "timestamp_epoch": row.observed_at_epoch,
        "session_id": row.session_id,
        "run_id": row.run_id,
        "call_id": row.call_id,
        "tool_call_id": row.tool_call_id,
        "metadata": _json_object(row.metadata_json),
        "metrics": _json_object(row.metrics_json),
    }


def _observability_context(row: ObservabilityEventRecord) -> dict[str, Any]:
    return {
        "id": row.id,
        "hook": row.hook,
        "timestamp": row.observed_at,
        "timestamp_epoch": row.observed_at_epoch,
        "session_id": row.session_id,
        "run_id": row.run_id,
        "call_id": row.call_id,
        "tool_call_id": row.tool_call_id,
        "metadata": _json_object(row.metadata_json),
        "metrics": _json_object(row.metrics_json),
    }


def _correlated_security_items(
    rows: list[ObservabilityEventRecord],
    reader: SqliteEventReader,
) -> list[dict[str, Any]]:
    correlation = SecurityCorrelationService(reader)
    results = correlation.find_correlated_many([_record_fields(row) for row in rows])
    items: list[dict[str, Any]] = []
    for row, correlated_events in zip(rows, results, strict=True):
        for correlated in correlated_events:
            items.append(
                {
                    "kind": "security",
                    "observability_event_id": row.id,
                    "observability": _observability_context(row),
                    "hook": row.hook,
                    "session_id": row.session_id,
                    "run_id": row.run_id,
                    "call_id": row.call_id,
                    "tool_call_id": row.tool_call_id,
                    "timestamp": correlated.event.timestamp,
                    "timestamp_epoch": correlated.security_timestamp_epoch,
                    "event": _event_payload(
                        correlated.event,
                        include_details=True,
                    ),
                    "match": {
                        "reason": correlated.match_reason,
                        "rank": correlated.match_rank,
                        "time_delta_seconds": correlated.time_delta_seconds,
                    },
                }
            )
    return items


def _record_fields(row: ObservabilityEventRecord) -> ObservabilityRecordFields:
    return ObservabilityRecordFields(
        hook=row.hook,
        session_id=row.session_id,
        run_id=row.run_id,
        tool_call_id=row.tool_call_id,
        observed_at_epoch=row.observed_at_epoch,
        metrics=_json_object(row.metrics_json),
    )


def _iso_range(params: dict[str, Any]) -> tuple[str | None, str | None]:
    since = _optional_string_param(params, "since")
    until = _optional_string_param(params, "until")
    start_ns = params.get("start_ns")
    end_ns = params.get("end_ns")
    if since is not None and start_ns is not None:
        raise BadRequestError("since and start_ns are mutually exclusive")
    if until is not None and end_ns is not None:
        raise BadRequestError("until and end_ns are mutually exclusive")
    if start_ns is not None:
        # Nanosecond filters are absolute epoch time, so convert directly to UTC.
        since = _nanoseconds_to_utc_iso(params, "start_ns")
    elif since is not None:
        # String filters may be naive local time; normalize before reader/repository calls.
        since = _normalize_iso_timestamp(since, "since")
    if end_ns is not None:
        until = _nanoseconds_to_utc_iso(params, "end_ns")
    elif until is not None:
        until = _normalize_iso_timestamp(until, "until")
    _validate_time_range(since, until)
    return since, until


def _nanoseconds_to_utc_iso(params: dict[str, Any], name: str) -> str:
    value = _integer_param(params, name)
    try:
        return ns_to_utc_iso(value)
    except (OSError, OverflowError, ValueError) as exc:
        raise BadRequestError(
            f"{name} is outside the supported timestamp range"
        ) from exc


def _validate_time_range(since: str | None, until: str | None) -> None:
    if since is None or until is None:
        return
    if datetime.fromisoformat(since) > datetime.fromisoformat(until):
        raise BadRequestError("time range start must not be after end")


def _epoch_range(params: dict[str, Any]) -> tuple[float | None, float | None]:
    since, until = _iso_range(params)
    start_epoch = (
        utc_iso_to_epoch(since, field_name="since") if since is not None else None
    )
    end_epoch = (
        utc_iso_to_epoch(until, field_name="until") if until is not None else None
    )
    return start_epoch, end_epoch


def _normalize_iso_timestamp(value: str, name: str) -> str:
    try:
        return normalize_iso_to_utc_iso(value, field_name=name)
    except ValueError as exc:
        raise BadRequestError(f"{name} must be an ISO-8601 timestamp") from exc


def _required_string_param(params: dict[str, Any], name: str) -> str:
    value = _optional_string_param(params, name)
    if value is None:
        raise BadRequestError(f"{name} is required")
    return value


def _optional_string_param(params: dict[str, Any], name: str) -> str | None:
    value = params.get(name)
    if value is None:
        return None
    if not isinstance(value, str):
        raise BadRequestError(f"{name} must be a string")
    value = value.strip()
    return value or None


def _optional_choice_param(
    params: dict[str, Any],
    name: str,
    choices: frozenset[str],
) -> str | None:
    if name not in params or params[name] is None:
        return None
    value = params[name]
    if not isinstance(value, str):
        raise BadRequestError(f"{name} must be a string")
    value = value.strip()
    if value not in choices:
        allowed = ", ".join(sorted(choices))
        raise BadRequestError(f"{name} must be one of: {allowed}")
    return value


def _limit_param(
    params: dict[str, Any],
    name: str,
    *,
    default: int,
    max_value: int = _MAX_LIMIT,
) -> int:
    value = params.get(name, default)
    if not isinstance(value, int) or isinstance(value, bool):
        raise BadRequestError(f"{name} must be an integer")
    if value <= 0:
        raise BadRequestError(f"{name} must be positive")
    if value > max_value:
        raise BadRequestError(f"{name} must not exceed {max_value}")
    return value


def _offset_param(params: dict[str, Any]) -> int:
    value = params.get("offset", 0)
    if not isinstance(value, int) or isinstance(value, bool):
        raise BadRequestError("offset must be an integer")
    if value < 0:
        raise BadRequestError("offset must not be negative")
    if value > _MAX_OFFSET:
        raise BadRequestError(f"offset must not exceed {_MAX_OFFSET}")
    return value


def _integer_param(params: dict[str, Any], name: str) -> int:
    value = params.get(name)
    if not isinstance(value, int) or isinstance(value, bool):
        raise BadRequestError(f"{name} must be an integer")
    return value


def _bool_param(params: dict[str, Any], name: str, *, default: bool) -> bool:
    value = params.get(name, default)
    if not isinstance(value, bool):
        raise BadRequestError(f"{name} must be a boolean")
    return value


def _reject_params(
    params: dict[str, Any],
    names: tuple[str, ...],
    method: str,
) -> None:
    for name in names:
        if name in params:
            raise BadRequestError(f"{name} is not supported for {method}")


def _json_object(raw: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (TypeError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def _count_map(groups: dict[Any, int]) -> dict[str, int]:
    return {
        str(key): int(value)
        for key, value in groups.items()
        if key is not None and key != ""
    }


def _count_items(groups: dict[Any, int]) -> list[dict[str, Any]]:
    items = [
        {"value": key, "count": int(value)}
        for key, value in groups.items()
        if key is not None and key != ""
    ]
    return sorted(items, key=lambda item: (-item["count"], str(item["value"])))


def _non_empty_group_count(groups: dict[Any, int]) -> int:
    return sum(1 for key in groups if key is not None and key != "")
