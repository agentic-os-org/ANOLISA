"""Unit tests for telemetry field projection."""

# ruff: noqa: I001

import copy
import json
from dataclasses import dataclass
from typing import Any

import pytest
from agent_sec_cli import __version__
from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.telemetry.sanitizer import AgentName
from agent_sec_cli.telemetry.schema import build_telemetry_security_event

COMPONENT_FIELDS = {
    "component.name",
    "component.version",
    "component.agent_name",
}
SECCORE_COMMON_FIELDS = {
    "seccore.event_type",
    "seccore.category",
    "seccore.result",
    "seccore.timestamp",
}
SCAN_FIELDS = {"seccore.verdict", "seccore.elapsed_ms"}
ASSET_FIELDS = {"seccore.asset_passed_count", "seccore.asset_failed_count"}
ASSET_OUTCOME_FIELD = {"seccore.asset_outcome"}
BASELINE_FIELDS = {
    "baseline.result",
    "baseline.timestamp",
    "baseline.passed",
    "baseline.fixed",
    "baseline.failed",
    "baseline.total",
}


@dataclass
class _TelemetryCtx:
    agent_name: str | None = None


class StringCanary:
    def __init__(self) -> None:
        self.stringified = False

    def __str__(self) -> str:
        self.stringified = True
        return "CUSTOMER-CANARY"


def _event(**overrides: Any) -> SecurityEvent:
    defaults: dict[str, Any] = {
        "event_id": "event-1",
        "event_type": "code_scan",
        "category": "code_scan",
        "result": "succeeded",
        "timestamp": "2026-06-15T12:00:00+00:00",
        "trace_id": "trace-CUSTOMER-CANARY",
        "session_id": "session-CUSTOMER-CANARY",
        "run_id": "run-CUSTOMER-CANARY",
        "call_id": "call-CUSTOMER-CANARY",
        "tool_call_id": "tool-CUSTOMER-CANARY",
        "details": {},
    }
    defaults.update(overrides)
    return SecurityEvent(**defaults)


def _assert_component_fields(record: dict[str, Any], agent_name: str = "") -> None:
    assert record["component.name"] == "agent-sec-core"
    assert record["component.version"] == __version__
    assert record["component.agent_name"] == agent_name


@pytest.mark.parametrize(
    ("action", "category"),
    [
        ("sandbox_prehook", "sandbox"),
        ("summary", "summary"),
        ("skill_ledger", "skill_ledger"),
    ],
)
def test_envelope_only_actions_have_exact_common_fields(
    action: str, category: str
) -> None:
    event = _event(
        event_type=action,
        category=category,
        details={
            "request": {"prompt": "CUSTOMER-CANARY"},
            "result": {"verdict": "deny", "summary": "CUSTOMER-CANARY"},
            "error": "CUSTOMER-CANARY",
            "unknown_customer_field": "CUSTOMER-CANARY",
        },
    )

    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert set(record) == COMPONENT_FIELDS | SECCORE_COMMON_FIELDS
    assert record["seccore.event_type"] == action
    assert record["seccore.category"] == category
    assert "CUSTOMER-CANARY" not in json.dumps(record)


@pytest.mark.parametrize("action", ["code_scan", "prompt_scan", "pii_scan"])
def test_scan_actions_share_verdict_and_elapsed_fields(action: str) -> None:
    event = _event(
        event_type=action,
        category=action,
        details={
            "request": {"text": "CUSTOMER-CANARY"},
            "result": {
                "verdict": "deny",
                "elapsed_ms": 28.5,
                "summary": {"customer": "CUSTOMER-CANARY"},
            },
        },
    )

    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert set(record) == COMPONENT_FIELDS | SECCORE_COMMON_FIELDS | SCAN_FIELDS
    assert record["seccore.verdict"] == "deny"
    assert record["seccore.elapsed_ms"] == 28.5
    assert "seccore.request" not in record
    assert "seccore.summary" not in record
    assert "CUSTOMER-CANARY" not in json.dumps(record)


def test_legacy_asset_verify_has_only_approved_numeric_counts() -> None:
    event = _event(
        event_type="verify",
        category="asset_verify",
        result="failed",
        details={
            "request": {"skill": "/CUSTOMER-CANARY"},
            "result": {"passed": 12, "failed": 1, "findings": "CUSTOMER-CANARY"},
        },
    )

    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert set(record) == COMPONENT_FIELDS | SECCORE_COMMON_FIELDS | ASSET_FIELDS
    assert record["seccore.category"] == "asset_verify"
    assert record["seccore.asset_passed_count"] == 12
    assert record["seccore.asset_failed_count"] == 1


@pytest.mark.parametrize("outcome", ["verified", "failed", "no_candidates"])
def test_asset_verify_projects_approved_outcome(outcome: str) -> None:
    event = _event(
        event_type="verify",
        category="asset_verify",
        details={
            "result": {
                "outcome": outcome,
                "passed": 0,
                "failed": 0,
            }
        },
    )

    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert set(record) == (
        COMPONENT_FIELDS | SECCORE_COMMON_FIELDS | ASSET_FIELDS | ASSET_OUTCOME_FIELD
    )
    assert record["seccore.asset_outcome"] == outcome


def test_asset_verify_omits_unapproved_outcome() -> None:
    event = _event(
        event_type="verify",
        category="asset_verify",
        details={"result": {"outcome": "CUSTOMER-CANARY"}},
    )

    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert set(record) == COMPONENT_FIELDS | SECCORE_COMMON_FIELDS


def test_asset_verify_operation_error_omits_outcome() -> None:
    event = _event(
        event_type="verify",
        category="asset_verify",
        result="failed",
        details={
            "result": {},
            "error_type": "PermissionError",
            "exit_code": 1,
        },
    )

    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert "seccore.asset_outcome" not in record
    assert record["seccore.error_type"] == "PermissionError"
    assert record["seccore.exit_code"] == 1


def test_harden_has_exact_baseline_fields() -> None:
    event = _event(
        event_type="harden",
        category="hardening",
        result="failed",
        details={
            "request": {"args": ["--config", "/CUSTOMER-CANARY"]},
            "result": {
                "passed": 12,
                "fixed": 2,
                "failed": 1,
                "total": 15,
                "output": "CUSTOMER-CANARY",
            },
        },
    )

    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert set(record) == COMPONENT_FIELDS | BASELINE_FIELDS
    assert record["baseline.result"] == "failed"
    assert record["baseline.passed"] == 12
    assert record["baseline.fixed"] == 2
    assert record["baseline.failed"] == 1
    assert record["baseline.total"] == 15
    assert not any(key.startswith("seccore.") for key in record)
    assert "CUSTOMER-CANARY" not in json.dumps(record)


def test_agent_name_only_emits_approved_product_names() -> None:
    for agent_name in AgentName:
        record = build_telemetry_security_event(
            _event(), _TelemetryCtx(agent_name=f" {agent_name.value} ")
        )

        assert record is not None
        _assert_component_fields(record, agent_name=agent_name.value)

    for invalid in ("future-agent-runtime", "customer@example.com", "customer-account"):
        record = build_telemetry_security_event(
            _event(), _TelemetryCtx(agent_name=invalid)
        )

        _assert_component_fields(record)


def test_new_action_uses_common_fields_without_special_projection() -> None:
    record = build_telemetry_security_event(
        _event(
            event_type="future_action",
            category="future_category",
            details={"result": {"verdict": "deny", "summary": "CUSTOMER-CANARY"}},
        ),
        _TelemetryCtx(),
    )

    assert set(record) == COMPONENT_FIELDS | SECCORE_COMMON_FIELDS
    assert record["seccore.event_type"] == "future_action"
    assert record["seccore.category"] == "future_category"
    assert "CUSTOMER-CANARY" not in json.dumps(record)


def test_untrusted_timestamp_text_is_replaced_not_forwarded() -> None:
    event = _event()
    event.timestamp = "CUSTOMER-CANARY"
    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert record["seccore.timestamp"] != "CUSTOMER-CANARY"
    assert "CUSTOMER-CANARY" not in json.dumps(record)


def test_structured_error_type_and_exit_code_are_emitted_without_message() -> None:
    event = _event(
        event_type="code_scan",
        result="failed",
        details={
            "error": "failed for /CUSTOMER-CANARY and secret prompt",
            "error_details": {"traceback": "CUSTOMER-CANARY"},
            "error_type": "CodeScanError",
            "exit_code": -9,
            "result": {"verdict": "error", "elapsed_ms": 4},
        },
    )

    record = build_telemetry_security_event(event, _TelemetryCtx())

    assert record is not None
    assert record["seccore.error_type"] == "CodeScanError"
    assert record["seccore.exit_code"] == -9
    assert "seccore.error" not in record
    assert "CUSTOMER-CANARY" not in json.dumps(record)


def test_error_metadata_requires_failed_result_and_valid_top_level_type() -> None:
    successful = build_telemetry_security_event(
        _event(
            result="succeeded",
            details={"error_type": "RuntimeError", "exit_code": 1},
        ),
        _TelemetryCtx(),
    )
    nested = build_telemetry_security_event(
        _event(
            result="failed",
            details={
                "result": {
                    "verdict": "error",
                    "summary": {"error_type": "RuntimeError"},
                },
                "exit_code": 1,
            },
        ),
        _TelemetryCtx(),
    )
    invalid = build_telemetry_security_event(
        _event(
            result="failed",
            details={"error_type": "RuntimeError: CUSTOMER-CANARY", "exit_code": 1},
        ),
        _TelemetryCtx(),
    )

    for record in (successful, nested, invalid):
        assert record is not None
        assert "seccore.error_type" not in record
        assert "seccore.exit_code" not in record


def test_baseline_uses_same_structured_error_contract() -> None:
    record = build_telemetry_security_event(
        _event(
            event_type="harden",
            category="hardening",
            result="failed",
            details={
                "error": "CUSTOMER-CANARY",
                "error_type": "FileNotFoundError",
                "exit_code": 127,
                "result": {},
            },
        ),
        _TelemetryCtx(),
    )

    assert record is not None
    assert record["baseline.error_type"] == "FileNotFoundError"
    assert record["baseline.exit_code"] == 127
    assert "baseline.error" not in record


def test_invalid_optional_values_are_omitted_without_losing_safe_envelope() -> None:
    record = build_telemetry_security_event(
        _event(
            event_type="code_scan",
            details={"result": {"verdict": "critical", "elapsed_ms": float("nan")}},
        ),
        _TelemetryCtx(),
    )

    assert record is not None
    assert set(record) == COMPONENT_FIELDS | SECCORE_COMMON_FIELDS


def test_bool_and_negative_counts_are_not_treated_as_valid_counts() -> None:
    verify_record = build_telemetry_security_event(
        _event(
            event_type="verify",
            category="asset_verify",
            details={"result": {"passed": True, "failed": -1}},
        ),
        _TelemetryCtx(),
    )
    baseline_record = build_telemetry_security_event(
        _event(
            event_type="harden",
            category="hardening",
            details={"result": {"passed": True, "fixed": -1, "failed": 0, "total": 1}},
        ),
        _TelemetryCtx(),
    )

    assert verify_record is not None
    assert set(verify_record) == COMPONENT_FIELDS | SECCORE_COMMON_FIELDS
    assert baseline_record is not None
    assert "baseline.passed" not in baseline_record
    assert "baseline.fixed" not in baseline_record
    assert baseline_record["baseline.failed"] == 0
    assert baseline_record["baseline.total"] == 1


def test_mapping_does_not_mutate_or_stringify_unapproved_values() -> None:
    canary = StringCanary()
    details = {
        "request": {"prompt": canary},
        "result": {"verdict": "pass", "elapsed_ms": 1, "unknown": canary},
        "error": canary,
        "unknown": canary,
    }
    original = copy.deepcopy(details)

    record = build_telemetry_security_event(
        _event(details=details), _TelemetryCtx(agent_name="qwencode")
    )

    assert record is not None
    assert details.keys() == original.keys()
    assert details["request"].keys() == original["request"].keys()
    assert canary.stringified is False
    json.dumps(record, allow_nan=False)
