"""Unit tests for cosh-extension/hooks/code_scanner_hook.py.

Coverage targets:
  - Fail-open paths (invalid JSON, empty command, subprocess errors, bad CLI output)
  - Decision matrix (pass -> allow; warn/deny -> ask with findings; error/unknown -> allow)
  - The enabled flag and the fixed ask-mode policy with its diagnostics
  - The exact scan-code argv contract and trace-context injection
"""

import io
import json
import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest
from standalone_hook_test_loader import load_standalone_hook

_HOOKS_DIR = str(
    Path(__file__).resolve().parents[2]
    / ".."
    / "cosh-extension"
    / "hooks"
)
code_scanner_hook = load_standalone_hook(
    "cosh_code_scanner_hook",
    Path(_HOOKS_DIR) / "code_scanner_hook.py",
)

_HOOK_SCRIPT = os.path.join(_HOOKS_DIR, "code_scanner_hook.py")


def _run_hook_process(input_data, *, env_override=None):
    """Run code_scanner_hook.py as a subprocess and return the process."""
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
    return proc


def _run_hook(input_data, *, env_override=None):
    """Run code_scanner_hook.py as a subprocess and return parsed JSON output."""
    proc = _run_hook_process(input_data, env_override=env_override)
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


class TestFailOpen:
    """Every unexpected condition must fail open to allow."""

    def _run_main(self, monkeypatch, capsys, input_data):
        monkeypatch.setattr(
            "sys.stdin",
            io.StringIO(
                json.dumps(input_data) if isinstance(input_data, dict) else input_data
            ),
        )
        code_scanner_hook.main()
        out = capsys.readouterr().out
        return json.loads(out) if out.strip() else {}

    def test_invalid_json_allows(self, monkeypatch, capsys):
        assert self._run_main(monkeypatch, capsys, "not-json{") == {"decision": "allow"}

    def test_empty_stdin_allows(self, monkeypatch, capsys):
        assert self._run_main(monkeypatch, capsys, "") == {"decision": "allow"}

    def test_missing_tool_input_allows(self, monkeypatch, capsys):
        assert self._run_main(monkeypatch, capsys, {"tool_name": "shell"}) == {
            "decision": "allow"
        }

    def test_unknown_tool_name_allows_without_scanning(self, monkeypatch, capsys):
        called = []

        def fake_run(*args, **kwargs):
            called.append(args)
            raise AssertionError("unknown tools must not reach the CLI")

        monkeypatch.setattr(code_scanner_hook.subprocess, "run", fake_run)
        output = self._run_main(
            monkeypatch, capsys, {"tool_name": "read_file", "tool_input": {"path": "/etc"}}
        )
        assert output == {"decision": "allow"}
        assert called == []

    def test_empty_command_allows(self, monkeypatch, capsys):
        output = self._run_main(
            monkeypatch,
            capsys,
            {"tool_name": "shell", "tool_input": {"command": ""}},
        )
        assert output == {"decision": "allow"}

    def test_whitespace_command_allows(self, monkeypatch, capsys):
        output = self._run_main(
            monkeypatch,
            capsys,
            {"tool_name": "shell", "tool_input": {"command": "   "}},
        )
        assert output == {"decision": "allow"}

    def test_non_string_command_allows(self, monkeypatch, capsys):
        output = self._run_main(
            monkeypatch,
            capsys,
            {"tool_name": "shell", "tool_input": {"command": ["rm", "-rf", "/"]}},
        )
        assert output == {"decision": "allow"}

    def test_subprocess_exception_allows(self, monkeypatch, capsys):
        def fail_run(*args, **kwargs):
            raise OSError("command not found")

        monkeypatch.setattr(code_scanner_hook.subprocess, "run", fail_run)
        output = self._run_main(
            monkeypatch, capsys, {"tool_input": {"command": "rm -rf /"}}
        )
        assert output == {"decision": "allow"}

    def test_cli_nonzero_exit_allows(self, mock_cli):
        env = mock_cli("", rc=1)
        output = _run_hook(
            {"tool_name": "shell", "tool_input": {"command": "echo hi"}}, env_override=env
        )
        assert output == {"decision": "allow"}

    def test_cli_invalid_json_stdout_allows(self, mock_cli):
        env = mock_cli("not-json{")
        output = _run_hook(
            {"tool_name": "shell", "tool_input": {"command": "echo hi"}}, env_override=env
        )
        assert output == {"decision": "allow"}


class TestDecisions:
    def _scan(self, monkeypatch, capsys, verdict, findings):
        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"verdict": verdict, "findings": findings}),
                stderr="",
            )

        monkeypatch.setattr(code_scanner_hook.subprocess, "run", fake_run)
        monkeypatch.setattr(
            "sys.stdin",
            io.StringIO(
                json.dumps({"tool_name": "shell", "tool_input": {"command": "echo hi"}})
            ),
        )
        code_scanner_hook.main()
        out = capsys.readouterr().out
        return json.loads(out) if out.strip() else {}

    def test_pass_verdict_allows(self, monkeypatch, capsys):
        output = self._scan(monkeypatch, capsys, "pass", [])
        assert output == {"decision": "allow"}

    def test_warn_verdict_asks_with_findings(self, monkeypatch, capsys):
        output = self._scan(
            monkeypatch,
            capsys,
            "warn",
            [{"desc_zh": "危险命令"}, {"desc_zh": "敏感路径"}],
        )
        assert output["decision"] == "ask"
        assert output["systemMessage"] == (
            "[code-scanner] Detected 2 issue(s):\n- 危险命令\n- 敏感路径"
        )

    def test_deny_verdict_asks_with_findings(self, monkeypatch, capsys):
        output = self._scan(
            monkeypatch, capsys, "deny", [{"desc_zh": "危险命令"}]
        )
        assert output["decision"] == "ask"
        assert "1 issue(s)" in output["systemMessage"]

    def test_error_verdict_allows(self, monkeypatch, capsys):
        output = self._scan(monkeypatch, capsys, "error", [])
        assert output == {"decision": "allow"}

    def test_unknown_verdict_allows(self, monkeypatch, capsys):
        output = self._scan(monkeypatch, capsys, "strange-verdict", [])
        assert output == {"decision": "allow"}

    def test_findings_are_ignored_on_pass(self, monkeypatch, capsys):
        output = self._scan(
            monkeypatch, capsys, "pass", [{"desc_zh": "危险命令"}]
        )
        assert output == {"decision": "allow"}


class TestArgvContract:
    """The scan-code invocation must keep its full argv shape.

    The suite previously had no test for this hook at all, so a renamed
    subcommand, a dropped --language, or a broken trace-context injection
    would have kept every scan silently failing open.
    """

    def _capture_run(self, monkeypatch, input_data):
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

        monkeypatch.setattr(code_scanner_hook.subprocess, "run", fake_run)
        monkeypatch.setattr(
            "sys.stdin", io.StringIO(json.dumps(input_data))
        )
        code_scanner_hook.main()
        return captured

    def test_scan_code_argv_is_exact(self, monkeypatch, capsys):
        captured = self._capture_run(
            monkeypatch,
            {"tool_name": "run_shell_command", "tool_input": {"command": "echo hi"}},
        )
        # The trace context always carries at least the agent name, so the
        # --trace-context pair is unconditional for cosh.
        assert captured["args"] == [
            "agent-sec-cli",
            "--trace-context",
            json.dumps({"agent_name": "cosh"}, ensure_ascii=False, separators=(",", ":")),
            "scan-code",
            "--code",
            "echo hi",
            "--language",
            "bash",
        ]

    def test_trace_context_fields_are_mapped(self, monkeypatch, capsys):
        captured = self._capture_run(
            monkeypatch,
            {
                "tool_name": "shell",
                "tool_input": {"command": "echo hi"},
                "trace_id": "trace-1",
                "session_id": "sess-1",
                "run_id": "run-1",
                "call_id": "call-1",
                "tool_use_id": "tc-1",
            },
        )
        expected_ctx = json.dumps(
            {
                "agent_name": "cosh",
                "trace_id": "trace-1",
                "session_id": "sess-1",
                "run_id": "run-1",
                "call_id": "call-1",
                "tool_call_id": "tc-1",
            },
            ensure_ascii=False,
            separators=(",", ":"),
        )
        assert captured["args"][1] == "--trace-context"
        assert captured["args"][2] == expected_ctx

    def test_blank_trace_fields_are_skipped(self, monkeypatch, capsys):
        captured = self._capture_run(
            monkeypatch,
            {
                "tool_name": "shell",
                "tool_input": {"command": "echo hi"},
                "trace_id": "   ",
                "session_id": "sess-1",
            },
        )
        ctx = json.loads(captured["args"][2])
        assert ctx == {"agent_name": "cosh", "session_id": "sess-1"}

    def test_shell_tool_alias_is_scanned(self, monkeypatch, capsys):
        captured = self._capture_run(
            monkeypatch, {"tool_name": "shell", "tool_input": {"command": "ls -la"}}
        )
        assert captured["args"][captured["args"].index("--code") + 1] == "ls -la"

    def test_timeout_is_bounded_to_ten_seconds(self, monkeypatch, capsys):
        captured = self._capture_run(
            monkeypatch, {"tool_name": "shell", "tool_input": {"command": "echo hi"}}
        )
        assert captured["kwargs"]["timeout"] == 10


class TestEnvironmentContract:
    """Load-time flags are verified through subprocess execution."""

    def test_hook_enabled_false_skips_scan(self, mock_cli):
        env = mock_cli(
            json.dumps({"verdict": "deny", "findings": [{"desc_zh": "危险命令"}]}),
            extra={"CODE_SCANNER_HOOK_ENABLED": "false"},
        )
        output = _run_hook(
            {"tool_name": "shell", "tool_input": {"command": "echo hi"}}, env_override=env
        )
        assert output == {"decision": "allow"}

    def test_enabled_flag_garbage_defaults_to_enabled(self, mock_cli):
        env = mock_cli(
            json.dumps({"verdict": "deny", "findings": [{"desc_zh": "危险命令"}]}),
            extra={"CODE_SCANNER_HOOK_ENABLED": "banana"},
        )
        output = _run_hook(
            {"tool_name": "shell", "tool_input": {"command": "echo hi"}}, env_override=env
        )
        assert output["decision"] == "ask"

    @pytest.mark.parametrize("mode", ["block", "observe", "warn", "nonsense"])
    def test_only_ask_mode_is_supported(self, mock_cli, mode):
        env = mock_cli(
            json.dumps({"verdict": "warn", "findings": [{"desc_zh": "危险命令"}]}),
            extra={"CODE_SCANNER_MODE": mode},
        )
        proc = _run_hook_process(
            {"tool_name": "shell", "tool_input": {"command": "echo hi"}},
            env_override=env,
        )
        output = json.loads(proc.stdout)
        # ask is the only supported policy: warn findings still escalate to ask
        assert output["decision"] == "ask"
        assert "CODE_SCANNER_MODE" in proc.stderr

    def test_ask_mode_is_accepted_without_diagnostic(self, mock_cli):
        env = mock_cli(
            json.dumps({"verdict": "warn", "findings": [{"desc_zh": "危险命令"}]}),
            extra={"CODE_SCANNER_MODE": "ask"},
        )
        proc = _run_hook_process(
            {"tool_name": "shell", "tool_input": {"command": "echo hi"}},
            env_override=env,
        )
        output = json.loads(proc.stdout)
        assert output["decision"] == "ask"
        assert "CODE_SCANNER_MODE" not in proc.stderr
