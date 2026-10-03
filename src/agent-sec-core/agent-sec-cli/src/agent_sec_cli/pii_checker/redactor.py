"""Redaction helpers for PII findings."""

import re

from agent_sec_cli.pii_checker.models import (
    PiiCategory,
    PiiFinding,
    PiiSeverity,
)


def _mask_middle(value: str, *, prefix: int = 4, suffix: int = 4) -> str:
    """Keep a short safe prefix/suffix and mask the middle."""
    if len(value) <= prefix + suffix:
        return "[REDACTED]"
    return f"{value[:prefix]}...[REDACTED]...{value[-suffix:]}"


def redact_value(value: str, pii_type: str, *, category: str | None = None) -> str:
    """Return a model-safe redaction for a detected value."""
    if category == PiiCategory.CUSTOM.value:
        return f"[{pii_type.upper()}_REDACTED]"

    if pii_type == "email":
        local, _, domain = value.partition("@")
        if not domain:
            return "[REDACTED_EMAIL]"
        safe_local = local[:1] + "***" if local else "***"
        return f"{safe_local}@{domain}"

    if pii_type == "phone_cn":
        digits = re.sub(r"\D", "", value)
        if len(digits) >= 11:
            core = digits[-11:]
            return f"{core[:3]}****{core[-4:]}"
        return "[REDACTED_PHONE]"

    if pii_type == "credit_card":
        digits = re.sub(r"\D", "", value)
        return (
            f"[REDACTED_CARD:{digits[-4:]}]" if len(digits) >= 4 else "[REDACTED_CARD]"
        )

    if pii_type == "cn_id":
        return (
            f"{value[:3]}***********{value[-4:]}"
            if len(value) >= 7
            else "[REDACTED_CN_ID]"
        )

    if pii_type == "private_key":
        return "[REDACTED_PRIVATE_KEY]"

    if pii_type in {
        "api_key",
        "bearer_token",
        "jwt",
        "aliyun_access_key_id",
        "aliyun_access_key_secret",
        "generic_secret_field",
    }:
        return _mask_middle(value)

    return "[REDACTED]"


def redact_text(text: str, findings: list[PiiFinding]) -> str:
    """Replace overlapping finding spans once without leaving sensitive tails."""
    ordered_by_span = sorted(
        findings,
        key=lambda item: (item.span[0], item.span[1], item.type),
    )
    groups: list[list[PiiFinding]] = []
    group_end = -1
    for finding in ordered_by_span:
        start, end = finding.span
        if groups and start < group_end:
            groups[-1].append(finding)
            group_end = max(group_end, end)
            continue
        groups.append([finding])
        group_end = end

    replacements: list[tuple[int, int, str]] = []
    for group in groups:
        selected = min(
            group,
            key=lambda item: (
                item.category != PiiCategory.CUSTOM.value,
                item.severity != PiiSeverity.DENY.value,
                item.type,
            ),
        )
        start = min(item.span[0] for item in group)
        end = max(item.span[1] for item in group)
        replacement = selected.evidence_redacted
        if len({item.span for item in group}) > 1:
            replacement = f"[{selected.type.upper()}_REDACTED]"
        replacements.append((start, end, replacement))

    redacted = text
    for start, end, replacement in reversed(replacements):
        redacted = redacted[:start] + replacement + redacted[end:]
    return redacted
