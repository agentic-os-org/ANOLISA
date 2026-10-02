"""Unit tests for strict telemetry scalar projectors."""

from datetime import datetime

from agent_sec_cli.telemetry.sanitizer import (
    AgentName,
    agent_name_value,
    details_dict,
    enum_value,
    error_type_value,
    integer_value,
    nonnegative_integer_value,
    nonnegative_number_value,
    now_iso,
    result_dict,
    string_value,
    timestamp_value,
)


class StringCanary:
    def __init__(self) -> None:
        self.stringified = False

    def __str__(self) -> str:
        self.stringified = True
        return "CUSTOMER-CANARY"


def test_now_iso_returns_parseable_timestamp() -> None:
    datetime.fromisoformat(now_iso())


def test_timestamp_value_accepts_only_timezone_aware_iso_values() -> None:
    assert timestamp_value(" 2026-07-16T08:00:00Z ") == "2026-07-16T08:00:00+00:00"
    assert timestamp_value("2026-07-16T08:00:00") is None
    assert timestamp_value("CUSTOMER-CANARY") is None
    assert timestamp_value(123) is None


def test_details_and_result_only_accept_dicts() -> None:
    result = {"verdict": "deny"}
    details = {"result": result}

    assert details_dict(details) is details
    assert details_dict(("not", "a", "dict")) == {}
    assert result_dict(details) is result
    assert result_dict({"result": "not-a-dict"}) == {}


def test_string_value_trims_bounds_and_rejects_non_strings() -> None:
    assert string_value(" value ", max_length=10) == "value"
    assert string_value("abcdef", max_length=3) == "abc"
    assert string_value(" ", max_length=10) is None
    assert string_value(" ", max_length=10, allow_empty=True) == ""
    assert string_value(42, max_length=10) is None


def test_agent_name_accepts_only_approved_products() -> None:
    assert {agent_name.value for agent_name in AgentName} == {
        "codex",
        "cosh",
        "hermes",
        "openclaw",
        "qoder",
        "qwencode",
    }
    for agent_name in AgentName:
        assert agent_name_value(f" {agent_name.value} ") == agent_name.value

    for invalid in (
        "future-agent-runtime",
        "customer@example.com",
        "customer-account",
        "x" * 300,
        "Codex",
        False,
        None,
    ):
        assert agent_name_value(invalid) == ""


def test_enum_value_requires_an_approved_exact_string() -> None:
    allowed = frozenset({"pass", "warn"})

    assert enum_value("pass", allowed) == "pass"
    assert enum_value("PASS", allowed) is None
    assert enum_value(1, allowed) is None


def test_error_type_accepts_only_bounded_structured_names() -> None:
    assert error_type_value("ValueError") == "ValueError"
    assert error_type_value("scanner.CodeScanError") == "scanner.CodeScanError"
    assert error_type_value("A" * 128) == "A" * 128
    assert error_type_value("A" * 129) is None
    assert error_type_value("customer error") is None
    assert error_type_value("ValueError: secret") is None
    assert error_type_value("") is None


def test_error_type_never_stringifies_unknown_objects() -> None:
    canary = StringCanary()

    assert error_type_value(canary) is None
    assert canary.stringified is False


def test_integer_projectors_reject_bool_and_coercion() -> None:
    assert integer_value(-9) == -9
    assert integer_value(True) is None
    assert integer_value("9") is None
    assert nonnegative_integer_value(0) == 0
    assert nonnegative_integer_value(-1) is None


def test_number_projector_requires_nonnegative_finite_number() -> None:
    assert nonnegative_number_value(0) == 0
    assert nonnegative_number_value(1.25) == 1.25
    assert nonnegative_number_value(-1) is None
    assert nonnegative_number_value(float("inf")) is None
    assert nonnegative_number_value(float("nan")) is None
    assert nonnegative_number_value(False) is None
