"""Conservative secret redaction for judge-bound and persisted artifacts."""

from __future__ import annotations

import json
import os
import re
from collections.abc import Mapping
from typing import Any

from agent_sec_cli.skill_score.contracts import ContractError

REDACTION_VERSION = "hybrid-redaction/v5"
_REDACTED = "[REDACTED]"
_MAX_ENCODED_DEPTH = 4
_MAX_ENCODED_BYTES = 1024 * 1024


def _compile(pattern: str, flags: int = 0) -> re.Pattern[str]:
    return re.compile(pattern, flags)


_SECRET_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    (
        "private_key",
        _compile(
            r"-----BEGIN(?: [A-Z0-9]+)? PRIVATE KEY-----.*?"
            r"-----END(?: [A-Z0-9]+)? PRIVATE KEY-----",
            re.DOTALL,
        ),
    ),
    (
        "authorization_header",
        _compile(
            r"\b(?:proxy-)?authorization[^\S\r\n]*:[^\S\r\n]*[^\r\n]+",
            re.IGNORECASE,
        ),
    ),
    (
        "cookie_header",
        _compile(
            r"\b(?:set-cookie|cookie)[^\S\r\n]*:[^\S\r\n]*[^\r\n]+",
            re.IGNORECASE,
        ),
    ),
    (
        "bearer_token",
        _compile(r"\bbearer\s+[A-Za-z0-9._~+/=-]{8,}", re.IGNORECASE),
    ),
    ("anthropic_api_key", _compile(r"\bsk-ant-[A-Za-z0-9_-]{8,}\b")),
    ("openai_api_key", _compile(r"\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}\b")),
    ("github_token", _compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b")),
    ("aws_access_key", _compile(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")),
    (
        "jwt",
        _compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"),
    ),
    (
        "credential_assignment",
        _compile(
            r"(?P<assignment>(?<![A-Za-z0-9_])[\"']?(?:agent_sec_model_api_key|anthropic_api_key|anthropic_auth_token|"
            r"openai_api_key|api[_-]?(?:key|token)|"
            r"access[_-]?token|refresh[_-]?token|session[_-]?token|client[_-]?secret|"
            r"authorization|proxy[_-]?authorization|cookie|set[_-]?cookie|token|secret|"
            r"password|passwd)[\"']?(?![A-Za-z0-9_])"
            r"[^\S\r\n\x1c-\x1e\x85\u2028\u2029]*[:=]"
            r"[^\S\r\n\x1c-\x1e\x85\u2028\u2029]*)"
            r"(?![\"']?\[REDACTED\])"
            r"(?P<quote>[\"']?)(?P<value>[^\s,;\"'}]{4,})",
            re.IGNORECASE,
        ),
    ),
    (
        "url_credentials",
        _compile(r"\bhttps?://[^\s/:@]+:[^\s/@]+@", re.IGNORECASE),
    ),
)

_SENSITIVE_KEY_NAMES = frozenset(
    {
        "authorization",
        "proxyauthorization",
        "cookie",
        "setcookie",
        "apikey",
        "apitoken",
        "agentsecmodelapikey",
        "anthropicapikey",
        "anthropicauthtoken",
        "openaiapikey",
        "token",
        "accesstoken",
        "refreshtoken",
        "sessiontoken",
        "password",
        "passwd",
        "secret",
        "clientsecret",
        "credential",
        "credentials",
    }
)
_SECRET_ENV_NAMES = (
    "AGENT_SEC_MODEL_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
)


def _canonical_key(key: str) -> str:
    return re.sub(r"[^a-z0-9]", "", key.lower())


def _literal_assignment(match: re.Match[str]) -> bool:
    """A credential variable name is not evidence that its source expression is a secret."""
    if match["quote"]:
        return True
    value = match["value"]
    if any(character in value for character in "()[]"):
        return False
    if "." in value and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.]*", value):
        return False
    # Bare values also occur in dotenv files and runtime logs; do not assume they are variables.
    return True


def find_secret_markers(text: str) -> list[str]:
    """Return secret-shape labels without returning the matched secret text."""

    if not isinstance(text, str):
        raise ContractError("secret scan input must be text")
    markers = [
        name
        for name, pattern in _SECRET_PATTERNS
        if any(
            name != "credential_assignment" or _literal_assignment(match)
            for match in pattern.finditer(text)
        )
    ]
    for variable in _SECRET_ENV_NAMES:
        value = os.environ.get(variable)
        if value and len(value) >= 4 and value in text:
            markers.append(f"environment:{variable.lower()}")
    return list(dict.fromkeys(markers))


def redact_text(text: str) -> str:
    """Replace common secret shapes without preserving secret fragments."""

    if not isinstance(text, str):
        raise ContractError("redaction input must be text")
    redacted = text
    for variable in _SECRET_ENV_NAMES:
        value = os.environ.get(variable)
        if value and len(value) >= 4:
            redacted = redacted.replace(value, _REDACTED)
    for name, pattern in _SECRET_PATTERNS:
        if name == "credential_assignment":
            # Persisted task fixtures must remain parseable after removing the secret value.
            redacted = pattern.sub(
                lambda match: (
                    match.group(0)
                    if not _literal_assignment(match)
                    else match["assignment"]
                    + (
                        match["quote"] + _REDACTED
                        if match["quote"]
                        else json.dumps(_REDACTED)
                    )
                ),
                redacted,
            )
        else:
            redacted = pattern.sub(_REDACTED, redacted)
    return redacted


def redact_value(value: Any) -> Any:
    """Return a redacted copy of JSON-like data and reject unsupported values."""

    return _redact_value(value, 0)


def _redact_string(value: str, encoded_depth: int) -> str:
    if value.lstrip().startswith(("{", "[", '"')):
        if len(value.encode("utf-8")) > _MAX_ENCODED_BYTES:
            return "[OMITTED: serialized JSON redaction size limit]"
        try:
            decoded = json.loads(value)
        except (ValueError, RecursionError):
            pass
        else:
            if encoded_depth >= _MAX_ENCODED_DEPTH:
                return "[OMITTED: serialized JSON redaction depth limit]"
            # Tool metadata can contain JSON encoded inside another JSON string.
            # Sanitize the decoded fields, never regex-rewrite serialized JSON.
            return json.dumps(
                _redact_value(decoded, encoded_depth + 1), ensure_ascii=False
            )
    return redact_text(value)


def _redact_value(value: Any, encoded_depth: int) -> Any:

    if value is None or isinstance(value, (bool, int, float)):
        return value
    if isinstance(value, str):
        return _redact_string(value, encoded_depth)
    if isinstance(value, list):
        return [_redact_value(item, encoded_depth) for item in value]
    if isinstance(value, Mapping):
        output: dict[str, Any] = {}
        for key, item in value.items():
            if not isinstance(key, str):
                raise ContractError("redaction only accepts string object keys")
            if _canonical_key(key) in _SENSITIVE_KEY_NAMES:
                output[key] = _REDACTED
            else:
                output[key] = _redact_value(item, encoded_depth)
        return output
    raise ContractError("redaction only accepts JSON-like values")


def assert_redacted(value: Any) -> None:
    """Reject JSON-like data that still contains a recognizable secret shape."""

    try:
        serialized = (
            value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
        )
    except (TypeError, ValueError) as exc:
        raise ContractError("redaction validation requires JSON-like data") from exc
    markers = find_secret_markers(serialized)
    if markers:
        labels = ", ".join(markers)
        raise ContractError(f"secret material remains after redaction: {labels}")
