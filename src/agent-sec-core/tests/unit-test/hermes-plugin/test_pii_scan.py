"""Unit tests for the Hermes PII-scan capability."""

from __future__ import annotations

import json
from unittest.mock import patch

import pytest
from hermes_plugin_src.capabilities.pii_scan import PiiScanCapability
from hermes_plugin_src.cli_runner import CliResult

_GENERAL_FINDING = {
    "type": "email",
    "severity": "warn",
    "evidence_redacted": "a***@example.com",
}
_HIGH_FINDING = {
    "type": "api_key",
    "severity": "deny",
    "evidence_redacted": "sk-a...[REDACTED]...1234",
    "raw_evidence": "sk-abcdefghijklmnop1234",
}


def _make_capability(*, include_low_confidence: bool = False) -> PiiScanCapability:
    cap = PiiScanCapability()
    cap._timeout = 5.0
    cap._include_low_confidence = include_low_confidence
    return cap


def _scan_result(
    verdict: str,
    findings: list[dict] | None = None,
) -> CliResult:
    payload: dict = {"verdict": verdict, "findings": findings or []}
    return CliResult(stdout=json.dumps(payload), stderr="", exit_code=0)


def _register_policy(
    capability: PiiScanCapability,
    monkeypatch: pytest.MonkeyPatch,
    policy: str,
) -> None:
    monkeypatch.delenv("PII_CHECKER_HOOK_ENABLED", raising=False)
    monkeypatch.delenv("PII_CHECKER_MODE", raising=False)
    capability._on_register({"policy": policy})


@pytest.fixture
def capability() -> PiiScanCapability:
    return _make_capability()


class TestPiiScanCapability:
    def test_registers_lifecycle_hooks_without_output_transform(self, capability):
        assert list(capability.get_hooks_define()) == [
            "pre_llm_call",
            "pre_tool_call",
            "post_tool_call",
            "post_llm_call",
        ]

    @pytest.mark.parametrize("policy", ["warn", "ask", "invalid"])
    def test_unsupported_policies_fall_back_to_observe(
        self, monkeypatch, caplog, policy
    ):
        cap = _make_capability()

        with caplog.at_level("WARNING", logger="agent-sec-core"):
            _register_policy(cap, monkeypatch, policy)

        assert cap._policy == "observe"
        assert "does not support capability policy" in caplog.text
        assert "using observe" in caplog.text

    @pytest.mark.parametrize(
        ("raw_policy", "expected"),
        [
            ("observe", "observe"),
            ("block", "block"),
            ("debug", "observe"),
            ("deny", "block"),
        ],
    )
    def test_native_policies_and_aliases(self, monkeypatch, raw_policy, expected):
        cap = _make_capability()

        _register_policy(cap, monkeypatch, raw_policy)

        assert cap._policy == expected

    def test_environment_policy_overrides_capability_policy(self, monkeypatch):
        monkeypatch.setenv("PII_CHECKER_MODE", "block")
        cap = _make_capability()

        cap._on_register({"policy": "observe"})

        assert cap._policy == "block"

    def test_removed_warning_config_is_ignored_with_diagnostic(self, caplog):
        cap = _make_capability()

        with caplog.at_level("WARNING", logger="agent-sec-core"):
            cap._on_register({"policy": "observe", "warning_ttl_seconds": 300})

        assert "warning_ttl_seconds is ignored" in caplog.text

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_environment_switch_disables_before_input_scan(self, mock_cli, monkeypatch):
        monkeypatch.setenv("PII_CHECKER_HOOK_ENABLED", "false")
        cap = _make_capability()
        cap._on_register({})

        assert cap._on_pre_llm_call(user_message="password=secret") is None
        mock_cli.assert_not_called()

    @pytest.mark.parametrize("verdict", ["warn", "deny"])
    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_observe_scans_input_without_returning_user_content(
        self, mock_cli, verdict, capability
    ):
        mock_cli.return_value = _scan_result(verdict, [_GENERAL_FINDING])

        result = capability._on_pre_llm_call(user_message="alice@example.com")

        assert result is None
        mock_cli.assert_called_once()

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_input_scan_does_not_require_session_key(self, mock_cli, capability):
        mock_cli.return_value = _scan_result("warn", [_GENERAL_FINDING])

        capability._on_pre_llm_call(user_message="alice@example.com")

        assert mock_cli.call_args.kwargs["stdin"] == "alice@example.com"
        assert mock_cli.call_args.kwargs["trace_context"] == {"agent_name": "hermes"}

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_passes_tool_trace_context(self, mock_cli, capability):
        mock_cli.return_value = _scan_result("pass")

        capability._on_pre_tool_call(
            tool_name="terminal",
            args={"command": "echo ok"},
            session_id="session-1",
            tool_call_id="tool-1",
        )

        assert mock_cli.call_args.kwargs["trace_context"] == {
            "agent_name": "hermes",
            "session_id": "session-1",
            "tool_call_id": "tool-1",
        }

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_include_low_confidence_adds_cli_arg(self, mock_cli):
        cap = _make_capability(include_low_confidence=True)
        mock_cli.return_value = _scan_result("pass")

        cap._on_pre_llm_call(user_message="hello")

        assert "--include-low-confidence" in mock_cli.call_args.args[0]

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_extracts_last_user_message(self, mock_cli, capability):
        mock_cli.return_value = _scan_result("pass")

        capability._on_pre_llm_call(
            messages=[
                {"role": "user", "content": "old"},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": [{"type": "text", "text": "new"}]},
            ]
        )

        assert mock_cli.call_args.kwargs["stdin"] == "new"

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_block_deny_pre_tool_returns_native_action(
        self, mock_cli, capability, monkeypatch
    ):
        _register_policy(capability, monkeypatch, "block")
        mock_cli.return_value = _scan_result("deny", [_HIGH_FINDING])

        result = capability._on_pre_tool_call(
            tool_name="terminal",
            args={"command": "API_KEY=sk-abcdefghijklmnop1234"},
        )

        assert result == {
            "action": "block",
            "message": "[pii-checker] 检测到 1 项高风险敏感信息；当前策略已阻断本次工具调用。",
        }
        assert "sk-abcdefghijklmnop1234" not in result["message"]

    def test_block_message_summarizes_mixed_risk_without_evidence(self, capability):
        message = capability._format_pii_message(
            "deny",
            [
                _GENERAL_FINDING,
                _HIGH_FINDING,
                {"type": "custom", "severity": "unknown", "raw_evidence": "raw"},
            ],
            outcome="当前策略已阻断本次工具调用。",
        )

        assert "检测到 3 项敏感信息（高风险 2、一般风险 1）" in message
        assert "alice@example.com" not in message
        assert "raw" not in message

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_block_warn_pre_tool_allows_without_synthetic_warning(
        self, mock_cli, capability, monkeypatch
    ):
        _register_policy(capability, monkeypatch, "block")
        mock_cli.return_value = _scan_result("warn", [_GENERAL_FINDING])

        result = capability._on_pre_tool_call(
            tool_name="terminal",
            args={"command": "echo alice@example.com"},
        )

        assert result is None

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_post_tool_findings_are_audit_only(self, mock_cli, capability, monkeypatch):
        _register_policy(capability, monkeypatch, "block")
        mock_cli.return_value = _scan_result("deny", [_HIGH_FINDING])

        result = capability._on_post_tool_call(
            tool_name="terminal",
            args={"command": "read"},
            result={"output": "sk-abcdefghijklmnop1234"},
        )

        assert result is None

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_model_output_findings_are_audit_only(
        self, mock_cli, capability, monkeypatch
    ):
        _register_policy(capability, monkeypatch, "block")
        mock_cli.return_value = _scan_result("deny", [_HIGH_FINDING])

        result = capability._on_post_llm_call(
            assistant_response="API_KEY=sk-abcdefghijklmnop1234",
            session_id="session-1",
        )

        assert result is None
        assert "model_output" in mock_cli.call_args.args[0]
        assert mock_cli.call_args.kwargs["stdin"] == "API_KEY=sk-abcdefghijklmnop1234"
        assert mock_cli.call_args.kwargs["trace_context"] == {
            "agent_name": "hermes",
            "session_id": "session-1",
        }

    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_empty_model_output_skips_scan(self, mock_cli, capability):
        assert capability._on_post_llm_call(assistant_response="  ") is None
        mock_cli.assert_not_called()

    @pytest.mark.parametrize(
        "cli_result",
        [
            CliResult(stdout="", stderr="boom", exit_code=1),
            CliResult(stdout="not-json", stderr="", exit_code=0),
            CliResult(stdout="[]", stderr="", exit_code=0),
        ],
    )
    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_cli_failures_fail_open(self, mock_cli, cli_result, capability):
        mock_cli.return_value = cli_result

        assert capability._on_pre_llm_call(user_message="hello") is None

    @pytest.mark.parametrize("verdict", ["error", "unknown"])
    @patch("hermes_plugin_src.capabilities.pii_scan.call_agent_sec_cli")
    def test_non_security_verdicts_fail_open(
        self, mock_cli, verdict, capability, monkeypatch
    ):
        _register_policy(capability, monkeypatch, "block")
        mock_cli.return_value = _scan_result(verdict, [_GENERAL_FINDING])

        assert capability._on_pre_llm_call(user_message="hello") is None
