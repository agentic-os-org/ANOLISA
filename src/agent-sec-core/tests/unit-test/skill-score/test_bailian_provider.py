"""Unit tests for the Bailian pay-as-you-go Anthropic provider."""

from __future__ import annotations

import ssl
import threading
from concurrent.futures import ThreadPoolExecutor
from types import SimpleNamespace
from typing import Any

import pytest
from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_cli.skill_score.provider import bailian as bailian_provider
from agent_sec_cli.skill_score.provider.bailian import (
    ProviderCallError,
    build_runtime_env,
    call_messages,
    call_messages_result,
    extract_final_text,
    extract_token_usage,
    resolve_payg_auth,
    validate_payg_anthropic_base_url,
    warm_messages_transport,
)


@pytest.mark.parametrize(
    "base_url",
    [
        "https://dashscope.aliyuncs.com/apps/anthropic",
        "https://dashscope-intl.aliyuncs.com/apps/anthropic",
        "https://dashscope-us.aliyuncs.com/apps/anthropic",
        "https://ws-abc123.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        "https://ws-abc123.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
        "https://ws-abc123.us-east-1.maas.aliyuncs.com/apps/anthropic",
        "https://ws-abc123.eu-central-1.maas.aliyuncs.com/apps/anthropic",
        "https://ws-abc123.ap-northeast-1.maas.aliyuncs.com/apps/anthropic/",
    ],
)
def test_validates_documented_payg_endpoints(base_url: str) -> None:
    assert validate_payg_anthropic_base_url(base_url).endswith("/apps/anthropic")


@pytest.mark.parametrize(
    "base_url",
    [
        "http://dashscope.aliyuncs.com/apps/anthropic",
        "https://user@dashscope.aliyuncs.com/apps/anthropic",
        "https://dashscope.aliyuncs.com:443/apps/anthropic",
        "https://dashscope.aliyuncs.com:/apps/anthropic",
        "https://dashscope.aliyuncs.com/apps/anthropic?",
        "https://dashscope.aliyuncs.com/apps/anthropic?key=value",
        "https://dashscope.aliyuncs.com/apps/anthropic#",
        "https://dashscope.aliyuncs.com/apps/anthropic#fragment",
        "https://dashscope.aliyuncs.com/v1/messages",
        "https://coding.dashscope.aliyuncs.com/apps/anthropic",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        "https://trial.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        "https://workspace.unknown.maas.aliyuncs.com/apps/anthropic",
        "https://maas.aliyuncs.com/apps/anthropic",
        "https://dashscope.aliyuncs.com.evil.example/apps/anthropic",
        "https://example.com/apps/anthropic",
    ],
)
def test_rejects_non_payg_or_unsafe_endpoints(base_url: str) -> None:
    with pytest.raises(ContractError):
        validate_payg_anthropic_base_url(base_url)


def test_builds_claude_runtime_env_from_one_key() -> None:
    result = build_runtime_env(
        api_key="sk-unit-test-placeholder",
        base_url="https://dashscope.aliyuncs.com/apps/anthropic/",
        model="qwen3.7-plus",
    )

    assert result == {
        "ANTHROPIC_AUTH_TOKEN": "sk-unit-test-placeholder",
        "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic",
        "ANTHROPIC_MODEL": "qwen3.7-plus",
    }


def test_resolves_one_key_for_runtime_and_judge() -> None:
    secret = "sk-unit-test-placeholder"
    auth = resolve_payg_auth(
        environment={
            "ANTHROPIC_API_KEY": secret,
            "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic/",
            "ANTHROPIC_AUTH_TOKEN": "must-not-be-used",
        }
    )

    assert auth.api_key == secret
    assert secret not in repr(auth)
    assert auth.api_base_url == "https://dashscope.aliyuncs.com/apps/anthropic"
    assert auth.runtime_env(
        model="qwen3.7-plus",
        small_fast_model="qwen3.7-plus",
    ) == {
        "ANTHROPIC_AUTH_TOKEN": secret,
        "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic",
        "ANTHROPIC_MODEL": "qwen3.7-plus",
        "ANTHROPIC_SMALL_FAST_MODEL": "qwen3.7-plus",
    }


@pytest.mark.parametrize(
    "environment",
    [
        {},
        {"ANTHROPIC_API_KEY": "sk-unit-test-placeholder"},
        {
            "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic",
        },
        {
            "ANTHROPIC_AUTH_TOKEN": "must-not-be-used",
            "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic",
        },
    ],
)
def test_shared_auth_requires_the_api_key_and_base_pair(
    environment: dict[str, str],
) -> None:
    with pytest.raises(ContractError):
        resolve_payg_auth(environment=environment)


class _Messages:
    def __init__(self, response: Any) -> None:
        self.response = response
        self.arguments: dict[str, Any] | None = None

    def create(self, **arguments: Any) -> Any:
        self.arguments = arguments
        return self.response


class _Client:
    def __init__(self, messages: _Messages) -> None:
        self.messages = messages


def test_warms_and_closes_sdk_transport_without_sending_a_message() -> None:
    factory_arguments: dict[str, Any] = {}

    class WarmClient:
        closed = False

        def close(self) -> None:
            self.closed = True

    client = WarmClient()

    def factory(**arguments: Any) -> WarmClient:
        factory_arguments.update(arguments)
        return client

    warm_messages_transport(
        api_key="sk-unit-test-placeholder",
        api_base_url="https://dashscope.aliyuncs.com/apps/anthropic/",
        client_factory=factory,
    )

    assert client.closed is True
    assert factory_arguments == {
        "api_key": "sk-unit-test-placeholder",
        "base_url": "https://dashscope.aliyuncs.com/apps/anthropic",
        "timeout": 120.0,
        "max_retries": 0,
    }


def test_calls_messages_with_thinking_disabled_and_extracts_text() -> None:
    messages = _Messages(
        SimpleNamespace(
            content=[
                SimpleNamespace(type="thinking", thinking="ignored"),
                SimpleNamespace(type="text", text="first"),
                {"type": "text", "text": "second"},
            ]
        )
    )
    factory_arguments: dict[str, Any] = {}

    def factory(**arguments: Any) -> _Client:
        factory_arguments.update(arguments)
        return _Client(messages)

    result = call_messages(
        api_key="sk-unit-test-placeholder",
        api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
        model="qwen3.7-plus",
        user_message="Judge this finding.",
        system_message="Return JSON.",
        max_tokens=512,
        client_factory=factory,
    )

    assert result == "first\nsecond"
    assert factory_arguments == {
        "api_key": "sk-unit-test-placeholder",
        "base_url": "https://dashscope.aliyuncs.com/apps/anthropic",
        "timeout": 120.0,
        "max_retries": 0,
    }
    assert messages.arguments == {
        "model": "qwen3.7-plus",
        "max_tokens": 512,
        "messages": [{"role": "user", "content": "Judge this finding."}],
        "thinking": {"type": "disabled"},
        "extra_body": {"temperature": 0},
        "system": "Return JSON.",
    }


def test_closes_message_client_after_success_and_ignores_close_return() -> None:
    class ClosingClient(_Client):
        close_calls = 0

        def close(self) -> object:
            self.close_calls += 1
            return object()

    client = ClosingClient(_Messages({"content": [{"type": "text", "text": "ok"}]}))

    result = call_messages(
        api_key="sk-unit-test-placeholder",
        api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
        model="qwen3.8-max",
        user_message="Judge this finding.",
        client_factory=lambda **_: client,
    )

    assert result == "ok"
    assert client.close_calls == 1


def test_closes_message_client_without_masking_provider_failure() -> None:
    class RateLimitError(RuntimeError):
        status_code = 429

    class FailingMessages:
        def create(self, **_arguments: Any) -> Any:
            raise RateLimitError("primary provider failure")

    class FailingCloseClient:
        def __init__(self) -> None:
            self.messages = FailingMessages()
            self.close_calls = 0

        def close(self) -> None:
            self.close_calls += 1
            raise RuntimeError("secondary close failure")

    client = FailingCloseClient()

    with pytest.raises(ProviderCallError) as raised:
        call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message="Judge this finding.",
            client_factory=lambda **_: client,
        )

    assert client.close_calls == 1
    assert raised.value.code == "rate_limited"
    assert raised.value.retryable is True
    assert raised.value.__cause__ is None


def test_successful_request_reports_close_failure_without_retrying() -> None:
    class FailingCloseClient(_Client):
        close_calls = 0

        def close(self) -> None:
            self.close_calls += 1
            raise ConnectionError("transport cleanup failed")

    client = FailingCloseClient(
        _Messages({"content": [{"type": "text", "text": "ok"}]})
    )

    with pytest.raises(ProviderCallError) as raised:
        call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message="Judge this finding.",
            client_factory=lambda **_: client,
        )

    assert client.close_calls == 1
    assert raised.value.code == "client_close_failed"
    assert raised.value.retryable is False
    assert raised.value.__cause__ is None


def test_transient_tls_failure_is_retryable() -> None:
    failure = bailian_provider._provider_failure(ssl.SSLError("TLS transport failed"))

    assert failure.code == "connection_failed"
    assert failure.retryable is True
    assert failure.retry_after_seconds is None


def test_certificate_verification_failure_is_not_retryable() -> None:
    failure = bailian_provider._provider_failure(
        ssl.SSLCertVerificationError("certificate verification failed")
    )

    assert failure.code == "tls_verification_failed"
    assert failure.retryable is False
    assert failure.retry_after_seconds is None


def test_wrapped_certificate_verification_failure_is_not_retryable() -> None:
    wrapped = ConnectionError("SDK connection failed")
    wrapped.__cause__ = ssl.SSLCertVerificationError("certificate verification failed")

    failure = bailian_provider._provider_failure(wrapped)

    assert failure.code == "tls_verification_failed"
    assert failure.retryable is False


def test_provider_failure_classification_bounds_exception_cycles() -> None:
    first = ConnectionError("first")
    second = RuntimeError("second")
    first.__cause__ = second
    second.__context__ = first

    failure = bailian_provider._provider_failure(first)

    assert failure.code == "connection_failed"
    assert failure.retryable is True


def test_calls_messages_with_strict_bailian_output_schema() -> None:
    messages = _Messages({"content": [{"type": "text", "text": '{"items":[]}'}]})
    schema = {
        "type": "object",
        "properties": {"items": {"type": "array"}},
        "required": ["items"],
        "additionalProperties": False,
    }

    result = call_messages(
        api_key="sk-unit-test-placeholder",
        api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
        model="qwen3.8-max",
        user_message="Return one JSON object.",
        output_schema=schema,
        client_factory=lambda **_: _Client(messages),
    )

    assert result == '{"items":[]}'
    assert messages.arguments is not None
    assert messages.arguments["thinking"] == {"type": "disabled"}
    assert messages.arguments["extra_body"] == {
        "temperature": 0,
        "output_config": {
            "format": {
                "type": "json_schema",
                "schema": schema,
            }
        },
    }


@pytest.mark.parametrize("effort", ["medium", "high"])
def test_reasoning_and_schema_share_output_config_without_exposing_thoughts(
    effort: str,
) -> None:
    messages = _Messages(
        {
            "content": [
                {"type": "thinking", "thinking": "private intermediate reasoning"},
                {"type": "text", "text": "{}"},
            ]
        }
    )
    schema = {"type": "object", "additionalProperties": False, "properties": {}}
    result = call_messages(
        api_key="sk-unit-test-placeholder",
        api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
        model="qwen3.8-max",
        user_message="Audit the actual consumer semantics.",
        output_schema=schema,
        reasoning=True,
        reasoning_effort=effort,
        client_factory=lambda **_: _Client(messages),
    )
    assert result == "{}"
    assert messages.arguments is not None
    assert messages.arguments["thinking"] == {"type": "enabled"}
    assert messages.arguments["extra_body"]["output_config"] == {
        "effort": effort,
        "format": {"type": "json_schema", "schema": schema},
    }


def test_rejects_non_json_output_schema_before_client() -> None:
    called = False

    def factory(**_arguments: str) -> _Client:
        nonlocal called
        called = True
        return _Client(_Messages({"content": [{"type": "text", "text": "ok"}]}))

    with pytest.raises(ContractError, match="JSON serializable"):
        call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message="Return JSON.",
            output_schema={"invalid": {1, 2}},
            client_factory=factory,
        )

    assert called is False


def test_calls_messages_result_includes_token_usage() -> None:
    messages = _Messages(
        SimpleNamespace(
            content=[SimpleNamespace(type="text", text="result")],
            usage=SimpleNamespace(input_tokens=17, output_tokens=5),
            stop_reason="max_tokens",
        )
    )

    result = call_messages_result(
        api_key="sk-unit-test-placeholder",
        api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
        model="qwen3.8-max",
        user_message="Judge this root.",
        client_factory=lambda **_: _Client(messages),
    )

    assert result == {
        "text": "result",
        "usage": {"input_tokens": 17, "output_tokens": 5, "total_tokens": 22},
        "diagnostics": {
            "reasoning_effort": "disabled",
            "stop_reason": "max_tokens",
            "output_bytes": 6,
        },
    }


def test_sdk_messages_calls_can_overlap_after_client_loading() -> None:
    first_entered = threading.Event()
    release_first = threading.Event()
    second_attempted = threading.Event()
    second_entered = threading.Event()
    active = 0
    max_active = 0
    state_lock = threading.Lock()

    class BlockingMessages:
        def create(self, **arguments: Any) -> Any:
            nonlocal active, max_active
            prompt = arguments["messages"][0]["content"]
            with state_lock:
                active += 1
                max_active = max(max_active, active)
            try:
                if prompt == "first":
                    first_entered.set()
                    assert release_first.wait(timeout=2)
                else:
                    second_entered.set()
                return {"content": [{"type": "text", "text": f"result:{prompt}"}]}
            finally:
                with state_lock:
                    active -= 1

    messages = BlockingMessages()

    def invoke(prompt: str) -> str:
        if prompt == "second":
            second_attempted.set()
        return call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message=prompt,
            client_factory=lambda **_: _Client(messages),
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        first = executor.submit(invoke, "first")
        assert first_entered.wait(timeout=2)
        second = executor.submit(invoke, "second")
        assert second_attempted.wait(timeout=2)
        assert second_entered.wait(timeout=2)
        release_first.set()
        assert first.result(timeout=2) == "result:first"
        assert second.result(timeout=2) == "result:second"

    assert second_entered.is_set()
    assert max_active == 2


def test_serializes_lazy_client_factory_loading_across_threads(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    first_entered = threading.Event()
    release_first = threading.Event()
    second_attempted = threading.Event()
    second_entered = threading.Event()
    active = 0
    calls = 0
    max_active = 0
    state_lock = threading.Lock()

    def load_factory() -> Any:
        nonlocal active, calls, max_active
        with state_lock:
            active += 1
            calls += 1
            call_number = calls
            max_active = max(max_active, active)
        try:
            if call_number == 1:
                first_entered.set()
                assert release_first.wait(timeout=2)
            else:
                second_entered.set()
            return lambda **_: _Client(
                _Messages({"content": [{"type": "text", "text": "ok"}]})
            )
        finally:
            with state_lock:
                active -= 1

    monkeypatch.setattr(bailian_provider, "_load_client_factory", load_factory)

    def invoke(prompt: str) -> str:
        if prompt == "second":
            second_attempted.set()
        return call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message=prompt,
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        first = executor.submit(invoke, "first")
        assert first_entered.wait(timeout=2)
        second = executor.submit(invoke, "second")
        assert second_attempted.wait(timeout=2)
        try:
            assert not second_entered.wait(timeout=0.2)
        finally:
            release_first.set()
        assert first.result(timeout=2) == "ok"
        assert second.result(timeout=2) == "ok"

    assert second_entered.is_set()
    assert calls == 2
    assert max_active == 1


def test_serialized_sdk_failure_releases_lock_and_keeps_contract() -> None:
    secret = "sk-do-not-leak-this-unit-test-key"

    def failing_factory(**_arguments: str) -> _Client:
        raise RuntimeError(f"request failed with credential {secret}")

    with pytest.raises(
        ContractError, match="^Bailian Anthropic Messages call failed$"
    ) as raised:
        call_messages(
            api_key=secret,
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message="first",
            client_factory=failing_factory,
        )

    assert secret not in str(raised.value)
    assert raised.value.__cause__ is None
    assert (
        call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message="second",
            client_factory=lambda **_: _Client(
                _Messages({"content": [{"type": "text", "text": "ok"}]})
            ),
        )
        == "ok"
    )


@pytest.mark.parametrize(
    "response",
    [
        {},
        {"usage": {}},
        {"usage": {"input_tokens": True, "output_tokens": 1}},
        {"usage": {"input_tokens": -1, "output_tokens": 1}},
    ],
)
def test_rejects_missing_or_invalid_token_usage(response: Any) -> None:
    with pytest.raises(ContractError, match="token usage|usage.input_tokens"):
        extract_token_usage(response)


def test_calls_messages_with_multiline_prompt_text_unchanged() -> None:
    messages = _Messages({"content": [{"type": "text", "text": "ok"}]})

    result = call_messages(
        api_key="sk-unit-test-placeholder",
        api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
        model="qwen3.8-max",
        user_message="Finding:\n\tline one\r\nline two",
        system_message="Return JSON.\nNo prose.",
        client_factory=lambda **_: _Client(messages),
    )

    assert result == "ok"
    assert messages.arguments is not None
    assert messages.arguments["messages"][0]["content"] == (
        "Finding:\n\tline one\r\nline two"
    )
    assert messages.arguments["system"] == "Return JSON.\nNo prose."


@pytest.mark.parametrize("control", ["\x00", "\x0b", "\x1b", "\x7f"])
def test_rejects_unsupported_prompt_controls_before_client(
    control: str,
) -> None:
    called = False

    def factory(**_arguments: str) -> _Client:
        nonlocal called
        called = True
        return _Client(_Messages({"content": [{"type": "text", "text": "ok"}]}))

    with pytest.raises(ContractError, match="unsupported control characters"):
        call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message=f"Finding{control}text",
            client_factory=factory,
        )

    assert called is False


def test_rejects_unsupported_system_prompt_controls_before_client() -> None:
    with pytest.raises(ContractError, match="unsupported control characters"):
        call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.8-max",
            user_message="Finding text",
            system_message="Return JSON.\x00",
            client_factory=lambda **_: _Client(
                _Messages({"content": [{"type": "text", "text": "ok"}]})
            ),
        )


@pytest.mark.parametrize(
    "response",
    [
        {},
        {"content": []},
        {"content": [{"type": "thinking", "thinking": "no final text"}]},
        {"content": [{"type": "text", "text": "   "}]},
        {"content": [{"type": "text", "text": 123}]},
        {"content": [{}]},
    ],
)
def test_rejects_empty_or_malformed_responses(response: Any) -> None:
    with pytest.raises(ContractError):
        extract_final_text(response)


@pytest.mark.parametrize("error_type", [RuntimeError, ContractError])
def test_transport_failure_does_not_expose_the_key(error_type: type[Exception]) -> None:
    secret = "sk-do-not-leak-this-unit-test-key"

    def factory(**_arguments: str) -> _Client:
        raise error_type(f"request failed with credential {secret}")

    with pytest.raises(ContractError) as raised:
        call_messages(
            api_key=secret,
            api_base_url="https://dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.7-plus",
            user_message="Judge this finding.",
            client_factory=factory,
        )

    assert secret not in str(raised.value)
    assert raised.value.__cause__ is None


def test_rejects_invalid_inputs_before_constructing_a_client() -> None:
    called = False

    def factory(**_arguments: str) -> _Client:
        nonlocal called
        called = True
        return _Client(_Messages({"content": [{"type": "text", "text": "ok"}]}))

    with pytest.raises(ContractError):
        call_messages(
            api_key="sk-unit-test-placeholder",
            api_base_url="https://coding.dashscope.aliyuncs.com/apps/anthropic",
            model="qwen3.7-plus",
            user_message="Judge this finding.",
            client_factory=factory,
        )

    assert called is False
