"""Audit sanitization helpers for pii_scan middleware events."""

import copy
import hashlib
from typing import Any

_AUDIT_ERROR_MESSAGE = "pii_scan error details omitted from audit"


def _hash_text(text: str) -> str:
    """Return a SHA-256 digest for audit correlation without storing text."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _sanitize_request(kwargs: dict[str, Any]) -> dict[str, Any]:
    """Remove raw PII scan inputs from audit details."""
    text = kwargs.get("text", "")
    text_length = len(text) if isinstance(text, str) else 0
    return {
        "source": kwargs.get("source", "unknown"),
        "text_length": text_length,
        "text_sha256": _hash_text(text) if isinstance(text, str) else "",
        "max_bytes": kwargs.get("max_bytes"),
        "include_low_confidence": bool(kwargs.get("include_low_confidence", False)),
        "redact_output": bool(kwargs.get("redact_output", False)),
        "input_truncated": bool(kwargs.get("input_truncated", False)),
    }


def _sanitize_result(data: dict[str, Any]) -> dict[str, Any]:
    """Keep only audit-safe PII scan result fields."""
    findings = data.get("findings", [])
    safe_findings: list[dict[str, Any]] = []
    if isinstance(findings, list):
        for item in findings:
            if not isinstance(item, dict):
                continue
            safe_findings.append(
                {
                    "type": item.get("type"),
                    "category": item.get("category"),
                    "severity": item.get("severity"),
                    "confidence": item.get("confidence"),
                    "evidence_redacted": item.get("evidence_redacted"),
                    "span": item.get("span"),
                    "metadata": copy.deepcopy(item.get("metadata", {})),
                }
            )

    summary = copy.deepcopy(data.get("summary", {}))
    if isinstance(summary, dict) and summary.get("error"):
        summary["error"] = _AUDIT_ERROR_MESSAGE

    return {
        "ok": data.get("ok"),
        "verdict": data.get("verdict"),
        "summary": summary if isinstance(summary, dict) else {},
        "findings": safe_findings,
        "elapsed_ms": data.get("elapsed_ms"),
    }


def build_audit_details(
    result_data: dict[str, Any], kwargs: dict[str, Any]
) -> dict[str, Any]:
    """Build audit-safe details for a successful pii_scan invocation."""
    return {
        "request": _sanitize_request(kwargs),
        "result": _sanitize_result(result_data),
    }


def build_error_audit_details(
    exception: Exception, kwargs: dict[str, Any]
) -> dict[str, Any]:
    """Build audit-safe details for a failed pii_scan invocation."""
    return {
        "request": _sanitize_request(kwargs),
        "error": _AUDIT_ERROR_MESSAGE,
        "error_type": type(exception).__name__,
    }
