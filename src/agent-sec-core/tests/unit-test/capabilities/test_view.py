"""Tests for the agent capability environment view."""

import importlib.util
import json
import os
import sys
from pathlib import Path
from types import ModuleType

import pytest
from agent_sec_cli.capabilities import view as capability_view
from agent_sec_cli.capabilities.view import (
    AGENTS,
    CapabilityViewError,
    query_capabilities,
    render_json,
    render_table,
)
from agent_sec_cli.prompt_scanner.cli import _resolve_l2_model
from standalone_hook_test_loader import (
    load_package_from_path,
    load_standalone_hook,
)

_SEC_CORE_ROOT = Path(__file__).resolve().parents[3]
_L2_MODEL_ENV = "PROMPT_SCANNER_L2_MODEL"
_QWEN3_GUARD = "modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF"
_WARDEN_GEN = "modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF"
_STUB_ENGINE_BACKENDS = (
    _QWEN3_GUARD,
    frozenset({_QWEN3_GUARD.lower(), _WARDEN_GEN.lower()}),
)


@pytest.fixture
def stub_engine_backends(monkeypatch: pytest.MonkeyPatch) -> None:
    """Pin the engine-derived L2 metadata so cases do not need the extension."""
    monkeypatch.setattr(
        capability_view, "_engine_l2_backends", lambda: _STUB_ENGINE_BACKENDS
    )


_PYTHON_HOOK_HELPERS = [
    ("qoder", _SEC_CORE_ROOT / "qoder-plugin" / "hooks" / "qoder_hook_common.py"),
    ("qwen", _SEC_CORE_ROOT / "qwen-code-extension" / "hooks" / "hook_config.py"),
    (
        "codex",
        _SEC_CORE_ROOT / "codex-plugin" / "hooks-plugin" / "hooks" / "hook_config.py",
    ),
    ("cosh", _SEC_CORE_ROOT / "cosh-extension" / "hooks" / "hook_config.py"),
    ("hermes", _SEC_CORE_ROOT / "hermes-plugin" / "src" / "hook_config.py"),
]

_PYTHON_OBSERVABILITY_HOOKS = [
    ("qoder", _SEC_CORE_ROOT / "qoder-plugin" / "hooks" / "observability_hook.py"),
    (
        "qwen",
        _SEC_CORE_ROOT / "qwen-code-extension" / "hooks" / "observability_hook.py",
    ),
    (
        "codex",
        _SEC_CORE_ROOT
        / "codex-plugin"
        / "hooks-plugin"
        / "hooks"
        / "observability_hook.py",
    ),
    ("cosh", _SEC_CORE_ROOT / "cosh-extension" / "hooks" / "observability_hook.py"),
]


def _load_module(path: Path, name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"cannot load module from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.path.insert(0, str(path.parent))
    try:
        spec.loader.exec_module(module)
    finally:
        sys.path.pop(0)
    return module


def _load_qwen_pii_module() -> ModuleType:
    hook_dir = _SEC_CORE_ROOT / "qwen-code-extension" / "hooks"
    for name in ("hook_config", "pii_text", "trace_context"):
        sys.modules.pop(name, None)
    return _load_module(
        hook_dir / "pii_checker_hook.py", "qwen_pii_checker_hook_for_caps"
    )


def _table_section(table: str, agent: str) -> list[str]:
    lines = table.splitlines()
    start = lines.index(f"[{agent}]")
    end = len(lines)
    for index in range(start + 1, len(lines)):
        if lines[index] == "":
            end = index
            break
    return lines[start:end]


def _section_row(table: str, agent: str, capability: str) -> str:
    for line in _table_section(table, agent):
        if line.startswith(capability):
            return line
    raise AssertionError(f"missing row for {agent}/{capability}")


def _single_record(agent: str, capability: str, env: dict[str, str] | None = None):
    records = query_capabilities(agent=agent, capability=capability, env=env or {})
    assert len(records) == 1
    return records[0]


def test_query_all_returns_six_agents_and_five_capabilities() -> None:
    records = query_capabilities(env={})

    assert len(records) == 30
    assert {record.agent for record in records} == {
        "qoder",
        "qwen",
        "codex",
        "cosh",
        "openclaw",
        "hermes",
    }
    assert {record.capability for record in records} == {
        "code-scan",
        "prompt-scan",
        "pii-check",
        "skill-ledger",
        "observability",
    }


@pytest.mark.parametrize(
    "capability",
    ["scan-code", "prompt-scan-user-input", "pii-scan-user-input", "code_scanner"],
)
def test_capability_filter_rejects_aliases(capability: str) -> None:
    with pytest.raises(CapabilityViewError, match="unknown capability"):
        query_capabilities(capability=capability, env={})


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        (None, True),
        ("true", True),
        ("TRUE", True),
        (" true ", True),
        ("false", False),
        ("FALSE", False),
        (" false ", False),
        ("0", True),
        ("no", True),
        ("off", True),
        ("invalid", True),
    ],
)
@pytest.mark.parametrize(("agent", "helper_path"), _PYTHON_HOOK_HELPERS)
def test_cli_strict_enabled_matches_python_hook_helpers(
    monkeypatch: pytest.MonkeyPatch,
    agent: str,
    helper_path: Path,
    raw: str | None,
    expected: bool,
) -> None:
    env_name = "OBSERVABILITY_HOOK_ENABLED"
    helper = _load_module(helper_path, f"{agent}_hook_config_for_caps")
    if raw is None:
        monkeypatch.delenv(env_name, raising=False)
        env = {}
    else:
        monkeypatch.setenv(env_name, raw)
        env = {env_name: raw}

    hook_enabled = helper.env_flag_enabled(env_name, True)
    record = _single_record(agent, "observability", env)

    assert hook_enabled is expected
    assert record.env[env_name]["effective"] is expected
    assert record.effective == ("enabled" if expected else "disabled")
    has_diagnostic = any(env_name in item for item in record.diagnostics)
    assert has_diagnostic is (
        raw not in {None, "true", "TRUE", " true ", "false", "FALSE", " false "}
    )


def test_static_agent_code_scanner_enabled_env_controls_effective() -> None:
    record = _single_record(
        "qoder",
        "code-scan",
        {"CODE_SCANNER_HOOK_ENABLED": "false"},
    )

    assert record.effective == "disabled"
    assert record.mode == "observe"
    assert record.scan_mode == "-"
    assert record.timeout == "10"
    assert record.env["CODE_SCANNER_HOOK_ENABLED"]["effective"] is False


def test_invalid_env_values_fall_back_with_diagnostic() -> None:
    record = _single_record(
        "qwen",
        "prompt-scan",
        {"PROMPT_SCANNER_MODE": "block", "PROMPT_SCANNER_SCAN_MODE": "fast"},
    )

    assert record.effective == "enabled"
    assert record.mode == "observe"
    assert record.scan_mode == "fast"
    assert record.timeout == "10"
    assert record.env["PROMPT_SCANNER_MODE"]["effective"] == "observe"
    assert record.env["PROMPT_SCANNER_SCAN_MODE"]["effective"] == "fast"
    assert any("PROMPT_SCANNER_MODE" in item for item in record.diagnostics)


def test_invalid_timeout_values_fall_back_with_diagnostic() -> None:
    record = _single_record(
        "qwen",
        "pii-check",
        {"PII_CHECKER_TIMEOUT": "nan"},
    )

    assert record.timeout == "5"
    assert record.env["PII_CHECKER_TIMEOUT"]["effective"] == "5"
    assert any("PII_CHECKER_TIMEOUT" in item for item in record.diagnostics)


@pytest.mark.parametrize(("value", "expected"), [("0", "0"), ("-1", "-1")])
def test_non_positive_int_timeout_matches_hook_runtime(
    value: str, expected: str
) -> None:
    record = _single_record(
        "qoder",
        "code-scan",
        {"CODE_SCANNER_TIMEOUT": value},
    )

    assert record.timeout == expected
    assert any("may fail open" in item for item in record.diagnostics)


@pytest.mark.parametrize("value", ["inf", "bad", "1.5"])
def test_invalid_int_timeout_values_fall_back(value: str) -> None:
    record = _single_record(
        "qoder",
        "code-scan",
        {"CODE_SCANNER_TIMEOUT": value},
    )

    assert record.timeout == "10"
    assert any("CODE_SCANNER_TIMEOUT" in item for item in record.diagnostics)


def test_timeout_parsing_matches_agent_specific_runtime_rules() -> None:
    qoder_skill = _single_record(
        "qoder",
        "skill-ledger",
        {"SKILL_LEDGER_TIMEOUT": "0.05"},
    )
    qwen_pii = _single_record(
        "qwen",
        "pii-check",
        {"PII_CHECKER_TIMEOUT": "99"},
    )
    qoder_code = _single_record(
        "qoder",
        "code-scan",
        {"CODE_SCANNER_TIMEOUT": "1.5"},
    )
    qwen_skill = _single_record(
        "qwen",
        "skill-ledger",
        {"SKILL_LEDGER_TIMEOUT": "99"},
    )

    assert qoder_skill.timeout == "0.05"
    assert qwen_pii.timeout == "8"
    assert qwen_pii.env["PII_CHECKER_TIMEOUT"]["effective"] == "8"
    assert qoder_code.timeout == "10"
    assert any("CODE_SCANNER_TIMEOUT" in item for item in qoder_code.diagnostics)
    assert qwen_skill.timeout == "5"
    assert "SKILL_LEDGER_TIMEOUT" not in qwen_skill.env


@pytest.mark.parametrize(
    ("value", "expected", "has_diagnostic"),
    [
        (None, "5", False),
        ("", "5", True),
        ("invalid", "5", True),
        ("1.5", "5", True),
        ("0", "5", True),
        ("-1", "5", True),
        ("3", "3", False),
        ("5", "5", False),
        ("7", "5", False),
        ("999999", "5", False),
    ],
)
@pytest.mark.parametrize(
    "agent", ("qoder", "qwen", "codex", "cosh", "openclaw", "hermes")
)
def test_observability_timeout_matches_shared_runtime_rules(
    agent: str,
    value: str | None,
    expected: str,
    has_diagnostic: bool,
) -> None:
    env = {} if value is None else {"OBSERVABILITY_TIMEOUT": value}
    record = _single_record(agent, "observability", env)

    assert record.timeout == expected
    assert record.env["OBSERVABILITY_TIMEOUT"] == {
        "raw": value,
        "effective": expected,
        "default": "5",
    }
    assert any("OBSERVABILITY_TIMEOUT" in item for item in record.diagnostics) is (
        has_diagnostic
    )


@pytest.mark.parametrize(("agent", "hook_path"), _PYTHON_OBSERVABILITY_HOOKS)
def test_observability_timeout_matches_python_hook_runtime(
    monkeypatch: pytest.MonkeyPatch,
    agent: str,
    hook_path: Path,
) -> None:
    hook = load_standalone_hook(f"{agent}_observability_hook_for_caps", hook_path)

    for value in (None, "", "invalid", "1.5", "0", "-1", "3", "5", "7", "999999"):
        if value is None:
            monkeypatch.delenv("OBSERVABILITY_TIMEOUT", raising=False)
            env = {}
        else:
            monkeypatch.setenv("OBSERVABILITY_TIMEOUT", value)
            env = {"OBSERVABILITY_TIMEOUT": value}

        hook_timeout = hook._read_cli_timeout_seconds()
        record = _single_record(agent, "observability", env)

        assert record.timeout == str(hook_timeout)


def test_observability_timeout_matches_hermes_runtime(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    package_name = "hermes_plugin_for_capability_view"
    load_package_from_path(package_name, _SEC_CORE_ROOT / "hermes-plugin" / "src")
    observability = sys.modules[f"{package_name}.capabilities.observability"]

    for value in (None, "", "invalid", "1.5", "0", "-1", "3", "5", "7", "999999"):
        if value is None:
            monkeypatch.delenv("OBSERVABILITY_TIMEOUT", raising=False)
            env = {}
        else:
            monkeypatch.setenv("OBSERVABILITY_TIMEOUT", value)
            env = {"OBSERVABILITY_TIMEOUT": value}

        hook_timeout = observability._read_observability_timeout(5.0)
        record = _single_record("hermes", "observability", env)

        assert float(record.timeout) == hook_timeout


def test_static_timeout_defaults_match_runtime_constants() -> None:
    for agent in ("qoder", "qwen", "codex", "cosh", "openclaw", "hermes"):
        assert _single_record(agent, "observability").timeout == "5"
    assert _single_record("cosh", "code-scan").timeout == "10"
    assert _single_record("hermes", "prompt-scan").timeout == "15"


def test_hermes_hooks_do_not_advertise_synthetic_warning_delivery() -> None:
    prompt = _single_record("hermes", "prompt-scan")
    pii = _single_record("hermes", "pii-check")
    skill = _single_record("hermes", "skill-ledger")

    assert prompt.hooks == ["pre_llm_call"]
    assert pii.hooks == [
        "pre_llm_call",
        "pre_tool_call",
        "post_tool_call",
        "post_llm_call",
    ]
    assert skill.hooks == ["pre_tool_call"]
    assert skill.mode == "observe"


@pytest.mark.parametrize("capability", ["pii-check", "skill-ledger"])
@pytest.mark.parametrize("mode", ["warn", "ask"])
def test_hermes_unsupported_advisory_modes_fall_back_to_observe(
    capability: str,
    mode: str,
) -> None:
    env_name = "PII_CHECKER_MODE" if capability == "pii-check" else "SKILL_LEDGER_MODE"

    record = _single_record("hermes", capability, {env_name: mode})

    assert record.mode == "observe"
    assert record.env[env_name]["effective"] == "observe"
    assert any(env_name in diagnostic for diagnostic in record.diagnostics)


def test_pii_include_low_confidence_is_only_exposed_for_supported_hooks() -> None:
    env = {"PII_CHECKER_INCLUDE_LOW_CONFIDENCE": "true"}
    qoder = _single_record("qoder", "pii-check", env)
    qwen = _single_record("qwen", "pii-check", env)
    codex = _single_record("codex", "pii-check", env)
    openclaw = _single_record("openclaw", "pii-check", env)
    hermes = _single_record("hermes", "pii-check", env)

    assert qoder.env["PII_CHECKER_INCLUDE_LOW_CONFIDENCE"]["effective"] is True
    assert qwen.env["PII_CHECKER_INCLUDE_LOW_CONFIDENCE"]["effective"] is True
    assert "PII_CHECKER_INCLUDE_LOW_CONFIDENCE" not in codex.env
    assert "PII_CHECKER_INCLUDE_LOW_CONFIDENCE" not in openclaw.env
    assert "PII_CHECKER_INCLUDE_LOW_CONFIDENCE" not in hermes.env


@pytest.mark.parametrize(
    ("env", "expected"),
    [
        ({"PII_CHECKER_ENABLED": "false"}, False),
        ({"PII_CHECKER_HOOK_ENABLED": "true", "PII_CHECKER_ENABLED": "false"}, True),
        ({"PII_CHECKER_HOOK_ENABLED": "invalid", "PII_CHECKER_ENABLED": "false"}, True),
        ({"PII_CHECKER_HOOK_ENABLED": "false", "PII_CHECKER_ENABLED": "true"}, False),
    ],
)
def test_qwen_pii_legacy_enabled_fallback_matches_hook_runtime(
    monkeypatch: pytest.MonkeyPatch, env: dict[str, str], expected: bool
) -> None:
    module = _load_qwen_pii_module()
    for name in ("PII_CHECKER_HOOK_ENABLED", "PII_CHECKER_ENABLED"):
        if name in env:
            monkeypatch.setenv(name, env[name])
        else:
            monkeypatch.delenv(name, raising=False)

    if "PII_CHECKER_HOOK_ENABLED" in os.environ:
        hook_enabled = module.env_flag_enabled("PII_CHECKER_HOOK_ENABLED", True)
    else:
        hook_enabled = module._environment_bool("PII_CHECKER_ENABLED", True)
    record = _single_record("qwen", "pii-check", env)

    assert hook_enabled is expected
    assert record.effective == ("enabled" if expected else "disabled")


def test_cosh_prompt_scan_scan_mode_does_not_change_interaction_mode() -> None:
    record = _single_record(
        "cosh",
        "prompt-scan",
        {"PROMPT_SCANNER_MODE": "deny", "PROMPT_SCANNER_SCAN_MODE": "strict"},
    )

    assert record.effective == "enabled"
    assert record.mode == "ask"
    assert record.scan_mode == "strict"
    assert record.timeout == "10"
    assert "PROMPT_SCANNER_MODE" not in record.env
    assert record.env["PROMPT_SCANNER_SCAN_MODE"]["effective"] == "strict"
    assert record.env[_L2_MODEL_ENV]["effective"] == (
        record.env[_L2_MODEL_ENV]["default"]
    )


@pytest.mark.parametrize("agent", AGENTS)
@pytest.mark.usefixtures("stub_engine_backends")
def test_prompt_scan_reports_l2_model_override_for_every_agent(agent: str) -> None:
    default_record = _single_record(agent, "prompt-scan", {})
    override_record = _single_record(agent, "prompt-scan", {_L2_MODEL_ENV: _WARDEN_GEN})

    assert default_record.env[_L2_MODEL_ENV] == {
        "raw": None,
        "effective": _QWEN3_GUARD,
        "default": _QWEN3_GUARD,
    }
    assert override_record.env[_L2_MODEL_ENV]["effective"] == _WARDEN_GEN
    assert override_record.env[_L2_MODEL_ENV]["default"] == _QWEN3_GUARD
    assert override_record.diagnostics == []


@pytest.mark.parametrize(
    "capability", ["code-scan", "pii-check", "skill-ledger", "observability"]
)
def test_l2_model_is_scoped_to_prompt_scan(capability: str) -> None:
    record = _single_record("qoder", capability, {_L2_MODEL_ENV: _WARDEN_GEN})

    assert _L2_MODEL_ENV not in record.env


@pytest.mark.parametrize(
    "raw",
    [
        _WARDEN_GEN,
        f"  {_WARDEN_GEN}  ",
        _WARDEN_GEN.upper(),
        _QWEN3_GUARD,
        "",
        "   ",
    ],
)
@pytest.mark.usefixtures("stub_engine_backends")
def test_l2_model_view_semantics_match_scan_prompt_resolution(
    monkeypatch: pytest.MonkeyPatch, raw: str
) -> None:
    monkeypatch.setenv(_L2_MODEL_ENV, raw)

    record = _single_record("qoder", "prompt-scan", {_L2_MODEL_ENV: raw})

    # ``scan-prompt`` returns ``None`` for "not set" and lets the engine pick
    # its default, which is what the view reports in that case.
    assert record.env[_L2_MODEL_ENV]["effective"] == (
        _resolve_l2_model() or _QWEN3_GUARD
    )
    assert record.diagnostics == []


@pytest.mark.usefixtures("stub_engine_backends")
def test_unsupported_l2_model_is_reported_with_a_diagnostic() -> None:
    configured = f"{_WARDEN_GEN}-3Domain:q4_K_M"

    record = _single_record("qoder", "prompt-scan", {_L2_MODEL_ENV: configured})

    # The engine rejects the name at construction, so the operator must see the
    # configured value instead of a default that will never run.
    assert record.env[_L2_MODEL_ENV]["effective"] == configured
    assert record.diagnostics == [
        f"{_L2_MODEL_ENV} is not a supported L2 backend; prompt scans will fail"
    ]
    assert configured not in record.diagnostics[0]


def test_l2_model_degrades_when_engine_is_unavailable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        capability_view, "_engine_l2_backends", lambda: ("", frozenset())
    )

    unset_record = _single_record("qoder", "prompt-scan", {})
    unknown_record = _single_record("qoder", "prompt-scan", {_L2_MODEL_ENV: "nope"})

    assert unset_record.env[_L2_MODEL_ENV]["effective"] == ""
    assert unset_record.env[_L2_MODEL_ENV]["default"] == ""
    assert unknown_record.env[_L2_MODEL_ENV]["effective"] == "nope"
    assert unknown_record.diagnostics == []


def test_l2_model_metadata_tracks_the_native_engine() -> None:
    """The reported default and backend list must come from the engine itself.

    Runs wherever the extension is importable so a renamed or added backend
    cannot drift away from what the view reports.
    """
    native = pytest.importorskip(
        "agent_sec_cli._native",
        reason="native extension not built; the drift check runs where it is",
    )
    info = json.loads(native.scanner_engine_info())

    capability_view._engine_l2_backends.cache_clear()
    default, known = capability_view._engine_l2_backends()

    assert default == info["l2_model"]
    assert known == frozenset(model.lower() for model in info["l2_models"])


def test_l2_model_never_reaches_output_unescaped() -> None:
    records = query_capabilities(
        agent="hermes",
        capability="prompt-scan",
        env={_L2_MODEL_ENV: "model\x1b[31m\n" + "x" * 120},
    )

    effective = json.loads(render_json(records))[0]["env"][_L2_MODEL_ENV]["effective"]

    assert "\x1b" not in effective
    assert "\n" not in effective
    assert effective.startswith("model\\x1b[31m\\x0a")
    assert len(effective) <= 80


def test_agent_home_and_config_files_do_not_affect_environment_view(
    tmp_path: Path,
) -> None:
    openclaw_config = tmp_path / "openclaw.json"
    hermes_config = (
        tmp_path / "hermes" / "plugins" / "agent-sec-core-hermes-plugin" / "config.toml"
    )
    openclaw_config.write_text(
        json.dumps(
            {
                "plugins": {
                    "entries": {
                        "agent-sec": {
                            "enabled": False,
                            "config": {
                                "capabilities": {"prompt-scan": {"enabled": False}}
                            },
                        }
                    }
                }
            }
        ),
        encoding="utf-8",
    )
    hermes_config.parent.mkdir(parents=True)
    hermes_config.write_text(
        "[capabilities.prompt-scan-user-input]\nenabled = false\n", encoding="utf-8"
    )

    env = {
        "HOME": str(tmp_path / "home"),
        "OPENCLAW_CONFIG_PATH": str(openclaw_config),
        "OPENCLAW_STATE_DIR": str(tmp_path / "state"),
        "HERMES_HOME": str(tmp_path / "hermes"),
        "PROMPT_SCANNER_HOOK_ENABLED": "true",
        "PROMPT_SCANNER_SCAN_MODE": "strict",
    }

    openclaw_record = _single_record("openclaw", "prompt-scan", env)
    hermes_record = _single_record("hermes", "prompt-scan", env)

    assert openclaw_record.effective == "enabled"
    assert hermes_record.effective == "enabled"
    assert openclaw_record.config == {}
    assert hermes_record.config == {}
    assert openclaw_record.config_path is None
    assert hermes_record.config_path is None
    assert openclaw_record.scan_mode == "strict"
    assert hermes_record.scan_mode == "strict"


def test_render_table_groups_prompt_scan_by_agent_with_env_only() -> None:
    records = query_capabilities(
        capability="prompt-scan",
        env={"PROMPT_SCANNER_MODE": "deny", "PROMPT_SCANNER_SCAN_MODE": "strict"},
    )
    table = render_table(records)

    assert "AGENT" not in _table_section(table, "codex")[1]
    assert _section_row(table, "codex", "prompt-scan").split() == [
        "prompt-scan",
        "enabled",
        "deny",
        "strict",
        "10",
        "-",
    ]
    assert _section_row(table, "qoder", "prompt-scan").split() == [
        "prompt-scan",
        "enabled",
        "deny",
        "strict",
        "10",
        "-",
    ]
    assert _section_row(table, "qwen", "prompt-scan").split() == [
        "prompt-scan",
        "enabled",
        "deny",
        "strict",
        "10",
        "-",
    ]
    for agent in ("cosh", "openclaw", "hermes"):
        expected_mode = "ask" if agent == "cosh" else "observe"
        expected_timeout = "15" if agent == "hermes" else "10"
        assert _section_row(table, agent, "prompt-scan").split() == [
            "prompt-scan",
            "enabled",
            expected_mode,
            "strict",
            expected_timeout,
            "-",
        ]


def test_render_table_shows_code_scan_env_disable_per_agent() -> None:
    records = query_capabilities(
        capability="code-scan",
        env={"CODE_SCANNER_HOOK_ENABLED": "false", "CODE_SCANNER_TIMEOUT": "21"},
    )
    table = render_table(records)

    for agent in ("codex", "qoder", "qwen"):
        assert _section_row(table, agent, "code-scan").split() == [
            "code-scan",
            "disabled",
            "observe",
            "-",
            "21",
            "-",
        ]
    for agent in ("cosh", "openclaw", "hermes"):
        expected_mode = "ask" if agent == "cosh" else "observe"
        assert _section_row(table, agent, "code-scan").split() == [
            "code-scan",
            "disabled",
            expected_mode,
            "-",
            "10",
            "-",
        ]


def test_public_outputs_do_not_expose_raw_environment_values() -> None:
    sensitive_value = "token-like-sensitive-value"
    records = query_capabilities(
        agent="qwen",
        capability="prompt-scan",
        env={"PROMPT_SCANNER_MODE": sensitive_value},
    )

    payload = json.loads(render_json(records))
    table = render_table(records)

    assert sensitive_value not in json.dumps(payload)
    assert sensitive_value not in table
    assert "raw" not in payload[0]["env"]["PROMPT_SCANNER_MODE"]
    assert payload[0]["env"]["PROMPT_SCANNER_MODE"] == {
        "effective": "observe",
        "default": "observe",
    }
    assert payload[0]["diagnostics"] == [
        "PROMPT_SCANNER_MODE has an invalid value; using 'observe'"
    ]


def test_renderers_emit_stable_shapes() -> None:
    records = query_capabilities(agent="cosh", capability="code-scan", env={})

    payload = json.loads(render_json(records))
    table = render_table(records)

    assert set(payload[0]) == {
        "agent",
        "capability",
        "enabled",
        "mode",
        "scan_mode",
        "timeout",
        "env",
        "diagnostics",
    }
    assert payload[0]["agent"] == "cosh"
    assert payload[0]["capability"] == "code-scan"
    assert payload[0]["enabled"] == "enabled"
    assert payload[0]["mode"] == "ask"
    assert payload[0]["scan_mode"] == "-"
    assert payload[0]["timeout"] == "10"
    assert "raw" not in payload[0]["env"]["CODE_SCANNER_HOOK_ENABLED"]
    assert "hooks" not in payload[0]
    assert "source" not in payload[0]
    assert "config" not in payload[0]
    assert "config_path" not in payload[0]
    lines = table.splitlines()
    assert lines[0] == "[cosh]"
    assert lines[1].startswith("CAPABILITY")
    assert "AGENT" not in lines[1]
    assert "SCAN_MODE" in lines[1]
    assert "TIMEOUT(s)" in lines[1]
    assert "HOOKS" not in lines[1]
    assert "SOURCE" not in lines[1]
    assert "code-scan" in table


@pytest.mark.parametrize(
    "data_home", [None, "", "relative", "/x/./y", "/x/../y", "/Data With Spaces"]
)
def test_cosh_ledger_xdg_view_preserves_paths_and_fallback(data_home):
    env = {} if data_home is None else {"XDG_DATA_HOME": data_home}
    record = query_capabilities(agent="cosh", capability="skill-ledger", env=env)[0]
    expected = data_home if data_home == "/Data With Spaces" else "~/.local/share"
    assert record.env["XDG_DATA_HOME"]["effective"] == expected
    assert record.mode == "ask"


def test_openclaw_pii_lists_version_dependent_input_hooks() -> None:
    record = _single_record("openclaw", "pii-check")
    assert record.hooks == [
        "before_agent_run",
        "before_dispatch",
        "before_tool_call",
        "after_tool_call",
        "llm_output",
    ]
