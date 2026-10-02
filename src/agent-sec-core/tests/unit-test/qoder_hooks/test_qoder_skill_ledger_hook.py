"""Unit tests for the Qoder Skill Ledger hook."""

import json
import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest
from standalone_hook_test_loader import load_standalone_hook

_PLUGIN_DIR = Path(__file__).resolve().parents[3] / "qoder-plugin"
_HOOK_SCRIPT = _PLUGIN_DIR / "hooks" / "skill_ledger_hook.py"

skill_ledger_hook = load_standalone_hook(
    "qoder_skill_ledger_hook_direct",
    _HOOK_SCRIPT,
)

_MOCK_CLI_SCRIPT = f"#!{sys.executable}\n" + textwrap.dedent("""\
    import json
    import os
    import sys
    import time

    capture_path = os.environ.get("_MOCK_CLI_CAPTURE")
    if capture_path:
        with open(capture_path, "w", encoding="utf-8") as handle:
            json.dump({"argv": sys.argv[1:]}, handle)

    delay = float(os.environ.get("_MOCK_CLI_SLEEP", "0"))
    if delay:
        time.sleep(delay)
    stderr = os.environ.get("_MOCK_CLI_STDERR", "")
    if stderr:
        print(stderr, file=sys.stderr)
    output = os.environ.get("_MOCK_CLI_OUTPUT", "")
    if output:
        print(output)
    sys.exit(int(os.environ.get("_MOCK_CLI_RC", "0")))
    """)


@pytest.fixture()
def mock_cli(tmp_path: Path):
    """Install a configurable fake agent-sec-cli for hook subprocess tests."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    cli = bin_dir / "agent-sec-cli"
    cli.write_text(_MOCK_CLI_SCRIPT)
    cli.chmod(cli.stat().st_mode | stat.S_IEXEC)
    capture = tmp_path / "capture.json"
    home = tmp_path / "home"
    home.mkdir()

    def make_env(
        output: str = "",
        *,
        rc: int = 0,
        extra: dict[str, str] | None = None,
        include_cli: bool = True,
    ) -> tuple[dict[str, str], Path, Path]:
        path_entries = [str(bin_dir)] if include_cli else []
        path_entries.append(os.environ.get("PATH", ""))
        env = {
            **os.environ,
            "HOME": str(home),
            "PATH": os.pathsep.join(path_entries),
            "PYTHONPATH": str(_PLUGIN_DIR / "hooks"),
            "SKILL_LEDGER_MODE": "ask",
            "SKILL_LEDGER_TIMEOUT": "5",
            "_MOCK_CLI_OUTPUT": output,
            "_MOCK_CLI_RC": str(rc),
            "_MOCK_CLI_CAPTURE": str(capture),
        }
        if extra:
            env.update(extra)
        return env, capture, home

    return make_env


def _make_skill(root: Path, directory: str, declared_name: str | None = None) -> Path:
    skill_dir = root / directory
    skill_dir.mkdir(parents=True)
    if declared_name is None:
        content = "# Skill without frontmatter\n"
    else:
        content = f"---\nname: {declared_name}\ndescription: test\n---\n# Test\n"
    (skill_dir / "SKILL.md").write_text(content)
    return skill_dir


def _event(project: Path, skill_name: object = "review-pr") -> dict[str, object]:
    return {
        "hook_event_name": "PreToolUse",
        "tool_name": "Skill",
        "tool_input": {"skill": skill_name},
        "cwd": str(project),
        "session_id": "session-1",
        "run_id": "run-1",
        "call_id": "call-1",
        "tool_use_id": "tool-1",
    }


def _run_hook(
    input_data: object, env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    stdin_text = (
        json.dumps(input_data) if isinstance(input_data, dict) else str(input_data)
    )
    return subprocess.run(
        [sys.executable, str(_HOOK_SCRIPT)],
        capture_output=True,
        check=False,
        env=env,
        input=stdin_text,
        text=True,
        timeout=15,
    )


def _stdout_json(proc: subprocess.CompletedProcess[str]) -> dict[str, object]:
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip()
    return json.loads(proc.stdout)


def _captured_call(path: Path) -> dict[str, object]:
    return json.loads(path.read_text())


def _permission(output: dict[str, object]) -> dict[str, object]:
    value = output["hookSpecificOutput"]
    assert isinstance(value, dict)
    return value


def test_ignores_non_skill_events(mock_cli, tmp_path: Path) -> None:
    env, capture, _home = mock_cli(output=json.dumps({"status": "deny"}))
    event = _event(tmp_path)
    event["tool_name"] = "Bash"

    proc = _run_hook(event, env)

    assert proc.returncode == 0
    assert proc.stdout == ""
    assert not capture.exists()


def test_hook_disabled_short_circuits_before_work(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(skill_ledger_hook, "_HOOK_ENABLED", False)
    monkeypatch.setattr(
        skill_ledger_hook,
        "load_hook_input",
        lambda: pytest.fail("input should not be read"),
    )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_resolve_skill",
        lambda *_args: pytest.fail("skills should not be resolved"),
    )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_run_check",
        lambda *_args: pytest.fail("CLI should not be called"),
    )

    skill_ledger_hook.main()

    assert capsys.readouterr().out == ""


def test_project_skill_resolves_to_canonical_path_with_trace_context(
    mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    skill = _make_skill(project / ".qoder" / "skills", "folder", "review-pr")
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))

    proc = _run_hook(_event(project), env)

    assert proc.returncode == 0
    assert proc.stdout == ""
    argv = _captured_call(capture)["argv"]
    assert argv[-3:] == ["skill-ledger", "check", str(skill.resolve())]
    context = json.loads(argv[argv.index("--trace-context") + 1])
    assert context == {
        "agent_name": "qoder",
        "session_id": "session-1",
        "run_id": "run-1",
        "call_id": "call-1",
        "tool_call_id": "tool-1",
    }


def test_user_skill_overrides_same_named_project_skill(
    mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "project-copy", "shared")
    env, capture, home = mock_cli(output=json.dumps({"status": "pass"}))
    user_skill = _make_skill(home / ".qoder" / "skills", "user-copy", "shared")

    proc = _run_hook(_event(project, "shared"), env)

    assert proc.returncode == 0
    argv = _captured_call(capture)["argv"]
    assert argv[-1] == str(user_skill.resolve())


def test_user_skill_is_used_when_project_has_no_match(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    env, capture, home = mock_cli(output=json.dumps({"status": "pass"}))
    user_skill = _make_skill(home / ".qoder" / "skills", "folder", "user-skill")

    proc = _run_hook(_event(project, "user-skill"), env)

    assert proc.returncode == 0
    assert _captured_call(capture)["argv"][-1] == str(user_skill.resolve())


def test_directory_name_is_frontmatter_fallback(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    skill = _make_skill(project / ".qoder" / "skills", "fallback-name")
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))

    proc = _run_hook(_event(project, "fallback-name"), env)

    assert proc.returncode == 0
    assert _captured_call(capture)["argv"][-1] == str(skill.resolve())


@pytest.mark.parametrize(
    "name_line",
    [
        "name: target # valid YAML comment",
        'name: "target" # valid YAML comment',
        "name: 'target' # valid YAML comment",
    ],
)
def test_supported_yaml_scalars_with_inline_comments_resolve(
    name_line: str, mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    skill = _make_skill(project / ".qoder" / "skills", "folder")
    (skill / "SKILL.md").write_text(
        f"---\n{name_line}\ndescription: test\n---\n# Test\n"
    )
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))

    proc = _run_hook(_event(project, "target"), env)

    assert proc.returncode == 0
    assert proc.stdout == ""
    assert _captured_call(capture)["argv"][-1] == str(skill.resolve())


@pytest.mark.parametrize(
    ("content", "reason"),
    [
        ("---\ndescription: test\n---\n", "invalid or missing its name"),
        ("---\nname: target\n", "frontmatter is invalid"),
        (
            "---\nname: target\nname: second\n---\n",
            "declares the name field more than once",
        ),
        ("---\nname: >-\n  target\n---\n", "unsupported name scalar"),
        ('---\nname: "tar\\x67et"\n---\n', "unsupported name scalar"),
        ('---\nname: " target "\n---\n', "unsupported name scalar"),
        ("---\nname: [target]\n---\n", "unsupported name scalar"),
        ("---\nname:\n---\n", "frontmatter is invalid"),
    ],
)
def test_untrusted_frontmatter_uses_resolution_policy(
    content: str, reason: str, mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    skill = _make_skill(project / ".qoder" / "skills", "target")
    (skill / "SKILL.md").write_text(content)
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))

    output = _stdout_json(_run_hook(_event(project, "target"), env))

    assert _permission(output)["permissionDecision"] == "ask"
    assert reason in _permission(output)["permissionDecisionReason"]
    assert not capture.exists()


def test_frontmatter_beyond_read_limit_uses_resolution_policy(
    mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    skill = _make_skill(project / ".qoder" / "skills", "target")
    (skill / "SKILL.md").write_text(
        "---\nname: target\ndescription: " + ("x" * 70_000) + "\n---\n"
    )
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))

    output = _stdout_json(_run_hook(_event(project, "target"), env))

    assert _permission(output)["permissionDecision"] == "ask"
    assert "frontmatter is invalid" in _permission(output)["permissionDecisionReason"]
    assert not capture.exists()


def test_json_string_tool_input_is_supported(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    skill = _make_skill(project / ".qoder" / "skills", "json-input")
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))
    event = _event(project, "json-input")
    event["tool_input"] = json.dumps({"skill": "json-input"})

    proc = _run_hook(event, env)

    assert proc.returncode == 0
    assert proc.stdout == ""
    assert _captured_call(capture)["argv"][-1] == str(skill.resolve())


@pytest.mark.parametrize("cwd", ["relative/path", "/path/that/does/not/exist"])
def test_invalid_cwd_uses_policy(cwd: str, mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))
    event = _event(project)
    event["cwd"] = cwd

    output = _stdout_json(_run_hook(event, env))

    assert _permission(output)["permissionDecision"] == "ask"
    assert "working directory" in _permission(output)["permissionDecisionReason"]
    assert not capture.exists()


@pytest.mark.parametrize("skill_name", ["../escape", "a/b", "", 123])
def test_invalid_skill_name_uses_policy(
    skill_name: object, mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))

    output = _stdout_json(_run_hook(_event(project, skill_name), env))

    assert _permission(output)["permissionDecision"] == "ask"
    assert (
        "name is missing or invalid" in _permission(output)["permissionDecisionReason"]
    )
    assert not capture.exists()


def test_symlink_escape_uses_policy(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    skills_root = project / ".qoder" / "skills"
    skills_root.mkdir(parents=True)
    external = _make_skill(tmp_path / "external", "target", "escape")
    try:
        (skills_root / "different-folder").symlink_to(
            external, target_is_directory=True
        )
    except OSError as exc:
        pytest.skip(f"symlinks unavailable: {exc}")
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))

    output = _stdout_json(_run_hook(_event(project, "escape"), env))

    assert _permission(output)["permissionDecision"] == "ask"
    assert "escapes its trusted root" in _permission(output)["permissionDecisionReason"]
    assert not capture.exists()


def test_duplicate_name_in_one_root_is_ambiguous(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    root = project / ".qoder" / "skills"
    _make_skill(root, "first", "duplicate")
    _make_skill(root, "second", "duplicate")
    env, capture, _home = mock_cli(output=json.dumps({"status": "pass"}))

    output = _stdout_json(_run_hook(_event(project, "duplicate"), env))

    assert _permission(output)["permissionDecision"] == "ask"
    assert "multiple project Skills" in _permission(output)["permissionDecisionReason"]
    assert not capture.exists()


def test_unmatched_skill_fails_open_as_non_local_source(
    mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    env, capture, _home = mock_cli(output=json.dumps({"status": "deny"}))

    proc = _run_hook(_event(project, "built-in-skill"), env)

    assert proc.returncode == 0
    assert proc.stdout == ""
    assert "built-in, plugin, or remote" in proc.stderr
    assert not capture.exists()


@pytest.mark.parametrize("status", ["none", "drifted", "warn", "deny", "tampered"])
def test_risky_status_defaults_to_ask(status: str, mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "risk")
    env, _capture, _home = mock_cli(output=json.dumps({"status": status}))

    output = _stdout_json(_run_hook(_event(project, "risk"), env))

    assert _permission(output)["permissionDecision"] == "ask"
    assert f"status: {status}" in _permission(output)["permissionDecisionReason"]


@pytest.mark.parametrize("policy", ["ask", "debug", "warn", "block"])
def test_pass_is_silent_for_every_policy(policy: str, mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "clean")
    env, _capture, _home = mock_cli(
        output=json.dumps({"status": "pass"}),
        extra={"SKILL_LEDGER_MODE": policy},
    )

    proc = _run_hook(_event(project, "clean"), env)

    assert proc.returncode == 0
    assert proc.stdout == ""


def test_warn_policy_allows_with_system_message(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "warned")
    env, _capture, _home = mock_cli(
        output=json.dumps({"status": "warn", "findings": [{"secret": "raw"}]}),
        extra={"SKILL_LEDGER_MODE": "warn"},
    )

    output = _stdout_json(_run_hook(_event(project, "warned"), env))

    assert output["decision"] == "allow"
    assert "systemMessage" in output
    assert "1 security warnings" in output["systemMessage"]
    assert "raw" not in json.dumps(output)


def test_block_policy_denies_pre_tool_use(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "blocked")
    env, _capture, _home = mock_cli(
        output=json.dumps({"status": "deny", "findings": [{}]}),
        extra={"SKILL_LEDGER_MODE": "block"},
    )

    output = _stdout_json(_run_hook(_event(project, "blocked"), env))

    assert _permission(output)["permissionDecision"] == "deny"


def test_debug_policy_allows_with_sanitized_stderr(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "debugged")
    env, _capture, _home = mock_cli(
        output=json.dumps(
            {"status": "deny", "findings": [{"secret": "finding-secret"}]}
        ),
        extra={
            "SKILL_LEDGER_MODE": "debug",
            "_MOCK_CLI_STDERR": "raw-cli-secret",
        },
    )

    proc = _run_hook(_event(project, "debugged"), env)

    assert proc.returncode == 0
    assert proc.stdout == ""
    assert "status: deny" in proc.stderr
    assert "finding-secret" not in proc.stderr
    assert "raw-cli-secret" not in proc.stderr


def test_nonzero_exit_with_valid_json_is_enforced(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "tampered")
    env, _capture, _home = mock_cli(
        output=json.dumps({"status": "tampered", "reason": "do-not-copy"}),
        rc=1,
    )

    output = _stdout_json(_run_hook(_event(project, "tampered"), env))

    assert _permission(output)["permissionDecision"] == "ask"
    reason = _permission(output)["permissionDecisionReason"]
    assert "status: tampered" in reason
    assert "do-not-copy" not in reason


@pytest.mark.parametrize(
    ("output", "expected"),
    [
        (json.dumps({"status": "error", "error": "raw-error"}), "could not complete"),
        (json.dumps({"status": "future"}), "unsupported status"),
        ("not-json", "unreadable result"),
    ],
)
def test_cli_result_failures_follow_policy(
    output: str, expected: str, mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "broken")
    env, _capture, _home = mock_cli(output=output)

    hook_output = _stdout_json(_run_hook(_event(project, "broken"), env))

    reason = _permission(hook_output)["permissionDecisionReason"]
    assert expected in reason
    assert "raw-error" not in reason


def test_missing_cli_follows_policy(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "missing-cli")
    env, _capture, _home = mock_cli(include_cli=False)
    env["PATH"] = str(tmp_path / "empty-bin")

    output = _stdout_json(_run_hook(_event(project, "missing-cli"), env))

    assert (
        "executable is unavailable" in _permission(output)["permissionDecisionReason"]
    )


def test_timeout_follows_policy(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "slow")
    env, _capture, _home = mock_cli(
        output=json.dumps({"status": "pass"}),
        extra={"_MOCK_CLI_SLEEP": "0.2", "SKILL_LEDGER_TIMEOUT": "0.05"},
    )

    output = _stdout_json(_run_hook(_event(project, "slow"), env))

    assert "timed out" in _permission(output)["permissionDecisionReason"]


@pytest.mark.parametrize(
    ("policy", "expected_decision"), [("warn", "allow"), ("block", "deny")]
)
def test_infrastructure_error_respects_enforcement_policy(
    policy: str, expected_decision: str, mock_cli, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "infra-error")
    env, _capture, _home = mock_cli(
        output="not-json", extra={"SKILL_LEDGER_MODE": policy}
    )

    output = _stdout_json(_run_hook(_event(project, "infra-error"), env))

    if policy == "warn":
        assert output["decision"] == expected_decision
        assert "unreadable result" in output["systemMessage"]
    else:
        assert _permission(output)["permissionDecision"] == expected_decision


def test_invalid_timeout_falls_back_to_default(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "valid-timeout")
    env, capture, _home = mock_cli(
        output=json.dumps({"status": "pass"}),
        extra={"SKILL_LEDGER_TIMEOUT": "invalid"},
    )

    proc = _run_hook(_event(project, "valid-timeout"), env)

    assert proc.returncode == 0
    assert proc.stdout == ""
    assert capture.exists()


def test_invalid_mode_falls_back_to_ask(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "unsigned")
    env, _capture, _home = mock_cli(
        output=json.dumps({"status": "none"}),
        extra={"SKILL_LEDGER_MODE": "invalid"},
    )

    proc = _run_hook(_event(project, "unsigned"), env)
    output = _stdout_json(proc)

    assert _permission(output)["permissionDecision"] == "ask"
    assert "invalid SKILL_LEDGER_MODE" in proc.stderr


def test_drift_notice_contains_counts_not_file_names(mock_cli, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    _make_skill(project / ".qoder" / "skills", "drifted")
    env, _capture, _home = mock_cli(
        output=json.dumps(
            {
                "status": "drifted",
                "added": ["secret-added.txt"],
                "removed": ["secret-removed.txt"],
                "modified": ["secret-one.txt", "secret-two.txt"],
            }
        )
    )

    output = _stdout_json(_run_hook(_event(project, "drifted"), env))

    reason = _permission(output)["permissionDecisionReason"]
    assert "added=1, removed=1, modified=2" in reason
    assert "secret-" not in reason
