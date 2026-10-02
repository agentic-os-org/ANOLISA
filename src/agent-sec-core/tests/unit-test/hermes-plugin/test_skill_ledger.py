"""Unit tests for the Hermes Skill Ledger capability."""

from __future__ import annotations

import json
import tomllib
from pathlib import Path
from unittest.mock import patch

import pytest
from hermes_plugin_src.capabilities.skill_ledger import SkillLedgerCapability
from hermes_plugin_src.cli_runner import CliResult
from hermes_plugin_src.registry import load_config, register_capabilities

_HERMES_PLUGIN_DIR = Path(__file__).resolve().parents[3] / "hermes-plugin"
_DEFAULT_MESSAGE = object()


def _make_capability(
    root: Path,
    *,
    policy: str = "observe",
    include_policy: bool = True,
    enable_block: bool | None = None,
) -> SkillLedgerCapability:
    cap = SkillLedgerCapability()
    cap._timeout = 5.0
    config: dict = {}
    if include_policy:
        config["policy"] = policy
    if enable_block is not None:
        config["enable_block"] = enable_block
    cap._on_register(config)
    cap._skills_dir = root
    return cap


def _make_skill(
    root: Path,
    rel: str,
    *,
    frontmatter_name: str | None = None,
) -> Path:
    skill_dir = root / rel
    skill_dir.mkdir(parents=True, exist_ok=True)
    name = frontmatter_name or skill_dir.name
    (skill_dir / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: Test skill\n---\nBody\n",
        encoding="utf-8",
    )
    return skill_dir


def _cli_status(
    status: str,
    *,
    exit_code: int = 0,
    message: str | None | object = _DEFAULT_MESSAGE,
    reason_code: str | None = None,
) -> CliResult:
    if message is _DEFAULT_MESSAGE:
        message = None if status == "pass" else f"summary message for status={status}"
    if not isinstance(message, str):
        message = None
    payload = {
        "latestStatus": status,
        "skillName": "test-skill",
        "message": message,
    }
    if reason_code is not None:
        payload["reasonCode"] = reason_code
    return CliResult(
        stdout=json.dumps(payload),
        stderr="",
        exit_code=exit_code,
    )


class _RecordingHermesContext:
    def __init__(self) -> None:
        self.hooks: list[str] = []

    def register_hook(self, hook_name, _callback):
        self.hooks.append(hook_name)


def test_default_config_uses_observe_policy():
    config = tomllib.loads(
        (_HERMES_PLUGIN_DIR / "src" / "config.toml").read_text(encoding="utf-8")
    )

    assert config["capabilities"]["skill-ledger"] == {
        "enabled": True,
        "timeout": 5,
        "policy": "observe",
    }


def test_default_config_registers_only_pre_tool_hook():
    ctx = _RecordingHermesContext()
    config = load_config(_HERMES_PLUGIN_DIR / "src")

    register_capabilities(ctx, [SkillLedgerCapability()], config)

    assert ctx.hooks == ["pre_tool_call"]


def test_environment_enabled_overrides_disabled_capability(monkeypatch):
    ctx = _RecordingHermesContext()
    config = load_config(_HERMES_PLUGIN_DIR / "src")
    config["capabilities"]["skill-ledger"]["enabled"] = False
    monkeypatch.setenv("SKILL_LEDGER_HOOK_ENABLED", "true")

    register_capabilities(ctx, [SkillLedgerCapability()], config)

    assert ctx.hooks == ["pre_tool_call"]


@pytest.mark.parametrize("policy", ["warn", "ask", "invalid"])
def test_unsupported_policies_fall_back_to_observe(tmp_path, caplog, policy):
    with caplog.at_level("WARNING", logger="agent-sec-core"):
        cap = _make_capability(tmp_path, policy=policy)

    assert cap._policy == "observe"
    assert "does not support capability policy" in caplog.text
    assert "using observe" in caplog.text


@pytest.mark.parametrize(
    ("raw_policy", "expected"),
    [
        ("observe", "observe"),
        ("block", "block"),
        ("debug", "observe"),
        ("deny", "block"),
    ],
)
def test_native_policies_and_aliases(tmp_path, raw_policy, expected):
    assert _make_capability(tmp_path, policy=raw_policy)._policy == expected


def test_environment_policy_overrides_capability_policy(monkeypatch, tmp_path):
    monkeypatch.setenv("SKILL_LEDGER_MODE", "block")

    capability = _make_capability(tmp_path, policy="observe")

    assert capability._policy == "block"


@pytest.mark.parametrize(
    ("enable_block", "expected"),
    [(False, "observe"), (True, "block")],
)
def test_legacy_enable_block_maps_to_native_policy(tmp_path, enable_block, expected):
    cap = _make_capability(
        tmp_path,
        include_policy=False,
        enable_block=enable_block,
    )

    assert cap._policy == expected


def test_removed_warning_configs_are_ignored_with_diagnostic(tmp_path, caplog):
    cap = SkillLedgerCapability()
    cap._timeout = 5.0

    with caplog.at_level("WARNING", logger="agent-sec-core"):
        cap._on_register(
            {
                "policy": "observe",
                "max_warnings_per_turn": 5,
                "max_warning_contexts": 128,
            }
        )

    assert "max_warnings_per_turn, max_warning_contexts ignored" in caplog.text


@patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
def test_hook_disabled_short_circuits_before_resolving(mock_cli, monkeypatch, tmp_path):
    monkeypatch.setenv("SKILL_LEDGER_HOOK_ENABLED", "false")
    capability = _make_capability(tmp_path)

    with patch.object(capability, "_resolved_skills_dir") as resolve_root:
        result = capability._on_pre_tool_call("skill_view", object())

    assert result is None
    resolve_root.assert_not_called()
    mock_cli.assert_not_called()


class TestSkillLedgerHooks:
    @pytest.mark.parametrize(
        ("status", "reason_code", "expected_level"),
        [
            ("none", "latest_unscanned", "INFO"),
            ("drifted", "root_drift", "INFO"),
            ("warn", "normal", "INFO"),
            ("deny", "latest_risk_pending_decision", "WARNING"),
            ("tampered", "tampered", "WARNING"),
            ("pass", "tampered", "WARNING"),
            ("warn", "tampered", "WARNING"),
        ],
    )
    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_observe_logs_by_risk_and_allows_without_user_content(
        self, mock_cli, status, reason_code, expected_level, tmp_path, caplog
    ):
        root = tmp_path / "skills"
        _make_skill(root, "test-skill")
        cap = _make_capability(root, policy="observe")
        mock_cli.return_value = _cli_status(
            status,
            reason_code=reason_code,
            message=f"summary message for status={status}",
        )

        with caplog.at_level("INFO", logger="agent-sec-core"):
            result = cap._on_pre_tool_call("skill_view", {"name": "test-skill"})

        assert result is None
        assert f"status={status}" in caplog.text
        matching_records = [
            record
            for record in caplog.records
            if f"status={status}" in record.getMessage()
        ]
        assert [record.levelname for record in matching_records] == [expected_level]
        assert "transform_llm_output" not in cap.get_hooks_define()

    @pytest.mark.parametrize("status", ["none", "drifted", "warn", "deny", "tampered"])
    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_block_policy_uses_native_pre_tool_action(self, mock_cli, status, tmp_path):
        root = tmp_path / "skills"
        _make_skill(root, "test-skill")
        cap = _make_capability(root, policy="block")
        mock_cli.return_value = _cli_status(status)

        result = cap._on_pre_tool_call("skill_view", {"name": "test-skill"})

        assert result == {
            "action": "block",
            "message": f"Skill 'test-skill': summary message for status={status}",
        }

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_pass_or_message_less_summary_allows(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        _make_skill(root, "test-skill")
        cap = _make_capability(root, policy="block")
        mock_cli.side_effect = [
            _cli_status("pass"),
            _cli_status("unmanaged", message=None),
        ]

        assert cap._on_pre_tool_call("skill_view", {"name": "test-skill"}) is None
        assert cap._on_pre_tool_call("skill_view", {"name": "test-skill"}) is None

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_nonzero_exit_with_valid_json_still_blocks(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        _make_skill(root, "test-skill")
        cap = _make_capability(root, policy="block")
        mock_cli.return_value = _cli_status("tampered", exit_code=1)

        result = cap._on_pre_tool_call("skill_view", {"name": "test-skill"})

        assert result is not None
        assert result["action"] == "block"

    @pytest.mark.parametrize(
        "cli_result",
        [
            CliResult(stdout="", stderr="boom", exit_code=1),
            CliResult(stdout="not-json", stderr="", exit_code=0),
            CliResult(stdout="[]", stderr="", exit_code=0),
        ],
    )
    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_cli_failures_fail_open(self, mock_cli, cli_result, tmp_path):
        root = tmp_path / "skills"
        _make_skill(root, "test-skill")
        cap = _make_capability(root, policy="block")
        mock_cli.return_value = cli_result

        assert cap._on_pre_tool_call("skill_view", {"name": "test-skill"}) is None

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_passes_hermes_trace_context_to_cli(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        skill_dir = _make_skill(root, "test-skill")
        cap = _make_capability(root)
        mock_cli.return_value = _cli_status("pass")

        cap._on_pre_tool_call(
            "skill_view",
            {"name": "test-skill"},
            session_id="session-1",
        )

        mock_cli.assert_called_once_with(
            ["skill-ledger", "show", str(skill_dir.resolve())],
            timeout=5.0,
            trace_context={"agent_name": "hermes", "session_id": "session-1"},
        )

    @pytest.mark.parametrize("sentinel", [".skillfs-inbox", "skill-discover/SKILL.md"])
    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_skillfs_inplace_sentinel_does_not_bypass_ledger(
        self, mock_cli, sentinel, tmp_path
    ):
        root = tmp_path / "skills"
        skill_dir = _make_skill(root, "devops/risky")
        sentinel_path = root / sentinel
        if sentinel_path.name == "SKILL.md":
            sentinel_path.parent.mkdir(parents=True)
            sentinel_path.write_text(
                "---\nname: skill-discover\n---\n", encoding="utf-8"
            )
        else:
            sentinel_path.mkdir(parents=True)
        cap = _make_capability(root)
        mock_cli.return_value = _cli_status("pass")

        cap._on_pre_tool_call("skill_view", {"name": "devops/risky"})

        assert mock_cli.call_args.args[0][-1] == str(skill_dir.resolve())


class TestSkillResolution:
    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_qualified_name_is_forwarded_as_canonical_path(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        root.mkdir()
        cap = _make_capability(root)
        mock_cli.return_value = _cli_status("pass")

        cap._on_pre_tool_call("skill_view", {"name": "apple/apple-notes"})

        assert mock_cli.call_args.args[0][-1] == str(root / "apple" / "apple-notes")

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_resolves_by_category_name(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        skill_dir = _make_skill(root, "mlops/axolotl")
        cap = _make_capability(root)
        mock_cli.return_value = _cli_status("pass")

        cap._on_pre_tool_call("skill_view", {"name": "mlops/axolotl"})

        assert mock_cli.call_args.args[0][-1] == str(skill_dir.resolve())

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_symlink_skills_root_preserves_canonical_path(self, mock_cli, tmp_path):
        physical_root = tmp_path / "physical-skills"
        _make_skill(physical_root, "mlops/axolotl")
        canonical_root = tmp_path / "skills"
        canonical_root.symlink_to(physical_root, target_is_directory=True)
        cap = _make_capability(canonical_root)
        mock_cli.return_value = _cli_status("pass")

        cap._on_pre_tool_call("skill_view", {"name": "mlops/axolotl"})

        canonical_skill_dir = canonical_root / "mlops" / "axolotl"
        assert mock_cli.call_args.args[0][-1] == str(canonical_skill_dir)
        assert canonical_skill_dir != canonical_skill_dir.resolve()

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_frontmatter_name_is_not_used_for_resolution(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        _make_skill(root, "directory-name", frontmatter_name="frontmatter-name")
        cap = _make_capability(root)

        cap._on_pre_tool_call("skill_view", {"skill_name": "frontmatter-name"})

        mock_cli.assert_not_called()

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_supporting_file_path_does_not_override_name(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        skill_dir = _make_skill(root, "tools/name-wins")
        other_dir = _make_skill(root, "tools/ignored-path")
        cap = _make_capability(root)
        mock_cli.return_value = _cli_status("pass")

        cap._on_pre_tool_call(
            "skill_view",
            {"name": "name-wins", "file_path": str(other_dir / "SKILL.md")},
        )

        assert mock_cli.call_args.args[0][-1] == str(skill_dir.resolve())

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_file_path_without_name_fails_open(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        _make_skill(root, "tools/relative")
        cap = _make_capability(root)

        result = cap._on_pre_tool_call("skill_view", {"file_path": "SKILL.md"})

        assert result is None
        mock_cli.assert_not_called()

    @pytest.mark.parametrize("name", ["hidden", ".archive/hidden", "plugin:skill"])
    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_unsupported_or_internal_names_are_not_resolved(
        self, mock_cli, name, tmp_path
    ):
        root = tmp_path / "skills"
        _make_skill(root, ".archive/hidden")
        _make_skill(root, "plugin/skill")
        cap = _make_capability(root)

        assert cap._on_pre_tool_call("skill_view", {"name": name}) is None
        mock_cli.assert_not_called()

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_qualified_symlink_escape_is_not_resolved(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        outside = tmp_path / "outside"
        root.mkdir()
        outside.mkdir()
        (root / "linked").symlink_to(outside, target_is_directory=True)
        cap = _make_capability(root)

        result = cap._on_pre_tool_call("skill_view", {"name": "linked/hidden"})

        assert result is None
        mock_cli.assert_not_called()

    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_ambiguous_bare_name_fails_open_without_cli(self, mock_cli, tmp_path):
        root = tmp_path / "skills"
        _make_skill(root, "devops/duplicate")
        _make_skill(root, "security/duplicate")
        cap = _make_capability(root)

        result = cap._on_pre_tool_call("skill_view", {"name": "duplicate"})

        assert result is None
        mock_cli.assert_not_called()

    @patch("hermes_plugin_src.capabilities.skill_ledger.Path.rglob")
    @patch("hermes_plugin_src.capabilities.skill_ledger.call_agent_sec_cli")
    def test_skill_file_traversal_error_fails_open(
        self, mock_cli, mock_rglob, tmp_path
    ):
        root = tmp_path / "skills"
        root.mkdir()
        cap = _make_capability(root)
        mock_rglob.side_effect = OSError("File system loop detected")

        assert cap._on_pre_tool_call("skill_view", {"name": "risky"}) is None
        mock_cli.assert_not_called()
