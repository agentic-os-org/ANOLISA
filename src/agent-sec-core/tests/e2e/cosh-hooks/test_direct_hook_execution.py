"""E2E checks for cosh hook command execution."""

import json
import os
import shlex
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

_SYSTEM_EXTENSION_DIR = Path("/usr/share/anolisa/extensions/agent-sec-core")
_USER_EXTENSION_DIR = Path.home() / ".copilot-shell" / "extensions" / "agent-sec-core"
_SOURCE_EXTENSION_DIR = Path(__file__).resolve().parents[3] / "cosh-extension"
_CODE_SCANNER_HOOK = _SOURCE_EXTENSION_DIR / "hooks" / "code_scanner_hook.py"
_PII_CHECKER_HOOK = _SOURCE_EXTENSION_DIR / "hooks" / "pii_checker_hook.py"


_MOCK_CLI_SCRIPT = f"#!{sys.executable}\n" + textwrap.dedent("""\
    import json
    import os
    import sys

    capture_path = os.environ.get("_MOCK_CLI_CAPTURE")
    if capture_path:
        with open(capture_path, "w", encoding="utf-8") as handle:
            json.dump({"argv": sys.argv[1:]}, handle)

    output = os.environ.get("_MOCK_CLI_OUTPUT", "")
    if output:
        print(output)
    sys.exit(0)
    """)


def _extension_dir() -> Path:
    if (_SYSTEM_EXTENSION_DIR / "cosh-extension.json").exists():
        return _SYSTEM_EXTENSION_DIR
    if (_USER_EXTENSION_DIR / "cosh-extension.json").exists():
        return _USER_EXTENSION_DIR
    return _SOURCE_EXTENSION_DIR


def _manifest_hook_commands(extension_dir: Path) -> list[str]:
    manifest = json.loads((extension_dir / "cosh-extension.json").read_text())
    commands: set[str] = set()
    for hook_groups in manifest["hooks"].values():
        for group in hook_groups:
            for hook in group.get("hooks", []):
                command = hook.get("command")
                if isinstance(command, str) and command.startswith("python3 "):
                    commands.add(command)
    return sorted(commands)


def _run_code_scanner_hook(tmp_path: Path, env_extra: dict[str, str]):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    cli = bin_dir / "agent-sec-cli"
    cli.write_text(_MOCK_CLI_SCRIPT)
    cli.chmod(0o755)
    capture = tmp_path / "capture.json"
    env = os.environ.copy()
    env.update(
        {
            "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
            "_MOCK_CLI_CAPTURE": str(capture),
            "_MOCK_CLI_OUTPUT": json.dumps(
                {"verdict": "deny", "findings": [{"desc_zh": "危险命令"}]}
            ),
        }
    )
    env.update(env_extra)
    proc = subprocess.run(
        [sys.executable, str(_CODE_SCANNER_HOOK)],
        input=json.dumps(
            {"tool_name": "shell", "tool_input": {"command": "rm -rf /secret/path"}}
        ),
        capture_output=True,
        check=False,
        env=env,
        text=True,
        timeout=15,
    )
    return proc, capture


def _run_pii_checker_hook(
    tmp_path: Path,
    payload: dict[str, object],
    policy: str,
):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    cli = bin_dir / "agent-sec-cli"
    cli.write_text(_MOCK_CLI_SCRIPT)
    cli.chmod(0o755)
    capture = tmp_path / "capture.json"
    env = os.environ.copy()
    env.update(
        {
            "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
            "_MOCK_CLI_CAPTURE": str(capture),
            "_MOCK_CLI_OUTPUT": json.dumps(
                {
                    "verdict": "deny",
                    "findings": [
                        {
                            "type": "email",
                            "severity": "deny",
                            "evidence_redacted": "a***@example.com",
                            "raw_evidence": "alice@example.com",
                        }
                    ],
                }
            ),
            "PII_CHECKER_HOOK_ENABLED": "true",
            "PII_CHECKER_MODE": policy,
        }
    )
    proc = subprocess.run(
        [sys.executable, str(_PII_CHECKER_HOOK)],
        input=json.dumps(payload),
        capture_output=True,
        check=False,
        env=env,
        text=True,
        timeout=15,
    )
    return proc, capture


def test_cosh_code_scanner_hook_enabled_false_allows_without_scan(
    tmp_path: Path,
) -> None:
    proc, capture = _run_code_scanner_hook(
        tmp_path,
        {"CODE_SCANNER_HOOK_ENABLED": "false"},
    )

    assert proc.returncode == 0
    assert json.loads(proc.stdout) == {"decision": "allow"}
    assert proc.stderr == ""
    assert not capture.exists()


def test_cosh_code_scanner_invalid_enabled_value_defaults_to_enabled(
    tmp_path: Path,
) -> None:
    proc, capture = _run_code_scanner_hook(
        tmp_path,
        {"CODE_SCANNER_HOOK_ENABLED": "maybe"},
    )

    assert proc.returncode == 0
    assert json.loads(proc.stdout)["decision"] == "ask"
    assert proc.stderr == ""
    assert capture.exists()


@pytest.mark.parametrize(
    "mode", ["ask", "observe", "block", "debug", "deny", "warn", "invalid"]
)
def test_cosh_code_scanner_mode_never_changes_fixed_ask_behavior(
    tmp_path: Path,
    mode: str,
) -> None:
    proc, capture = _run_code_scanner_hook(
        tmp_path,
        {"CODE_SCANNER_MODE": mode},
    )

    assert proc.returncode == 0
    assert json.loads(proc.stdout)["decision"] == "ask"
    if mode == "ask":
        assert proc.stderr == ""
    else:
        assert "CODE_SCANNER_MODE" in proc.stderr
        assert mode in proc.stderr
        assert "rm -rf /secret/path" not in proc.stderr
    assert capture.exists()


@pytest.mark.parametrize(
    ("payload", "policy", "expected_decision", "message_fragment"),
    [
        (
            {
                "hook_event_name": "PreToolUse",
                "tool_input": {"command": "send alice@example.com"},
            },
            "ask",
            "ask",
            "当前策略要求确认",
        ),
        (
            {
                "hook_event_name": "PreToolUse",
                "tool_input": {"command": "send alice@example.com"},
            },
            "block",
            "block",
            "当前策略已阻断本次工具调用",
        ),
        (
            {
                "hook_event_name": "PostToolUse",
                "tool_response": {"stdout": "alice@example.com"},
            },
            "block",
            "block",
            "原始工具结果不会进入模型上下文",
        ),
        (
            {
                "hook_event_name": "AfterModel",
                "llm_response": {"text": "Contact alice@example.com"},
            },
            "block",
            "allow",
            "当前环节不支持确认/阻断",
        ),
    ],
)
def test_cosh_pii_policy_uses_event_level_decisions(
    tmp_path: Path,
    payload: dict[str, object],
    policy: str,
    expected_decision: str,
    message_fragment: str,
) -> None:
    proc, capture = _run_pii_checker_hook(tmp_path, payload, policy)

    assert proc.returncode == 0
    output = json.loads(proc.stdout)
    assert output["decision"] == expected_decision
    assert message_fragment in output["reason"]
    assert "高风险敏感信息" in output["reason"]
    assert "email" not in output["reason"]
    assert "deny" not in output["reason"]
    assert "a***@example.com" not in output["reason"]
    assert "alice@example.com" not in output["reason"]
    assert proc.stderr == ""
    assert capture.exists()
    assert "scan-pii" in json.loads(capture.read_text(encoding="utf-8"))["argv"]

    if expected_decision in {"ask", "block"}:
        assert "不会阻断" not in output["reason"]
    else:
        assert "已阻断本次" not in output["reason"]
        assert "本次仅提醒，不会阻断" in output["reason"]

    if payload.get("hook_event_name") == "PostToolUse":
        assert "工具已经执行" in output["reason"]
        assert "外部副作用不会撤销" in output["reason"]


def test_cosh_manifest_hooks_are_directly_executable() -> None:
    extension_dir = _extension_dir()
    commands = _manifest_hook_commands(extension_dir)
    assert commands

    env = os.environ.copy()
    env.pop("PYTHONPATH", None)

    failed: list[str] = []
    for command in commands:
        argv = [
            part.replace("${extensionPath}", str(extension_dir))
            for part in shlex.split(command)
        ]
        proc = subprocess.run(
            argv,
            input="{}\n",
            capture_output=True,
            check=False,
            env=env,
            text=True,
            timeout=5,
        )
        if proc.returncode != 0:
            failed.append(
                f"{command}: exit={proc.returncode}, stderr={proc.stderr.strip()}"
            )
            continue
        try:
            json.loads(proc.stdout)
        except json.JSONDecodeError as exc:
            failed.append(f"{command}: invalid stdout JSON: {exc}: {proc.stdout!r}")

    assert failed == []
