"""Fail-closed adapter for Bailian pay-as-you-go Anthropic endpoints."""

from __future__ import annotations

import json
import os
import re
import ssl
import threading
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any, Literal
from urllib.parse import SplitResult, urlsplit, urlunsplit

try:
    import anthropic
except ImportError:
    anthropic = None

from agent_sec_cli.skill_score.contracts import ContractError

_SHARED_PAYG_HOSTS = frozenset(
    {
        "dashscope.aliyuncs.com",
        "dashscope-intl.aliyuncs.com",
        "dashscope-us.aliyuncs.com",
    }
)
_PAYG_REGIONS = (
    "cn-beijing",
    "ap-southeast-1",
    "us-east-1",
    "eu-central-1",
    "ap-northeast-1",
)
_WORKSPACE_HOST = re.compile(
    r"^(?P<workspace>[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)\."
    rf"(?:{'|'.join(re.escape(region) for region in _PAYG_REGIONS)})"
    r"\.maas\.aliyuncs\.com$"
)
_NON_PAYG_LABELS = frozenset({"coding", "token-plan", "trial"})
_MODEL_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$")
_API_TIMEOUT_SECONDS = 120.0
_OUTPUT_SCHEMA_LIMIT_BYTES = 64 * 1024
_SDK_MESSAGES_LOCK = threading.Lock()
_SAFE_STOP_REASONS = frozenset(
    {
        "end_turn",
        "max_tokens",
        "model_context_window_exceeded",
        "pause_turn",
        "refusal",
        "stop_sequence",
        "tool_use",
    }
)

PROVIDER_ID = "bailian-payg-anthropic"
API_KEY_ENV = "ANTHROPIC_API_KEY"
API_BASE_URL_ENV = "ANTHROPIC_BASE_URL"
RUNTIME_AUTH_ENV = "ANTHROPIC_AUTH_TOKEN"


class ProviderCallError(ContractError):
    """Sanitized provider failure with a stable retry classification."""

    def __init__(
        self,
        code: str,
        *,
        retryable: bool,
        retry_after_seconds: float | None = None,
    ) -> None:
        super().__init__("Bailian Anthropic Messages call failed")
        self.code = code
        self.retryable = retryable
        self.retry_after_seconds = retry_after_seconds


def _exception_chain(exc: Exception) -> tuple[Exception, ...]:
    """Return a bounded exception chain without reading exception messages."""

    chain: list[Exception] = []
    seen: set[int] = set()
    current: BaseException | None = exc
    while isinstance(current, Exception) and id(current) not in seen and len(chain) < 8:
        chain.append(current)
        seen.add(id(current))
        current = current.__cause__ or current.__context__
    return tuple(chain)


def _provider_failure(exc: Exception) -> ProviderCallError:
    """Classify SDK failures without retaining exception text or credentials."""

    status = getattr(exc, "status_code", None)
    response = getattr(exc, "response", None)
    if status is None:
        status = getattr(response, "status_code", None)
    retry_after: float | None = None
    headers = getattr(response, "headers", None)
    if isinstance(headers, Mapping):
        raw_retry_after = headers.get("retry-after") or headers.get("Retry-After")
        try:
            retry_after = (
                float(raw_retry_after) if raw_retry_after is not None else None
            )
        except (TypeError, ValueError):
            retry_after = None
    if status in {408, 429}:
        return ProviderCallError(
            "rate_limited" if status == 429 else "timeout",
            retryable=True,
            retry_after_seconds=retry_after,
        )
    if isinstance(status, int) and status >= 500:
        return ProviderCallError(
            "server_error",
            retryable=True,
            retry_after_seconds=retry_after,
        )
    if status in {401, 403}:
        return ProviderCallError("authentication_failed", retryable=False)
    if isinstance(status, int) and 400 <= status < 500:
        return ProviderCallError("request_rejected", retryable=False)
    exception_chain = _exception_chain(exc)
    if any(
        isinstance(item, ssl.SSLCertVerificationError)
        or "certificateverification" in type(item).__name__.casefold()
        for item in exception_chain
    ):
        return ProviderCallError("tls_verification_failed", retryable=False)
    if any(
        isinstance(item, ssl.SSLError) or "sslerror" in type(item).__name__.casefold()
        for item in exception_chain
    ):
        return ProviderCallError("connection_failed", retryable=True)
    name = type(exc).__name__.casefold()
    if isinstance(exc, TimeoutError) or "timeout" in name:
        return ProviderCallError("timeout", retryable=True)
    if any(marker in name for marker in ("connection", "network", "connect")):
        return ProviderCallError("connection_failed", retryable=True)
    return ProviderCallError("provider_failure", retryable=False)


@dataclass(frozen=True)
class ResolvedBailianAuth:
    """One validated pay-as-you-go credential shared by runtime and judge."""

    api_key: str = field(repr=False)
    api_base_url: str
    api_key_env: str
    api_base_url_env: str

    def runtime_env(self, *, model: str, small_fast_model: str) -> dict[str, str]:
        """Map this API key to the bearer-token variables consumed by Claude Code."""

        values = build_runtime_env(
            api_key=self.api_key,
            base_url=self.api_base_url,
            model=model,
        )
        values["ANTHROPIC_SMALL_FAST_MODEL"] = _validate_model(small_fast_model)
        return values


def _require_clean_string(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value or value != value.strip():
        raise ContractError(
            f"{field} must be a non-empty string without outer whitespace"
        )
    if any(ord(character) < 0x20 or ord(character) == 0x7F for character in value):
        raise ContractError(f"{field} must not contain control characters")
    return value


def _require_message_text(value: Any, field: str, *, allow_empty: bool = False) -> str:
    """Allow ordinary multiline prompt text while rejecting unsafe controls."""

    if not isinstance(value, str):
        raise ContractError(f"{field} must be a string")
    if not allow_empty and not value.strip():
        raise ContractError(f"{field} must contain non-whitespace text")
    allowed_controls = frozenset({"\t", "\n", "\r"})
    if any(
        (ord(character) < 0x20 and character not in allowed_controls)
        or ord(character) == 0x7F
        for character in value
    ):
        raise ContractError(f"{field} must not contain unsupported control characters")
    return value


def _parse_base_url(base_url: str) -> SplitResult:
    value = _require_clean_string(base_url, "Bailian Anthropic base URL")
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError as exc:
        raise ContractError("Bailian Anthropic base URL is malformed") from exc
    if parsed.scheme != "https":
        raise ContractError("Bailian Anthropic base URL must use HTTPS")
    if parsed.username is not None or parsed.password is not None:
        raise ContractError(
            "Bailian Anthropic base URL must not contain user information"
        )
    if port is not None or (
        parsed.hostname and parsed.netloc.lower() != parsed.hostname.lower()
    ):
        raise ContractError("Bailian Anthropic base URL must not specify a port")
    if parsed.query or parsed.fragment or "?" in value or "#" in value:
        raise ContractError(
            "Bailian Anthropic base URL must not contain a query or fragment"
        )
    if parsed.path not in {"/apps/anthropic", "/apps/anthropic/"}:
        raise ContractError("Bailian Anthropic base URL path must be /apps/anthropic")
    if not parsed.hostname:
        raise ContractError("Bailian Anthropic base URL must contain a host")
    return parsed


def validate_payg_anthropic_base_url(base_url: str) -> str:
    """Validate and canonicalize an official Bailian pay-as-you-go endpoint.

    Subscription, trial, arbitrary Aliyun, and non-Aliyun hosts are rejected so
    the same credential can be used by both the runtime and automated judge.
    """

    parsed = _parse_base_url(base_url)
    host = parsed.hostname.lower()
    labels = frozenset(host.split("."))
    if labels & _NON_PAYG_LABELS:
        raise ContractError(
            "Bailian Anthropic base URL must use a pay-as-you-go endpoint"
        )
    workspace_match = _WORKSPACE_HOST.fullmatch(host)
    if host not in _SHARED_PAYG_HOSTS and workspace_match is None:
        raise ContractError(
            "Bailian Anthropic base URL host is not an approved pay-as-you-go host"
        )
    if workspace_match and workspace_match.group("workspace") in _NON_PAYG_LABELS:
        raise ContractError(
            "Bailian Anthropic base URL must use a pay-as-you-go workspace"
        )
    return urlunsplit(("https", host, "/apps/anthropic", "", ""))


def _validate_api_key(api_key: str) -> str:
    # Deliberately validate only in-memory shape. Key prefixes are not stable
    # enough to determine the Bailian billing plan safely.
    return _require_clean_string(api_key, "Bailian API key")


def _validate_model(model: str) -> str:
    value = _require_clean_string(model, "Bailian model")
    if _MODEL_NAME.fullmatch(value) is None:
        raise ContractError("Bailian model contains unsupported characters")
    return value


def _clean_output_schema(value: Mapping[str, Any] | None) -> dict[str, Any] | None:
    """Copy an optional JSON Schema into plain bounded JSON data."""

    if value is None:
        return None
    if not isinstance(value, Mapping):
        raise ContractError("Bailian output schema must be an object")
    try:
        encoded = json.dumps(
            dict(value),
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
        decoded = json.loads(encoded)
    except (TypeError, ValueError, UnicodeError):
        raise ContractError("Bailian output schema must be JSON serializable") from None
    if len(encoded) > _OUTPUT_SCHEMA_LIMIT_BYTES:
        raise ContractError("Bailian output schema exceeds the configured size limit")
    if not isinstance(decoded, dict):
        raise ContractError("Bailian output schema must encode one object")
    return decoded


def build_runtime_env(*, api_key: str, base_url: str, model: str) -> dict[str, str]:
    """Build the minimal Claude Code environment from one pay-as-you-go key."""

    key = _validate_api_key(api_key)
    endpoint = validate_payg_anthropic_base_url(base_url)
    runtime_model = _validate_model(model)
    return {
        RUNTIME_AUTH_ENV: key,
        API_BASE_URL_ENV: endpoint,
        "ANTHROPIC_MODEL": runtime_model,
    }


def resolve_payg_auth(
    *,
    api_key: str | None = None,
    api_base_url: str | None = None,
    api_key_env: str = API_KEY_ENV,
    api_base_url_env: str = API_BASE_URL_ENV,
    environment: Mapping[str, str] | None = None,
) -> ResolvedBailianAuth:
    """Resolve exactly one pay-as-you-go key/base pair without token fallbacks."""

    key_name = _require_clean_string(api_key_env, "Bailian API key environment name")
    base_name = _require_clean_string(
        api_base_url_env,
        "Bailian API base URL environment name",
    )
    source = os.environ if environment is None else environment
    key = api_key if api_key is not None else source.get(key_name)
    base = api_base_url if api_base_url is not None else source.get(base_name)
    if not key or not base:
        raise ContractError(
            f"Bailian pay-as-you-go credentials require both {key_name} and {base_name}"
        )
    return ResolvedBailianAuth(
        api_key=_validate_api_key(key),
        api_base_url=validate_payg_anthropic_base_url(base),
        api_key_env=key_name,
        api_base_url_env=base_name,
    )


def _field(value: Any, name: str) -> Any:
    if isinstance(value, Mapping):
        return value.get(name)
    return getattr(value, name, None)


def extract_final_text(response: Any) -> str:
    """Extract ordered text blocks and reject empty or malformed responses."""

    content = _field(response, "content")
    if (
        not isinstance(content, Sequence)
        or isinstance(content, (str, bytes, bytearray))
        or not content
    ):
        raise ContractError(
            "Bailian Anthropic response content must be a non-empty array"
        )

    text_parts: list[str] = []
    for index, block in enumerate(content):
        block_type = _field(block, "type")
        if not isinstance(block_type, str) or not block_type:
            raise ContractError(
                f"Bailian Anthropic response content[{index}] has no block type"
            )
        if block_type != "text":
            continue
        block_text = _field(block, "text")
        if not isinstance(block_text, str):
            raise ContractError(
                f"Bailian Anthropic response content[{index}].text is invalid"
            )
        if block_text.strip():
            text_parts.append(block_text)

    final_text = "\n".join(text_parts).strip()
    if not final_text:
        raise ContractError("Bailian Anthropic response contains no final text")
    return final_text


def extract_token_usage(response: Any) -> dict[str, int]:
    """Extract required Anthropic token counters from one response."""

    usage = _field(response, "usage")
    if usage is None:
        raise ContractError("Bailian Anthropic response contains no token usage")
    output: dict[str, int] = {}
    for field_name in ("input_tokens", "output_tokens"):
        value = _field(usage, field_name)
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise ContractError(
                f"Bailian Anthropic response usage.{field_name} is invalid"
            )
        output[field_name] = value
    output["total_tokens"] = output["input_tokens"] + output["output_tokens"]
    return output


def _load_client_factory() -> Callable[..., Any]:
    if anthropic is None:
        raise ContractError(
            "install the skill-score optional dependencies before calling the provider"
        )
    return anthropic.Anthropic


def warm_messages_transport(
    *,
    api_key: str,
    api_base_url: str,
    client_factory: Callable[..., Any] | None = None,
) -> None:
    """Initialize and close one SDK transport before threaded pilot work starts."""

    key = _validate_api_key(api_key)
    endpoint = validate_payg_anthropic_base_url(api_base_url)
    client: Any = None
    try:
        with _SDK_MESSAGES_LOCK:
            factory = client_factory or _load_client_factory()
            client = factory(
                api_key=key,
                base_url=endpoint,
                timeout=_API_TIMEOUT_SECONDS,
                max_retries=0,
            )
        close = getattr(client, "close", None)
        if callable(close):
            close()
    except Exception:
        raise ContractError("Bailian Anthropic SDK transport warmup failed") from None


def _create_message(
    *,
    api_key: str,
    api_base_url: str,
    model: str,
    user_message: str,
    system_message: str = "",
    max_tokens: int = 16_000,
    output_schema: Mapping[str, Any] | None = None,
    reasoning: bool = False,
    reasoning_effort: Literal["medium", "high"] = "medium",
    client_factory: Callable[..., Any] | None = None,
) -> Any:
    """Call Bailian through the Anthropic SDK without persisting credentials.

    Raises a sanitized ``ContractError`` for transport, SDK, or response-shape
    failures. The underlying exception is deliberately not chained because it
    may include request authentication material.
    """

    key = _validate_api_key(api_key)
    endpoint = validate_payg_anthropic_base_url(api_base_url)
    judge_model = _validate_model(model)
    prompt = _require_message_text(user_message, "Bailian user message")
    system_prompt = _require_message_text(
        system_message,
        "Bailian system message",
        allow_empty=True,
    )
    if (
        isinstance(max_tokens, bool)
        or not isinstance(max_tokens, int)
        or max_tokens <= 0
    ):
        raise ContractError("Bailian max_tokens must be a positive integer")
    schema = _clean_output_schema(output_schema)

    extra_body: dict[str, Any] = {"temperature": 0}
    if schema is not None:
        extra_body["output_config"] = {
            "format": {
                "type": "json_schema",
                "schema": schema,
            }
        }
    if reasoning:
        extra_body.setdefault("output_config", {})["effort"] = reasoning_effort
    arguments: dict[str, Any] = {
        "model": judge_model,
        "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": prompt}],
        "thinking": {"type": "enabled" if reasoning else "disabled"},
        # anthropic 1.1 exposes vendor-compatible body extensions through
        # extra_body but does not accept temperature as a typed argument.
        "extra_body": extra_body,
    }
    if system_prompt:
        arguments["system"] = system_prompt

    # Serialize lazy class resolution and client construction only. Each call
    # owns its client, so network requests can overlap safely after that point.
    client: Any = None
    response: Any = None
    request_error: Exception | None = None
    close_error: Exception | None = None
    try:
        try:
            with _SDK_MESSAGES_LOCK:
                factory = client_factory or _load_client_factory()
                client = factory(
                    api_key=key,
                    base_url=endpoint,
                    timeout=300.0 if reasoning else _API_TIMEOUT_SECONDS,
                    max_retries=0,
                )
            response = client.messages.create(**arguments)
        except Exception as exc:
            request_error = exc
    finally:
        if client is not None:
            try:
                close = getattr(client, "close", None)
                if callable(close):
                    close()
            except Exception as exc:
                close_error = exc

    if request_error is not None:
        raise _provider_failure(request_error) from None
    if close_error is not None:
        # The request has already completed, so retrying a transport cleanup
        # failure could duplicate cost or side effects.
        raise ProviderCallError("client_close_failed", retryable=False) from None
    return response


def call_messages(
    *,
    api_key: str,
    api_base_url: str,
    model: str,
    user_message: str,
    system_message: str = "",
    max_tokens: int = 16_000,
    output_schema: Mapping[str, Any] | None = None,
    reasoning: bool = False,
    reasoning_effort: Literal["medium", "high"] = "medium",
    client_factory: Callable[..., Any] | None = None,
) -> str:
    """Return final text from one Bailian Anthropic Messages call."""

    response = _create_message(
        api_key=api_key,
        api_base_url=api_base_url,
        model=model,
        user_message=user_message,
        system_message=system_message,
        max_tokens=max_tokens,
        output_schema=output_schema,
        reasoning=reasoning,
        reasoning_effort=reasoning_effort,
        client_factory=client_factory,
    )
    return extract_final_text(response)


def call_messages_result(
    *,
    api_key: str,
    api_base_url: str,
    model: str,
    user_message: str,
    system_message: str = "",
    max_tokens: int = 16_000,
    output_schema: Mapping[str, Any] | None = None,
    reasoning: bool = False,
    reasoning_effort: Literal["medium", "high"] = "medium",
    client_factory: Callable[..., Any] | None = None,
) -> dict[str, Any]:
    """Return final text and required token counters for reporting."""

    response = _create_message(
        api_key=api_key,
        api_base_url=api_base_url,
        model=model,
        user_message=user_message,
        system_message=system_message,
        max_tokens=max_tokens,
        output_schema=output_schema,
        reasoning=reasoning,
        reasoning_effort=reasoning_effort,
        client_factory=client_factory,
    )
    text = extract_final_text(response)
    raw_stop_reason = _field(response, "stop_reason")
    stop_reason = (
        raw_stop_reason
        if isinstance(raw_stop_reason, str) and raw_stop_reason in _SAFE_STOP_REASONS
        else "unavailable"
    )
    return {
        "text": text,
        "usage": extract_token_usage(response),
        "diagnostics": {
            "reasoning_effort": reasoning_effort if reasoning else "disabled",
            "stop_reason": stop_reason,
            "output_bytes": len(text.encode("utf-8")),
        },
    }
