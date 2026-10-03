"""End-to-end coverage for the Qoder Skill Ledger hook."""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

_CLI_BIN = shutil.which("agent-sec-cli")
_HOOK_SCRIPT = (
    Path(__file__).resolve().parents[3]
    / "qoder-plugin"
    / "hooks"
    / "skill_ledger_hook.py"
)


def test_unsigned_project_skill_is_audited_with_qoder_trace(tmp_path: Path) -> None:
    """A real pre-use check writes its result and Qoder trace to the event log."""
    if _CLI_BIN is None:
        pytest.skip("agent-sec-cli binary not on PATH")

    project = tmp_path / "project"
    skill_dir = project / ".qoder" / "skills" / "unsigned-skill"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text(
        "---\nname: unsigned-skill\ndescription: test\n---\n# Test\n"
    )
    home = tmp_path / "home"
    home.mkdir()
    event_data = tmp_path / "events"
    event = {
        "hook_event_name": "PreToolUse",
        "tool_name": "Skill",
        "tool_input": {"skill": "unsigned-skill"},
        "cwd": str(project),
        "session_id": "qoder-session",
        "run_id": "qoder-run",
        "call_id": "qoder-call",
        "tool_use_id": "qoder-tool",
    }
    env = {
        **os.environ,
        "HOME": str(home),
        "AGENT_SEC_DATA_DIR": str(event_data),
        "SKILL_LEDGER_MODE": "warn",
    }

    proc = subprocess.run(
        [sys.executable, str(_HOOK_SCRIPT)],
        capture_output=True,
        check=False,
        env=env,
        input=json.dumps(event),
        text=True,
        timeout=30,
    )

    assert proc.returncode == 0, proc.stderr
    output = json.loads(proc.stdout)
    assert output["decision"] == "allow"
    assert "status: none" in output["systemMessage"]

    records = [
        json.loads(line)
        for line in (event_data / "security-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    check_event = next(
        record
        for record in records
        if record["category"] == "skill_ledger"
        and record["details"]["result"].get("command") == "check"
    )
    assert check_event["event_type"] == "skill_ledger"
    assert check_event["session_id"] == "qoder-session"
    assert check_event["run_id"] == "qoder-run"
    assert check_event["call_id"] == "qoder-call"
    assert check_event["tool_call_id"] == "qoder-tool"
    assert check_event["details"]["request"]["skill_dir"] == str(skill_dir.resolve())
    assert check_event["details"]["result"]["status"] == "none"


def test_signed_skill_passes_then_drift_blocks(tmp_path: Path) -> None:
    """The real hook observes file changes on the next Skill invocation."""
    if _CLI_BIN is None:
        pytest.skip("agent-sec-cli binary not on PATH")

    project = tmp_path / "project"
    skill_dir = project / ".qoder" / "skills" / "drift-skill"
    skill_dir.mkdir(parents=True)
    skill_md = skill_dir / "SKILL.md"
    original = "---\nname: drift-skill\ndescription: test\n---\n# Test\n"
    skill_md.write_text(original)
    home = tmp_path / "home"
    home.mkdir()
    event_data = tmp_path / "events"
    env = {
        **os.environ,
        "HOME": str(home),
        "XDG_DATA_HOME": str(tmp_path / "xdg-data"),
        "XDG_CONFIG_HOME": str(tmp_path / "xdg-config"),
        "AGENT_SEC_DATA_DIR": str(event_data),
        "SKILL_LEDGER_MODE": "block",
    }
    scan = subprocess.run(
        [_CLI_BIN, "skill-ledger", "scan", str(skill_dir), "--force"],
        capture_output=True,
        check=False,
        env=env,
        text=True,
        timeout=30,
    )
    assert scan.returncode == 0, f"stdout={scan.stdout}\nstderr={scan.stderr}"

    event = {
        "hook_event_name": "PreToolUse",
        "tool_name": "Skill",
        "tool_input": {"skill": "drift-skill"},
        "cwd": str(project),
        "session_id": "qoder-drift-session",
        "run_id": "qoder-drift-run",
        "tool_use_id": "qoder-drift-tool",
    }
    clean = subprocess.run(
        [sys.executable, str(_HOOK_SCRIPT)],
        capture_output=True,
        check=False,
        env=env,
        input=json.dumps(event),
        text=True,
        timeout=30,
    )
    assert clean.returncode == 0, clean.stderr
    assert clean.stdout == ""

    skill_md.write_text(original + "\nChanged after signing.\n")
    drifted = subprocess.run(
        [sys.executable, str(_HOOK_SCRIPT)],
        capture_output=True,
        check=False,
        env=env,
        input=json.dumps(event),
        text=True,
        timeout=30,
    )
    assert drifted.returncode == 0, drifted.stderr
    output = json.loads(drifted.stdout)
    hook_output = output["hookSpecificOutput"]
    assert hook_output["permissionDecision"] == "deny"
    assert "status: drifted" in hook_output["permissionDecisionReason"]
    assert "modified=1" in hook_output["permissionDecisionReason"]

    records = [
        json.loads(line)
        for line in (event_data / "security-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    statuses = [
        record["details"]["result"]["status"]
        for record in records
        if record["category"] == "skill_ledger"
        and record["details"]["result"].get("command") == "check"
    ]
    assert statuses == ["pass", "drifted"]


def test_same_named_user_skill_is_checked_before_clean_project_copy(
    tmp_path: Path,
) -> None:
    """Qoder precedence cannot let a drifted user Skill hide behind a clean project copy."""
    if _CLI_BIN is None:
        pytest.skip("agent-sec-cli binary not on PATH")

    project = tmp_path / "project"
    project_skill = project / ".qoder" / "skills" / "project-copy"
    project_skill.mkdir(parents=True)
    home = tmp_path / "home"
    user_skill = home / ".qoder" / "skills" / "user-copy"
    user_skill.mkdir(parents=True)
    original = "---\nname: shared-skill\ndescription: test\n---\n# Test\n"
    (project_skill / "SKILL.md").write_text(original)
    user_skill_md = user_skill / "SKILL.md"
    user_skill_md.write_text(original)

    event_data = tmp_path / "events"
    env = {
        **os.environ,
        "HOME": str(home),
        "XDG_DATA_HOME": str(tmp_path / "xdg-data"),
        "XDG_CONFIG_HOME": str(tmp_path / "xdg-config"),
        "AGENT_SEC_DATA_DIR": str(event_data),
        "SKILL_LEDGER_MODE": "block",
    }
    for skill_dir in (project_skill, user_skill):
        scan = subprocess.run(
            [_CLI_BIN, "skill-ledger", "scan", str(skill_dir), "--force"],
            capture_output=True,
            check=False,
            env=env,
            text=True,
            timeout=30,
        )
        assert scan.returncode == 0, f"stdout={scan.stdout}\nstderr={scan.stderr}"

    user_skill_md.write_text(original + "\nChanged after signing.\n")
    event = {
        "hook_event_name": "PreToolUse",
        "tool_name": "Skill",
        "tool_input": {"skill": "shared-skill"},
        "cwd": str(project),
        "session_id": "qoder-precedence-session",
        "run_id": "qoder-precedence-run",
        "tool_use_id": "qoder-precedence-tool",
    }

    proc = subprocess.run(
        [sys.executable, str(_HOOK_SCRIPT)],
        capture_output=True,
        check=False,
        env=env,
        input=json.dumps(event),
        text=True,
        timeout=30,
    )

    assert proc.returncode == 0, proc.stderr
    output = json.loads(proc.stdout)
    hook_output = output["hookSpecificOutput"]
    assert hook_output["permissionDecision"] == "deny"
    assert "status: drifted" in hook_output["permissionDecisionReason"]

    records = [
        json.loads(line)
        for line in (event_data / "security-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    check_events = [
        record
        for record in records
        if record["category"] == "skill_ledger"
        and record["details"]["result"].get("command") == "check"
    ]
    assert len(check_events) == 1
    assert check_events[0]["details"]["request"]["skill_dir"] == str(
        user_skill.resolve()
    )
    assert check_events[0]["details"]["result"]["status"] == "drifted"
