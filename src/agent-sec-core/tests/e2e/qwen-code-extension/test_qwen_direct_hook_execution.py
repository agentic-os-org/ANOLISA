"""E2E checks for Qwen Code command-hook execution."""

import hashlib
import json
import os
import shlex
import shutil
import subprocess
import sys
from pathlib import Path

_RPM_EXTENSION_DIR = Path("/opt/agent-sec/qwen-code-extension")
_SYSTEM_EXTENSION_DIR = Path("/usr/local/lib/anolisa/sec-core/qwen-code-extension")
_USER_EXTENSION_DIR = (
    Path.home() / ".local" / "lib" / "anolisa" / "sec-core" / "qwen-code-extension"
)
_SOURCE_EXTENSION_DIR = Path(__file__).resolve().parents[3] / "qwen-code-extension"
_ZERO_RUN_ID = "00000000-0000-0000-0000-000000000000"


def _extension_dir() -> Path:
    override = os.environ.get("QWEN_CODE_EXTENSION_E2E_DIR")
    if override:
        return Path(override).expanduser().resolve()
    for extension_dir in (
        _RPM_EXTENSION_DIR,
        _SYSTEM_EXTENSION_DIR,
        _USER_EXTENSION_DIR,
    ):
        if (extension_dir / "qwen-extension.json").exists():
            return extension_dir
    return _SOURCE_EXTENSION_DIR


def _manifest_hook_commands(extension_dir: Path) -> dict[str, dict[str, str]]:
    manifest = json.loads(
        (extension_dir / "qwen-extension.json").read_text(encoding="utf-8")
    )
    commands: dict[str, dict[str, str]] = {}
    for event_name, hook_groups in manifest["hooks"].items():
        event_commands: dict[str, str] = {}
        for group in hook_groups:
            for hook in group.get("hooks", []):
                name = hook.get("name")
                command = hook.get("command")
                if (
                    isinstance(name, str)
                    and isinstance(command, str)
                    and command.startswith("python3 ")
                ):
                    event_commands[name] = command
        commands[event_name] = event_commands
    return commands


def _command_argv(command: str, extension_dir: Path) -> list[str]:
    expanded = command.replace("${extensionPath}", str(extension_dir)).replace(
        "${/}", os.sep
    )
    return shlex.split(expanded)


def _event_argvs(extension_dir: Path, event_name: str) -> list[list[str]]:
    commands = _manifest_hook_commands(extension_dir)[event_name]
    assert commands
    return [_command_argv(command, extension_dir) for command in commands.values()]


def _event_argv(extension_dir: Path, event_name: str, hook_name: str) -> list[str]:
    commands = _manifest_hook_commands(extension_dir)[event_name]
    return _command_argv(commands[hook_name], extension_dir)


def _ensure_agent_sec_cli(env: dict[str, str], tmp_path: Path) -> None:
    if shutil.which("agent-sec-cli", path=env.get("PATH")):
        return
    wrapper_dir = tmp_path / "bin"
    wrapper_dir.mkdir()
    wrapper = wrapper_dir / "agent-sec-cli"
    wrapper.write_text(
        f"#!{sys.executable}\n"
        "import runpy\n"
        'runpy.run_module("agent_sec_cli.cli", run_name="__main__")\n',
        encoding="utf-8",
    )
    wrapper.chmod(0o755)
    env["PATH"] = f"{wrapper_dir}{os.pathsep}{env.get('PATH', '')}"


def _run_hooks(
    extension_dir: Path,
    input_data: dict,
    *,
    env: dict[str, str],
    cwd: Path,
) -> list[subprocess.CompletedProcess]:
    event_name = input_data["hook_event_name"]
    argvs = _event_argvs(extension_dir, event_name)
    return [
        subprocess.run(
            argv,
            input=json.dumps(input_data),
            capture_output=True,
            check=False,
            cwd=cwd,
            env=env,
            text=True,
            timeout=15,
        )
        for argv in argvs
    ]


def _run_hook(
    extension_dir: Path,
    input_data: dict,
    *,
    env: dict[str, str],
    cwd: Path,
    hook_name: str = "agent-sec-observability",
) -> subprocess.CompletedProcess:
    event_name = input_data["hook_event_name"]
    return subprocess.run(
        _event_argv(extension_dir, event_name, hook_name),
        input=json.dumps(input_data),
        capture_output=True,
        check=False,
        cwd=cwd,
        env=env,
        text=True,
        timeout=15,
    )


def _payload(event_name: str, timestamp: str, **fields) -> dict:
    return {
        "session_id": "qwen-e2e-session",
        "transcript_path": "/unused/qwen-e2e-transcript.jsonl",
        "cwd": "/workspace",
        "hook_event_name": event_name,
        "timestamp": timestamp,
        **fields,
    }


def test_qwen_manifest_hooks_are_directly_executable(tmp_path) -> None:
    extension_dir = _extension_dir()
    commands_by_event = _manifest_hook_commands(extension_dir)
    assert set(commands_by_event) == {
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
    }

    env = os.environ.copy()
    env.pop("PYTHONPATH", None)
    env["AGENT_SEC_DATA_DIR"] = str(tmp_path / "agent-sec-data")

    failed: list[str] = []
    commands = sorted(
        {
            command
            for event_commands in commands_by_event.values()
            for command in event_commands.values()
        }
    )
    for command in commands:
        proc = subprocess.run(
            _command_argv(command, extension_dir),
            input="{}\n",
            capture_output=True,
            check=False,
            cwd=tmp_path,
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
            output = json.loads(proc.stdout)
        except json.JSONDecodeError as exc:
            failed.append(f"{command}: invalid stdout JSON: {exc}: {proc.stdout!r}")
            continue
        if output != {}:
            failed.append(f"{command}: expected fail-open empty output, got {output!r}")

    assert failed == []


def test_qwen_observability_lifecycle_records_through_cli(tmp_path) -> None:
    extension_dir = _extension_dir()
    data_dir = tmp_path / "agent-sec-data"
    env = os.environ.copy()
    env.pop("PYTHONPATH", None)
    env["AGENT_SEC_DATA_DIR"] = str(data_dir)
    _ensure_agent_sec_cli(env, tmp_path)

    payloads = [
        _payload(
            "UserPromptSubmit",
            "2026-07-14T10:00:00Z",
            prompt="List repository files.",
        ),
        _payload(
            "UserPromptSubmit",
            "2026-07-14T10:00:01Z",
            prompt="",
        ),
        _payload(
            "PreToolUse",
            "2026-07-14T10:00:02Z",
            permission_mode="default",
            tool_name="run_shell_command",
            tool_input={"command": "pwd"},
            tool_call_id="call-success",
            tool_use_id="call-success",
        ),
        _payload(
            "PostToolUse",
            "2026-07-14T10:00:03Z",
            permission_mode="default",
            tool_name="run_shell_command",
            tool_input={"command": "pwd"},
            tool_call_id="call-success",
            tool_use_id="call-success",
            tool_response={"returnDisplay": {"stdout": "/workspace\n", "exitCode": 0}},
        ),
        _payload(
            "Stop",
            "2026-07-14T10:00:04Z",
            stop_hook_active=False,
            last_assistant_message="Done.",
        ),
        _payload(
            "UserPromptSubmit",
            "2026-07-14T10:01:00Z",
            prompt="Run a failing command.",
        ),
        _payload(
            "PreToolUse",
            "2026-07-14T10:01:01Z",
            permission_mode="default",
            tool_name="run_shell_command",
            tool_input={"command": "exit 1"},
            tool_call_id="call-failure",
            tool_use_id="call-failure",
        ),
        _payload(
            "PostToolUseFailure",
            "2026-07-14T10:01:02Z",
            permission_mode="default",
            tool_name="run_shell_command",
            tool_input={"command": "exit 1"},
            tool_call_id="call-failure",
            tool_use_id="call-failure",
            error="command failed",
            is_interrupt=False,
        ),
        _payload(
            "StopFailure",
            "2026-07-14T10:01:03Z",
            error="API_ERROR",
            error_details="model request failed",
            last_assistant_message="",
        ),
    ]

    for payload in payloads:
        procs = _run_hooks(extension_dir, payload, env=env, cwd=tmp_path)
        for proc in procs:
            assert proc.returncode == 0, proc.stderr
            assert json.loads(proc.stdout) == {}

    records = [
        json.loads(line)
        for line in (data_dir / "observability.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    assert [record["hook"] for record in records] == [
        "before_agent_run",
        "before_tool_call",
        "after_tool_call",
        "after_agent_run",
        "before_agent_run",
        "before_tool_call",
        "after_tool_call",
        "after_agent_run",
    ]
    assert [
        record["metrics"]["prompt"]
        for record in records
        if record["hook"] == "before_agent_run"
    ] == [
        "List repository files.",
        "Run a failing command.",
    ]
    assert {record["metadata"]["runId"] for record in records} == {_ZERO_RUN_ID}
    assert [
        record["metadata"]["toolCallId"]
        for record in records
        if "toolCallId" in record["metadata"]
    ] == ["call-success", "call-success", "call-failure", "call-failure"]
    assert [
        record["metrics"]["status"]
        for record in records
        if record["hook"] == "after_tool_call"
    ] == ["success", "error"]
    assert [
        record["metrics"]["success"]
        for record in records
        if record["hook"] == "after_agent_run"
    ] == [True, False]
    assert not (data_dir / "qwen-code-extension").exists()


def test_qwen_pii_hook_sources_and_security_events(tmp_path) -> None:
    extension_dir = _extension_dir()
    data_dir = tmp_path / "agent-sec-data"
    env = os.environ.copy()
    env.pop("PYTHONPATH", None)
    env["AGENT_SEC_DATA_DIR"] = str(data_dir)
    env["PII_CHECKER_MODE"] = "observe"
    _ensure_agent_sec_cli(env, tmp_path)

    secret = "password=supersecretvalue12345"
    payloads = [
        _payload(
            "UserPromptSubmit",
            "2026-07-14T11:00:00Z",
            prompt=secret,
        ),
        _payload(
            "PreToolUse",
            "2026-07-14T11:00:01Z",
            tool_name="run_shell_command",
            tool_input={"command": secret},
            tool_call_id="call-input",
        ),
        _payload(
            "PostToolUse",
            "2026-07-14T11:00:02Z",
            tool_name="run_shell_command",
            tool_response={"stdout": secret},
            tool_call_id="call-output",
        ),
        _payload(
            "PostToolUseFailure",
            "2026-07-14T11:00:03Z",
            tool_name="run_shell_command",
            error=secret,
            tool_call_id="call-failure",
        ),
        _payload(
            "Stop",
            "2026-07-14T11:00:04Z",
            last_assistant_message=secret,
            stop_hook_active=False,
        ),
        _payload(
            "StopFailure",
            "2026-07-14T11:00:05Z",
            error="server_error",
            last_assistant_message=secret,
        ),
    ]

    for payload in payloads:
        proc = _run_hook(
            extension_dir,
            payload,
            env=env,
            cwd=tmp_path,
            hook_name="agent-sec-pii-checker",
        )
        assert proc.returncode == 0
        assert proc.stderr == ""
        assert secret not in proc.stdout
        output = json.loads(proc.stdout)
        # Observe keeps scanner evidence without surfacing a user-visible warning.
        assert output == {}

    observability_proc = _run_hook(
        extension_dir,
        payloads[0],
        env=env,
        cwd=tmp_path,
        hook_name="agent-sec-observability",
    )
    assert observability_proc.returncode == 0
    assert observability_proc.stderr == ""
    assert json.loads(observability_proc.stdout) == {}

    event_text = (data_dir / "security-events.jsonl").read_text(encoding="utf-8")
    observability_text = (data_dir / "observability.jsonl").read_text(encoding="utf-8")
    assert secret not in event_text
    assert secret not in observability_text

    events = [json.loads(line) for line in event_text.splitlines()]
    assert len(events) == 8
    assert all(
        event["event_type"] == "pii_scan"
        and event["category"] == "pii_scan"
        and event["session_id"] == "qwen-e2e-session"
        for event in events
    )
    assert [event["details"]["request"]["source"] for event in events] == [
        "user_input",
        "tool_input",
        "tool_output",
        "tool_output",
        "model_output",
        "model_output",
        "observability",
        "observability",
    ]
    assert [event.get("tool_call_id") for event in events[:6]] == [
        None,
        "call-input",
        "call-output",
        "call-failure",
        None,
        None,
    ]
    assert (
        events[0]["details"]["request"]["text_sha256"]
        == hashlib.sha256(secret.encode("utf-8")).hexdigest()
    )
    assert all(
        "raw_evidence" not in json.dumps(event["details"], ensure_ascii=False)
        for event in events
    )


def test_qwen_pii_hook_blocks_deny_verdicts(tmp_path) -> None:
    extension_dir = _extension_dir()
    env = os.environ.copy()
    env.pop("PYTHONPATH", None)
    env["AGENT_SEC_DATA_DIR"] = str(tmp_path / "agent-sec-data")
    env["PII_CHECKER_MODE"] = "block"
    _ensure_agent_sec_cli(env, tmp_path)

    benign = _payload(
        "UserPromptSubmit",
        "2026-07-14T12:00:00Z",
        prompt="List repository files.",
    )
    benign_proc = _run_hook(
        extension_dir,
        benign,
        env=env,
        cwd=tmp_path,
        hook_name="agent-sec-pii-checker",
    )
    assert benign_proc.returncode == 0
    assert json.loads(benign_proc.stdout) == {}

    secret = "password=supersecretvalue12345"
    prompt_proc = _run_hook(
        extension_dir,
        _payload(
            "UserPromptSubmit",
            "2026-07-14T12:00:01Z",
            prompt=secret,
        ),
        env=env,
        cwd=tmp_path,
        hook_name="agent-sec-pii-checker",
    )
    assert prompt_proc.returncode == 0
    prompt_output = json.loads(prompt_proc.stdout)
    assert prompt_output["decision"] == "block"
    assert secret not in prompt_proc.stdout

    tool_proc = _run_hook(
        extension_dir,
        _payload(
            "PreToolUse",
            "2026-07-14T12:00:02Z",
            tool_name="run_shell_command",
            tool_input={"command": secret},
            tool_call_id="call-blocked",
        ),
        env=env,
        cwd=tmp_path,
        hook_name="agent-sec-pii-checker",
    )
    assert tool_proc.returncode == 0
    tool_output = json.loads(tool_proc.stdout)
    assert tool_output["hookSpecificOutput"]["permissionDecision"] == "deny"
    assert secret not in tool_proc.stdout

    post_tool_proc = _run_hook(
        extension_dir,
        _payload(
            "PostToolUse",
            "2026-07-14T12:00:03Z",
            tool_name="run_shell_command",
            tool_response={"stdout": secret},
            tool_call_id="call-output",
        ),
        env=env,
        cwd=tmp_path,
        hook_name="agent-sec-pii-checker",
    )
    assert post_tool_proc.returncode == 0
    post_tool_output = json.loads(post_tool_proc.stdout)
    assert post_tool_output["continue"] is False
    assert post_tool_output["decision"] == "block"
    assert post_tool_output["stopReason"] == post_tool_output["reason"]
    assert "high-risk sensitive data" in post_tool_output["reason"]
    assert "tool has already run" in post_tool_output["reason"]
    assert "raw output will not enter model context" in post_tool_output["reason"]
    assert "external side effects were not undone" in post_tool_output["reason"]
    assert "[REDACTED]" not in post_tool_output["reason"]
    assert secret not in post_tool_proc.stdout

    failure_proc = _run_hook(
        extension_dir,
        _payload(
            "PostToolUseFailure",
            "2026-07-14T12:00:04Z",
            tool_name="run_shell_command",
            error=secret,
            tool_call_id="call-failure",
        ),
        env=env,
        cwd=tmp_path,
        hook_name="agent-sec-pii-checker",
    )
    assert failure_proc.returncode == 0
    assert json.loads(failure_proc.stdout) == {}
    assert secret not in failure_proc.stdout

    events = [
        json.loads(line)
        for line in (tmp_path / "agent-sec-data" / "security-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    tool_output_events = [
        event
        for event in events
        if event["details"]["request"]["source"] == "tool_output"
    ]
    assert [event["tool_call_id"] for event in tool_output_events] == [
        "call-output",
        "call-failure",
    ]


def test_qwen_skill_ledger_hook_uses_qwen_home_and_blocks_managed_drift(
    tmp_path,
) -> None:
    extension_dir = _extension_dir()
    project_dir = tmp_path / "project"
    project_dir.mkdir()
    qwen_home = tmp_path / "custom-qwen-home"
    skill_dir = qwen_home / "skills" / "managed-skill"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text(
        "---\nname: managed-skill\ndescription: E2E managed skill\n---\n",
        encoding="utf-8",
    )

    home_dir = tmp_path / "home"
    data_dir = tmp_path / "agent-sec-data"
    xdg_data = tmp_path / "xdg-data"
    xdg_config = tmp_path / "xdg-config"
    home_dir.mkdir()
    env = os.environ.copy()
    env.pop("PYTHONPATH", None)
    env.update(
        {
            "HOME": str(home_dir),
            "QWEN_HOME": str(qwen_home),
            "AGENT_SEC_DATA_DIR": str(data_dir),
            "XDG_DATA_HOME": str(xdg_data),
            "XDG_CONFIG_HOME": str(xdg_config),
            "SKILL_LEDGER_MODE": "block",
        }
    )
    _ensure_agent_sec_cli(env, tmp_path)
    cli = shutil.which("agent-sec-cli", path=env["PATH"])
    assert cli is not None

    scan = subprocess.run(
        [cli, "skill-ledger", "scan", str(skill_dir)],
        capture_output=True,
        check=False,
        cwd=project_dir,
        env=env,
        text=True,
        timeout=30,
    )
    assert scan.returncode == 0, scan.stderr
    config = json.loads(
        (xdg_config / "agent-sec" / "skill-ledger" / "config.json").read_text(
            encoding="utf-8"
        )
    )
    assert str(skill_dir.resolve()) in config["managedSkillDirs"]

    with (skill_dir / "SKILL.md").open("a", encoding="utf-8") as stream:
        stream.write("\nChanged after certification.\n")

    payload = {
        "session_id": "qwen-skill-session",
        "cwd": str(project_dir),
        "hook_event_name": "PreToolUse",
        "permission_mode": "default",
        "tool_name": "skill",
        "tool_input": {"skill": "managed-skill"},
        "tool_call_id": "qwen-skill-call",
        "tool_use_id": "qwen-skill-use",
    }
    hook = _run_hook(
        extension_dir,
        payload,
        env=env,
        cwd=project_dir,
        hook_name="agent-sec-skill-ledger",
    )
    assert hook.returncode == 0, hook.stderr
    hook_output = json.loads(hook.stdout)
    hook_specific = hook_output["hookSpecificOutput"]
    assert hook_specific["hookEventName"] == "PreToolUse"
    assert hook_specific["permissionDecision"] == "deny"
    assert "Skill Ledger [drifted]" in hook_specific["permissionDecisionReason"]

    events = [
        json.loads(line)
        for line in (data_dir / "security-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    correlated = [
        event
        for event in events
        if event["category"] == "skill_ledger"
        and event["session_id"] == "qwen-skill-session"
        and event["tool_call_id"] == "qwen-skill-call"
    ]
    assert len(correlated) == 1
    assert correlated[0]["details"]["request"]["command"] == "show"
    assert correlated[0]["details"]["result"]["latest_status"] == "drifted"


def test_qwen_disabled_skill_skips_ledger_show_under_block_policy(tmp_path) -> None:
    extension_dir = _extension_dir()
    project_dir = tmp_path / "project"
    project_dir.mkdir()
    qwen_home = tmp_path / "custom-qwen-home"
    skill_dir = qwen_home / "skills" / "disabled-skill"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text(
        "---\nname: disabled-skill\ndescription: E2E disabled skill\n---\n",
        encoding="utf-8",
    )
    (qwen_home / "settings.json").write_text(
        "{\n  // A same-named command may remain model-invocable.\n"
        '  "skills": {"disabled": ["DISABLED-SKILL"]}\n}\n',
        encoding="utf-8",
    )

    home_dir = tmp_path / "home"
    data_dir = tmp_path / "agent-sec-data"
    xdg_data = tmp_path / "xdg-data"
    xdg_config = tmp_path / "xdg-config"
    home_dir.mkdir()
    env = os.environ.copy()
    env.pop("PYTHONPATH", None)
    env.update(
        {
            "HOME": str(home_dir),
            "QWEN_HOME": str(qwen_home),
            "AGENT_SEC_DATA_DIR": str(data_dir),
            "XDG_DATA_HOME": str(xdg_data),
            "XDG_CONFIG_HOME": str(xdg_config),
            "SKILL_LEDGER_MODE": "block",
        }
    )
    _ensure_agent_sec_cli(env, tmp_path)
    cli = shutil.which("agent-sec-cli", path=env["PATH"])
    assert cli is not None

    scan = subprocess.run(
        [cli, "skill-ledger", "scan", str(skill_dir)],
        capture_output=True,
        check=False,
        cwd=project_dir,
        env=env,
        text=True,
        timeout=30,
    )
    assert scan.returncode == 0, scan.stderr

    payload = {
        "session_id": "qwen-disabled-session",
        "cwd": str(project_dir),
        "hook_event_name": "PreToolUse",
        "permission_mode": "default",
        "tool_name": "skill",
        "tool_input": {"skill": "disabled-skill"},
        "tool_call_id": "qwen-disabled-call",
        "tool_use_id": "qwen-disabled-use",
    }
    hook = _run_hook(
        extension_dir,
        payload,
        env=env,
        cwd=project_dir,
        hook_name="agent-sec-skill-ledger",
    )

    assert hook.returncode == 0, hook.stderr
    assert json.loads(hook.stdout) == {}
    assert '"code":"skill_disabled_by_settings"' in hook.stderr
    events = [
        json.loads(line)
        for line in (data_dir / "security-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    assert not any(
        event.get("session_id") == "qwen-disabled-session" for event in events
    )
