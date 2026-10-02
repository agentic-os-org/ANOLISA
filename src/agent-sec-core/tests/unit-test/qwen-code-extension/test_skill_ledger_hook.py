"""Unit tests for the Qwen Code Skill Ledger command hook."""

import io
import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest
from standalone_hook_test_loader import load_standalone_hook

_ROOT = Path(__file__).resolve().parents[3]
_EXTENSION_DIR = _ROOT / "qwen-code-extension"
_HOOK_PATH = _EXTENSION_DIR / "hooks" / "skill_ledger_hook.py"

skill_ledger_hook = load_standalone_hook("qwen_skill_ledger_hook", _HOOK_PATH)


def _event(skill_name="test-skill", cwd="/workspace", **overrides):
    payload = {
        "hook_event_name": "PreToolUse",
        "tool_name": "skill",
        "tool_input": {"skill": skill_name},
        "cwd": str(cwd),
        "session_id": "session-1",
        "tool_use_id": "tool-use-1",
        "tool_call_id": "tool-call-1",
    }
    payload.update(overrides)
    return payload


def _run_main(monkeypatch, capsys, payload):
    value = json.dumps(payload) if not isinstance(payload, str) else payload
    monkeypatch.setattr(sys, "stdin", io.StringIO(value))
    skill_ledger_hook.main()
    captured = capsys.readouterr()
    return json.loads(captured.out), captured.err


def _create_skill(
    root,
    directory_name="test-skill",
    manifest_name=None,
    *,
    disable_model_invocation=None,
):
    skill_dir = root / directory_name
    skill_dir.mkdir(parents=True)
    manifest_name = manifest_name or directory_name
    disabled_line = (
        f"disable-model-invocation: {disable_model_invocation}\n"
        if disable_model_invocation is not None
        else ""
    )
    (skill_dir / "SKILL.md").write_text(
        f"---\nname: {manifest_name}\ndescription: test\n{disabled_line}---\n",
        encoding="utf-8",
    )
    return skill_dir


@pytest.mark.parametrize(
    "payload",
    [
        "not-json",
        [],
        {"hook_event_name": "PostToolUse"},
        _event(tool_name="run_shell_command"),
        _event(tool_input={}),
        _event(tool_input={"skill": 7}),
        _event(cwd=None),
    ],
)
def test_invalid_or_unrelated_input_is_fail_open(monkeypatch, capsys, payload):
    monkeypatch.setattr(
        skill_ledger_hook,
        "_show_skill",
        lambda *_args: (_ for _ in ()).throw(AssertionError("unexpected show")),
    )

    output, _ = _run_main(monkeypatch, capsys, payload)

    assert output == {}


def test_hook_disabled_short_circuits_before_work(monkeypatch, capsys):
    monkeypatch.setattr(skill_ledger_hook, "_HOOK_ENABLED", False)
    monkeypatch.setattr(
        skill_ledger_hook.json,
        "load",
        lambda _stream: pytest.fail("input should not be read"),
    )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_resolve_skill_dir",
        lambda *_args: pytest.fail("skills should not be resolved"),
    )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_ensure_keys",
        lambda *_args: pytest.fail("keys should not be initialized"),
    )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_show_skill",
        lambda *_args: pytest.fail("CLI should not be called"),
    )

    skill_ledger_hook.main()

    assert json.loads(capsys.readouterr().out) == {}


def test_resolves_frontmatter_name_and_project_precedes_user(monkeypatch, tmp_path):
    project_root = tmp_path / "project" / ".qwen" / "skills"
    user_root = tmp_path / "home" / ".qwen" / "skills"
    project_skill = _create_skill(project_root, "project-dir", "shared-name")
    _create_skill(user_root, "user-dir", "shared-name")
    monkeypatch.setattr(
        skill_ledger_hook,
        "_supported_skill_bases",
        lambda _cwd: [project_root, user_root],
    )
    payload = _event("shared-name", cwd=tmp_path / "project")

    resolved = skill_ledger_hook._resolve_skill_dir(
        "shared-name", str(tmp_path / "project"), payload
    )

    assert resolved.directory == project_skill.resolve()
    assert resolved.disable_model_invocation is False


@pytest.mark.parametrize(
    "configured",
    (None, "", "absolute", "relative/config", "~", "~/custom-qwen", "~\\custom-qwen"),
)
def test_qwen_home_matches_qwen_path_semantics(monkeypatch, tmp_path, configured):
    home = tmp_path / "home"
    cwd = tmp_path / "project"
    home.mkdir()
    cwd.mkdir()
    monkeypatch.setenv("HOME", str(home))
    if configured is None:
        monkeypatch.delenv("QWEN_HOME", raising=False)
        expected = home / ".qwen"
    elif configured == "":
        monkeypatch.setenv("QWEN_HOME", "")
        expected = home / ".qwen"
    elif configured == "absolute":
        expected = tmp_path / "custom-qwen"
        monkeypatch.setenv("QWEN_HOME", str(expected))
    elif configured == "relative/config":
        monkeypatch.setenv("QWEN_HOME", configured)
        expected = cwd / configured
    elif configured == "~":
        monkeypatch.setenv("QWEN_HOME", configured)
        expected = home
    else:
        monkeypatch.setenv("QWEN_HOME", configured)
        expected = home / "custom-qwen"

    assert skill_ledger_hook._qwen_home(str(cwd)) == expected.resolve(strict=False)


def test_custom_qwen_home_is_used_instead_of_legacy_user_root(monkeypatch, tmp_path):
    home = tmp_path / "home"
    project = tmp_path / "project"
    qwen_home = tmp_path / "custom-qwen"
    home.mkdir()
    project.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("QWEN_HOME", str(qwen_home))
    expected = _create_skill(qwen_home / "skills", "active", "shared-name")
    _create_skill(home / ".qwen" / "skills", "legacy", "shared-name")
    payload = _event("shared-name", cwd=project)

    resolved = skill_ledger_hook._resolve_skill_dir(
        "shared-name", str(project), payload
    )

    assert resolved.directory == expected.resolve()


@pytest.mark.parametrize(
    ("value", "expected"),
    (
        (None, False),
        ("false", False),
        ('"false"', False),
        ("true", True),
        ("TRUE", True),
        ('"true"', True),
        ("'true'", True),
    ),
)
def test_resolved_skill_carries_disable_model_invocation(
    monkeypatch, tmp_path, value, expected
):
    root = tmp_path / "skills"
    _create_skill(root, disable_model_invocation=value)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_supported_skill_bases",
        lambda _cwd: [root],
    )

    resolved = skill_ledger_hook._resolve_skill_dir(
        "test-skill", str(tmp_path), _event(cwd=tmp_path)
    )

    assert resolved.disable_model_invocation is expected


def test_same_level_duplicate_is_ambiguous_and_does_not_fall_back(
    monkeypatch, capsys, tmp_path
):
    project_root = tmp_path / "project-skills"
    user_root = tmp_path / "user-skills"
    _create_skill(project_root, "one", "duplicate")
    _create_skill(project_root, "two", "duplicate")
    _create_skill(user_root, "fallback", "duplicate")
    monkeypatch.setattr(
        skill_ledger_hook,
        "_supported_skill_bases",
        lambda _cwd: [project_root, user_root],
    )
    payload = _event("duplicate", cwd=tmp_path)

    resolved = skill_ledger_hook._resolve_skill_dir("duplicate", str(tmp_path), payload)

    assert resolved is None
    assert '"code":"ambiguous_same_level"' in capsys.readouterr().err


@pytest.mark.parametrize("skill_name", ("../escape", "/absolute", "a/b", "a\\b"))
def test_path_like_skill_names_are_never_resolved(
    monkeypatch, capsys, tmp_path, skill_name
):
    monkeypatch.setattr(
        skill_ledger_hook,
        "_candidate_skill_dirs",
        lambda *_args: (_ for _ in ()).throw(AssertionError("unexpected lookup")),
    )

    resolved = skill_ledger_hook._resolve_skill_dir(
        skill_name, str(tmp_path), _event(skill_name, cwd=tmp_path)
    )

    assert resolved is None
    assert '"code":"invalid_skill_name"' in capsys.readouterr().err


def test_qwen_home_resolution_failure_is_fail_open(monkeypatch, capsys, tmp_path):
    monkeypatch.setattr(
        skill_ledger_hook,
        "_supported_skill_bases",
        lambda _cwd: (_ for _ in ()).throw(RuntimeError("no home")),
    )

    resolved = skill_ledger_hook._resolve_skill_dir(
        "test-skill", str(tmp_path), _event(cwd=tmp_path)
    )

    assert resolved is None
    assert '"code":"invalid_qwen_home"' in capsys.readouterr().err


def test_symlink_inside_root_is_supported(monkeypatch, tmp_path):
    root = tmp_path / "skills"
    target = _create_skill(root / ".targets", "target", "linked-skill")
    (root / "link").symlink_to(target, target_is_directory=True)
    monkeypatch.setattr(
        skill_ledger_hook, "_supported_skill_bases", lambda _cwd: [root]
    )
    payload = _event("linked-skill", cwd=tmp_path)

    resolved = skill_ledger_hook._resolve_skill_dir(
        "linked-skill", str(tmp_path), payload
    )

    assert resolved.directory == target.resolve()


def test_symlink_outside_root_is_unsupported(monkeypatch, capsys, tmp_path):
    root = tmp_path / "skills"
    root.mkdir()
    external = _create_skill(tmp_path / "external", "target", "linked-skill")
    (root / "link").symlink_to(external, target_is_directory=True)
    monkeypatch.setattr(
        skill_ledger_hook, "_supported_skill_bases", lambda _cwd: [root]
    )
    payload = _event("linked-skill", cwd=tmp_path)

    resolved = skill_ledger_hook._resolve_skill_dir(
        "linked-skill", str(tmp_path), payload
    )

    assert resolved is None
    stderr = capsys.readouterr().err
    assert '"code":"symlink_outside_skill_root"' in stderr
    assert '"code":"unsupported_or_unresolved"' in stderr


def test_broken_symlink_is_fail_open(monkeypatch, capsys, tmp_path):
    root = tmp_path / "skills"
    root.mkdir()
    (root / "broken").symlink_to(root / "missing", target_is_directory=True)
    monkeypatch.setattr(
        skill_ledger_hook, "_supported_skill_bases", lambda _cwd: [root]
    )
    payload = _event("broken", cwd=tmp_path)

    assert (
        skill_ledger_hook._resolve_skill_dir("broken", str(tmp_path), payload) is None
    )
    assert '"code":"invalid_skill_candidate"' in capsys.readouterr().err


def test_symlink_loop_is_fail_open(monkeypatch, capsys, tmp_path):
    root = tmp_path / "skills"
    root.mkdir()
    (root / "one").symlink_to(root / "two", target_is_directory=True)
    (root / "two").symlink_to(root / "one", target_is_directory=True)
    monkeypatch.setattr(
        skill_ledger_hook, "_supported_skill_bases", lambda _cwd: [root]
    )
    payload = _event("loop", cwd=tmp_path)

    assert skill_ledger_hook._resolve_skill_dir("loop", str(tmp_path), payload) is None
    assert '"code":"invalid_skill_candidate"' in capsys.readouterr().err


def test_non_directory_candidate_is_ignored(monkeypatch, capsys, tmp_path):
    root = tmp_path / "skills"
    root.mkdir()
    (root / "plain-file").write_text("not a skill", encoding="utf-8")
    monkeypatch.setattr(
        skill_ledger_hook, "_supported_skill_bases", lambda _cwd: [root]
    )
    payload = _event("plain-file", cwd=tmp_path)

    assert (
        skill_ledger_hook._resolve_skill_dir("plain-file", str(tmp_path), payload)
        is None
    )
    assert '"code":"unsupported_or_unresolved"' in capsys.readouterr().err


def test_inaccessible_project_root_does_not_fall_back_to_user(
    monkeypatch, capsys, tmp_path
):
    project_root = tmp_path / "project-skills"
    user_root = tmp_path / "user-skills"
    project_root.mkdir()
    _create_skill(user_root, "user", "test-skill")
    original_iterdir = Path.iterdir

    def fail_project_iterdir(path):
        if path == project_root:
            raise PermissionError("denied")
        return original_iterdir(path)

    monkeypatch.setattr(Path, "iterdir", fail_project_iterdir)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_supported_skill_bases",
        lambda _cwd: [project_root, user_root],
    )
    payload = _event(cwd=tmp_path)

    assert (
        skill_ledger_hook._resolve_skill_dir("test-skill", str(tmp_path), payload)
        is None
    )
    assert '"code":"inaccessible_skill_root"' in capsys.readouterr().err


def test_settings_paths_follow_qwen_scope_order(monkeypatch, tmp_path):
    home = tmp_path / "home"
    project = tmp_path / "project"
    qwen_home = tmp_path / "qwen-home"
    system = tmp_path / "system" / "settings.json"
    defaults = tmp_path / "defaults" / "settings.json"
    home.mkdir()
    project.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("QWEN_HOME", str(qwen_home))
    monkeypatch.setenv("QWEN_CODE_SYSTEM_SETTINGS_PATH", str(system))
    monkeypatch.setenv("QWEN_CODE_SYSTEM_DEFAULTS_PATH", str(defaults))

    paths = skill_ledger_hook._settings_paths(str(project))

    assert paths == [
        defaults,
        qwen_home / "settings.json",
        project / ".qwen" / "settings.json",
        system,
    ]


def test_settings_paths_skip_workspace_scope_in_home(monkeypatch, tmp_path):
    home = tmp_path / "home"
    qwen_home = tmp_path / "qwen-home"
    system = tmp_path / "system" / "settings.json"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("QWEN_HOME", str(qwen_home))
    monkeypatch.setenv("QWEN_CODE_SYSTEM_SETTINGS_PATH", str(system))
    monkeypatch.delenv("QWEN_CODE_SYSTEM_DEFAULTS_PATH", raising=False)

    paths = skill_ledger_hook._settings_paths(str(home))

    assert paths == [
        system.parent / "system-defaults.json",
        qwen_home / "settings.json",
        system,
    ]


def test_relative_system_settings_overrides_resolve_from_cwd(monkeypatch, tmp_path):
    monkeypatch.setenv("QWEN_CODE_SYSTEM_SETTINGS_PATH", "system/settings.json")
    monkeypatch.setenv("QWEN_CODE_SYSTEM_DEFAULTS_PATH", "defaults.json")

    system = skill_ledger_hook._system_settings_path(str(tmp_path))
    defaults = skill_ledger_hook._system_defaults_path(str(tmp_path), system)

    assert system == tmp_path / "system" / "settings.json"
    assert defaults == tmp_path / "defaults.json"


@pytest.mark.parametrize(
    ("platform", "expected"),
    (
        ("darwin", "/Library/Application Support/QwenCode/settings.json"),
        ("linux", "/etc/qwen-code/settings.json"),
        ("win32", r"C:\ProgramData\qwen-code\settings.json"),
    ),
)
def test_system_settings_uses_qwen_platform_defaults(
    monkeypatch, tmp_path, platform, expected
):
    monkeypatch.delenv("QWEN_CODE_SYSTEM_SETTINGS_PATH", raising=False)
    monkeypatch.setattr(skill_ledger_hook.sys, "platform", platform)

    path = skill_ledger_hook._system_settings_path(str(tmp_path))

    assert str(path) == expected


def test_json_comment_stripper_preserves_escaped_string_content():
    content = (
        '{"note": "escaped \\" // still text /* still text */", '
        '"skills": {"disabled": ["one"]}} // removed\n'
    )

    parsed = json.loads(skill_ledger_hook._strip_json_comments(content))

    assert parsed["note"] == 'escaped " // still text /* still text */'
    assert parsed["skills"]["disabled"] == ["one"]


def test_disabled_skill_names_union_jsonc_and_environment(monkeypatch, tmp_path):
    defaults = tmp_path / "system-defaults.json"
    user = tmp_path / "user-settings.json"
    workspace = tmp_path / "workspace-settings.json"
    system = tmp_path / "system-settings.json"
    defaults.write_text(
        """{
          // line comment
          "skills": {"disabled": [" One ", 7, null, "https://host//path"]}
        }""",
        encoding="utf-8",
    )
    user.write_text(
        """{
          "skills": {
            /* block comment */
            "disabled": ["$DISABLED_SKILL", "${SECOND_SKILL}", "$UNSET_SKILL"]
          }
        }""",
        encoding="utf-8",
    )
    workspace.write_text(
        '{"note": "/* text */ // text", "skills": {"disabled": ["WORKSPACE"]}}',
        encoding="utf-8",
    )
    system.write_text(
        '{"skills": {"disabled": "not-an-array"}}',
        encoding="utf-8",
    )
    monkeypatch.setenv("DISABLED_SKILL", "From-Env")
    monkeypatch.setenv("SECOND_SKILL", "Second")
    monkeypatch.delenv("UNSET_SKILL", raising=False)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_settings_paths",
        lambda _cwd: [defaults, user, workspace, system],
    )

    disabled = skill_ledger_hook._read_disabled_skill_names(
        str(tmp_path), _event(cwd=tmp_path), "test-skill"
    )

    assert disabled == frozenset(
        {
            "one",
            "https://host//path",
            "from-env",
            "second",
            "$unset_skill",
            "workspace",
        }
    )


def test_missing_settings_files_are_empty(monkeypatch, tmp_path):
    monkeypatch.setattr(
        skill_ledger_hook,
        "_settings_paths",
        lambda _cwd: [tmp_path / "missing.json"],
    )

    disabled = skill_ledger_hook._read_disabled_skill_names(
        str(tmp_path), _event(cwd=tmp_path), "test-skill"
    )

    assert disabled == frozenset()


@pytest.mark.parametrize("content", ("{", "[]", "{/* unterminated"))
def test_invalid_settings_are_visibility_unknown(
    monkeypatch, capsys, tmp_path, content
):
    settings = tmp_path / "settings.json"
    settings.write_text(content, encoding="utf-8")
    monkeypatch.setattr(
        skill_ledger_hook,
        "_settings_paths",
        lambda _cwd: [settings],
    )

    disabled = skill_ledger_hook._read_disabled_skill_names(
        str(tmp_path), _event(cwd=tmp_path), "test-skill"
    )

    assert disabled is None
    assert '"code":"skill_visibility_unknown"' in capsys.readouterr().err


def test_unreadable_settings_are_visibility_unknown(monkeypatch, capsys, tmp_path):
    settings = tmp_path / "settings.json"
    settings.write_text("{}", encoding="utf-8")
    original_read_text = Path.read_text

    def fail_settings_read(path, *args, **kwargs):
        if path == settings:
            raise PermissionError("denied")
        return original_read_text(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", fail_settings_read)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_settings_paths",
        lambda _cwd: [settings],
    )

    disabled = skill_ledger_hook._read_disabled_skill_names(
        str(tmp_path), _event(cwd=tmp_path), "test-skill"
    )

    assert disabled is None
    assert '"code":"skill_visibility_unknown"' in capsys.readouterr().err


def test_inaccessible_settings_path_is_visibility_unknown(
    monkeypatch, capsys, tmp_path
):
    settings = tmp_path / "settings.json"
    settings.write_text("{}", encoding="utf-8")
    original_stat = Path.stat

    def fail_settings_stat(path, *args, **kwargs):
        if path == settings:
            raise PermissionError("denied")
        return original_stat(path, *args, **kwargs)

    monkeypatch.setattr(Path, "stat", fail_settings_stat)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_settings_paths",
        lambda _cwd: [settings],
    )

    disabled = skill_ledger_hook._read_disabled_skill_names(
        str(tmp_path), _event(cwd=tmp_path), "test-skill"
    )

    assert disabled is None
    assert '"code":"skill_visibility_unknown"' in capsys.readouterr().err


@pytest.mark.parametrize(
    ("visibility", "expected_code"),
    (
        ("frontmatter", "model_invocation_disabled"),
        ("settings", "skill_disabled_by_settings"),
        ("unknown", "skill_visibility_unknown"),
    ),
)
def test_non_model_invocable_candidates_skip_ledger(
    monkeypatch, capsys, tmp_path, visibility, expected_code
):
    root = tmp_path / "skills"
    _create_skill(
        root,
        disable_model_invocation="true" if visibility == "frontmatter" else None,
    )
    monkeypatch.setenv("SKILL_LEDGER_MODE", "block")
    monkeypatch.setattr(
        skill_ledger_hook,
        "_supported_skill_bases",
        lambda _cwd: [root],
    )
    if visibility == "frontmatter":
        monkeypatch.setattr(
            skill_ledger_hook,
            "_read_disabled_skill_names",
            lambda *_args: (_ for _ in ()).throw(
                AssertionError("unexpected settings read")
            ),
        )
    elif visibility == "settings":
        monkeypatch.setattr(
            skill_ledger_hook,
            "_read_disabled_skill_names",
            lambda *_args: frozenset({"test-skill"}),
        )
    else:

        def unknown_settings(_cwd, input_data, skill_name):
            skill_ledger_hook._diagnostic(
                "skill_visibility_unknown", input_data, skill_name=skill_name
            )
            return None

        monkeypatch.setattr(
            skill_ledger_hook,
            "_read_disabled_skill_names",
            unknown_settings,
        )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_ensure_keys",
        lambda *_args: (_ for _ in ()).throw(AssertionError("unexpected init")),
    )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_show_skill",
        lambda *_args: (_ for _ in ()).throw(AssertionError("unexpected show")),
    )

    output, stderr = _run_main(monkeypatch, capsys, _event(cwd=tmp_path))

    assert output == {}
    assert f'"code":"{expected_code}"' in stderr


def test_main_calls_show_for_project_candidate_without_falling_back(
    monkeypatch, capsys, tmp_path
):
    project_root = tmp_path / "project-skills"
    project_skill = _create_skill(project_root)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_supported_skill_bases",
        lambda _cwd: [project_root],
    )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_read_disabled_skill_names",
        lambda *_args: frozenset(),
    )
    monkeypatch.setattr(skill_ledger_hook, "_ensure_keys", lambda *_args: None)
    captured = {}

    def fake_show(skill_dir, input_data, skill_name):
        captured.update(
            skill_dir=skill_dir,
            input_data=input_data,
            skill_name=skill_name,
        )
        return {"managed": False, "latestStatus": "unmanaged", "message": None}

    monkeypatch.setattr(skill_ledger_hook, "_show_skill", fake_show)

    output, stderr = _run_main(monkeypatch, capsys, _event(cwd=tmp_path))

    assert output == {}
    assert captured["skill_dir"] == project_skill.resolve()
    assert captured["skill_name"] == "test-skill"
    assert '"code":"unmanaged"' in stderr


def test_main_defaults_to_ask_for_managed_risk(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    tmp_path: Path,
) -> None:
    monkeypatch.delenv("SKILL_LEDGER_MODE", raising=False)
    skill_root = tmp_path / "skills"
    _create_skill(skill_root)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_supported_skill_bases",
        lambda _cwd: [skill_root],
    )
    monkeypatch.setattr(
        skill_ledger_hook,
        "_read_disabled_skill_names",
        lambda *_args: frozenset(),
    )
    monkeypatch.setattr(skill_ledger_hook, "_ensure_keys", lambda *_args: None)
    monkeypatch.setattr(
        skill_ledger_hook,
        "_show_skill",
        lambda *_args: {
            "latestStatus": "deny",
            "message": "review required",
        },
    )

    output, _ = _run_main(monkeypatch, capsys, _event(cwd=tmp_path))

    specific = output["hookSpecificOutput"]
    assert specific["hookEventName"] == "PreToolUse"
    assert specific["permissionDecision"] == "ask"
    assert "review required" in specific["permissionDecisionReason"]


@pytest.mark.parametrize("status", ("pass", "warn"))
@pytest.mark.parametrize("policy", ("debug", "warn", "ask", "block"))
def test_trusted_null_message_never_overrides_permission(status, policy, monkeypatch):
    monkeypatch.setenv("SKILL_LEDGER_MODE", policy)
    summary = {"latestStatus": status, "message": None}

    output = skill_ledger_hook._format_qwen(summary, "test-skill", policy, _event())

    assert json.loads(output) == {}


@pytest.mark.parametrize("status", ("none", "drifted", "deny", "tampered"))
@pytest.mark.parametrize(
    ("policy", "expected"),
    (
        ("debug", "noop"),
        ("warn", "warn"),
        ("ask", "ask"),
        ("block", "deny"),
    ),
)
def test_exposure_message_uses_policy(status, policy, expected, capsys):
    output = json.loads(
        skill_ledger_hook._format_qwen(
            {
                "latestStatus": status,
                "message": "review required",
            },
            "test-skill",
            policy,
            _event(),
        )
    )

    if expected == "noop":
        assert output == {}
        assert '"code":"exposure_warning"' in capsys.readouterr().err
    elif expected == "warn":
        assert output["systemMessage"].startswith(f"Skill Ledger [{status}]")
        assert "hookSpecificOutput" not in output
    else:
        specific = output["hookSpecificOutput"]
        assert specific["hookEventName"] == "PreToolUse"
        assert specific["permissionDecision"] == expected
        assert "review required" in specific["permissionDecisionReason"]


@pytest.mark.parametrize("policy", ("debug", "warn", "ask", "block"))
def test_unmanaged_is_always_fail_open(policy, capsys):
    output = skill_ledger_hook._format_qwen(
        {"managed": False, "latestStatus": "unmanaged", "message": None},
        "test-skill",
        policy,
        _event(),
    )

    assert json.loads(output) == {}
    assert '"code":"unmanaged"' in capsys.readouterr().err


def test_prior_user_decision_null_message_is_not_blocked():
    output = skill_ledger_hook._format_qwen(
        {
            "managed": True,
            "latestStatus": "deny",
            "message": None,
            "userDecision": {"action": "allow"},
        },
        "test-skill",
        "block",
        _event(),
    )

    assert json.loads(output) == {}


@pytest.mark.parametrize(
    "summary",
    (
        {"managed": True, "message": "warning"},
        {"managed": True, "latestStatus": "unknown", "message": "warning"},
        {"managed": True, "latestStatus": "deny"},
        {"managed": True, "latestStatus": "deny", "message": []},
        {
            "managed": "true",
            "latestStatus": "deny",
            "message": "warning",
        },
    ),
)
def test_incomplete_or_unknown_summary_is_fail_open(summary):
    output = skill_ledger_hook._format_qwen(summary, "test-skill", "block", _event())

    assert json.loads(output) == {}


def test_trace_context_prefers_tool_call_id(monkeypatch):
    calls = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(
                {"managed": True, "latestStatus": "pass", "message": None}
            ),
            stderr="",
        )

    monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
    payload = _event()

    summary = skill_ledger_hook._show_skill(
        Path("/resolved/skill"), payload, "test-skill"
    )

    assert summary["latestStatus"] == "pass"
    command = calls[0][0]
    context = json.loads(command[2])
    assert command[0:2] == ["agent-sec-cli", "--trace-context"]
    assert command[3:] == [
        "skill-ledger",
        "show",
        "/resolved/skill",
    ]
    assert context == {
        "agent_name": "qwen-code",
        "session_id": "session-1",
        "tool_call_id": "tool-call-1",
    }


def test_trace_context_falls_back_to_tool_use_id():
    payload = _event()
    payload.pop("tool_call_id")

    context = skill_ledger_hook.trace_context(payload)

    assert context["tool_call_id"] == "tool-use-1"


def test_missing_policy_defaults_to_ask(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("SKILL_LEDGER_MODE", raising=False)

    assert skill_ledger_hook._read_policy(_event()) == "ask"


def test_invalid_mode_defaults_to_ask(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("SKILL_LEDGER_MODE", "invalid")

    policy = skill_ledger_hook._read_policy(_event())

    assert policy == "ask"
    diagnostic = capsys.readouterr().err
    assert '"code":"invalid_policy"' in diagnostic
    assert "invalid SKILL_LEDGER_MODE; using ask" in diagnostic


def test_missing_keys_trigger_best_effort_init(monkeypatch, tmp_path):
    calls = []
    monkeypatch.setattr(skill_ledger_hook, "_keys_exist", lambda: False)

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)

    skill_ledger_hook._ensure_keys(_event(cwd=tmp_path), "test-skill")

    assert calls[0][0][3:] == ["skill-ledger", "init", "--no-baseline"]
    assert calls[0][1]["timeout"] == skill_ledger_hook._INIT_TIMEOUT_SECONDS


@pytest.mark.parametrize(
    "result",
    (
        SimpleNamespace(returncode=1, stdout="", stderr="failed"),
        SimpleNamespace(returncode=0, stdout="", stderr=""),
        SimpleNamespace(returncode=0, stdout="[]", stderr=""),
        SimpleNamespace(returncode=0, stdout="not-json", stderr=""),
    ),
)
def test_show_cli_failures_are_fail_open(monkeypatch, result):
    monkeypatch.setattr(
        skill_ledger_hook.subprocess,
        "run",
        lambda *_args, **_kwargs: result,
    )

    assert skill_ledger_hook._show_skill(Path("/skill"), _event(), "test-skill") is None


def test_show_process_exception_is_fail_open(monkeypatch):
    monkeypatch.setattr(
        skill_ledger_hook.subprocess,
        "run",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(FileNotFoundError()),
    )

    assert skill_ledger_hook._show_skill(Path("/skill"), _event(), "test-skill") is None
