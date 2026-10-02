"""Unit tests for the top-level CLI entry points."""

import json
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import Mock, patch

import pytest
from agent_sec_cli.asset_verify.verifier import format_verification_result
from agent_sec_cli.cli import (
    _extract_trace_context_arg,
    _is_read_only_skill_analyze,
    _resolve_time_range,
    app,
    main,
)
from agent_sec_cli.correlation_context import (
    TraceContext,
    clear_process_trace_context,
    get_current_trace_context,
)
from agent_sec_cli.security_middleware.result import ActionResult
from click import unstyle
from typer.testing import CliRunner


@patch("agent_sec_cli.cli.invoke")
def test_verify_preserves_shared_formatter_output(mock_invoke):
    rendered = format_verification_result(
        {
            "outcome": "no_candidates",
            "checked": 0,
            "passed": [],
            "failed": [],
        }
    )
    mock_invoke.return_value = ActionResult(
        success=True,
        exit_code=0,
        stdout=rendered,
    )

    result = CliRunner().invoke(app, ["verify"])

    assert result.exit_code == 0
    assert result.output == rendered
    mock_invoke.assert_called_once_with("verify", skill=None)


@patch("agent_sec_cli.cli.invoke")
def test_trace_context_is_hidden_global_option_and_commands_do_not_forward_it(
    mock_invoke,
):
    mock_invoke.return_value = ActionResult(success=True, exit_code=0, stdout="{}")

    try:
        result = CliRunner().invoke(
            app,
            [
                "--trace-context",
                '{"sessionId":"session-1","runId":"run-1"}',
                "scan-code",
                "--code",
                "echo ok",
                "--language",
                "bash",
            ],
        )
    finally:
        clear_process_trace_context()

    assert result.exit_code == 0
    mock_invoke.assert_called_once_with(
        "code_scan", code="echo ok", language="bash", mode="regex"
    )


@patch("agent_sec_cli.cli.invoke")
def test_trace_context_option_is_declared_but_not_used_by_typer_callback(mock_invoke):
    mock_invoke.return_value = ActionResult(success=True, exit_code=0, stdout="{}")

    try:
        result = CliRunner().invoke(
            app,
            ["--trace-context", "not-json", "scan-code", "--code", "echo ok"],
        )
    finally:
        clear_process_trace_context()

    assert result.exit_code == 0
    assert get_current_trace_context() is None
    mock_invoke.assert_called_once_with(
        "code_scan", code="echo ok", language="bash", mode="regex"
    )


def test_trace_context_option_is_hidden_from_help():
    result = CliRunner().invoke(app, ["--help"])

    assert result.exit_code == 0
    assert "--trace-context" not in result.output


def test_extract_trace_context_arg_supports_future_pre_app_initialization():
    assert (
        _extract_trace_context_arg(
            [
                "agent-sec-cli",
                "--trace-context",
                '{"session_id":"session-1"}',
                "scan-code",
            ]
        )
        == '{"session_id":"session-1"}'
    )


@pytest.mark.parametrize(
    "argv",
    [
        ["agent-sec-cli", "skill-ledger", "analyze", "/tmp/skill"],
        [
            "agent-sec-cli",
            "--trace-context",
            '{"session_id":"session-1"}',
            "skill-ledger",
            "analyze",
            "/tmp/skill",
        ],
    ],
)
def test_read_only_skill_analyze_skips_cli_logging(argv):
    assert _is_read_only_skill_analyze(argv) is True


def test_other_skill_ledger_commands_keep_cli_logging():
    assert (
        _is_read_only_skill_analyze(
            ["agent-sec-cli", "skill-ledger", "scan", "/tmp/skill"]
        )
        is False
    )


def test_extract_trace_context_arg_supports_equals_style():
    assert (
        _extract_trace_context_arg(
            ["agent-sec-cli", '--trace-context={"session_id":"session-1"}', "scan-code"]
        )
        == '{"session_id":"session-1"}'
    )


@pytest.mark.parametrize(
    "argv",
    [
        ["agent-sec-cli", "--trace-context", "", "scan-code"],
        ["agent-sec-cli", "--trace-context=", "scan-code"],
    ],
)
def test_extract_trace_context_arg_treats_empty_value_as_unset(argv):
    assert _extract_trace_context_arg(argv) is None


def test_extract_trace_context_arg_requires_value_before_another_option():
    with pytest.raises(ValueError, match="missing trace context value"):
        _extract_trace_context_arg(["agent-sec-cli", "--trace-context", "--version"])


def test_extract_trace_context_arg_uses_last_top_level_value():
    assert (
        _extract_trace_context_arg(
            [
                "agent-sec-cli",
                "--trace-context",
                '{"session_id":"session-1"}',
                '--trace-context={"session_id":"session-2"}',
                "scan-code",
            ]
        )
        == '{"session_id":"session-2"}'
    )


def test_resolve_time_range_normalizes_explicit_offsets_to_utc() -> None:
    since, until = _resolve_time_range(
        None,
        "2026-05-20T12:00:00+08:00",
        "2026-05-20T13:00:00+08:00",
    )

    assert since == "2026-05-20T04:00:00+00:00"
    assert until == "2026-05-20T05:00:00+00:00"


def test_resolve_time_range_last_hours_returns_utc_iso(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("agent_sec_cli.cli.time.time", lambda: 1_800_000_000.0)

    since, until = _resolve_time_range(1.5, None, None)

    assert (
        since
        == datetime.fromtimestamp(
            1_800_000_000.0 - 5400,
            tz=timezone.utc,
        ).isoformat()
    )
    assert until == "2027-01-15T08:00:00+00:00"


@pytest.mark.parametrize(
    ("args", "error"),
    [
        (["--last-hours", "-1", "--count"], "--last-hours must be non-negative"),
        (["--limit", "0", "--count"], "--limit must be positive"),
        (["--offset", "-5", "--count"], "--offset must be non-negative"),
    ],
)
def test_events_rejects_out_of_range_query_values_before_opening_reader(
    args: list[str], error: str
) -> None:
    with patch("agent_sec_cli.cli.get_reader") as get_reader:
        result = CliRunner().invoke(app, ["events", *args])

    assert result.exit_code == 1
    assert error in result.output
    get_reader.assert_not_called()


def test_events_accepts_query_value_boundaries() -> None:
    reader = Mock()
    reader.count.return_value = 0

    with patch("agent_sec_cli.cli.get_reader", return_value=reader):
        result = CliRunner().invoke(
            app,
            [
                "events",
                "--last-hours",
                "0",
                "--limit",
                "1",
                "--offset",
                "0",
                "--count",
            ],
        )

    assert result.exit_code == 0
    assert result.output == "0\n"
    reader.count.assert_called_once()


def test_extract_trace_context_arg_stops_at_posix_double_dash():
    assert (
        _extract_trace_context_arg(
            [
                "agent-sec-cli",
                "scan-code",
                "--",
                "--trace-context",
                '{"session_id":"not-top-level"}',
            ]
        )
        is None
    )


@pytest.mark.parametrize(
    "argv",
    [
        [
            "agent-sec-cli",
            "scan-code",
            "--trace-context",
            '{"session_id":"command-session"}',
        ],
        [
            "agent-sec-cli",
            "harden",
            "--trace-context",
            '{"session_id":"downstream-session"}',
        ],
    ],
)
def test_extract_trace_context_arg_ignores_command_arguments(argv):
    assert _extract_trace_context_arg(argv) is None


@patch("agent_sec_cli.cli.app")
def test_main_initializes_trace_context_before_app(mock_app, monkeypatch):
    monkeypatch.setattr(
        "sys.argv",
        ["agent-sec-cli", "--trace-context", '{"session_id":"session-1"}', "scan-code"],
    )

    try:
        main()
    finally:
        clear_process_trace_context()

    mock_app.assert_called_once()


@patch("agent_sec_cli.cli.invoke")
@patch("agent_sec_cli.cli.init_process_trace_context")
def test_main_initializes_process_trace_context_once(
    mock_init_process_trace_context,
    mock_invoke,
    monkeypatch,
):
    mock_invoke.return_value = ActionResult(success=True, exit_code=0, stdout="{}")
    monkeypatch.setattr(
        "sys.argv",
        [
            "agent-sec-cli",
            "--trace-context",
            '{"session_id":"session-1","run_id":"run-1"}',
            "scan-code",
            "--code",
            "echo ok",
        ],
    )

    with pytest.raises(SystemExit) as exc:
        main()

    assert exc.value.code == 0
    mock_init_process_trace_context.assert_called_once_with(
        TraceContext(session_id="session-1", run_id="run-1")
    )
    mock_invoke.assert_called_once_with(
        "code_scan", code="echo ok", language="bash", mode="regex"
    )


@patch("agent_sec_cli.cli.app")
def test_main_does_not_initialize_session_from_env(mock_app, monkeypatch):
    monkeypatch.setenv("AGENT_SEC_SESSION_ID", "env-session")
    monkeypatch.setattr("sys.argv", ["agent-sec-cli", "scan-code"])

    try:
        main()
        assert get_current_trace_context() is None
    finally:
        clear_process_trace_context()

    mock_app.assert_called_once()


@patch("agent_sec_cli.cli.app")
def test_main_invalid_trace_context_exits_before_app(mock_app, monkeypatch, capsys):
    monkeypatch.setattr(
        "sys.argv",
        ["agent-sec-cli", "--trace-context", "not-json", "scan-code"],
    )

    with pytest.raises(SystemExit) as exc:
        main()

    assert exc.value.code == 1
    assert "invalid trace context JSON" in capsys.readouterr().err
    mock_app.assert_not_called()


def test_main_initializes_invocation_context_and_logging_after_trace_context(
    monkeypatch,
):
    calls = []

    def fake_init_trace_context(trace_context):
        calls.append(("trace", trace_context))

    def fake_init_invocation_context():
        calls.append(("invocation", None))

    def fake_setup_cli_logging():
        calls.append(("logging", None))

    def fake_app():
        calls.append(("app", None))

    monkeypatch.setattr("sys.argv", ["agent-sec-cli", "scan-code"])
    monkeypatch.setattr(
        "agent_sec_cli.cli._init_trace_context", fake_init_trace_context
    )
    monkeypatch.setattr(
        "agent_sec_cli.cli.init_invocation_context",
        fake_init_invocation_context,
        raising=False,
    )
    monkeypatch.setattr(
        "agent_sec_cli.cli.setup_cli_logging",
        fake_setup_cli_logging,
        raising=False,
    )
    monkeypatch.setattr("agent_sec_cli.cli.app", fake_app)

    main()

    assert calls == [
        ("trace", None),
        ("invocation", None),
        ("logging", None),
        ("app", None),
    ]


def test_events_count_forwards_filters():
    captured = {}

    class Reader:
        def count(
            self,
            *,
            event_type=None,
            category=None,
            trace_id=None,
            session_id=None,
            run_id=None,
            since=None,
            until=None,
            offset=0,
        ):
            captured.update(
                {
                    "event_type": event_type,
                    "category": category,
                    "trace_id": trace_id,
                    "session_id": session_id,
                    "run_id": run_id,
                    "since": since,
                    "until": until,
                    "offset": offset,
                }
            )
            return 2

    with patch("agent_sec_cli.cli.get_reader", return_value=Reader()):
        result = CliRunner().invoke(
            app,
            [
                "events",
                "--trace-id",
                "trace-abc",
                "--session-id",
                "session-abc",
                "--run-id",
                "run-abc",
                "--count",
            ],
        )

    assert result.exit_code == 0
    assert result.output == "2\n"
    assert captured["trace_id"] == "trace-abc"
    assert captured["session_id"] == "session-abc"
    assert captured["run_id"] == "run-abc"


def test_events_count_by_forwards_filters():
    captured = {}

    class Reader:
        def count_by(
            self,
            group_field,
            *,
            event_type=None,
            category=None,
            trace_id=None,
            session_id=None,
            run_id=None,
            since=None,
            until=None,
            offset=0,
        ):
            captured.update(
                {
                    "group_field": group_field,
                    "event_type": event_type,
                    "category": category,
                    "trace_id": trace_id,
                    "session_id": session_id,
                    "run_id": run_id,
                    "since": since,
                    "until": until,
                    "offset": offset,
                }
            )
            return {"sandbox": 1}

    with patch("agent_sec_cli.cli.get_reader", return_value=Reader()):
        result = CliRunner().invoke(
            app,
            [
                "events",
                "--count-by",
                "category",
                "--event-type",
                "alpha",
                "--category",
                "sandbox",
                "--trace-id",
                "trace-abc",
                "--session-id",
                "session-abc",
                "--run-id",
                "run-abc",
            ],
        )

    assert result.exit_code == 0
    assert captured["group_field"] == "category"
    assert captured["event_type"] == "alpha"
    assert captured["category"] == "sandbox"
    assert captured["trace_id"] == "trace-abc"
    assert captured["session_id"] == "session-abc"
    assert captured["run_id"] == "run-abc"


def test_events_list_forwards_session_and_run_filters():
    reader = Mock()
    reader.query.return_value = []

    with patch("agent_sec_cli.cli.get_reader", return_value=reader):
        result = CliRunner().invoke(
            app,
            [
                "events",
                "--session-id",
                "session-abc",
                "--run-id",
                "run-abc",
                "--output",
                "json",
            ],
        )

    assert result.exit_code == 0
    assert result.output == "[]\n"
    assert reader.query.call_args.kwargs["session_id"] == "session-abc"
    assert reader.query.call_args.kwargs["run_id"] == "run-abc"


def test_events_summary_forwards_session_and_run_filters():
    reader = Mock()
    reader.query.return_value = []

    with patch("agent_sec_cli.cli.get_reader", return_value=reader):
        result = CliRunner().invoke(
            app,
            [
                "events",
                "--session-id",
                "session-abc",
                "--run-id",
                "run-abc",
                "--summary",
            ],
        )

    assert result.exit_code == 0
    assert result.output == "No security events recorded.\n\n"
    assert reader.query.call_args.kwargs["session_id"] == "session-abc"
    assert reader.query.call_args.kwargs["run_id"] == "run-abc"


def test_events_help_lists_session_and_run_filters():
    result = CliRunner().invoke(app, ["events", "--help"])

    assert result.exit_code == 0
    help_text = unstyle(result.output)
    assert "--session-id" in help_text
    assert "--run-id" in help_text


def test_capabilities_json_filter_outputs_current_environment(monkeypatch):
    monkeypatch.setenv("CODE_SCANNER_HOOK_ENABLED", "false")
    result = CliRunner().invoke(
        app,
        [
            "capabilities",
            "--agent",
            "qoder",
            "--capability",
            "code-scan",
            "--output",
            "json",
        ],
    )

    assert result.exit_code == 0
    payload = json.loads(result.output)
    assert len(payload) == 1
    assert payload[0]["agent"] == "qoder"
    assert payload[0]["capability"] == "code-scan"
    assert payload[0]["enabled"] == "disabled"
    assert payload[0]["mode"] == "observe"
    assert payload[0]["scan_mode"] == "-"
    assert payload[0]["timeout"] == "10"
    assert "hooks" not in payload[0]
    assert "source" not in payload[0]
    assert "config" not in payload[0]
    assert "config_path" not in payload[0]
    assert "raw" not in payload[0]["env"]["CODE_SCANNER_HOOK_ENABLED"]
    assert payload[0]["env"]["CODE_SCANNER_HOOK_ENABLED"]["effective"] is False


def test_capabilities_rejects_control_characters_without_terminal_escape():
    result = CliRunner().invoke(
        app,
        ["capabilities", "--agent", "\x1b[31m" + "x" * 100],
    )

    assert result.exit_code == 1
    assert "\x1b" not in result.output
    assert "\\x1b[31m" in result.output
    assert "x" * 80 not in result.output


def test_capabilities_default_table_output(monkeypatch):
    monkeypatch.delenv("OPENCLAW_CONFIG_PATH", raising=False)
    result = CliRunner().invoke(app, ["capabilities", "--agent", "cosh"])

    assert result.exit_code == 0
    lines = result.output.strip().split("\n")
    assert lines[0] == "[cosh]"
    assert lines[1].startswith("CAPABILITY")
    assert "AGENT" not in lines[1]
    assert "CAPABILITY" in lines[1]
    assert "ENABLED" in lines[1]
    assert "MODE" in lines[1]
    assert "SCAN_MODE" in lines[1]
    assert "TIMEOUT(s)" in lines[1]
    assert "HOOKS" not in lines[1]
    assert "SOURCE" not in lines[1]
    assert "code-scan" in result.output


def test_capabilities_rejects_plugin_capability_alias():
    result = CliRunner().invoke(
        app,
        ["capabilities", "--capability", "scan-code"],
    )

    assert result.exit_code == 1
    assert "unknown capability" in result.stderr


def test_capabilities_rejects_invalid_output_format():
    result = CliRunner().invoke(app, ["capabilities", "--output", "yaml"])

    assert result.exit_code == 1
    assert "--output must be one of" in result.stderr


class TestHardenCli(unittest.TestCase):
    def setUp(self):
        self.runner = CliRunner()

    def test_harden_help_shows_concise_summary(self):
        result = self.runner.invoke(app, ["harden", "--help"])

        self.assertEqual(result.exit_code, 0)
        self.assertIn("Usage: agent-sec-cli harden [SEHARDEN_ARGS]...", result.output)
        self.assertIn("Defaults:", result.output)
        self.assertIn("--scan --config agentos_baseline", result.output)
        self.assertIn("Examples:", result.output)
        self.assertIn("Common SEHarden flags:", result.output)
        self.assertIn("--downstream-help", result.output)
        self.assertNotIn(
            "Pass arguments through to `loongshield seharden`.", result.output
        )
        self.assertNotIn("-- --help", result.output)

    @patch("agent_sec_cli.cli.invoke")
    def test_harden_adds_default_scan_and_config_on_zero_args(self, mock_invoke):
        mock_invoke.return_value = ActionResult(success=True, exit_code=0)

        result = self.runner.invoke(app, ["harden"])

        self.assertEqual(result.exit_code, 0)
        mock_invoke.assert_called_once_with(
            "harden",
            args=["--scan", "--config", "agentos_baseline"],
        )

    @patch("agent_sec_cli.cli.invoke")
    def test_harden_forwards_unknown_args_to_backend(self, mock_invoke):
        mock_invoke.return_value = ActionResult(success=True, exit_code=0)

        result = self.runner.invoke(
            app,
            ["harden", "--scan", "--config", "agentos_baseline", "--dry-run"],
        )

        self.assertEqual(result.exit_code, 0)
        mock_invoke.assert_called_once_with(
            "harden",
            args=["--scan", "--config", "agentos_baseline", "--dry-run"],
        )

    @patch("agent_sec_cli.cli.invoke")
    def test_harden_adds_default_config_when_missing(self, mock_invoke):
        mock_invoke.return_value = ActionResult(success=True, exit_code=0)

        result = self.runner.invoke(app, ["harden", "--scan"])

        self.assertEqual(result.exit_code, 0)
        mock_invoke.assert_called_once_with(
            "harden",
            args=["--scan", "--config", "agentos_baseline"],
        )

    @patch("agent_sec_cli.cli.invoke")
    def test_harden_adds_default_scan_when_only_config_is_provided(self, mock_invoke):
        mock_invoke.return_value = ActionResult(success=True, exit_code=0)

        result = self.runner.invoke(app, ["harden", "--config", "custom_profile"])

        self.assertEqual(result.exit_code, 0)
        mock_invoke.assert_called_once_with(
            "harden",
            args=["--scan", "--config", "custom_profile"],
        )

    @patch("agent_sec_cli.cli.invoke")
    def test_harden_keeps_explicit_equals_style_config(self, mock_invoke):
        mock_invoke.return_value = ActionResult(success=True, exit_code=0)

        result = self.runner.invoke(
            app, ["harden", "--scan", "--config=custom_profile"]
        )

        self.assertEqual(result.exit_code, 0)
        mock_invoke.assert_called_once_with(
            "harden",
            args=["--scan", "--config=custom_profile"],
        )

    @patch("agent_sec_cli.cli.invoke")
    def test_harden_does_not_add_default_scan_for_reinforce(self, mock_invoke):
        mock_invoke.return_value = ActionResult(success=True, exit_code=0)

        result = self.runner.invoke(app, ["harden", "--reinforce", "--dry-run"])

        self.assertEqual(result.exit_code, 0)
        mock_invoke.assert_called_once_with(
            "harden",
            args=["--reinforce", "--dry-run", "--config", "agentos_baseline"],
        )

    @patch("agent_sec_cli.cli.invoke")
    def test_harden_keeps_explicit_verbose(self, mock_invoke):
        mock_invoke.return_value = ActionResult(success=True, exit_code=0)

        result = self.runner.invoke(app, ["harden", "--scan", "--verbose"])

        self.assertEqual(result.exit_code, 0)
        mock_invoke.assert_called_once_with(
            "harden",
            args=["--scan", "--verbose", "--config", "agentos_baseline"],
        )

    @patch("agent_sec_cli.cli.invoke")
    def test_harden_downstream_help_uses_backend_help(self, mock_invoke):
        mock_invoke.return_value = ActionResult(
            success=True,
            exit_code=0,
            stdout="seharden help\n",
        )

        result = self.runner.invoke(app, ["harden", "--downstream-help"])

        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.output, "seharden help\n")
        mock_invoke.assert_called_once_with("harden", args=["--help"])


class TestScanPiiCli(unittest.TestCase):
    def setUp(self):
        self.runner = CliRunner()

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_text_json(self, mock_invoke):
        mock_invoke.return_value = ActionResult(
            success=True,
            exit_code=0,
            stdout='{"ok": true, "verdict": "warn"}',
            data={
                "ok": True,
                "verdict": "warn",
                "summary": {"total": 1},
                "findings": [],
            },
        )

        result = self.runner.invoke(
            app,
            ["scan-pii", "--text", "alice@example.com", "--source", "manual"],
        )

        self.assertEqual(result.exit_code, 0)
        self.assertIn('"verdict": "warn"', result.output)
        mock_invoke.assert_called_once()
        _, kwargs = mock_invoke.call_args
        self.assertEqual(mock_invoke.call_args.args[0], "pii_scan")
        self.assertEqual(kwargs["text"], "alice@example.com")
        self.assertEqual(kwargs["source"], "manual")
        self.assertFalse(kwargs["raw_evidence"])
        self.assertIsNone(kwargs["max_bytes"])

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_stdin_json(self, mock_invoke):
        mock_invoke.return_value = ActionResult(
            success=True,
            exit_code=0,
            stdout='{"ok": true, "verdict": "warn"}',
            data={
                "ok": True,
                "verdict": "warn",
                "summary": {"total": 1},
                "findings": [],
            },
        )

        result = self.runner.invoke(
            app,
            ["scan-pii", "--stdin", "--source", "manual"],
            input="alice@example.com",
        )

        self.assertEqual(result.exit_code, 0)
        mock_invoke.assert_called_once()
        _, kwargs = mock_invoke.call_args
        self.assertEqual(kwargs["text"], "alice@example.com")
        self.assertEqual(kwargs["source"], "manual")

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_stdin_reports_byte_limit(self, mock_invoke):
        mock_invoke.return_value = ActionResult(
            success=True,
            exit_code=0,
            stdout='{"ok": true, "verdict": "pass"}',
            data={
                "ok": True,
                "verdict": "pass",
                "summary": {"total": 0},
                "findings": [],
            },
        )

        text = "备注🙂 alice@example.com"
        max_bytes = len("备注".encode("utf-8")) + 1
        result = self.runner.invoke(
            app,
            ["scan-pii", "--stdin", "--max-bytes", str(max_bytes)],
            input=text,
        )

        self.assertEqual(result.exit_code, 0)
        _, kwargs = mock_invoke.call_args
        self.assertEqual(kwargs["text"], "备注")
        self.assertTrue(kwargs["input_truncated"])
        self.assertEqual(kwargs["input_bytes_scanned"], max_bytes)
        self.assertNotIn("\ufffd", kwargs["text"])

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_stdin_rejects_invalid_utf8(self, mock_invoke):
        result = self.runner.invoke(app, ["scan-pii", "--stdin"], input=b"\xff")

        self.assertEqual(result.exit_code, 1)
        self.assertIn("--stdin must be valid UTF-8", result.output)
        mock_invoke.assert_not_called()

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_text_output(self, mock_invoke):
        mock_invoke.return_value = ActionResult(
            success=True,
            exit_code=0,
            data={
                "ok": True,
                "verdict": "deny",
                "summary": {"total": 1, "source": "manual"},
                "findings": [
                    {
                        "type": "api_key",
                        "severity": "deny",
                        "confidence": 0.99,
                        "evidence_redacted": "sk-a...[REDACTED]...7890",
                    }
                ],
            },
        )

        result = self.runner.invoke(
            app,
            ["scan-pii", "--text", "api_key=secret", "--format", "text"],
        )

        self.assertEqual(result.exit_code, 0)
        self.assertIn("Verdict: deny", result.output)
        self.assertIn("api_key", result.output)

    def test_scan_pii_requires_one_input(self):
        result = self.runner.invoke(app, ["scan-pii"])

        self.assertEqual(result.exit_code, 1)
        self.assertIn("provide exactly one", result.output)

    def test_scan_pii_rejects_multiple_inputs(self):
        result = self.runner.invoke(
            app,
            ["scan-pii", "--text", "hello", "--stdin"],
            input="alice@example.com",
        )

        self.assertEqual(result.exit_code, 1)
        self.assertIn("provide exactly one", result.output)

    def test_scan_pii_rejects_invalid_source(self):
        result = self.runner.invoke(
            app,
            ["scan-pii", "--text", "hello", "--source", "browser"],
        )

        self.assertEqual(result.exit_code, 1)
        self.assertIn("--source must be one of", result.output)

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_accepts_runtime_sources(self, mock_invoke):
        mock_invoke.return_value = ActionResult(
            success=True,
            exit_code=0,
            stdout='{"ok": true, "verdict": "pass"}',
            data={"ok": True, "verdict": "pass", "summary": {"total": 0}},
        )

        for source in ["tool_input", "tool_output", "model_output", "observability"]:
            with self.subTest(source=source):
                result = self.runner.invoke(
                    app,
                    ["scan-pii", "--text", "hello", "--source", source],
                )

                self.assertEqual(result.exit_code, 0)
                _, kwargs = mock_invoke.call_args
                self.assertEqual(kwargs["source"], source)

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_input_default_reads_full_file(self, mock_invoke):
        mock_invoke.return_value = ActionResult(
            success=True,
            exit_code=0,
            stdout='{"ok": true, "verdict": "warn"}',
            data={
                "ok": True,
                "verdict": "warn",
                "summary": {"total": 1},
                "findings": [],
            },
        )

        with self.runner.isolated_filesystem():
            text = "备注🙂 alice@example.com"
            Path("input.txt").write_text(text, encoding="utf-8")

            result = self.runner.invoke(
                app,
                ["scan-pii", "--input", "input.txt"],
            )

        self.assertEqual(result.exit_code, 0)
        _, kwargs = mock_invoke.call_args
        self.assertEqual(kwargs["text"], text)
        self.assertFalse(kwargs["input_truncated"])
        self.assertEqual(kwargs["input_bytes_scanned"], len(text.encode("utf-8")))
        self.assertIsNone(kwargs["max_bytes"])

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_input_reports_file_byte_limit(self, mock_invoke):
        mock_invoke.return_value = ActionResult(
            success=True,
            exit_code=0,
            stdout='{"ok": true, "verdict": "pass"}',
            data={
                "ok": True,
                "verdict": "pass",
                "summary": {"total": 0},
                "findings": [],
            },
        )

        with self.runner.isolated_filesystem():
            Path("input.txt").write_bytes("备注🙂 alice".encode("utf-8"))
            max_bytes = len("备注".encode("utf-8")) + 1

            result = self.runner.invoke(
                app,
                [
                    "scan-pii",
                    "--input",
                    "input.txt",
                    "--max-bytes",
                    str(max_bytes),
                ],
            )

        self.assertEqual(result.exit_code, 0)
        _, kwargs = mock_invoke.call_args
        self.assertTrue(kwargs["input_truncated"])
        self.assertEqual(kwargs["input_bytes_scanned"], max_bytes)
        self.assertNotIn("\ufffd", kwargs["text"])

    @patch("agent_sec_cli.pii_checker.cli.invoke")
    def test_scan_pii_input_rejects_invalid_utf8(self, mock_invoke):
        with self.runner.isolated_filesystem():
            Path("input.txt").write_bytes(b"\xff")

            result = self.runner.invoke(
                app,
                ["scan-pii", "--input", "input.txt"],
            )

        self.assertEqual(result.exit_code, 1)
        self.assertIn("--input must be valid UTF-8", result.output)
        mock_invoke.assert_not_called()

    def test_scan_pii_rejects_zero_max_bytes(self):
        result = self.runner.invoke(
            app,
            ["scan-pii", "--text", "hello", "--max-bytes", "0"],
        )

        self.assertEqual(result.exit_code, 1)
        self.assertIn("--max-bytes must be greater than zero", result.output)


if __name__ == "__main__":
    unittest.main()
