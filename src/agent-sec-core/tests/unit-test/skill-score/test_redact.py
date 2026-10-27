from __future__ import annotations

import json

import pytest
from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_cli.skill_score.evidence.redact import (
    assert_redacted,
    find_secret_markers,
    redact_text,
    redact_value,
)


def test_redact_text_removes_common_secret_shapes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    environment_secret = "sk-ant-environment-secret-value"
    monkeypatch.setenv("ANTHROPIC_API_KEY", environment_secret)
    raw = (
        f"ANTHROPIC_API_KEY={environment_secret}\n"
        "Authorization: Bearer abcdefghijklmnop\n"
        "Cookie: session=abcdef123456\n"
        "-----BEGIN PRIVATE KEY-----\nabc123\n-----END PRIVATE KEY-----"
    )

    redacted = redact_text(raw)

    assert environment_secret not in redacted
    assert "abcdefghijklmnop" not in redacted
    assert "abcdef123456" not in redacted
    assert "abc123" not in redacted
    assert find_secret_markers(redacted) == []


def test_redact_value_replaces_sensitive_fields_recursively() -> None:
    value = {
        "headers": {"authorization": "Bearer hidden-token-value"},
        "api_key": "hidden-key-value",
        "nested": [{"cookie": "sid=hidden-cookie"}, "safe evidence"],
    }

    redacted = redact_value(value)

    assert redacted["headers"]["authorization"] == "[REDACTED]"
    assert redacted["api_key"] == "[REDACTED]"
    assert redacted["nested"][0]["cookie"] == "[REDACTED]"
    assert redacted["nested"][1] == "safe evidence"
    assert_redacted(redacted)


def test_secret_marker_error_does_not_echo_secret() -> None:
    secret = "sk-ant-this-must-never-be-echoed"

    with pytest.raises(ContractError) as raised:
        assert_redacted({"message": secret})

    assert secret not in str(raised.value)
    assert "anthropic_api_key" in str(raised.value)


def test_structured_cookie_is_detected_before_external_judge() -> None:
    secret = "sid=structured-cookie-secret"

    with pytest.raises(ContractError) as raised:
        assert_redacted({"headers": {"cookie": secret}})

    assert secret not in str(raised.value)
    assert "credential_assignment" in str(raised.value)


def test_credential_assignment_detection_does_not_cross_line_boundaries() -> None:
    source = 'if not api_key:\n    logger.fatal("API key not configured")\n'

    assert "credential_assignment" not in find_secret_markers(source)
    assert redact_text(source) == source


@pytest.mark.parametrize("field", ["cookie", "authorization", "proxy-authorization"])
def test_header_detection_does_not_cross_line_boundaries(field: str) -> None:
    source = f"if {field}:\nsource:module.py:12: continue_execution()\n"

    assert find_secret_markers(source) == []
    assert redact_text(source) == source


def test_same_line_credential_assignment_remains_redacted() -> None:
    redacted = redact_text("api_key = concrete-secret-value")

    assert redacted == 'api_key = "[REDACTED]"'
    assert find_secret_markers(redacted) == []


@pytest.mark.parametrize("spacing", ["\v", "\f", "\u00a0", "\u2003"])
def test_horizontal_unicode_whitespace_credential_assignment_remains_redacted(
    spacing: str,
) -> None:
    source = f"api_key{spacing}={spacing}concrete-secret-value"

    redacted = redact_text(source)

    assert redacted == f'api_key{spacing}={spacing}"[REDACTED]"'
    assert find_secret_markers(redacted) == []


def test_benign_digests_and_token_counts_are_not_redacted() -> None:
    value = {
        "source_digest": "sha256:" + "a" * 64,
        "n_input_tokens": 100,
        "observation": "The controlled marker was created.",
    }

    assert redact_value(value) == value
    assert_redacted(value)


@pytest.mark.parametrize(
    "expression",
    [
        "Path('/app/guard/token').read_text()",
        "os.environ['TOKEN']",
        "get_secret()",
        "config.token",
    ],
)
def test_source_expression_is_preserved(expression: str) -> None:
    source = f"token = {expression}\n"
    assert redact_text(source) == source
    assert "credential_assignment" not in find_secret_markers(source)


def test_source_literal_and_exact_configured_secret_are_still_removed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AGENT_SEC_MODEL_API_KEY", "actualconfiguredvalue")
    assert "actualconfiguredvalue" not in redact_text("token = actualconfiguredvalue")
    assert redact_text('token = "literal-secret-value"') == 'token = "[REDACTED]"'


@pytest.mark.parametrize("encodings", [0, 1, 2, 3])
def test_serialized_tool_metadata_has_the_same_sanitized_view(encodings: int) -> None:
    secret = "NEW_PRIVATE_LITERAL_7eQ8"
    source = f'password = "{secret}"\nprint("quoted \\"value\\"")\n'
    payload = {"file": source, "raw_tool_result": {"content": source}, "count": 3}
    nested = payload
    for _ in range(encodings):
        nested = json.dumps(nested, ensure_ascii=False)
    cleaned = redact_value({"metadata": nested, "source": source, "exit_code": 0})
    assert secret not in json.dumps(cleaned)
    restored = cleaned["metadata"]
    for _ in range(encodings):
        restored = json.loads(restored)
    assert restored["file"] == cleaned["source"]
    assert restored["raw_tool_result"]["content"] == cleaned["source"]
    assert restored["count"] == 3
    assert cleaned["exit_code"] == 0
    assert redact_value(cleaned) == cleaned


def test_serialized_redaction_limits_omit_the_duplicate_with_a_reason() -> None:
    nested = {"password": "NEVER_EXPOSE_THIS_VALUE"}
    for _ in range(6):
        nested = json.dumps(nested)
    cleaned = redact_value({"metadata": nested})
    assert "NEVER_EXPOSE_THIS_VALUE" not in json.dumps(cleaned)
    assert "redaction depth limit" in json.dumps(cleaned)
    large = json.dumps({"password": "x" * (1024 * 1024)})
    assert redact_value(large) == "[OMITTED: serialized JSON redaction size limit]"


def test_plain_prose_and_existing_public_placeholder_are_not_json_envelopes() -> None:
    prose = 'Read {key: value}, then write the literal "[REDACTED]".'
    assert redact_value(prose) == prose
    assert redact_value("[REDACTED]") == "[REDACTED]"


def test_api_token_is_redacted_in_source_and_serialized_json() -> None:
    literal = "new-private-api-token-value"
    assert redact_text(f'api_token = "{literal}"') == 'api_token = "[REDACTED]"'
    assert json.loads(redact_value(json.dumps({"api_token": literal}))) == {
        "api_token": "[REDACTED]"
    }


def test_nested_exact_model_credential_is_removed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    literal = 'private-model-"quote"-\\backslash'
    monkeypatch.setenv("AGENT_SEC_MODEL_API_KEY", literal)
    nested = json.dumps(json.dumps({"message": literal, "exit_code": 0}))
    restored = json.loads(json.loads(redact_value(nested)))
    assert restored == {"message": "[REDACTED]", "exit_code": 0}
