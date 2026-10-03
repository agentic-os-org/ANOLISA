"""Unit tests for structured CodeScanBackend product errors."""

from unittest.mock import patch

from agent_sec_cli.code_scanner.models import Language, ScanResult, Verdict
from agent_sec_cli.security_middleware.backends.code_scan import CodeScanBackend
from agent_sec_cli.security_middleware.context import RequestContext


def _scan_result(*, ok: bool, verdict: Verdict) -> ScanResult:
    return ScanResult(
        ok=ok,
        verdict=verdict,
        summary="CUSTOMER-CANARY must remain local",
        findings=[],
        language=Language.BASH,
        elapsed_ms=3,
    )


def test_unsupported_language_has_structured_error_type() -> None:
    result = CodeScanBackend().execute(
        RequestContext(action="code_scan"),
        code="CUSTOMER-CANARY",
        language="unsupported-customer-language",
    )

    assert result.success is False
    assert result.exit_code == 1
    assert result.error_type == "ErrUnsupportedLang"


@patch("agent_sec_cli.security_middleware.backends.code_scan.scan")
def test_scanner_error_verdict_has_structured_error_type(mock_scan) -> None:
    mock_scan.return_value = _scan_result(ok=False, verdict=Verdict.ERROR)

    result = CodeScanBackend().execute(
        RequestContext(action="code_scan"), code="CUSTOMER-CANARY"
    )

    assert result.success is False
    assert result.exit_code == 1
    assert result.error_type == "CodeScanError"


@patch("agent_sec_cli.security_middleware.backends.code_scan.scan")
def test_security_verdict_is_not_classified_as_product_error(mock_scan) -> None:
    mock_scan.return_value = _scan_result(ok=True, verdict=Verdict.DENY)

    result = CodeScanBackend().execute(
        RequestContext(action="code_scan"), code="CUSTOMER-CANARY"
    )

    assert result.success is True
    assert result.exit_code == 0
    assert result.error_type == ""
