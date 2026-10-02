from unittest.mock import patch

from agent_sec_cli.code_scanner.errors import (
    CodeScanError,
    ErrRegexCompile,
    ErrRuleFileNotFound,
    ErrRuleRefResolve,
    ErrRuleValidation,
    ErrRuleYamlParse,
)
from agent_sec_cli.code_scanner.models import (
    Finding,
    Language,
    Severity,
    Verdict,
)
from agent_sec_cli.code_scanner.scanner import scan


def test_scan_detection(scan_test_case: tuple) -> None:
    """Parametrised four-tuple test: (code, language, rule_id, expected_count)."""
    code, language, rule_id, expected_count = scan_test_case
    result = scan(code, language, rules=[rule_id])
    assert result.ok is True
    assert len(result.findings) == expected_count, (
        f"Expected {expected_count} finding(s) for rule '{rule_id}' on: {code!r}, "
        f"got {len(result.findings)}: {[f.rule_id for f in result.findings]}"
    )


def test_scan_pass_verdict() -> None:
    """When no rule matches, verdict should be PASS."""
    result = scan("echo hello", Language.BASH)
    assert result.ok is True
    assert result.verdict == Verdict.PASS
    assert result.findings == []


def test_scan_warn_verdict() -> None:
    """A matching warn-severity rule should produce WARN verdict."""
    result = scan("rm -rf /tmp/test", Language.BASH)
    assert result.ok is True
    assert result.verdict == Verdict.WARN
    assert len(result.findings) > 0
    assert all(f.severity.value == "warn" for f in result.findings)


def test_scan_result_schema_fields() -> None:
    """ScanResult must expose all required fields."""
    result = scan("ls -la", Language.BASH)
    assert result.language == Language.BASH
    assert isinstance(result.elapsed_ms, int)
    assert result.elapsed_ms >= 0


def test_scan_evidence_is_list() -> None:
    """evidence field must be a list of strings."""
    result = scan("rm -rf /a && rm -r /b", Language.BASH)
    assert result.ok is True
    for finding in result.findings:
        assert isinstance(finding.evidence, list)
        assert len(finding.evidence) >= 1
        assert all(isinstance(e, str) for e in finding.evidence)


def test_scan_unknown_language_no_rules() -> None:
    """Scanning with a language that has no rule files still returns ok=True, PASS."""
    result = scan("print('hello')", Language.PYTHON)
    assert result.ok is True
    assert result.verdict == Verdict.PASS
    assert result.findings == []


# -- Error handling tests --


def test_scan_empty_code_returns_error() -> None:
    """Empty input should return ERROR verdict with ErrInputEmpty message."""
    result = scan("", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert result.summary == "scan error: empty input code"


def test_scan_whitespace_only_returns_error() -> None:
    """Whitespace-only input should return ERROR verdict."""
    result = scan("   \n\t  ", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert result.summary == "scan error: empty input code"


@patch("agent_sec_cli.code_scanner.scanner.load_rules")
def test_scan_rule_file_not_found(mock_load: object) -> None:
    mock_load.side_effect = ErrRuleFileNotFound("/missing/path")  # type: ignore[attr-defined]
    result = scan("echo hello", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert "rule file not found" in result.summary


@patch("agent_sec_cli.code_scanner.scanner.load_rules")
def test_scan_rule_yaml_parse_error(mock_load: object) -> None:
    mock_load.side_effect = ErrRuleYamlParse()  # type: ignore[attr-defined]
    result = scan("echo hello", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert "rule file YAML parse error" in result.summary


@patch("agent_sec_cli.code_scanner.scanner.run_regex_rules")
@patch("agent_sec_cli.code_scanner.scanner.load_rules", return_value=[])
def test_scan_regex_compile_error(mock_load: object, mock_run: object) -> None:
    mock_run.side_effect = ErrRegexCompile("bad pattern")  # type: ignore[attr-defined]
    result = scan("echo hello", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert "regex compile failed" in result.summary


@patch("agent_sec_cli.code_scanner.scanner.load_rules")
def test_scan_memory_error(mock_load: object) -> None:
    mock_load.side_effect = MemoryError()  # type: ignore[attr-defined]
    result = scan("echo hello", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert result.summary == "scan error: engine resource exhausted"


@patch("agent_sec_cli.code_scanner.scanner.load_rules")
def test_scan_unexpected_exception(mock_load: object) -> None:
    mock_load.side_effect = RuntimeError("boom")  # type: ignore[attr-defined]
    result = scan("echo hello", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert result.summary == "scan error: internal error"


def test_scan_normal_no_error_summary() -> None:
    """Normal scan should not have 'scan error' in summary."""
    result = scan("echo hello", Language.BASH)
    assert result.ok is True
    assert "scan error" not in result.summary


# -- Language extraction / auto-switch tests --


def test_scan_inline_python_switches_language() -> None:
    """bash input containing `python3 -c "..."` should switch to Python rules."""
    result = scan('python3 -c "pickle.loads(data)"', Language.BASH)
    assert result.ok is True
    assert result.language == Language.PYTHON
    matched = [f for f in result.findings if f.rule_id == "py-unsafe-deserialization"]
    assert len(matched) == 1


def test_scan_inline_bash_keeps_language() -> None:
    """bash input containing `bash -c "..."` should stay on Bash rules."""
    result = scan('bash -c "rm -rf /tmp"', Language.BASH)
    assert result.ok is True
    assert result.language == Language.BASH
    matched = [f for f in result.findings if f.rule_id == "shell-recursive-delete"]
    assert len(matched) == 1


def test_scan_inline_python_no_bash_rules() -> None:
    """After language switch, Bash rules must NOT be applied to Python code."""
    result = scan("python3 -c \"exec(base64.b64decode('cHJpbnQ='))\"", Language.BASH)
    assert result.ok is True
    assert result.language == Language.PYTHON
    # Should trigger py-obfuscation, NOT any shell-* rule
    bash_findings = [f for f in result.findings if f.rule_id.startswith("shell-")]
    assert bash_findings == [], f"Unexpected bash findings: {bash_findings}"
    py_findings = [f for f in result.findings if f.rule_id == "py-obfuscation"]
    assert len(py_findings) == 1


def test_scan_inline_uv_run_python_switches() -> None:
    """uv run prefix should not prevent language detection."""
    result = scan("uv run python3 -c \"pty.spawn('/bin/sh')\"", Language.BASH)
    assert result.ok is True
    assert result.language == Language.PYTHON
    matched = [f for f in result.findings if f.rule_id == "py-reverse-shell"]
    assert len(matched) == 1


def test_scan_no_inline_keeps_bash() -> None:
    """Plain bash code without -c pattern should stay on Bash."""
    result = scan("rm -rf /tmp/test", Language.BASH)
    assert result.ok is True
    assert result.language == Language.BASH
    assert any(f.rule_id.startswith("shell-") for f in result.findings)


def test_scan_python_input_no_extraction() -> None:
    """Language=PYTHON input should NOT trigger inline extraction at all."""
    result = scan('bash -c "rm -rf /"', Language.PYTHON)
    assert result.ok is True
    # extraction only runs for BASH input; Python input stays as-is
    assert result.language == Language.PYTHON
    # No Python rules should match raw bash text
    assert result.findings == []


# -- DENY verdict path (no real DENY-severity rules exist yet) --


@patch("agent_sec_cli.code_scanner.scanner.run_regex_rules")
@patch("agent_sec_cli.code_scanner.scanner.load_rules", return_value=[])
def test_scan_deny_verdict(mock_load: object, mock_run: object) -> None:
    """A DENY-severity finding should produce verdict=DENY."""
    mock_run.return_value = [  # type: ignore[attr-defined]
        Finding(
            rule_id="test-deny-rule",
            severity=Severity.DENY,
            desc_en="test deny",
            desc_zh="测试拒绝",
            evidence=["evil code"],
        )
    ]
    result = scan("evil code", Language.BASH)
    assert result.ok is True
    assert result.verdict == Verdict.DENY


@patch("agent_sec_cli.code_scanner.scanner.run_regex_rules")
@patch("agent_sec_cli.code_scanner.scanner.load_rules", return_value=[])
def test_scan_mixed_severity_verdict(mock_load: object, mock_run: object) -> None:
    """Mixed WARN + DENY findings should produce verdict=DENY (highest severity wins)."""
    mock_run.return_value = [  # type: ignore[attr-defined]
        Finding(
            rule_id="warn-rule",
            severity=Severity.WARN,
            desc_en="test warn",
            desc_zh="测试警告",
            evidence=["warn evidence"],
        ),
        Finding(
            rule_id="deny-rule",
            severity=Severity.DENY,
            desc_en="test deny",
            desc_zh="测试拒绝",
            evidence=["deny evidence"],
        ),
    ]
    result = scan("evil code", Language.BASH)
    assert result.ok is True
    assert result.verdict == Verdict.DENY
    assert len(result.findings) == 2


# -- Missing error type tests --


@patch("agent_sec_cli.code_scanner.scanner.load_rules")
def test_scan_rule_validation_error(mock_load: object) -> None:
    """ErrRuleValidation should produce ERROR verdict."""
    mock_load.side_effect = ErrRuleValidation("bad-rule")  # type: ignore[attr-defined]
    result = scan("echo hello", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert "rule validation failed" in result.summary


@patch("agent_sec_cli.code_scanner.scanner.load_rules")
def test_scan_rule_ref_resolve_error(mock_load: object) -> None:
    """ErrRuleRefResolve should produce ERROR verdict."""
    mock_load.side_effect = ErrRuleRefResolve("missing-ref")  # type: ignore[attr-defined]
    result = scan("echo hello", Language.BASH)
    assert result.ok is False
    assert result.verdict == Verdict.ERROR
    assert "rule reference resolve failed" in result.summary
