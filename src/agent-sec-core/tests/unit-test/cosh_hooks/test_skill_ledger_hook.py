"""Unit tests for cosh-extension/hooks/skill_ledger_hook.py.

The hook is self-contained (no agent_sec_cli imports), so we test it
by piping JSON via subprocess — identical to the code_scanner_hook tests.

Tests are grouped into four categories:

1. **Early fail-open paths** — invalid input and wrong tools.
2. **Host input compatibility** — legacy Cosh and Cosh-NG normalization.
3. **Skill directory resolution** — project-level lookup, context binding.
4. **Output mapping** — exposure summary message → prompt formatting.
   Uses a mock CLI script to return canned show results, verifying that only
   ``message`` controls user prompts.
"""

import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import textwrap
from pathlib import Path

import pytest
from standalone_hook_test_loader import load_standalone_hook

# Path to the standalone cosh hook script
_COSH_HOOK = str(
    Path(__file__).resolve().parents[2]
    / ".."
    / "cosh-extension"
    / "hooks"
    / "skill_ledger_hook.py"
)

skill_ledger_hook = load_standalone_hook(
    "cosh_skill_ledger_hook",
    Path(_COSH_HOOK),
)

_COSH_MANIFEST = Path(_COSH_HOOK).parents[1] / "cosh-extension.json"
_UNSET = object()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_hook(input_data, *, env_override=None, return_stderr=False):
    """Run the hook as a subprocess with *input_data* as stdin JSON.

    Returns the parsed JSON output dict.
    """
    env = os.environ.copy()
    if env_override:
        env.update(env_override)
    proc = subprocess.run(
        [sys.executable, _COSH_HOOK],
        input=json.dumps(input_data) if isinstance(input_data, dict) else input_data,
        capture_output=True,
        check=False,
        text=True,
        timeout=15,
        env=env,
    )
    assert proc.returncode == 0, f"Hook stderr: {proc.stderr}"
    output = json.loads(proc.stdout)
    if return_stderr:
        return output, proc.stderr
    return output


def _make_skill_event(skill_name, cwd=".", skill_file_path=None):
    """Build a minimal PreToolUse event for the skill tool."""
    event = {
        "hook_event_name": "PreToolUse",
        "tool_name": "skill",
        "tool_input": {"skill": skill_name},
        "cwd": cwd,
    }
    if skill_file_path is not None:
        event["skill_context"] = {
            "skill_name": skill_name,
            "file_path": str(skill_file_path),
        }
    return event


def _make_ng_skill_event(
    skill_name,
    cwd=".",
    skill_file_path=None,
    *,
    action=_UNSET,
    legacy_skill=_UNSET,
):
    """Build a Cosh-NG skill event, optionally omitting its action."""
    tool_input = {"name": skill_name}
    if action is not _UNSET:
        tool_input["action"] = action
    if legacy_skill is not _UNSET:
        tool_input["skill"] = legacy_skill
    event = {
        "hook_event_name": "PreToolUse",
        "tool_name": "skill",
        "tool_input": tool_input,
        "cwd": cwd,
    }
    if skill_file_path is not None:
        event["skill_context"] = {
            "skill_name": skill_name,
            "file_path": str(skill_file_path),
        }
    return event


def _create_skill_dir(parent, name="test-skill", manifest_name=None):
    """Create a minimal skill directory with a SKILL.md file.

    Returns the absolute path to ``<parent>/.copilot-shell/skills/<name>/``.
    """
    manifest_name = manifest_name or name
    skill_dir = Path(parent) / ".copilot-shell" / "skills" / name
    skill_dir.mkdir(parents=True, exist_ok=True)
    (skill_dir / "SKILL.md").write_text(
        f"---\nname: {manifest_name}\ndescription: A test skill\n---\nHello\n"
    )
    return str(skill_dir)


def test_cosh_manifest_registers_skill_ledger_by_default():
    """Default Cosh installs keep the Skill Ledger hook mounted."""
    manifest = json.loads(_COSH_MANIFEST.read_text(encoding="utf-8"))
    registered_hook_names = [
        hook.get("name")
        for event_hooks in manifest.get("hooks", {}).values()
        for matcher_hooks in event_hooks
        for hook in matcher_hooks.get("hooks", [])
    ]

    assert "skill-ledger" in registered_hook_names


@pytest.mark.parametrize("value", [None, "invalid"])
def test_missing_or_invalid_enabled_value_defaults_to_true(monkeypatch, value):
    if value is None:
        monkeypatch.delenv("SKILL_LEDGER_HOOK_ENABLED", raising=False)
    else:
        monkeypatch.setenv("SKILL_LEDGER_HOOK_ENABLED", value)

    assert skill_ledger_hook.env_flag_enabled("SKILL_LEDGER_HOOK_ENABLED", True)


@pytest.mark.parametrize(
    "tool_input",
    [
        {"skill": "test-skill"},
        {"action": "invoke", "name": "test-skill"},
        {"name": "test-skill"},
    ],
    ids=["legacy", "ng-explicit-invoke", "ng-implicit-invoke"],
)
def test_injects_trace_context_into_skill_ledger_show_command(
    monkeypatch, capsys, tool_input
):
    captured = {}

    def fake_run(args, **kwargs):
        captured["args"] = args
        captured["kwargs"] = kwargs
        return subprocess.CompletedProcess(
            args=args,
            returncode=0,
            stdout=json.dumps({"latestStatus": "pass", "message": None}),
            stderr="",
        )

    monkeypatch.setattr(skill_ledger_hook, "_ensure_keys", lambda _input_data: None)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_resolve_skill_dir",
        lambda _skill_name, _cwd: ("/project/.copilot-shell/skills/test-skill", False),
    )
    monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
    monkeypatch.setattr(
        skill_ledger_hook.sys,
        "stdin",
        io.StringIO(
            json.dumps(
                {
                    "hook_event_name": "PreToolUse",
                    "tool_name": "skill",
                    "tool_input": tool_input,
                    "cwd": "/project",
                    "trace_id": "trace-1",
                    "session_id": "session-1",
                    "run_id": "run-1",
                    "tool_use_id": "tool-1",
                }
            )
        ),
    )

    skill_ledger_hook.main()

    output = json.loads(capsys.readouterr().out)
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
        "skill-ledger",
        "show",
        "/project/.copilot-shell/skills/test-skill",
    ]
    assert captured["kwargs"]["check"] is False


# ---------------------------------------------------------------------------
# Early exits and infrastructure fail-open tests
# ---------------------------------------------------------------------------


class TestFailOpen:
    """Infrastructure errors and unrelated inputs remain fail-open."""

    def test_hook_disabled_short_circuits_before_work(self, monkeypatch, capsys):
        monkeypatch.setattr(skill_ledger_hook, "_HOOK_ENABLED", False)
        monkeypatch.setattr(
            skill_ledger_hook.json,
            "load",
            lambda _stream: pytest.fail("input should not be read"),
        )
        monkeypatch.setattr(
            skill_ledger_hook,
            "_resolve_skill_dir_from_context",
            lambda *_args: pytest.fail("skills should not be resolved"),
        )
        monkeypatch.setattr(
            skill_ledger_hook,
            "_ensure_keys",
            lambda *_args: pytest.fail("keys should not be initialized"),
        )
        monkeypatch.setattr(
            skill_ledger_hook.subprocess,
            "run",
            lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
        )

        skill_ledger_hook.main()

        assert json.loads(capsys.readouterr().out) == {"decision": "allow"}

    def test_invalid_json_allows(self):
        """Malformed stdin should fail-open."""
        output = _run_hook("not-json")
        assert output == {"decision": "allow"}

    def test_empty_stdin_allows(self):
        output = _run_hook("")
        assert output == {"decision": "allow"}

    def test_wrong_tool_name_allows(self):
        output = _run_hook(
            {
                "tool_name": "run_shell_command",
                "tool_input": {"command": "echo hello"},
            }
        )
        assert output == {"decision": "allow"}

    def test_missing_tool_name_allows(self):
        output = _run_hook({"tool_input": {"skill": "test"}})
        assert output == {"decision": "allow"}

    def test_skill_dir_not_found_allows(self):
        """Skill name that resolves to no on-disk directory → fail-open."""
        output, stderr = _run_hook(
            _make_skill_event("nonexistent-skill-xyz", "/tmp"),
            return_stderr=True,
        )
        assert output == {"decision": "allow"}
        assert "reason" not in output
        assert "not found on disk" in stderr
        assert "nonexistent-skill-xyz" in stderr

    def test_path_traversal_blocked(self, tmp_path):
        """A ``../`` skill name that escapes the skills base emits a warning.

        Layout::
            <tmp>/project/.copilot-shell/skills/   (skills base — empty)
            <tmp>/project/.copilot-shell/evil/      (valid SKILL.md, outside base)

        ``../evil`` resolves outside the skills base → hook must warn about
        path traversal and never reach the CLI.
        """
        project = tmp_path / "project"
        skills_base = project / ".copilot-shell" / "skills"
        skills_base.mkdir(parents=True)
        evil = project / ".copilot-shell" / "evil"
        evil.mkdir()
        (evil / "SKILL.md").write_text("---\nname: evil\n---\n")

        output, stderr = _run_hook(
            _make_skill_event("../evil", str(project)),
            return_stderr=True,
        )
        assert output == {"decision": "allow"}
        assert "reason" not in output
        assert "path traversal" in stderr
        assert "../evil" in stderr


# ---------------------------------------------------------------------------
# Host input compatibility and contract validation
# ---------------------------------------------------------------------------


class TestHostInputCompatibility:
    """Bind Ledger checks to the identity each Cosh generation executes."""

    @pytest.mark.parametrize(
        "action",
        [_UNSET, None, 42, {}],
        ids=["missing", "null", "number", "object"],
    )
    def test_ng_missing_or_nonstring_action_is_implicit_invoke(
        self, mock_cli_env, action
    ):
        env = mock_cli_env["make_env"](
            json.dumps({"latestStatus": "drifted", "message": "drifted latest"})
        )
        output = _run_hook(
            _make_ng_skill_event(
                "test-skill",
                mock_cli_env["cwd"],
                action=action,
            ),
            env_override=env,
        )

        assert output["decision"] == "ask"
        assert "drifted latest" in output["reason"]

    @pytest.mark.parametrize(
        "tool_input",
        [
            {"action": "list"},
            {
                "action": "list",
                "name": "ignored-name",
                "skill": "conflicting-name",
            },
        ],
        ids=["missing-identity", "conflicting-identities"],
    )
    def test_ng_list_is_a_silent_side_effect_free_allow(
        self, monkeypatch, capsys, tool_input
    ):
        monkeypatch.setattr(
            skill_ledger_hook,
            "_read_policy",
            lambda: pytest.fail("policy should not be read"),
        )
        monkeypatch.setattr(
            skill_ledger_hook,
            "_validate_skill_context",
            lambda *_args: pytest.fail("context should not be validated"),
        )
        monkeypatch.setattr(
            skill_ledger_hook,
            "_resolve_skill_dir_from_context",
            lambda *_args: pytest.fail("skills should not be resolved"),
        )
        monkeypatch.setattr(
            skill_ledger_hook,
            "_resolve_skill_dir",
            lambda *_args: pytest.fail("skills should not be resolved by name"),
        )
        monkeypatch.setattr(
            skill_ledger_hook,
            "_ensure_keys",
            lambda *_args: pytest.fail("keys should not be initialized"),
        )
        monkeypatch.setattr(
            skill_ledger_hook.subprocess,
            "run",
            lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
        )
        monkeypatch.setattr(
            skill_ledger_hook.sys,
            "stdin",
            io.StringIO(
                json.dumps(
                    {
                        "tool_name": "skill",
                        "tool_input": tool_input,
                        "skill_context": None,
                    }
                )
            ),
        )

        skill_ledger_hook.main()

        captured = capsys.readouterr()
        assert json.loads(captured.out) == {"decision": "allow"}
        assert captured.err == ""

    def test_matching_ng_and_legacy_identities_are_accepted(self, mock_cli_env):
        env = mock_cli_env["make_env"](
            json.dumps({"latestStatus": "drifted", "message": "drifted latest"})
        )
        output = _run_hook(
            _make_ng_skill_event(
                "test-skill",
                mock_cli_env["cwd"],
                action="invoke",
                legacy_skill="test-skill",
            ),
            env_override=env,
        )

        assert output["decision"] == "ask"
        assert "test-skill" in output["reason"]

    def test_conflicting_ng_and_legacy_identities_are_rejected(self):
        output = _run_hook(
            _make_ng_skill_event(
                "canonical-name",
                action="invoke",
                legacy_skill="other-name",
            ),
            env_override={"SKILL_LEDGER_MODE": "block"},
        )

        assert output["decision"] == "block"
        assert "identities conflict" in output["reason"]

    def test_identity_whitespace_is_not_normalized(self):
        assert skill_ledger_hook._normalize_skill_call({"name": " test-skill "}) == (
            " test-skill ",
            None,
            False,
        )

    @pytest.mark.parametrize(
        ("tool_input", "expected_detail"),
        [
            ({}, "skill is empty or missing"),
            ({"skill": "   "}, "skill is empty or missing"),
            ({"skill": 42}, "skill must be a string"),
            ({"action": "invoke"}, "name is empty or missing"),
            ({"name": "   "}, "name is empty or missing"),
            ({"name": 42}, "name must be a string"),
            ({"action": "inspect", "name": "test-skill"}, "unsupported Cosh-NG action"),
        ],
        ids=[
            "legacy-missing",
            "legacy-blank",
            "legacy-nonstring",
            "ng-missing",
            "ng-blank",
            "ng-nonstring",
            "ng-unknown-action",
        ],
    )
    def test_invalid_invocations_are_contract_errors(self, tool_input, expected_detail):
        output = _run_hook(
            {"tool_name": "skill", "tool_input": tool_input},
            env_override={"SKILL_LEDGER_MODE": "block"},
        )

        assert output["decision"] == "block"
        assert expected_detail in output["reason"]

    @pytest.mark.parametrize(
        ("policy", "expected_decision", "visible_reason", "debug_stderr"),
        [
            ("observe", "allow", False, True),
            ("warn", "allow", True, False),
            ("ask", "ask", True, False),
            ("block", "block", True, False),
        ],
    )
    def test_contract_errors_follow_policy(
        self,
        policy,
        expected_decision,
        visible_reason,
        debug_stderr,
    ):
        output, stderr = _run_hook(
            {"tool_name": "skill", "tool_input": {"action": "invoke"}},
            env_override={"SKILL_LEDGER_MODE": policy},
            return_stderr=True,
        )

        assert output["decision"] == expected_decision
        assert ("reason" in output) is visible_reason
        assert ("cannot bind the invoked identity" in stderr) is debug_stderr


# ---------------------------------------------------------------------------
# Skill directory resolution tests
# ---------------------------------------------------------------------------


class TestSkillDirResolution:
    """Verify the hook correctly locates skill directories."""

    def test_project_level_skill_found(self, mock_cli_env):
        """Skill in <cwd>/.copilot-shell/skills/<name>/ should be found.

        We verify by feeding a mock CLI that returns a non-empty ``message``.
        If the skill dir is found the hook calls the CLI and produces a
        ``reason`` field; if the skill dir were *not* found, the hook would
        return plain allow with no ``reason`` at all.
        """
        env = mock_cli_env["make_env"](
            json.dumps({"latestStatus": "drifted", "message": "drifted latest"})
        )
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output["decision"] == "ask"
        assert "reason" in output, "Skill dir not found — CLI was never called"

    @pytest.mark.parametrize("profile", ["legacy", "ng"])
    def test_skill_context_resolves_name_directory_mismatch(
        self, mock_cli_env, profile
    ):
        """skill_context.file_path should locate project skills by real path.

        This covers the case where the Skill tool receives the frontmatter
        name, but the on-disk directory uses a different name.
        """
        skill_dir = _create_skill_dir(
            mock_cli_env["cwd"],
            name="directory-name",
            manifest_name="frontmatter-name",
        )
        env = mock_cli_env["make_env"](
            json.dumps({"latestStatus": "drifted", "message": "drifted latest"})
        )
        make_event = _make_skill_event if profile == "legacy" else _make_ng_skill_event
        output = _run_hook(
            make_event(
                "frontmatter-name",
                mock_cli_env["cwd"],
                Path(skill_dir) / "SKILL.md",
            ),
            env_override=env,
        )
        assert output["decision"] == "ask"
        assert "drifted latest" in output["reason"]

    @pytest.mark.parametrize(
        "skill_context",
        [
            None,
            [],
            {},
            {"skill_name": "test-skill"},
            {"file_path": "/project/SKILL.md"},
            {"skill_name": 42, "file_path": "/project/SKILL.md"},
            {"skill_name": "test-skill", "file_path": "   "},
        ],
        ids=[
            "null",
            "array",
            "empty",
            "missing-path",
            "missing-name",
            "nonstring-name",
            "blank-path",
        ],
    )
    def test_present_malformed_context_is_a_contract_error(self, skill_context):
        event = _make_skill_event("test-skill")
        event["skill_context"] = skill_context

        output = _run_hook(
            event,
            env_override={"SKILL_LEDGER_MODE": "block"},
        )

        assert output["decision"] == "block"
        assert "skill_context" in output["reason"]

    def test_context_identity_cannot_override_invoked_identity(self, tmp_path):
        skill_file = tmp_path / "SKILL.md"
        skill_file.write_text("---\nname: context-name\n---\n")
        event = _make_ng_skill_event(
            "invoked-name",
            action="invoke",
        )
        event["skill_context"] = {
            "skill_name": "context-name",
            "file_path": str(skill_file),
        }

        output = _run_hook(
            event,
            env_override={"SKILL_LEDGER_MODE": "block"},
        )

        assert output["decision"] == "block"
        assert "conflicts with the invoked identity" in output["reason"]

    def test_skill_context_skips_only_unresolvable_supported_base(self, mock_cli_env):
        """A bad project base should not discard user/system base checks."""
        home = Path(mock_cli_env["cwd"]).parent / "home"
        skill_dir = home / ".copilot-shell" / "skills" / "user-dir"
        skill_dir.mkdir(parents=True)
        skill_file = skill_dir / "SKILL.md"
        skill_file.write_text(
            "---\nname: user-skill\ndescription: A user skill\n---\nHello\n"
        )

        env = mock_cli_env["make_env"](
            json.dumps({"latestStatus": "drifted", "message": "drifted latest"})
        )
        env["HOME"] = str(home)
        output = _run_hook(
            _make_skill_event("user-skill", "\0bad-project", skill_file),
            env_override=env,
        )

        assert output["decision"] == "ask"
        assert "drifted latest" in output["reason"]

    @pytest.mark.parametrize("scope_name", ["custom", "extension", "remote"])
    def test_skill_context_outside_supported_scope_debug_skips(
        self, tmp_path, scope_name
    ):
        """custom/extension/remote paths are out of scope for this hook."""
        home = tmp_path / "home"
        project = tmp_path / "project"
        home.mkdir()
        project.mkdir()

        if scope_name == "custom":
            skill_dir = tmp_path / "custom-skills" / "custom-skill"
        elif scope_name == "extension":
            skill_dir = (
                home
                / ".copilot-shell"
                / "extensions"
                / "test-ext"
                / "skills"
                / "extension-skill"
            )
        else:
            skill_dir = (
                home / ".copilot-shell" / "remote-skills" / "system" / "remote-skill"
            )

        skill_dir.mkdir(parents=True)
        skill_file = skill_dir / "SKILL.md"
        skill_file.write_text(
            f"---\nname: {scope_name}-skill\ndescription: A test skill\n---\n"
        )

        output, stderr = _run_hook(
            _make_skill_event(f"{scope_name}-skill", str(project), skill_file),
            env_override={"HOME": str(home)},
            return_stderr=True,
        )

        assert output == {"decision": "allow"}
        assert "outside current skill-ledger hook scope" in stderr
        assert "project/user/system" in stderr

    def test_missing_skill_md_not_found(self):
        """Directory exists but no SKILL.md → not recognized as a skill."""
        with tempfile.TemporaryDirectory() as tmpdir:
            skill_dir = Path(tmpdir) / ".copilot-shell" / "skills" / "bad"
            skill_dir.mkdir(parents=True)
            # No SKILL.md file
            output, stderr = _run_hook(
                _make_skill_event("bad", tmpdir),
                return_stderr=True,
            )
            assert output == {"decision": "allow"}
            assert "reason" not in output
            assert "not found on disk" in stderr
            assert "bad" in stderr


# A tiny script that pretends to be agent-sec-cli.
# It reads _MOCK_CHECK_OUTPUT env var and prints it to stdout.
# For "init --no-baseline", it's a no-op.
_MOCK_CLI_SCRIPT = f"#!{sys.executable}\n" + textwrap.dedent("""\
    import os, sys
    # init --no-baseline → silent success
    if len(sys.argv) >= 4 and sys.argv[2] == "init" and sys.argv[3] == "--no-baseline":
        sys.exit(0)
    # show → return canned output from env
    output = os.environ.get("_MOCK_CHECK_OUTPUT", "")
    rc = int(os.environ.get("_MOCK_CHECK_RC", "0"))
    if output:
        print(output)
    sys.exit(rc)
""")


@pytest.fixture()
def mock_cli_env(tmp_path):
    """Create a fake ``agent-sec-cli`` and a skill dir in a temp project.

    Returns a dict with ``cwd``, ``skill_dir``, and a function
    ``env(summary, rc=0)`` that builds the env dict for a given canned
    show response.
    """
    # Write the mock CLI script
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    cli_script = bin_dir / "agent-sec-cli"
    cli_script.write_text(_MOCK_CLI_SCRIPT)
    cli_script.chmod(cli_script.stat().st_mode | stat.S_IEXEC)

    # Create skill directory
    project = tmp_path / "project"
    project.mkdir()
    _create_skill_dir(str(project), "test-skill")

    # Create fake key files so _ensure_keys() is a no-op
    data_dir = tmp_path / "xdg-data" / "agent-sec" / "skill-ledger"
    data_dir.mkdir(parents=True)
    (data_dir / "key.pub").write_text("fake-pub")
    (data_dir / "key.enc").write_text("fake-enc")

    def _make_env(check_output, *, rc=0):
        """Build env override dict for a given canned CLI response."""
        return {
            "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
            "XDG_DATA_HOME": str(tmp_path / "xdg-data"),
            "_MOCK_CHECK_OUTPUT": check_output,
            "_MOCK_CHECK_RC": str(rc),
        }

    return {
        "cwd": str(project),
        "make_env": _make_env,
    }


class TestOutputMapping:
    """Verify exposure summary message → decision/reason mapping."""

    def test_default_policy_is_ask(self):
        assert skill_ledger_hook._DEFAULT_POLICY == "ask"

    def test_warn_null_message_returns_silent_allow(self, mock_cli_env):
        """latestStatus=warn with message=null → allow with NO reason field."""
        env = mock_cli_env["make_env"](
            json.dumps(
                {
                    "latestStatus": "warn",
                    "message": None,
                }
            )
        )
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output == {"decision": "allow"}

    def test_nonempty_message_requires_confirmation(self, mock_cli_env):
        """message text → ask with the shared Skill Ledger message."""
        env = mock_cli_env["make_env"](
            json.dumps(
                {
                    "latestStatus": "deny",
                    "reasonCode": "latest_risk_fallback_to_previous",
                    "message": "Latest skill status is deny; active version is v000001.",
                }
            )
        )
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output["decision"] == "ask"
        assert "Latest skill status is deny" in output["reason"]
        assert "test-skill" in output["reason"]

    def test_debug_policy_logs_message_without_prompt(self, mock_cli_env):
        """SKILL_LEDGER_MODE=debug keeps message-based prompts silent."""
        env = mock_cli_env["make_env"](
            json.dumps(
                {
                    "latestStatus": "deny",
                    "message": "Latest skill status is deny; active version is hidden.",
                }
            )
        )
        env["SKILL_LEDGER_MODE"] = "debug"
        output, stderr = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
            return_stderr=True,
        )
        assert output == {"decision": "allow"}
        assert "Latest skill status is deny" in stderr

    def test_warn_policy_returns_allow_with_reason(self, mock_cli_env):
        """SKILL_LEDGER_MODE=warn turns message into allow + reason."""
        env = mock_cli_env["make_env"](
            json.dumps(
                {
                    "latestStatus": "deny",
                    "message": "Latest skill status is deny; active version is hidden.",
                }
            )
        )
        env["SKILL_LEDGER_MODE"] = "warn"
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output["decision"] == "allow"
        assert "Latest skill status is deny" in output["reason"]

    def test_block_policy_returns_block_with_reason(self, mock_cli_env):
        """SKILL_LEDGER_MODE=block rejects execution with the summary message."""
        env = mock_cli_env["make_env"](
            json.dumps(
                {
                    "latestStatus": "deny",
                    "message": "Latest skill status is deny; active version is hidden.",
                }
            )
        )
        env["SKILL_LEDGER_MODE"] = "block"
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output["decision"] == "block"
        assert "Latest skill status is deny" in output["reason"]

    def test_block_policy_allows_unmanaged_summary_without_message(self, mock_cli_env):
        """Unmanaged roots are diagnostic-only and must not block hook execution."""
        env = mock_cli_env["make_env"](
            json.dumps(
                {
                    "latestStatus": "unmanaged",
                    "managed": False,
                    "reasonCode": "unmanaged_skill_root",
                    "message": None,
                }
            )
        )
        env["SKILL_LEDGER_MODE"] = "block"
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output == {"decision": "allow"}

    def test_missing_message_field_allows(self, mock_cli_env):
        """CLI returns JSON without message → fail-open silent allow."""
        env = mock_cli_env["make_env"](json.dumps({"latestStatus": "deny"}))
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output == {"decision": "allow"}

    def test_cli_invalid_json_stdout_allows(self, mock_cli_env):
        """CLI returns non-JSON stdout → fail-open."""
        env = mock_cli_env["make_env"]("not-json-at-all")
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output == {"decision": "allow"}

    def test_cli_empty_stdout_allows(self, mock_cli_env):
        """CLI returns empty stdout → fail-open."""
        env = mock_cli_env["make_env"]("")
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output == {"decision": "allow"}

    def test_empty_message_field_allows(self, mock_cli_env):
        """CLI returns empty message → silent allow."""
        env = mock_cli_env["make_env"](json.dumps({"message": ""}))
        output = _run_hook(
            _make_skill_event("test-skill", mock_cli_env["cwd"]),
            env_override=env,
        )
        assert output == {"decision": "allow"}


def test_invalid_mode_reports_ask_fallback(monkeypatch, capsys):
    monkeypatch.setenv("SKILL_LEDGER_MODE", "banana")

    assert skill_ledger_hook._read_policy() == "ask"
    assert "invalid SKILL_LEDGER_MODE; using ask" in capsys.readouterr().err


@pytest.mark.parametrize(
    "data_home", [None, "", "relative", "/x/./y", "/x/../y", "absolute"]
)
@pytest.mark.parametrize("with_context", [False, True])
def test_raw_user_skill_reaches_block_policy(
    mock_cli_env, tmp_path, data_home, with_context
):
    env = mock_cli_env["make_env"](json.dumps({"message": "raw skill denied"}))
    home = tmp_path / "home"
    root = tmp_path / "XDG Data" if data_home == "absolute" else home / ".local/share"
    skill = root / "anolisa/skills/raw-probe"
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text("---\nname: raw-probe\n---\n")
    env.update(
        HOME=str(home),
        XDG_DATA_HOME=str(root) if data_home == "absolute" else data_home or "",
        SKILL_LEDGER_MODE="block",
    )
    output = _run_hook(
        _make_ng_skill_event(
            "raw-probe",
            mock_cli_env["cwd"],
            skill / "SKILL.md" if with_context else None,
        ),
        env_override=env,
    )
    assert output["decision"] == "block"
    assert "raw skill denied" in output["reason"]
