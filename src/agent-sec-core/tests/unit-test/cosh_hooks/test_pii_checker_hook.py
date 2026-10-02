"""Unit tests for cosh-extension/hooks/pii_checker_hook.py."""

import io
import json
import subprocess
from pathlib import Path

import pytest
from standalone_hook_test_loader import load_standalone_hook

_COSH_HOOK = str(
    Path(__file__).resolve().parents[2]
    / ".."
    / "cosh-extension"
    / "hooks"
    / "pii_checker_hook.py"
)

pii_checker_hook = load_standalone_hook(
    "cosh_pii_checker_hook",
    Path(_COSH_HOOK),
)
_format_cosh = pii_checker_hook._format_cosh


class TestFormatCosh:
    def test_pass_returns_allow(self):
        result = json.loads(_format_cosh({"verdict": "pass", "findings": []}))
        assert result == {"decision": "allow"}

    def test_warn_returns_allow_with_general_risk_summary(self):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "warn",
                    "findings": [
                        {
                            "type": "email",
                            "severity": "warn",
                            "evidence_redacted": "a***@example.com",
                            "raw_evidence": "alice@example.com",
                        }
                    ],
                }
            )
        )

        assert result["decision"] == "allow"
        assert result["reason"] == (
            "[pii-checker] 检测到 1 项一般风险敏感信息；"
            "本次仅提醒，未触发确认或阻断。"
        )
        assert "email" not in result["reason"]
        assert "a***@example.com" not in result["reason"]
        assert "alice@example.com" not in result["reason"]
        assert "raw_evidence" not in result["reason"]

    def test_unknown_type_uses_severity_for_high_risk_summary(self):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "deny",
                    "findings": [
                        {
                            "type": "credential",
                            "severity": "deny",
                            "evidence_redacted": "password=[REDACTED]",
                        }
                    ],
                }
            )
        )

        assert result["decision"] == "allow"
        assert result["reason"] == (
            "[pii-checker] 检测到 1 项高风险敏感信息；" "本次仅提醒，未触发确认或阻断。"
        )
        assert "credential" not in result["reason"]
        assert "password=[REDACTED]" not in result["reason"]

    def test_risk_summary_uses_finding_severity(self):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "deny",
                    "findings": [
                        {"type": "jwt", "severity": "deny"},
                        {"type": "email", "severity": "deny"},
                        {"type": "custom_secret", "severity": "deny"},
                        {"type": "custom_contact", "severity": "warn"},
                    ],
                },
                "ask",
            )
        )

        assert result == {
            "decision": "ask",
            "reason": (
                "[pii-checker] 检测到 4 项敏感信息（高风险 3、一般风险 1）；"
                "当前策略要求确认，请确认后继续。"
            ),
        }

    @pytest.mark.parametrize(
        ("pii_type", "severity", "expected_risk"),
        [
            ("jwt", "warn", "一般风险"),
            ("cn_id", "warn", "一般风险"),
            ("credit_card", "warn", "一般风险"),
            ("email", "deny", "高风险"),
            ("phone_cn", "deny", "高风险"),
            ("custom", "deny", "高风险"),
            ("custom", "warn", "一般风险"),
        ],
    )
    def test_type_does_not_override_severity(self, pii_type, severity, expected_risk):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": severity,
                    "findings": [{"type": pii_type, "severity": severity}],
                },
                "warn",
            )
        )

        assert f"1 项{expected_risk}敏感信息" in result["reason"]

    def test_risk_summary_does_not_count_malformed_findings(self):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "deny",
                    "findings": [
                        {"type": "jwt", "severity": "deny"},
                        "malformed",
                        None,
                    ],
                },
                "warn",
            )
        )

        assert result["reason"] == (
            "[pii-checker] 检测到 1 项高风险敏感信息；" "本次仅提醒，未触发确认或阻断。"
        )

    def test_only_malformed_findings_fail_open(self):
        result = json.loads(
            _format_cosh(
                {"verdict": "deny", "findings": ["malformed", None]},
                "block",
            )
        )

        assert result == {"decision": "allow"}

    @pytest.mark.parametrize(
        ("verdict", "expected_risk"),
        [("deny", "高风险"), ("warn", "一般风险")],
    )
    def test_missing_finding_fields_fall_back_to_verdict(self, verdict, expected_risk):
        result = json.loads(
            _format_cosh(
                {"verdict": verdict, "findings": [{"type": "custom"}]},
                "warn",
            )
        )

        assert f"1 项{expected_risk}敏感信息" in result["reason"]

    def test_warn_without_findings_allows(self):
        result = json.loads(_format_cosh({"verdict": "warn", "findings": []}))
        assert result == {"decision": "allow"}

    @pytest.mark.parametrize("verdict", ["error", "unknown", ""])
    def test_error_and_unknown_verdicts_allow(self, verdict):
        result = json.loads(_format_cosh({"verdict": verdict, "findings": [{}]}))
        assert result == {"decision": "allow"}

    @pytest.mark.parametrize("verdict", ["warn", "deny"])
    def test_observe_is_silent(self, verdict):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": verdict,
                    "findings": [
                        {
                            "type": "credential",
                            "severity": "deny",
                            "evidence_redacted": "token=[REDACTED]",
                        }
                    ],
                },
                "observe",
                "PreToolUse",
            )
        )

        assert result == {"decision": "allow"}

    @pytest.mark.parametrize(
        "event_name",
        [
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "AfterModel",
            "PostToolUseFailure",
        ],
    )
    @pytest.mark.parametrize("policy", ["warn", "ask", "block"])
    def test_scanner_warn_never_escalates(self, event_name, policy):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "warn",
                    "findings": [
                        {
                            "type": "email",
                            "severity": "warn",
                            "evidence_redacted": "a***@example.com",
                            "raw_evidence": "alice@example.com",
                        }
                    ],
                },
                policy,
                event_name,
            )
        )

        assert result["decision"] == "allow"
        if event_name in {"PostToolUse", "PostToolUseFailure"}:
            assert "工具已经执行" in result["reason"]
            assert "原始工具结果仍会进入模型上下文" in result["reason"]
            assert "外部副作用不会撤销" in result["reason"]
        else:
            assert result["reason"] == (
                "[pii-checker] 检测到 1 项一般风险敏感信息；"
                "本次仅提醒，未触发确认或阻断。"
            )
        assert "a***@example.com" not in result["reason"]
        assert "alice@example.com" not in result["reason"]

    @pytest.mark.parametrize("pii_type", ["cn_id", "credit_card"])
    def test_warn_personal_data_is_general_and_non_blocking(self, pii_type):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "warn",
                    "findings": [{"type": pii_type, "severity": "warn"}],
                },
                "block",
                "UserPromptSubmit",
            )
        )

        assert result == {
            "decision": "allow",
            "reason": (
                "[pii-checker] 检测到 1 项一般风险敏感信息；"
                "本次仅提醒，未触发确认或阻断。"
            ),
        }

    @pytest.mark.parametrize(
        ("event_name", "policy", "expected_decision", "message_fragment"),
        [
            ("UserPromptSubmit", "ask", "ask", "当前策略要求确认"),
            ("UserPromptSubmit", "block", "block", "当前策略已阻断本次请求"),
            ("PreToolUse", "ask", "ask", "当前策略要求确认"),
            ("PreToolUse", "block", "block", "当前策略已阻断本次工具调用"),
            ("PostToolUse", "ask", "allow", "当前环节不支持确认/阻断"),
            ("PostToolUse", "block", "block", "原始工具结果不会进入模型上下文"),
            ("AfterModel", "ask", "allow", "当前环节不支持确认/阻断"),
            ("AfterModel", "block", "allow", "当前环节不支持确认/阻断"),
            ("PostToolUseFailure", "ask", "allow", "当前环节不支持确认/阻断"),
            ("PostToolUseFailure", "block", "allow", "当前环节不支持确认/阻断"),
        ],
    )
    def test_deny_uses_event_level_policy(
        self,
        event_name,
        policy,
        expected_decision,
        message_fragment,
    ):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "deny",
                    "findings": [
                        {
                            "type": "credential",
                            "severity": "deny",
                            "evidence_redacted": "token=[REDACTED]",
                            "raw_evidence": "raw-secret-value",
                        }
                    ],
                },
                policy,
                event_name,
            )
        )

        assert result["decision"] == expected_decision
        assert message_fragment in result["reason"]
        assert "token=[REDACTED]" not in result["reason"]
        assert "credential" not in result["reason"]
        assert "raw-secret-value" not in result["reason"]
        assert "raw_evidence" not in result["reason"]
        if expected_decision in {"ask", "block"}:
            assert "不会阻断" not in result["reason"]
        else:
            assert "已阻断本次" not in result["reason"]
            assert "本次仅提醒，不会阻断" in result["reason"]

    def test_post_tool_block_describes_content_boundary(self):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "deny",
                    "findings": [
                        {
                            "type": "credential",
                            "severity": "deny",
                            "evidence_redacted": "token=[REDACTED]",
                        }
                    ],
                },
                "block",
                "PostToolUse",
            )
        )

        assert result["decision"] == "block"
        assert "工具已经执行" in result["reason"]
        assert "不会进入模型上下文" in result["reason"]
        assert "外部副作用不会撤销" in result["reason"]
        assert "不会阻断" not in result["reason"]

    def test_post_tool_ask_describes_warning_boundary(self):
        result = json.loads(
            _format_cosh(
                {
                    "verdict": "deny",
                    "findings": [{"type": "credential", "severity": "deny"}],
                },
                "ask",
                "PostToolUse",
            )
        )

        assert result["decision"] == "allow"
        assert "工具已经执行" in result["reason"]
        assert "当前环节不支持确认/阻断" in result["reason"]
        assert "原始工具结果仍会进入模型上下文" in result["reason"]
        assert "外部副作用不会撤销" in result["reason"]


@pytest.mark.parametrize(
    ("payload", "expected"),
    [
        ({"prompt": "hello"}, "UserPromptSubmit"),
        ({"hookEventName": "PreToolUse"}, "PreToolUse"),
        (
            {
                "hook_event_name": "PostToolUse",
                "hookEventName": "PreToolUse",
            },
            "PostToolUse",
        ),
    ],
)
def test_hook_event_name_supports_both_fields_and_legacy_default(payload, expected):
    assert pii_checker_hook._hook_event_name(payload) == expected


class TestCoshHookMain:
    def _run_main(self, monkeypatch, capsys, input_data, policy="warn"):
        monkeypatch.setenv("PII_CHECKER_MODE", policy)
        monkeypatch.setattr(pii_checker_hook.sys, "stdin", io.StringIO(input_data))
        pii_checker_hook.main()
        return json.loads(capsys.readouterr().out)

    def test_empty_prompt_allows_without_cli(self, monkeypatch, capsys):
        def fail_run(*args, **kwargs):
            raise AssertionError("CLI should not be called")

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fail_run)

        output = self._run_main(monkeypatch, capsys, '{"prompt": ""}')
        assert output == {"decision": "allow"}

    def test_invalid_json_allows_without_cli(self, monkeypatch, capsys):
        def fail_run(*args, **kwargs):
            raise AssertionError("CLI should not be called")

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fail_run)

        output = self._run_main(monkeypatch, capsys, "not-json")
        assert output == {"decision": "allow"}

    def test_missing_prompt_allows_without_cli(self, monkeypatch, capsys):
        def fail_run(*args, **kwargs):
            raise AssertionError("CLI should not be called")

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fail_run)

        output = self._run_main(monkeypatch, capsys, '{"session_id": "abc"}')
        assert output == {"decision": "allow"}

    def test_calls_scan_pii_with_user_input_source(self, monkeypatch, capsys):
        captured = {}

        def fake_run(args, **kwargs):
            captured["args"] = args
            captured["kwargs"] = kwargs
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps(
                    {
                        "verdict": "warn",
                        "findings": [
                            {
                                "type": "phone_cn",
                                "severity": "warn",
                                "evidence_redacted": "138****8000",
                            }
                        ],
                    }
                ),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

        output = self._run_main(
            monkeypatch,
            capsys,
            json.dumps({"prompt": "Phone: 13800138000"}),
        )

        expected_context = json.dumps(
            {"agent_name": "cosh"},
            ensure_ascii=False,
            separators=(",", ":"),
        )
        assert captured["args"] == [
            "agent-sec-cli",
            "--trace-context",
            expected_context,
            "scan-pii",
            "--stdin",
            "--format",
            "json",
            "--redact-output",
            "--source",
            "user_input",
        ]
        assert captured["kwargs"]["input"] == "Phone: 13800138000"
        assert captured["kwargs"]["timeout"] == 10
        assert output["decision"] == "allow"
        assert output["reason"] == (
            "[pii-checker] 检测到 1 项一般风险敏感信息；"
            "本次仅提醒，未触发确认或阻断。"
        )

    def test_missing_event_defaults_to_user_prompt_policy_mapping(
        self, monkeypatch, capsys
    ):
        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps(
                    {
                        "verdict": "deny",
                        "findings": [
                            {
                                "type": "email",
                                "severity": "deny",
                                "evidence_redacted": "a***@example.com",
                            }
                        ],
                    }
                ),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

        output = self._run_main(
            monkeypatch,
            capsys,
            json.dumps({"prompt": "Contact alice@example.com"}),
            policy="ask",
        )

        assert output["decision"] == "ask"
        assert "当前策略要求确认，请确认后继续" in output["reason"]
        assert "email" not in output["reason"]
        assert "alice@example.com" not in output["reason"]

    def test_injects_trace_context_into_scan_pii_command(self, monkeypatch, capsys):
        captured = {}

        def fake_run(args, **kwargs):
            captured["args"] = args
            captured["kwargs"] = kwargs
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"verdict": "pass", "findings": []}),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

        output = self._run_main(
            monkeypatch,
            capsys,
            json.dumps(
                {
                    "prompt": "Phone: 13800138000",
                    "trace_id": "trace-1",
                    "session_id": "session-1",
                    "sessionId": "wrong-session",
                    "run_id": "run-1",
                    "tool_use_id": "tool-1",
                }
            ),
        )

        expected_context = json.dumps(
            {
                "agent_name": "cosh",
                "trace_id": "trace-1",
                "session_id": "session-1",
                "run_id": "run-1",
                "tool_call_id": "tool-1",
            },
            ensure_ascii=False,
            separators=(",", ":"),
        )
        assert output == {"decision": "allow"}
        assert captured["args"] == [
            "agent-sec-cli",
            "--trace-context",
            expected_context,
            "scan-pii",
            "--stdin",
            "--format",
            "json",
            "--redact-output",
            "--source",
            "user_input",
        ]
        assert captured["kwargs"]["check"] is False

    @pytest.mark.parametrize(
        ("payload", "expected_stdin", "expected_source"),
        [
            (
                {"hookEventName": "PreToolUse", "tool_input": {"command": "echo ok"}},
                '{"command":"echo ok"}',
                "tool_input",
            ),
            (
                {
                    "hook_event_name": "PostToolUse",
                    "tool_response": {"stdout": "alice@example.com"},
                },
                '{"stdout":"alice@example.com"}',
                "tool_output",
            ),
            (
                {
                    "hook_event_name": "PostToolUseFailure",
                    "error": "token=secret123456",
                },
                "token=secret123456",
                "tool_output",
            ),
            (
                {
                    "hook_event_name": "AfterModel",
                    "llm_response": {"text": "Contact alice@example.com"},
                },
                "Contact alice@example.com",
                "model_output",
            ),
        ],
    )
    def test_scans_additional_hook_events(
        self,
        monkeypatch,
        capsys,
        payload,
        expected_stdin,
        expected_source,
    ):
        captured = {}

        def fake_run(args, **kwargs):
            captured["args"] = args
            captured["kwargs"] = kwargs
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps(
                    {
                        "verdict": "warn",
                        "findings": [
                            {
                                "type": "email",
                                "severity": "warn",
                                "evidence_redacted": "a***@example.com",
                            }
                        ],
                    }
                ),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

        output = self._run_main(monkeypatch, capsys, json.dumps(payload))

        expected_context = json.dumps(
            {"agent_name": "cosh"},
            ensure_ascii=False,
            separators=(",", ":"),
        )
        assert captured["args"] == [
            "agent-sec-cli",
            "--trace-context",
            expected_context,
            "scan-pii",
            "--stdin",
            "--format",
            "json",
            "--redact-output",
            "--source",
            expected_source,
        ]
        assert captured["kwargs"]["input"] == expected_stdin
        assert output["decision"] == "allow"
        if payload.get("hook_event_name") in {"PostToolUse", "PostToolUseFailure"}:
            assert "工具已经执行" in output["reason"]
            assert "原始工具结果仍会进入模型上下文" in output["reason"]
            assert "外部副作用不会撤销" in output["reason"]
        else:
            assert output["reason"] == (
                "[pii-checker] 检测到 1 项一般风险敏感信息；"
                "本次仅提醒，未触发确认或阻断。"
            )
        assert "a***@example.com" not in output["reason"]

    def test_cli_nonzero_allows(self, monkeypatch, capsys):
        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args=args,
                returncode=1,
                stdout="",
                stderr="boom",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

        output = self._run_main(monkeypatch, capsys, '{"prompt": "hello"}')
        assert output == {"decision": "allow"}

    def test_cli_bad_json_allows(self, monkeypatch, capsys):
        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout="not-json",
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)

        output = self._run_main(monkeypatch, capsys, '{"prompt": "hello"}')
        assert output == {"decision": "allow"}


def test_environment_disabled_short_circuits_before_input_and_cli(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setenv("PII_CHECKER_HOOK_ENABLED", "false")
    disabled_hook = load_standalone_hook(
        "cosh_pii_checker_disabled_hook",
        Path(_COSH_HOOK),
    )
    monkeypatch.setattr(
        disabled_hook.sys,
        "stdin",
        type(
            "UnreadableInput",
            (),
            {"read": lambda *_args, **_kwargs: pytest.fail("input should not be read")},
        )(),
    )
    monkeypatch.setattr(
        disabled_hook.subprocess,
        "run",
        lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
    )

    disabled_hook.main()

    captured = capsys.readouterr()
    assert json.loads(captured.out) == {"decision": "allow"}
    assert captured.err == ""


def test_manifest_registers_all_supported_pii_events():
    manifest_path = (
        Path(__file__).resolve().parents[2]
        / ".."
        / "cosh-extension"
        / "cosh-extension.json"
    )
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

    pii_locations = []
    for hook_name, groups in manifest["hooks"].items():
        for group in groups:
            for hook in group.get("hooks", []):
                if hook.get("name") == "pii-checker":
                    pii_locations.append(hook_name)

    assert pii_locations == [
        "PreToolUse",
        "UserPromptSubmit",
        "AfterModel",
        "PostToolUse",
        "PostToolUseFailure",
    ]


def test_invalid_mode_reports_observe_fallback(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "banana")

    assert pii_checker_hook._read_policy() == "observe"
    assert "invalid PII_CHECKER_MODE; using observe" in capsys.readouterr().err
