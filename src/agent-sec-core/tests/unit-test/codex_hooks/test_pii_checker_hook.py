"""Unit tests for codex-plugin/hooks/pii_checker_hook.py.

Coverage targets:
  - Triple hook point routing (UserPromptSubmit, PreToolUse & PostToolUse)
  - Text extraction from different event types
  - Fail-open paths (invalid JSON, empty text, subprocess errors)
  - Mode-based decisions (observe vs deny)
  - Output formatting for warnings and block reasons
  - Evidence sanitization (no raw PII in output)
  - Trace context injection
"""

import io
import json
import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import Any

import pytest
from standalone_hook_test_loader import load_standalone_hook

# ---------------------------------------------------------------------------
# Hook path & module import
# ---------------------------------------------------------------------------

_HOOKS_DIR = str(
    Path(__file__).resolve().parents[2]
    / ".."
    / "codex-plugin"
    / "hooks-plugin"
    / "hooks"
)
pii_checker_hook = load_standalone_hook(
    "codex_pii_checker_hook",
    Path(_HOOKS_DIR) / "pii_checker_hook.py",
)

_HOOK_SCRIPT = os.path.join(_HOOKS_DIR, "pii_checker_hook.py")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_hook(input_data, *, env_override=None):
    """Run pii_checker_hook.py as subprocess and return parsed JSON output."""
    env = os.environ.copy()
    if env_override:
        env.update(env_override)
    stdin_text = json.dumps(input_data) if isinstance(input_data, dict) else input_data
    proc = subprocess.run(
        [sys.executable, _HOOK_SCRIPT],
        input=stdin_text,
        capture_output=True,
        check=False,
        text=True,
        timeout=15,
        env=env,
    )
    assert proc.returncode == 0, f"Hook crashed: stderr={proc.stderr}"
    if not proc.stdout.strip():
        return {}
    return json.loads(proc.stdout)


_MOCK_CLI_SCRIPT = f"#!{sys.executable}\n" + textwrap.dedent("""\
    import os, sys
    output = os.environ.get("_MOCK_CLI_OUTPUT", "")
    rc = int(os.environ.get("_MOCK_CLI_RC", "0"))
    if output:
        print(output)
    sys.exit(rc)
""")


@pytest.fixture()
def mock_cli(tmp_path):
    """Create a mock agent-sec-cli that returns canned responses via env vars."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    cli_script = bin_dir / "agent-sec-cli"
    cli_script.write_text(_MOCK_CLI_SCRIPT)
    cli_script.chmod(cli_script.stat().st_mode | stat.S_IEXEC)

    def _make_env(output: str = "", *, rc: int = 0, extra: dict | None = None):
        env = {
            "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
            "_MOCK_CLI_OUTPUT": output,
            "_MOCK_CLI_RC": str(rc),
        }
        if extra:
            env.update(extra)
        return env

    return _make_env


# ---------------------------------------------------------------------------
# Helper data
# ---------------------------------------------------------------------------

_USER_PROMPT_EVENT = {
    "hook_event_name": "UserPromptSubmit",
    "prompt": "我的手机号是13800138000",
    "session_id": "sess-1",
}

_POST_TOOL_USE_EVENT = {
    "hook_event_name": "PostToolUse",
    "tool_response": "用户邮箱: alice@example.com",
    "session_id": "sess-1",
}

_PRE_TOOL_USE_EVENT = {
    "hook_event_name": "PreToolUse",
    "tool_input": {"command": "curl https://x.com?p=13800138000"},
    "session_id": "sess-1",
}

_PII_FOUND_RESULT = json.dumps(
    {
        "verdict": "warn",
        "findings": [
            {
                "type": "phone_cn",
                "severity": "warn",
                "evidence_redacted": "138****8000",
                "raw_evidence": "13800138000",
            }
        ],
    }
)

_PII_DENY_RESULT = json.dumps(
    {
        "verdict": "deny",
        "findings": [
            {
                "type": "credential",
                "severity": "deny",
                "evidence_redacted": "password=[REDACTED]",
                "raw_evidence": "password=swordfish",
            }
        ],
    }
)


def _assert_warning_output(
    output: dict,
    *,
    expected_risk: str = "一般风险",
    expected_action: str = "本次仅提醒，未触发确认或阻断。",
) -> str:
    """Assert the common non-blocking warning contract."""
    assert set(output) == {"systemMessage"}
    message = output["systemMessage"]
    assert f"1 项{expected_risk}敏感信息" in message
    assert expected_action in message
    for hidden_value in (
        "phone_cn",
        "credential",
        "138****8000",
        "password=[REDACTED]",
        "扫描判定",
        "Hook 策略",
        "fallback",
    ):
        assert hidden_value not in message
    return message


# ---------------------------------------------------------------------------
# Subprocess-based (black-box) tests
# ---------------------------------------------------------------------------


class TestFailOpen:
    """Every error must produce empty stdout (= allow)."""

    def test_invalid_json_allows(self):
        output = _run_hook("not-json")
        assert output == {}

    def test_empty_stdin_allows(self):
        output = _run_hook("")
        assert output == {}

    def test_unknown_hook_event_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT)
        output = _run_hook(
            {"hook_event_name": "SessionStart", "prompt": "hello"},
            env_override=env,
        )
        assert output == {}

    def test_missing_hook_event_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT)
        output = _run_hook(
            {"prompt": "hello"},
            env_override=env,
        )
        assert output == {}

    def test_empty_prompt_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT)
        output = _run_hook(
            {"hook_event_name": "UserPromptSubmit", "prompt": ""},
            env_override=env,
        )
        assert output == {}

    def test_whitespace_prompt_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT)
        output = _run_hook(
            {"hook_event_name": "UserPromptSubmit", "prompt": "   "},
            env_override=env,
        )
        assert output == {}

    def test_cli_nonzero_exit_allows(self, mock_cli):
        env = mock_cli(output="", rc=1, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)
        assert output == {}

    def test_cli_invalid_json_allows(self, mock_cli):
        env = mock_cli(output="not-json", extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)
        assert output == {}


class TestTextExtraction:
    """Verify text extraction for different hook events."""

    def test_post_tool_use_string_response(self, mock_cli):
        env = mock_cli(output=_PII_DENY_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {
                "hook_event_name": "PostToolUse",
                "tool_response": "Phone: 13800138000",
            },
            env_override=env,
        )
        assert output["decision"] == "block"

    def test_post_tool_use_dict_response(self, mock_cli):
        env = mock_cli(output=_PII_DENY_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {
                "hook_event_name": "PostToolUse",
                "tool_response": {"output": "email: alice@corp.com"},
            },
            env_override=env,
        )
        assert output["decision"] == "block"

    def test_post_tool_use_empty_string_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {"hook_event_name": "PostToolUse", "tool_response": ""},
            env_override=env,
        )
        assert output == {}

    def test_post_tool_use_none_response_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {"hook_event_name": "PostToolUse"},
            env_override=env,
        )
        assert output == {}

    def test_pre_tool_use_string_input(self, mock_cli):
        env = mock_cli(output=_PII_DENY_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {
                "hook_event_name": "PreToolUse",
                "tool_input": "curl https://x.com?p=13800138000",
            },
            env_override=env,
        )
        assert output["decision"] == "block"

    def test_pre_tool_use_dict_input(self, mock_cli):
        env = mock_cli(output=_PII_DENY_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {
                "hook_event_name": "PreToolUse",
                "tool_input": {"command": "curl https://x.com?p=13800138000"},
            },
            env_override=env,
        )
        assert output["decision"] == "block"

    def test_pre_tool_use_empty_string_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {"hook_event_name": "PreToolUse", "tool_input": ""},
            env_override=env,
        )
        assert output == {}

    def test_pre_tool_use_none_input_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {"hook_event_name": "PreToolUse"},
            env_override=env,
        )
        assert output == {}

    def test_pre_tool_use_empty_dict_allows(self, mock_cli):
        # Empty dict serializes to "{}" (non-empty string) but has no PII;
        # the hook must short-circuit and NOT call scan-pii. If it did scan,
        # the mock would return PII and deny mode would block.
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {"hook_event_name": "PreToolUse", "tool_input": {}},
            env_override=env,
        )
        assert output == {}

    def test_pre_tool_use_empty_list_allows(self, mock_cli):
        # Empty list serializes to "[]" — same short-circuit as empty dict.
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(
            {"hook_event_name": "PreToolUse", "tool_input": []},
            env_override=env,
        )
        assert output == {}


class TestObserveMode:
    """In observe mode, PII is detected but not blocked."""

    def test_pii_in_prompt_not_blocked(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "observe"})
        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)
        assert output == {}

    def test_pii_in_tool_output_not_blocked(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "observe"})
        output = _run_hook(_POST_TOOL_USE_EVENT, env_override=env)
        assert output == {}

    def test_pii_in_tool_input_not_blocked(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "observe"})
        output = _run_hook(_PRE_TOOL_USE_EVENT, env_override=env)
        assert output == {}


def test_environment_disabled_short_circuits_before_input_and_cli(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setenv("PII_CHECKER_HOOK_ENABLED", "false")
    disabled_hook = load_standalone_hook(
        "codex_pii_checker_disabled_hook",
        Path(_HOOK_SCRIPT),
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
    assert captured.out == ""
    assert captured.err == ""


class TestUnifiedHookPolicyWarnings:
    """Warnings show user-facing risk and actual behavior only."""

    @pytest.mark.parametrize(
        (
            "policy",
            "scan_output",
            "expected_risk",
            "expected_action",
        ),
        (
            (
                "warn",
                _PII_DENY_RESULT,
                "高风险",
                "本次仅提醒，未触发确认或阻断。",
            ),
            (
                "ask",
                _PII_DENY_RESULT,
                "高风险",
                "当前环节不支持确认/阻断，本次仅提醒，不会阻断。",
            ),
            (
                "block",
                _PII_FOUND_RESULT,
                "一般风险",
                "本次仅提醒，未触发确认或阻断。",
            ),
            (
                "warn",
                _PII_FOUND_RESULT,
                "一般风险",
                "本次仅提醒，未触发确认或阻断。",
            ),
        ),
    )
    def test_non_blocking_output_reports_actual_behavior(
        self,
        mock_cli,
        policy,
        scan_output,
        expected_risk,
        expected_action,
    ):
        env = mock_cli(
            output=scan_output,
            extra={
                "PII_CHECKER_HOOK_ENABLED": "true",
                "PII_CHECKER_MODE": policy,
            },
        )

        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)

        _assert_warning_output(
            output,
            expected_risk=expected_risk,
            expected_action=expected_action,
        )

    def test_block_policy_still_blocks_deny_verdict(self, mock_cli):
        env = mock_cli(
            output=_PII_DENY_RESULT,
            extra={
                "PII_CHECKER_HOOK_ENABLED": "true",
                "PII_CHECKER_MODE": "block",
            },
        )

        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)

        assert output["decision"] == "block"
        assert "1 项高风险敏感信息" in output["reason"]
        assert "当前策略已阻断本次请求。" in output["reason"]
        for hidden_value in (
            "credential",
            "password=[REDACTED]",
            "password=swordfish",
            "deny",
            "block",
        ):
            assert hidden_value not in output["reason"]


class TestDenyMode:
    """Deny mode preserves scanner warn and deny severity."""

    def test_pass_verdict_allows(self, mock_cli):
        env = mock_cli(
            output=json.dumps({"verdict": "pass", "findings": []}),
            extra={"PII_CHECKER_MODE": "deny"},
        )
        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)
        assert output == {}

    def test_warn_with_no_findings_allows(self, mock_cli):
        env = mock_cli(
            output=json.dumps({"verdict": "warn", "findings": []}),
            extra={"PII_CHECKER_MODE": "deny"},
        )
        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)
        assert output == {}

    def test_warn_verdict_alerts_user_prompt(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)
        _assert_warning_output(output)

    def test_warn_verdict_alerts_post_tool_use(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(_POST_TOOL_USE_EVENT, env_override=env)
        message = _assert_warning_output(
            output,
            expected_action="工具已经执行；本次仅提醒，未触发确认或阻断",
        )
        assert "原始工具结果仍会进入模型上下文" in message
        assert "外部副作用不会撤销" in message

    @pytest.mark.parametrize(
        ("event_data", "expected_action"),
        (
            (_USER_PROMPT_EVENT, "当前策略已阻断本次请求。"),
            (_PRE_TOOL_USE_EVENT, "当前策略已阻断本次工具调用。"),
            (
                _POST_TOOL_USE_EVENT,
                (
                    "工具已经执行；原始工具结果不会进入模型上下文，"
                    "已发生的外部副作用不会撤销。"
                ),
            ),
        ),
    )
    def test_deny_verdict_blocks(self, mock_cli, event_data, expected_action):
        env = mock_cli(output=_PII_DENY_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(event_data, env_override=env)
        assert output["decision"] == "block"
        assert "1 项高风险敏感信息" in output["reason"]
        assert expected_action in output["reason"]
        assert "credential" not in output["reason"]
        assert "password=[REDACTED]" not in output["reason"]

    def test_no_raw_pii_in_output(self, mock_cli):
        """Warning output must never contain raw PII content."""
        env = mock_cli(
            output=json.dumps(
                {
                    "verdict": "warn",
                    "findings": [
                        {
                            "type": "phone_cn",
                            "severity": "warn",
                            "evidence_redacted": "138****8000",
                            "raw_evidence": "13800138000",
                        }
                    ],
                }
            ),
            extra={"PII_CHECKER_MODE": "deny"},
        )
        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)
        message = _assert_warning_output(output)
        assert "13800138000" not in message

    def test_warn_verdict_alerts_pre_tool_use(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "deny"})
        output = _run_hook(_PRE_TOOL_USE_EVENT, env_override=env)
        _assert_warning_output(output)

    def test_unknown_verdict_with_findings_fails_open(self, mock_cli: Any) -> None:
        env = mock_cli(
            output=json.dumps(
                {
                    "verdict": "unexpected",
                    "findings": [
                        {
                            "type": "unknown",
                            "severity": "unexpected",
                            "evidence_redacted": "[REDACTED]",
                        }
                    ],
                }
            ),
            extra={"PII_CHECKER_MODE": "deny"},
        )
        output = _run_hook(_PRE_TOOL_USE_EVENT, env_override=env)
        assert output == {}


class TestUnknownMode:
    """Unknown mode acts as fail-open."""

    def test_unknown_mode_allows(self, mock_cli):
        env = mock_cli(output=_PII_FOUND_RESULT, extra={"PII_CHECKER_MODE": "banana"})
        output = _run_hook(_USER_PROMPT_EVENT, env_override=env)
        assert output == {}


def test_invalid_mode_reports_observe_fallback(monkeypatch, capsys):
    monkeypatch.setenv("PII_CHECKER_MODE", "banana")

    assert pii_checker_hook._read_policy() == "observe"
    assert "invalid PII_CHECKER_MODE; using observe" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Monkeypatch-based (white-box) tests
# ---------------------------------------------------------------------------


class TestMainMonkeypatch:
    """Direct main() testing with mocked subprocess."""

    def _run_main(self, monkeypatch, capsys, input_data, *, mode="deny"):
        monkeypatch.setattr(pii_checker_hook, "MODE", mode)
        monkeypatch.setattr(
            pii_checker_hook.sys,
            "stdin",
            io.StringIO(
                json.dumps(input_data) if isinstance(input_data, dict) else input_data
            ),
        )
        pii_checker_hook.main()
        out = capsys.readouterr().out
        return json.loads(out) if out.strip() else {}

    def test_subprocess_exception_allows(self, monkeypatch, capsys):
        def fail_run(*args, **kwargs):
            raise OSError("command not found")

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fail_run)
        output = self._run_main(monkeypatch, capsys, _USER_PROMPT_EVENT)
        assert output == {}

    def test_trace_context_injected_for_user_prompt(self, monkeypatch, capsys):
        captured = {}

        def fake_run(args, **kwargs):
            captured["args"] = args
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"verdict": "pass", "findings": []}),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "hello world",
                "trace_id": "t1",
                "session_id": "s1",
            },
        )
        assert "--trace-context" in captured["args"]
        assert "--source" in captured["args"]
        source_idx = captured["args"].index("--source")
        assert captured["args"][source_idx + 1] == "user_input"

    def test_trace_context_injected_for_post_tool_use(self, monkeypatch, capsys):
        captured = {}

        def fake_run(args, **kwargs):
            captured["args"] = args
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"verdict": "pass", "findings": []}),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "PostToolUse",
                "tool_response": "output data",
                "trace_id": "t1",
            },
        )
        source_idx = captured["args"].index("--source")
        assert captured["args"][source_idx + 1] == "tool_output"

    def test_trace_context_injected_for_pre_tool_use(self, monkeypatch, capsys):
        captured = {}

        def fake_run(args, **kwargs):
            captured["args"] = args
            captured["input"] = kwargs.get("input")
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"verdict": "pass", "findings": []}),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "PreToolUse",
                "tool_input": {"command": "curl https://x.com?p=13800138000"},
                "trace_id": "t1",
            },
        )
        source_idx = captured["args"].index("--source")
        assert captured["args"][source_idx + 1] == "tool_input"
        # dict tool_input is serialized to JSON before scanning
        assert "command" in captured["input"]
        assert "13800138000" in captured["input"]

    def test_deny_mode_blocks_pre_tool_use(self, monkeypatch, capsys):
        """deny mode + PreToolUse PII → block with tool input message."""

        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps(
                    {
                        "verdict": "deny",
                        "findings": [
                            {
                                "type": "phone_cn",
                                "severity": "deny",
                                "evidence_redacted": "138****",
                            },
                        ],
                    }
                ),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        output = self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "PreToolUse",
                "tool_input": {"command": "curl x?p=13800138000"},
            },
            mode="deny",
        )
        assert output["decision"] == "block"
        assert "1 项高风险敏感信息" in output["reason"]
        assert "当前策略已阻断本次工具调用。" in output["reason"]

    def test_scan_text_passed_via_stdin(self, monkeypatch, capsys):
        captured = {}

        def fake_run(args, **kwargs):
            captured["input"] = kwargs.get("input")
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"verdict": "pass", "findings": []}),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "my phone 13800138000",
            },
        )
        assert captured["input"] == "my phone 13800138000"

    def test_deny_mode_alerts_warn_findings(self, monkeypatch, capsys):
        """deny mode + warn findings → non-blocking system message."""

        def fake_run(args, **kwargs):
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
                                "evidence_redacted": "138****",
                            },
                        ],
                    }
                ),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        output = self._run_main(
            monkeypatch,
            capsys,
            {"hook_event_name": "UserPromptSubmit", "prompt": "my phone 13800138000"},
            mode="deny",
        )
        assert set(output) == {"systemMessage"}
        assert "1 项一般风险敏感信息" in output["systemMessage"]
        assert "本次仅提醒，未触发确认或阻断。" in output["systemMessage"]
        assert "phone_cn" not in output["systemMessage"]

    def test_deny_mode_blocks_post_tool_use(self, monkeypatch, capsys):
        """deny mode + PostToolUse PII → block with tool output message."""

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
                                "evidence_redacted": "a***@x.com",
                            },
                        ],
                    }
                ),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        output = self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "PostToolUse",
                "tool_response": "email is alice@example.com",
            },
            mode="deny",
        )
        assert output["decision"] == "block"
        assert "1 项高风险敏感信息" in output["reason"]
        assert "工具已经执行" in output["reason"]
        assert "原始工具结果不会进入模型上下文" in output["reason"]
        assert "已发生的外部副作用不会撤销" in output["reason"]

    def test_observe_mode_allows_findings(self, monkeypatch, capsys):
        """observe mode + findings → allow."""

        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps(
                    {
                        "verdict": "warn",
                        "findings": [{"type": "phone_cn", "severity": "warn"}],
                    }
                ),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        output = self._run_main(
            monkeypatch,
            capsys,
            {"hook_event_name": "UserPromptSubmit", "prompt": "13800138000"},
            mode="observe",
        )
        assert output == {}

    def test_nonzero_returncode_allows(self, monkeypatch, capsys):
        """CLI error → fail-open."""

        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args=args, returncode=1, stdout="", stderr="error"
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        output = self._run_main(
            monkeypatch,
            capsys,
            {"hook_event_name": "UserPromptSubmit", "prompt": "13800138000"},
        )
        assert output == {}

    def test_invalid_json_stdout_allows(self, monkeypatch, capsys):
        """Invalid JSON from CLI → fail-open."""

        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args=args, returncode=0, stdout="not-json", stderr=""
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        output = self._run_main(
            monkeypatch,
            capsys,
            {"hook_event_name": "UserPromptSubmit", "prompt": "hello"},
        )
        assert output == {}

    def test_invalid_stdin_allows(self, monkeypatch, capsys):
        """Invalid JSON stdin → fail-open."""
        output = self._run_main(monkeypatch, capsys, "{{not valid")
        assert output == {}

    def test_unknown_hook_event_allows(self, monkeypatch, capsys):
        """Unknown hook event → fail-open."""
        output = self._run_main(
            monkeypatch,
            capsys,
            {"hook_event_name": "UnknownEvent", "prompt": "hello"},
        )
        assert output == {}

    def test_post_tool_use_dict_response(self, monkeypatch, capsys):
        """PostToolUse with dict tool_response → serialized for scan."""
        captured = {}

        def fake_run(args, **kwargs):
            captured["input"] = kwargs.get("input")
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"verdict": "pass", "findings": []}),
                stderr="",
            )

        monkeypatch.setattr(pii_checker_hook.subprocess, "run", fake_run)
        self._run_main(
            monkeypatch,
            capsys,
            {"hook_event_name": "PostToolUse", "tool_response": {"data": "value"}},
        )
        # Should be JSON-serialized
        assert "data" in captured["input"]
        assert "value" in captured["input"]

    def test_post_tool_use_empty_string_allows(self, monkeypatch, capsys):
        """PostToolUse with empty string response → nothing to scan."""
        output = self._run_main(
            monkeypatch,
            capsys,
            {"hook_event_name": "PostToolUse", "tool_response": "  "},
        )
        assert output == {}

    def test_post_tool_use_none_response_allows(self, monkeypatch, capsys):
        """PostToolUse with null response → nothing to scan."""
        output = self._run_main(
            monkeypatch,
            capsys,
            {"hook_event_name": "PostToolUse", "tool_response": None},
        )
        assert output == {}

    def test_pass_verdict_allows(self, monkeypatch, capsys):
        """verdict=pass with empty findings → allow."""

        def fake_run(args, **kwargs):
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
            {"hook_event_name": "UserPromptSubmit", "prompt": "hello world"},
        )
        assert output == {}


# ---------------------------------------------------------------------------
# Unit tests for helper functions
# ---------------------------------------------------------------------------


class TestHelpers:
    """Test internal helper functions."""

    def test_as_list_with_list(self):
        assert pii_checker_hook._as_list([1, 2]) == [1, 2]

    def test_as_list_with_non_list(self):
        assert pii_checker_hook._as_list("hello") == []
        assert pii_checker_hook._as_list(None) == []

    def test_safe_text_with_string(self):
        assert pii_checker_hook._safe_text("hello") == "hello"

    def test_safe_text_with_non_string(self):
        assert pii_checker_hook._safe_text(None) == ""
        assert pii_checker_hook._safe_text(123) == ""

    @pytest.mark.parametrize(
        ("severity", "verdict", "expected"),
        (
            ("deny", "warn", "high"),
            ("warn", "deny", "general"),
            ("unknown", "deny", "high"),
            ("unknown", "warn", "general"),
        ),
    )
    def test_finding_risk_uses_severity_with_verdict_fallback(
        self, severity, verdict, expected
    ):
        assert (
            pii_checker_hook._finding_risk({"severity": severity}, verdict) == expected
        )

    @pytest.mark.parametrize(
        ("verdict", "findings", "expected"),
        (
            ("deny", [{"severity": "deny"}], "检测到 1 项高风险敏感信息"),
            ("warn", [{"severity": "warn"}], "检测到 1 项一般风险敏感信息"),
            (
                "deny",
                [{"severity": "deny"}, {"severity": "warn"}],
                "检测到 2 项敏感信息（高风险 1、一般风险 1）",
            ),
        ),
    )
    def test_risk_summary(self, verdict, findings, expected):
        assert pii_checker_hook._risk_summary(verdict, findings) == expected

    def test_truncated_details_use_total_counts_and_reject_malformed_counts(self):
        summary = {
            "findings_truncated": True,
            "total": 20001,
            "by_severity": {"deny": 1, "warn": 20000},
        }
        findings = [{"severity": "deny"}]
        message = pii_checker_hook._risk_summary("deny", findings, summary)
        assert "20001" in message and "20000" in message
        assert "明细已省略" in message
        summary["total"] = "20001"
        assert (
            pii_checker_hook._risk_summary("deny", findings, summary)
            == "检测到 1 项高风险敏感信息"
        )


class TestFormatBlockReason:
    """Test _format_block_reason output formatting."""

    def test_includes_mixed_risk_counts_without_internal_details(self):
        findings = [
            {"type": "credential", "severity": "deny", "evidence_redacted": "secret"},
            {"type": "email", "severity": "warn", "evidence_redacted": "a***@x.com"},
        ]
        reason = pii_checker_hook._format_block_reason(
            findings, "UserPromptSubmit", "deny"
        )
        assert "检测到 2 项敏感信息（高风险 1、一般风险 1）" in reason
        for hidden_value in ("credential", "email", "secret", "a***@x.com", "deny"):
            assert hidden_value not in reason

    def test_post_tool_use_message(self):
        findings = [{"type": "credential", "severity": "deny"}]
        reason = pii_checker_hook._format_block_reason(findings, "PostToolUse", "deny")
        assert "工具已经执行" in reason
        assert "原始工具结果不会进入模型上下文" in reason
        assert "已发生的外部副作用不会撤销" in reason

    def test_pre_tool_use_message(self):
        findings = [{"type": "credential", "severity": "deny"}]
        reason = pii_checker_hook._format_block_reason(findings, "PreToolUse", "deny")
        assert "当前策略已阻断本次工具调用。" in reason

    def test_user_prompt_submit_message(self):
        findings = [{"type": "credential", "severity": "deny"}]
        reason = pii_checker_hook._format_block_reason(
            findings, "UserPromptSubmit", "deny"
        )
        assert "当前策略已阻断本次请求。" in reason


class TestFormatWarningMessage:
    """Test non-blocking warning output formatting."""

    def test_hides_internal_details_and_reports_warning_behavior(self):
        findings = [
            {
                "type": "phone_cn",
                "severity": "warn",
                "evidence_redacted": "138****8000",
                "raw_evidence": "13800138000",
            }
        ]
        message = pii_checker_hook._format_warning_message(
            findings,
            "UserPromptSubmit",
            "warn",
            "warn",
        )
        assert "检测到 1 项一般风险敏感信息" in message
        assert "本次仅提醒，未触发确认或阻断。" in message
        for hidden_value in (
            "phone_cn",
            "warn",
            "138****8000",
            "13800138000",
            "扫描判定",
            "Hook 策略",
        ):
            assert hidden_value not in message

    def test_ask_reports_capability_limit(self):
        message = pii_checker_hook._format_warning_message(
            [{"severity": "deny"}],
            "UserPromptSubmit",
            "deny",
            "ask",
        )
        assert "检测到 1 项高风险敏感信息" in message
        assert "当前环节不支持确认/阻断，本次仅提醒，不会阻断。" in message
        assert "ask" not in message
        assert "fallback" not in message

    def test_warn_finding_does_not_report_capability_degradation(self):
        message = pii_checker_hook._format_warning_message(
            [{"severity": "warn"}],
            "UserPromptSubmit",
            "warn",
            "ask",
        )
        assert "本次仅提醒，未触发确认或阻断。" in message
        assert "当前环节不支持" not in message

    def test_post_tool_warning_reports_execution_and_content_boundary(self):
        message = pii_checker_hook._format_warning_message(
            [{"severity": "warn"}],
            "PostToolUse",
            "warn",
            "warn",
        )
        assert "工具已经执行" in message
        assert "原始工具结果仍会进入模型上下文" in message
        assert "外部副作用不会撤销" in message
