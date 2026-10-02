"""Build telemetry records from SecurityEvent values."""

from typing import Any, Protocol

from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.telemetry.config import get_component_fields
from agent_sec_cli.telemetry.sanitizer import (
    agent_name_value,
    details_dict,
    enum_value,
    error_type_value,
    integer_value,
    nonnegative_integer_value,
    nonnegative_number_value,
    now_iso,
    result_dict,
    timestamp_value,
)

_VERDICTS = frozenset({"pass", "warn", "deny", "error"})
_RESULTS = frozenset({"succeeded", "failed"})
_ASSET_OUTCOMES = frozenset({"verified", "failed", "no_candidates"})
_BASELINE_ACTION = "harden"
_ASSET_VERIFY_ACTION = "verify"
_SCAN_ACTIONS = frozenset({"code_scan", "prompt_scan", "pii_scan"})


class TelemetryContext(Protocol):
    """Context fields consumed by telemetry mapping."""

    agent_name: str | None


def build_telemetry_security_event(
    event: SecurityEvent,
    ctx: TelemetryContext,
) -> dict[str, Any]:
    """Build a telemetry record using action-specific field projections."""
    if event.event_type == _BASELINE_ACTION:
        return _build_baseline_record(event, ctx)
    return _build_seccore_record(event, ctx)


def _build_seccore_record(
    event: SecurityEvent,
    ctx: TelemetryContext,
) -> dict[str, Any]:
    """Build an allowlisted seccore.* telemetry record."""
    details = details_dict(event.details)
    result = result_dict(details)
    record: dict[str, Any] = _component_fields(ctx)
    record.update(
        {
            "seccore.event_type": event.event_type,
            "seccore.category": event.category,
            "seccore.result": enum_value(event.result, _RESULTS) or "failed",
            "seccore.timestamp": _timestamp(event),
        }
    )

    if event.event_type in _SCAN_ACTIONS:
        _add_optional(
            record,
            "seccore.verdict",
            enum_value(result.get("verdict"), _VERDICTS),
        )
        _add_optional(
            record,
            "seccore.elapsed_ms",
            nonnegative_number_value(result.get("elapsed_ms")),
        )

    if event.event_type == _ASSET_VERIFY_ACTION:
        _add_optional(
            record,
            "seccore.asset_outcome",
            enum_value(result.get("outcome"), _ASSET_OUTCOMES),
        )
        _add_optional(
            record,
            "seccore.asset_passed_count",
            nonnegative_integer_value(result.get("passed")),
        )
        _add_optional(
            record,
            "seccore.asset_failed_count",
            nonnegative_integer_value(result.get("failed")),
        )

    _add_error_fields(
        record, details, namespace="seccore", failed=event.result == "failed"
    )
    return record


def _build_baseline_record(
    event: SecurityEvent,
    ctx: TelemetryContext,
) -> dict[str, Any]:
    """Build an allowlisted baseline.* telemetry record."""
    details = details_dict(event.details)
    result = result_dict(details)
    record: dict[str, Any] = _component_fields(ctx)
    record.update(
        {
            "baseline.result": enum_value(event.result, _RESULTS) or "failed",
            "baseline.timestamp": _timestamp(event),
        }
    )
    for key in ("passed", "fixed", "failed", "total"):
        _add_optional(
            record,
            f"baseline.{key}",
            nonnegative_integer_value(result.get(key)),
        )
    _add_error_fields(
        record, details, namespace="baseline", failed=event.result == "failed"
    )
    return record


def _component_fields(ctx: TelemetryContext) -> dict[str, str]:
    """Return component fields with runtime agent_name resolved at mapping time."""
    fields = get_component_fields()
    fields["component.agent_name"] = _component_agent_name(ctx)
    return fields


def _component_agent_name(ctx: TelemetryContext) -> str:
    return agent_name_value(ctx.agent_name)


def _timestamp(event: SecurityEvent) -> str:
    """Return the source timestamp or generate one when missing."""
    return timestamp_value(event.timestamp) or now_iso()


def _add_error_fields(
    record: dict[str, Any],
    details: dict[str, Any],
    *,
    namespace: str,
    failed: bool,
) -> None:
    if not failed:
        return
    normalized_error_type = error_type_value(details.get("error_type"))
    if normalized_error_type is None:
        return
    record[f"{namespace}.error_type"] = normalized_error_type
    _add_optional(
        record,
        f"{namespace}.exit_code",
        integer_value(details.get("exit_code")),
    )


def _add_optional(record: dict[str, Any], key: str, value: Any) -> None:
    if value is not None:
        record[key] = value
