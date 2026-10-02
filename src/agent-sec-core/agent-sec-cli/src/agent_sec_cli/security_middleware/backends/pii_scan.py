"""PII scan backend."""

import json
from typing import Any

from agent_sec_cli.pii_checker import audit as pii_audit
from agent_sec_cli.pii_checker.models import PiiScanResult, Verdict
from agent_sec_cli.pii_checker.scanner import PiiScanner
from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult


def _error_result(message: str, *, error_type: str = "PiiScanError") -> PiiScanResult:
    """Build a fixed-schema error result."""
    return PiiScanResult(
        ok=False,
        verdict=Verdict.ERROR.value,
        summary={
            "total": 0,
            "by_type": {},
            "by_category": {},
            "by_severity": {},
            "source": "unknown",
            "bytes_scanned": 0,
            "truncated": False,
            "error": message,
            "error_type": error_type,
        },
        findings=[],
        elapsed_ms=0,
    )


class PiiScanBackend(BaseBackend):
    """Scan text for PII and credentials."""

    def execute(self, ctx: RequestContext, **kwargs: Any) -> ActionResult:
        text = kwargs.get("text", "")
        if text is None:
            text = ""
        if not isinstance(text, str):
            return self._to_action_result(
                _error_result(
                    "pii_scan error: text must be a string",
                    error_type="TypeError",
                )
            )

        source = str(kwargs.get("source", "unknown"))
        include_low_confidence = bool(kwargs.get("include_low_confidence", False))
        raw_evidence = bool(kwargs.get("raw_evidence", False))
        redact_output = bool(kwargs.get("redact_output", False))
        max_bytes_arg = kwargs.get("max_bytes")
        if max_bytes_arg is None:
            max_bytes: int | None = None
        else:
            try:
                max_bytes = int(max_bytes_arg)
            except (TypeError, ValueError) as exc:
                return self._to_action_result(
                    _error_result(
                        "pii_scan error: max_bytes must be an integer",
                        error_type=type(exc).__name__,
                    )
                )
        input_truncated = bool(kwargs.get("input_truncated", False))
        input_bytes_scanned = kwargs.get("input_bytes_scanned")
        if input_bytes_scanned is not None:
            try:
                input_bytes_scanned = int(input_bytes_scanned)
            except (TypeError, ValueError):
                input_bytes_scanned = None

        if max_bytes is not None and max_bytes <= 0:
            return self._to_action_result(
                _error_result(
                    "pii_scan error: max_bytes must be greater than zero",
                    error_type="ValueError",
                )
            )

        try:
            result = PiiScanner().scan(
                text,
                source=source,
                include_low_confidence=include_low_confidence,
                raw_evidence=raw_evidence,
                redact_output=redact_output,
                max_bytes=max_bytes,
            )
            if input_truncated:
                result.summary["truncated"] = True
                if input_bytes_scanned is not None and input_bytes_scanned >= 0:
                    result.summary["bytes_scanned"] = input_bytes_scanned
        except Exception as exc:  # noqa: BLE001
            result = _error_result(
                f"pii_scan error: {exc}",
                error_type=type(exc).__name__,
            )

        return self._to_action_result(result)

    def _to_action_result(self, result: PiiScanResult) -> ActionResult:
        """Convert scanner output to middleware ActionResult."""
        data = result.to_dict()
        is_error = result.verdict == Verdict.ERROR.value
        error_type = result.summary.get("error_type", "") if is_error else ""
        if not isinstance(error_type, str):
            error_type = "PiiScanError"
        return ActionResult(
            success=not is_error,
            data=data,
            stdout=json.dumps(data, indent=2, ensure_ascii=False),
            error="" if not is_error else str(result.summary.get("error", "")),
            exit_code=1 if is_error else 0,
            error_type=error_type,
        )

    def build_event_details(
        self, result: ActionResult, kwargs: dict[str, Any]
    ) -> dict[str, Any]:
        """Build sanitized pii_scan success audit details."""
        return pii_audit.build_audit_details(result.data, kwargs)

    def build_error_details(
        self, exception: Exception, kwargs: dict[str, Any]
    ) -> dict[str, Any]:
        """Build sanitized pii_scan failure audit details."""
        return pii_audit.build_error_audit_details(exception, kwargs)
