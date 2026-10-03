"""Six Hook adapters against real Rust CLI/daemon processes, without Agent hosts.

Installed-layout runs require installed assets; they never fall back to source.
The CLI router records attribution and execs Rust without changing any response.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Callable

import pytest

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "tests/v2/fixtures"
HOSTS = ("codex", "qoder", "qwen", "cosh", "hermes", "openclaw")
INITIALIZERS = {"codex", "qwen", "cosh", "openclaw"}
CHECK_HOSTS = {"codex", "qoder"}
HookCase = tuple[str, Path, Any, dict[str, str], Path]


def _asset(host: str) -> Path:
    relative = {
        "codex": "codex-plugin/hooks-plugin/hooks",
        "qoder": "qoder-plugin/hooks",
        "qwen": "qwen-code-extension/hooks",
        "cosh": "cosh-extension/hooks",
        "hermes": "hermes-plugin",
        "openclaw": "openclaw-plugin/dist",
    }[host]
    installed = os.environ.get("SKILLSEC_HOOK_LAYOUT", "source") == "installed"
    path = (Path("/opt/agent-sec") if installed else ROOT) / relative
    if installed and host == "cosh":
        path = Path("/usr/share/anolisa/extensions/agent-sec-core/hooks")
    assert path.is_dir(), f"required {host} assets missing: {path}"
    return path


def _lines(path: Path) -> list[dict]:
    return (
        [json.loads(line) for line in path.read_text().splitlines()]
        if path.exists()
        else []
    )


def _wait_startup(daemon: Any, directory: Path, skills: int = 1) -> None:
    # Completion of activation also proves one-shot discovery has finished.
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        events = [
            event
            for event in _lines(directory / "audit/security-events.jsonl")
            if event.get("pid") == daemon.process.pid
            and event.get("details", {}).get("request", {}).get("command") == "activate"
        ]
        if len(events) == skills:
            assert all(event["result"] == "succeeded" for event in events), events
            return
        assert daemon.process.poll() is None, "daemon exited during startup scanning"
        time.sleep(0.02)
    pytest.fail("startup Skill activation did not complete within ten seconds")


@pytest.fixture(params=HOSTS)
def hook_case(
    request: pytest.FixtureRequest,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    start_daemon: Callable[..., Any],
) -> HookCase:
    host = request.param
    home = tmp_path / "home"
    project = tmp_path / "project"
    project.mkdir()
    folder = {
        "codex": ".codex",
        "qoder": ".qoder",
        "qwen": ".qwen",
        "cosh": ".copilot-shell",
        "hermes": ".hermes",
        "openclaw": ".openclaw",
    }[host]
    skill = home / folder / "skills/example"
    binary = shutil.which("agent-sec-cli")
    assert binary, "the real Rust CLI must be on PATH"
    router = tmp_path / "bin"
    router.mkdir()
    entry = router / "agent-sec-cli"
    entry.write_text(
        f"#!{sys.executable}\n" + (FIXTURES / "skillsec_cli_router.py").read_text()
    )
    entry.chmod(0o755)
    audit = tmp_path / "audit"
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(audit))
    startup = tmp_path / "startup"
    startup.mkdir()
    (startup / "SKILL.md").write_text(
        "---\nname: startup\ndescription: Startup readiness fixture\n---\nSafe content\n"
    )
    daemon = start_daemon(skillsec_roots=[startup, skill])
    _wait_startup(daemon, tmp_path)
    # A Skill introduced after discovery stays unscanned until the test prepares it.
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text(
        "---\nname: example\ndescription: SkillSec Hook contract fixture\n---\nSafe content\n"
    )
    env = {
        **os.environ,
        "HOME": str(home),
        "XDG_DATA_HOME": str(home / ".local/share"),
        "CODEX_HOME": str(home / ".codex"),
        "QWEN_HOME": str(home / ".qwen"),
        "HERMES_HOME": str(home / ".hermes"),
        "AGENT_SEC_DAEMON_SOCKET": str(daemon.socket_path),
        "SKILLSEC_TEST_RUST_CLI": binary,
        "SKILLSEC_TEST_CALLS": str(tmp_path / "calls.jsonl"),
        "SKILL_LEDGER_HOOK_ENABLED": "true",
        "PATH": str(router) + os.pathsep + os.environ["PATH"],
    }
    env.pop("SKILL_LEDGER_MODE", None)
    return host, skill, daemon, env, tmp_path


def _invoke(
    case: HookCase,
    *,
    policy: str | None = None,
    enabled: bool = True,
    matched: bool = True,
) -> tuple[dict | None, str]:
    host, skill, _, env, directory = case
    env = {**env, "SKILL_LEDGER_HOOK_ENABLED": str(enabled).lower()}
    if policy is not None:
        env["SKILL_LEDGER_MODE"] = policy
    trace = {
        "session_id": "skillsec-session",
        "run_id": "skillsec-run",
        "tool_use_id": "skillsec-tool",
    }
    payload = {
        **trace,
        "cwd": str(directory / "project"),
        "hook_event_name": "PreToolUse",
        "tool_name": "skill",
        "tool_input": {"skill": "example"},
    }
    if host == "codex":
        payload.update(
            hook_event_name="UserPromptSubmit",
            prompt="$example" if matched else "hello",
        )
    elif host == "qoder":
        payload["tool_name"] = "Skill"
    elif host == "cosh":
        payload["tool_input"] = {"action": "invoke", "name": "example"}
        payload["skill_context"] = {
            "skill_name": "example",
            "file_path": str(skill / "SKILL.md"),
        }
    if not matched:
        payload["tool_name"] = "unrelated"
    if host == "hermes":
        argv = [sys.executable, str(FIXTURES / "hermes_skillsec_hook.py")]
        env["PYTHONPATH"] = str(_asset(host))
        payload = {
            **trace,
            "tool_name": "skill_view" if matched else "unrelated",
            "args": {"name": "example"},
        }
    elif host == "openclaw":
        argv = ["node", str(FIXTURES / "openclaw_skillsec_hook.mjs")]
        env["SKILLSEC_TEST_OPENCLAW_DIST"] = str(_asset(host))
        payload = {
            "event": {
                "toolName": "read" if matched else "unrelated",
                "params": {"file_path": str(skill / "SKILL.md")},
            },
            "context": {
                "sessionId": "skillsec-session",
                "runId": "skillsec-run",
                "toolCallId": "skillsec-tool",
            },
        }
    else:
        argv = [sys.executable, str(_asset(host) / "skill_ledger_hook.py")]
    result = subprocess.run(
        argv,
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        env=env,
        timeout=20,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout) if result.stdout.strip() else None, result.stderr


def _allowed(output: dict | None) -> None:
    assert output in (None, {}, {"decision": "allow"}), output


def _risk(host: str, output: dict | None, *, policy: str = "block") -> None:
    assert output, f"{host} must expose the risk under {policy}"
    if policy == "ask" and host == "codex":
        assert output.get("systemMessage")
    elif host in {"qoder", "qwen"}:
        assert output["hookSpecificOutput"]["permissionDecision"] == (
            "deny" if policy == "block" else "ask"
        )
    elif host == "hermes":
        assert output["action"] == "block"
    elif host == "openclaw":
        assert (
            output.get("block") is True
            if policy == "block"
            else output.get("requireApproval")
        )
    else:
        assert output["decision"] == policy


def _prepare(case: HookCase, status: str) -> None:
    _, skill, daemon, _, directory = case
    daemon.request("skill-ledger", "init", "--no-baseline")
    if status == "none":
        return
    findings = directory / "findings.json"
    findings.write_text(
        json.dumps(
            [
                {
                    "rule": "synthetic",
                    "level": status,
                    "message": "Synthetic contract risk",
                }
            ]
            if status in {"warn", "deny"}
            else []
        )
    )
    daemon.request("skill-ledger", "certify", str(skill), "--findings", str(findings))
    if status == "drifted":
        with (skill / "SKILL.md").open("a") as stream:
            stream.write("Harmless post-certification change\n")
    if status == "tampered":
        (skill / ".skill-meta/latest.json").write_text("{}")


def _hook_events(case: HookCase) -> list[dict]:
    directory = case[4]
    pids = {call["pid"] for call in _lines(directory / "calls.jsonl")}
    return [
        event
        for event in _lines(directory / "audit/security-events.jsonl")
        if event.get("pid") in pids
    ]


def _audit(case: HookCase) -> None:
    host, _, _, _, directory = case
    calls = _lines(directory / "calls.jsonl")
    expected = (["init"] if host in INITIALIZERS else []) + [
        "check" if host in CHECK_HOSTS else "show"
    ]
    assert [call["command"] for call in calls] == expected
    events = _hook_events(case)
    assert len(events) == len(
        calls
    ), "each real Hook subprocess must have one final audit event"
    assert len({event["event_id"] for event in events}) == len(events)
    for event in events:
        assert event["event_type"] == "skill_ledger"
        assert event["uid"] == os.getuid()
        assert event["session_id"] == "skillsec-session"


@pytest.mark.parametrize(
    "status", ["pass", "none", "warn", "deny", "drifted", "tampered"]
)
def test_real_ledger_states_reach_host_policy(hook_case: HookCase, status: str) -> None:
    _prepare(hook_case, status)
    output, _ = _invoke(hook_case, policy="block")
    if status == "pass" or (status == "warn" and hook_case[0] not in CHECK_HOSTS):
        _allowed(output)
    else:
        _risk(hook_case[0], output)
    _audit(hook_case)


def test_default_policy_is_preserved(hook_case: HookCase) -> None:
    _prepare(hook_case, "deny")
    output, stderr = _invoke(hook_case)
    if hook_case[0] == "hermes":
        _allowed(output)
        assert "skill-ledger" in stderr
    else:
        _risk(hook_case[0], output, policy="ask")
    _audit(hook_case)


def test_uninitialized_daemon_ignores_legacy_home_keys(
    hook_case: HookCase, start_daemon: Callable[..., Any]
) -> None:
    host, skill, daemon, env, directory = hook_case
    daemon.process.terminate()
    daemon.process.communicate(timeout=5)
    assert daemon.process.returncode == 0
    # No authorized Skills: only the Hook may initialize this fresh daemon's keys.
    daemon = start_daemon(name="uninitialized.sock", skillsec_roots=[])
    env["AGENT_SEC_DAEMON_SOCKET"] = str(daemon.socket_path)
    hook_case = host, skill, daemon, env, directory
    legacy = Path(env["XDG_DATA_HOME"]) / "agent-sec/skill-ledger"
    legacy.mkdir(parents=True)
    for name in ("key.pub", "key.enc"):
        (legacy / name).write_text("legacy fixture, not a daemon key")
    assert not daemon.request("skill-ledger", "status")["keys"]["initialized"]
    output, stderr = _invoke(hook_case, policy="block")
    initialized = daemon.request("skill-ledger", "status")["keys"]["initialized"]
    assert initialized == (host in INITIALIZERS)
    # The subsequent query is scope-denied, not an unscanned-Skill risk verdict.
    if host == "qoder":
        _risk(host, output)
    else:
        _allowed(output)
        assert "skill-ledger" in stderr
    assert not (skill / ".skill-meta").exists(), "Hook init must not baseline or scan"
    _audit(hook_case)


def test_repeated_init_does_not_rotate_or_rescan(hook_case: HookCase) -> None:
    _prepare(hook_case, "pass")
    _, skill, daemon, _, _ = hook_case
    before = daemon.request("skill-ledger", "status")["keys"]["fingerprint"]
    manifest = (skill / ".skill-meta/latest.json").read_bytes()
    for _ in range(2):
        _allowed(_invoke(hook_case)[0])
    assert daemon.request("skill-ledger", "status")["keys"]["fingerprint"] == before
    assert (skill / ".skill-meta/latest.json").read_bytes() == manifest


@pytest.mark.parametrize("enabled,matched", [(False, True), (True, False)])
def test_disabled_or_unmatched_hook_never_calls_cli(
    hook_case: HookCase, enabled: bool, matched: bool
) -> None:
    daemon = hook_case[2]
    before = daemon.request("skill-ledger", "status")["keys"]
    _allowed(_invoke(hook_case, enabled=enabled, matched=matched)[0])
    assert not _lines(hook_case[4] / "calls.jsonl")
    assert daemon.request("skill-ledger", "status")["keys"] == before
    assert not (hook_case[1] / ".skill-meta").exists()


def test_daemon_unavailable_preserves_error_policy(hook_case: HookCase) -> None:
    host, _, _, env, directory = hook_case
    env["AGENT_SEC_DAEMON_SOCKET"] = str(directory / "missing.sock")
    output, stderr = _invoke(hook_case, policy="block")
    if host == "qoder":
        _risk(host, output)
        assert "unreadable result" in json.dumps(output)
    else:
        _allowed(output)
        assert "skill-ledger" in stderr
    calls = _lines(directory / "calls.jsonl")
    assert len(calls) == 1, "failed init must not continue to check/show"
    assert not _hook_events(hook_case)


@pytest.mark.parametrize(
    "hook_case", ["qwen", "cosh", "hermes", "openclaw"], indirect=True
)
@pytest.mark.parametrize(
    "decision,exposure", [("allow", "active"), ("block", "hidden")]
)
def test_show_consumers_follow_manual_exposure(
    hook_case: HookCase, decision: str, exposure: str
) -> None:
    host, skill, daemon, _, _ = hook_case
    _prepare(hook_case, "deny")
    daemon.request("skill-ledger", "decide", str(skill), "--action", decision)
    summary = daemon.request("skill-ledger", "show", str(skill))
    assert summary["exposureState"] == exposure
    output, _ = _invoke(hook_case, policy="block")
    # Manual decisions have no review message; SkillFS enforces hidden exposure.
    assert summary["message"] is None
    _allowed(output)
    _audit(hook_case)


def test_daemon_restart_preserves_existing_trust(
    hook_case: HookCase, start_daemon: Callable[..., Any]
) -> None:
    _prepare(hook_case, "pass")
    _, skill, daemon, _, directory = hook_case
    before = daemon.request("skill-ledger", "status")["keys"]["fingerprint"]
    _allowed(_invoke(hook_case)[0])
    daemon.process.terminate()
    daemon.process.communicate(timeout=5)
    assert daemon.process.returncode == 0
    restarted = start_daemon(skillsec_roots=[directory / "startup", skill])
    _wait_startup(restarted, directory, skills=2)
    _allowed(_invoke(hook_case)[0])
    assert restarted.request("skill-ledger", "status")["keys"]["fingerprint"] == before


def test_admin_rotation_requires_recertification(hook_case: HookCase) -> None:
    _prepare(hook_case, "pass")
    host, skill, daemon, _, directory = hook_case
    before = daemon.request("skill-ledger", "status")["keys"]["fingerprint"]
    daemon.request("skill-ledger", "rotate-keys")
    _risk(host, _invoke(hook_case, policy="block")[0])
    assert daemon.request("skill-ledger", "status")["keys"]["fingerprint"] != before
    daemon.request(
        "skill-ledger",
        "certify",
        str(skill),
        "--findings",
        str(directory / "findings.json"),
    )
    _allowed(_invoke(hook_case, policy="block")[0])


def test_unsafe_system_key_is_not_replaced_by_hook(hook_case: HookCase) -> None:
    _prepare(hook_case, "pass")
    host, _, daemon, _, _ = hook_case
    key = daemon.socket_path.with_suffix(".state") / "signing-key.pk8"
    before = key.read_bytes()
    key.chmod(0o666)
    try:
        output, stderr = _invoke(hook_case, policy="block")
        if host == "qoder":
            _risk(host, output)
        else:
            _allowed(output)
            assert "skill-ledger" in stderr
        assert key.read_bytes() == before
        assert key.stat().st_mode & 0o777 == 0o666
    finally:
        key.chmod(0o600)


def test_unresponsive_socket_preserves_timeout_policy(hook_case: HookCase) -> None:
    # This is a transport timeout, not a simulated scanner response or a daemon success.
    host, _, _, env, directory = hook_case
    stalled = directory / "stalled.sock"
    env["AGENT_SEC_DAEMON_SOCKET"] = str(stalled)
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(str(stalled))
        listener.listen(1)
        output, stderr = _invoke(hook_case, policy="block")
    if host == "qoder":
        _risk(host, output)
        assert "timed out" in json.dumps(output)
    else:
        _allowed(output)
        assert "skill-ledger" in stderr
    assert len(_lines(directory / "calls.jsonl")) == 1
    assert not _hook_events(hook_case)


@pytest.mark.parametrize(
    "hook_case", ["qwen", "cosh", "hermes", "openclaw"], indirect=True
)
def test_out_of_scope_show_preserves_error_policy_without_registration(
    hook_case: HookCase, start_daemon: Callable[..., Any]
) -> None:
    _, skill, daemon, env, _ = hook_case
    daemon.process.terminate()
    daemon.process.communicate(timeout=5)
    assert daemon.process.returncode == 0
    restricted = start_daemon(name="restricted.sock", skillsec_roots=[])
    restricted.request("skill-ledger", "init", "--no-baseline")
    env["AGENT_SEC_DAEMON_SOCKET"] = str(restricted.socket_path)
    result = restricted.cli("skill-ledger", "show", str(skill))
    assert result.returncode == 1
    failure = json.loads(result.stdout)
    assert failure["status"] == "error"
    assert "outside managedSkillDirs" in failure["error"]
    output, stderr = _invoke(hook_case, policy="block")
    _allowed(output)
    assert "skill-ledger" in stderr
    assert not (skill / ".skill-meta").exists()
    assert not (
        restricted.socket_path.with_suffix(".state") / "managed-skills.json"
    ).exists()


@pytest.mark.parametrize(
    "hook_case", ["qwen", "cosh", "hermes", "openclaw"], indirect=True
)
def test_pending_snapshot_warning_is_not_hidden_by_pass_status(
    hook_case: HookCase,
) -> None:
    _prepare(hook_case, "pass")
    host, skill, daemon, _, _ = hook_case
    (skill / ".skill-meta/versions/v000001.snapshot/SKILL.md").write_text(
        "Damaged fixture\n"
    )
    result = daemon.cli("skill-ledger", "show", str(skill))
    assert result.returncode == 0, result.stderr
    summary = json.loads(result.stdout)
    assert summary["latestStatus"] == "pass"
    assert summary["exposureState"] == "pending"
    assert summary["message"]
    _risk(host, _invoke(hook_case, policy="block")[0])
    _audit(hook_case)
