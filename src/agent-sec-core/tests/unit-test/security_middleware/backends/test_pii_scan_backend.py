"""Unit tests for security_middleware.backends.pii_scan."""

import json
from pathlib import Path

import pytest
from agent_sec_cli.pii_checker.scanner import DEFAULT_MAX_BYTES
from agent_sec_cli.security_middleware.backends.pii_scan import PiiScanBackend
from agent_sec_cli.security_middleware.context import RequestContext


@pytest.fixture(autouse=True)
def _isolate_custom_rules_home(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))


def _write_custom_rules(home: Path, content: str) -> None:
    path = home / ".config" / "agent-sec" / "pii-checker" / "rules.yaml"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def test_backend_returns_json_result():
    backend = PiiScanBackend()
    result = backend.execute(
        RequestContext(action="pii_scan"),
        text="email alice@company.cn",
        source="manual",
    )

    assert result.success is True
    assert result.exit_code == 0
    parsed = json.loads(result.stdout)
    assert parsed == result.data
    assert parsed["verdict"] == "warn"


def test_backend_redact_output():
    backend = PiiScanBackend()
    result = backend.execute(
        RequestContext(action="pii_scan"),
        text="password=supersecretvalue12345",
        redact_output=True,
    )

    assert result.data["verdict"] == "deny"
    assert "redacted_text" in result.data
    assert "supersecretvalue12345" not in result.data["redacted_text"]


def test_backend_defaults_to_unlimited_scan():
    backend = PiiScanBackend()
    email = "alice@company.cn"
    text = f"{'x' * (DEFAULT_MAX_BYTES + 10)} {email}"
    result = backend.execute(RequestContext(action="pii_scan"), text=text)

    assert result.success is True
    assert result.data["summary"]["truncated"] is False
    assert result.data["summary"]["bytes_scanned"] == len(text.encode("utf-8"))
    assert any(finding["type"] == "email" for finding in result.data["findings"])


def test_backend_accepts_none_max_bytes():
    backend = PiiScanBackend()
    result = backend.execute(
        RequestContext(action="pii_scan"),
        text="email alice@company.cn",
        max_bytes=None,
    )

    assert result.success is True
    assert result.data["verdict"] == "warn"
    assert result.data["summary"]["truncated"] is False


def test_backend_rejects_invalid_max_bytes():
    backend = PiiScanBackend()
    result = backend.execute(RequestContext(action="pii_scan"), text="x", max_bytes=0)

    assert result.success is False
    assert result.exit_code == 1
    assert result.data["verdict"] == "error"
    assert result.data["summary"]["error_type"] == "ValueError"
    assert result.error_type == "ValueError"


def test_backend_rejects_non_integer_max_bytes():
    backend = PiiScanBackend()
    result = backend.execute(
        RequestContext(action="pii_scan"),
        text="x",
        max_bytes="not-an-int",
    )

    assert result.success is False
    assert result.exit_code == 1
    assert result.data["summary"]["error_type"] == "ValueError"
    assert result.error_type == "ValueError"


def test_backend_error_preserves_error_type_without_traceback(monkeypatch):
    def fail_scan(self, text, **kwargs):
        raise RuntimeError("scanner failed")

    monkeypatch.setattr(
        "agent_sec_cli.security_middleware.backends.pii_scan.PiiScanner.scan",
        fail_scan,
    )

    backend = PiiScanBackend()
    result = backend.execute(RequestContext(action="pii_scan"), text="hello")

    assert result.success is False
    assert result.exit_code == 1
    assert result.data["summary"]["error_type"] == "RuntimeError"
    assert result.error_type == "RuntimeError"
    assert result.error == "pii_scan error: scanner failed"
    assert "Traceback" not in result.stdout


def test_backend_audit_details_omit_exception_text_with_input(monkeypatch):
    sensitive = "alice@example.com"

    def fail_scan(self, text, **kwargs):
        raise RuntimeError(f"scanner failed on {sensitive}")

    monkeypatch.setattr(
        "agent_sec_cli.security_middleware.backends.pii_scan.PiiScanner.scan",
        fail_scan,
    )

    backend = PiiScanBackend()
    result = backend.execute(RequestContext(action="pii_scan"), text=sensitive)
    details = backend.build_event_details(result, {"text": sensitive})
    details_text = json.dumps(details, ensure_ascii=False)

    assert sensitive not in details_text
    assert details["result"]["summary"]["error"] == (
        "pii_scan error details omitted from audit"
    )
    assert details["result"]["summary"]["error_type"] == "RuntimeError"


def test_backend_error_audit_details_omit_exception_text_with_input():
    sensitive = "alice@example.com"
    backend = PiiScanBackend()
    details = backend.build_error_details(
        RuntimeError(f"router failed on {sensitive}"),
        {"text": sensitive},
    )
    details_text = json.dumps(details, ensure_ascii=False)

    assert sensitive not in details_text
    assert details["error"] == "pii_scan error details omitted from audit"
    assert details["error_type"] == "RuntimeError"


def test_backend_audit_details_allow_null_max_bytes_without_input_text():
    sensitive = "alice@example.com"
    backend = PiiScanBackend()
    result = backend.execute(
        RequestContext(action="pii_scan"),
        text=sensitive,
        max_bytes=None,
    )
    details = backend.build_event_details(
        result,
        {"text": sensitive, "max_bytes": None},
    )
    details_text = json.dumps(details, ensure_ascii=False)

    assert details["request"]["max_bytes"] is None
    assert sensitive not in details_text


def test_backend_custom_rules_flow_into_sanitized_audit_details(tmp_path: Path):
    pattern = "DFT-[A-Z0-9]{8}"
    sensitive = "DFT-ABC12345"
    _write_custom_rules(
        tmp_path,
        f"- type: dogfood_token\n  regex: '{pattern}'\n  severity: deny\n",
    )
    backend = PiiScanBackend()

    result = backend.execute(
        RequestContext(action="pii_scan"),
        text=sensitive,
        source="tool_output",
        redact_output=True,
    )
    details = backend.build_event_details(
        result,
        {"text": sensitive, "source": "tool_output", "redact_output": True},
    )
    details_text = json.dumps(details, ensure_ascii=False)

    custom_summary = details["result"]["summary"]["custom_rules"]
    assert result.success is True
    assert result.data["verdict"] == "deny"
    assert details["result"]["findings"][0]["type"] == "dogfood_token"
    assert custom_summary["status"] == "loaded"
    assert custom_summary["rule_count"] == 1
    assert len(custom_summary["ruleset_sha256"]) == 64
    assert pattern not in details_text
    assert sensitive not in details_text
    assert ".config" not in details_text


def test_backend_invalid_custom_rules_fail_open_with_builtins(tmp_path: Path, caplog):
    sensitive_pattern = "[private-business-pattern"
    _write_custom_rules(
        tmp_path,
        f"- type: dogfood_token\n  regex: '{sensitive_pattern}'\n",
    )
    backend = PiiScanBackend()

    result = backend.execute(
        RequestContext(action="pii_scan"),
        text="alice@company.cn",
    )
    details = backend.build_event_details(
        result,
        {"text": "alice@company.cn", "source": "user_input"},
    )
    details_text = json.dumps(details, ensure_ascii=False)
    custom_summary = details["result"]["summary"]["custom_rules"]

    assert result.success is True
    assert result.exit_code == 0
    assert result.data["verdict"] == "warn"
    assert result.data["findings"][0]["type"] == "email"
    assert result.data["summary"]["custom_rules"]["status"] == "invalid"
    assert custom_summary["status"] == "invalid"
    assert custom_summary["error_code"] == "invalid_regex"
    assert len(custom_summary["ruleset_sha256"]) == 64
    assert sensitive_pattern not in caplog.text
    assert sensitive_pattern not in details_text
    assert "alice@company.cn" not in details_text
    assert ".config" not in details_text
