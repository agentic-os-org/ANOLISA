"""Environment-variable capability view for agent-sec plugin integrations."""

from __future__ import annotations

import json
import math
import os
from dataclasses import dataclass, field
from functools import lru_cache
from typing import Any, Mapping

from agent_sec_cli.skill_ledger.paths import valid_anolisa_data_home

CANONICAL_CAPABILITIES = (
    "code-scan",
    "prompt-scan",
    "pii-check",
    "skill-ledger",
    "observability",
)

AGENTS = ("qoder", "qwen", "codex", "cosh", "openclaw", "hermes")
_OUTPUT_COLUMNS = (
    "CAPABILITY",
    "ENABLED",
    "MODE",
    "SCAN_MODE",
    "TIMEOUT(s)",
    "DIAGNOSTICS",
)
_STRICT_TRUE_FALSE = {"true": True, "false": False}
_BOOLEAN_TRUE_VALUES = {"1", "true", "yes", "on"}
_BOOLEAN_FALSE_VALUES = {"0", "false", "no", "off"}
_HOOK_POLICY_ALIASES = {"debug": "observe", "deny": "block"}
_HOOK_POLICIES = {"observe", "warn", "ask", "block"}
_VALID_SCAN_MODES = {"fast", "standard", "strict"}
_L2_MODEL_ENV = "PROMPT_SCANNER_L2_MODEL"


@dataclass(frozen=True)
class EnvSpec:
    name: str
    default: Any
    valid_values: frozenset[str] | None = None
    bool_style: str | None = None
    aliases: Mapping[str, str] = field(default_factory=dict)
    value_kind: str = "string"
    max_value: float | None = None
    require_positive: bool = False


@dataclass(frozen=True)
class CapabilitySpec:
    hooks: tuple[str, ...]
    env: tuple[EnvSpec, ...] = ()
    default_mode: str = "observe"


@dataclass
class CapabilityRecord:
    agent: str
    capability: str
    hooks: list[str]
    effective: str
    source: str
    mode: str = "observe"
    scan_mode: str = "-"
    timeout: str = "-"
    config: dict[str, Any] = field(default_factory=dict)
    env: dict[str, Any] = field(default_factory=dict)
    config_path: str | None = None
    diagnostics: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "agent": self.agent,
            "capability": self.capability,
            "enabled": self.effective,
            "mode": self.mode,
            "scan_mode": self.scan_mode,
            "timeout": self.timeout,
            "env": {
                name: {
                    "effective": value["effective"],
                    "default": value["default"],
                }
                for name, value in self.env.items()
            },
            "diagnostics": self.diagnostics,
        }


def _hook_enabled(name: str) -> EnvSpec:
    return EnvSpec(name, True, bool_style="strict")


def _timeout(
    name: str,
    default: str,
    value_kind: str = "int",
    max_value: float | None = None,
    require_positive: bool = False,
) -> EnvSpec:
    return EnvSpec(
        name,
        default,
        value_kind=value_kind,
        max_value=max_value,
        require_positive=require_positive,
    )


def _observability_timeout() -> EnvSpec:
    return _timeout(
        "OBSERVABILITY_TIMEOUT",
        "5",
        max_value=5.0,
        require_positive=True,
    )


def _mode(name: str, default: str, valid_values: set[str]) -> EnvSpec:
    return EnvSpec(
        name,
        default,
        frozenset(valid_values),
        aliases=_HOOK_POLICY_ALIASES,
    )


def _plain_mode(name: str, default: str, valid_values: set[str]) -> EnvSpec:
    return EnvSpec(name, default, frozenset(valid_values))


def _prompt_scan_mode() -> EnvSpec:
    return EnvSpec("PROMPT_SCANNER_SCAN_MODE", "standard", frozenset(_VALID_SCAN_MODES))


def _l2_model() -> EnvSpec:
    """Prompt scanner L2 backend override, shared by every integration.

    No hook reads this variable itself: each one shells out to
    ``agent-sec-cli scan-prompt``, which resolves it, so every host inherits
    it. The default and the selectable backends come from the native engine at
    resolution time, so this view never carries a second copy of the model
    list.
    """
    return EnvSpec(_L2_MODEL_ENV, "", value_kind="identifier")


@lru_cache(maxsize=1)
def _engine_l2_backends() -> tuple[str, frozenset[str]]:
    """Return the engine's default L2 backend and its selectable backends.

    The native scanner owns both, so querying it keeps the view from
    duplicating the model list. Falls back to ``("", frozenset())`` for any
    import or payload problem: ``capabilities`` must stay usable before
    ``maturin develop`` has built the extension, in which case the view
    reports no default and cannot flag an unsupported backend.
    """
    try:
        from agent_sec_cli import _native  # noqa: PLC0415 - optional at runtime

        info = json.loads(_native.scanner_engine_info())
        default = info["l2_model"]
        models = info["l2_models"]
        if not isinstance(default, str) or not isinstance(models, list):
            return "", frozenset()
        return default, frozenset(
            item.lower() for item in models if isinstance(item, str)
        )
    except Exception:  # noqa: BLE001 - an unusable engine only degrades the view
        return "", frozenset()


_CODE_MODES_INTERACTIVE = {"observe", "ask", "block"}
_CODE_MODES_BLOCK_ONLY = {"observe", "block"}
_HERMES_NATIVE_MODES = {"observe", "block"}
_PROMPT_MODES = {"observe", "deny"}

_QODER_HOOKS = {
    "code-scan": ("PreToolUse:Bash",),
    "prompt-scan": ("UserPromptSubmit",),
    "pii-check": ("UserPromptSubmit", "PreToolUse", "PostToolUse"),
    "skill-ledger": ("PreToolUse:Skill",),
    "observability": (
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
    ),
}

_QWEN_HOOKS = {
    "code-scan": ("PreToolUse:run_shell_command",),
    "prompt-scan": ("UserPromptSubmit",),
    "pii-check": (
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
    ),
    "skill-ledger": ("PreToolUse:skill",),
    "observability": (
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
    ),
}

_CODEX_HOOKS = {
    "code-scan": ("PreToolUse:Bash",),
    "prompt-scan": ("UserPromptSubmit",),
    "pii-check": ("PreToolUse", "UserPromptSubmit", "PostToolUse"),
    "skill-ledger": ("UserPromptSubmit",),
    "observability": ("PreToolUse", "UserPromptSubmit", "PostToolUse", "Stop"),
}

_COSH_HOOKS = {
    "code-scan": ("PreToolUse:run_shell_command|shell",),
    "prompt-scan": ("UserPromptSubmit",),
    "pii-check": (
        "PreToolUse",
        "UserPromptSubmit",
        "AfterModel",
        "PostToolUse",
        "PostToolUseFailure",
    ),
    "skill-ledger": ("PreToolUse:skill",),
    "observability": (
        "PreToolUse",
        "UserPromptSubmit",
        "BeforeModel",
        "AfterModel",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
    ),
}

_OPENCLAW_HOOKS = {
    "code-scan": ("before_tool_call",),
    "prompt-scan": ("before_dispatch",),
    "pii-check": (
        "before_agent_run",
        "before_dispatch",
        "before_tool_call",
        "after_tool_call",
        "llm_output",
    ),
    "skill-ledger": ("before_tool_call",),
    "observability": (
        "llm_input",
        "model_call_started",
        "model_call_ended",
        "llm_output",
        "agent_end",
        "before_tool_call",
        "after_tool_call",
    ),
}

_HERMES_HOOKS = {
    "code-scan": ("pre_tool_call",),
    "prompt-scan": ("pre_llm_call",),
    "pii-check": (
        "pre_llm_call",
        "pre_tool_call",
        "post_tool_call",
        "post_llm_call",
    ),
    "skill-ledger": ("pre_tool_call",),
    "observability": (
        "pre_llm_call",
        "pre_api_request",
        "post_api_request",
        "pre_tool_call",
        "post_tool_call",
        "post_llm_call",
    ),
}


def _agent_specs(
    hooks: Mapping[str, tuple[str, ...]],
    code_modes: set[str],
    include_timeouts: bool = True,
    qwen_legacy_pii_enabled: bool = False,
    include_prompt_mode: bool = True,
) -> dict[str, CapabilitySpec]:
    code_env = [
        _hook_enabled("CODE_SCANNER_HOOK_ENABLED"),
        _mode("CODE_SCANNER_MODE", "observe", code_modes),
    ]
    prompt_env = [_hook_enabled("PROMPT_SCANNER_HOOK_ENABLED")]
    if include_prompt_mode:
        prompt_env.append(_plain_mode("PROMPT_SCANNER_MODE", "observe", _PROMPT_MODES))
    prompt_env.append(_prompt_scan_mode())
    prompt_env.append(_l2_model())
    pii_env = [
        _hook_enabled("PII_CHECKER_HOOK_ENABLED"),
        _mode("PII_CHECKER_MODE", "observe", _HOOK_POLICIES),
    ]
    skill_env = [
        _hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
        _mode("SKILL_LEDGER_MODE", "ask", _HOOK_POLICIES),
    ]
    if include_timeouts:
        code_env.append(_timeout("CODE_SCANNER_TIMEOUT", "10"))
        prompt_env.append(_timeout("PROMPT_SCANNER_TIMEOUT", "10"))
        pii_env.append(_timeout("PII_CHECKER_TIMEOUT", "5"))
        skill_env.append(_timeout("SKILL_LEDGER_TIMEOUT", "5"))
    if qwen_legacy_pii_enabled:
        pii_env.insert(1, EnvSpec("PII_CHECKER_ENABLED", True, bool_style="broad"))
    return {
        "code-scan": CapabilitySpec(hooks["code-scan"], tuple(code_env)),
        "prompt-scan": CapabilitySpec(hooks["prompt-scan"], tuple(prompt_env)),
        "pii-check": CapabilitySpec(hooks["pii-check"], tuple(pii_env)),
        "skill-ledger": CapabilitySpec(hooks["skill-ledger"], tuple(skill_env), "ask"),
        "observability": CapabilitySpec(
            hooks["observability"],
            (
                _hook_enabled("OBSERVABILITY_HOOK_ENABLED"),
                _observability_timeout(),
            ),
        ),
    }


AGENT_SPECS: dict[str, dict[str, CapabilitySpec]] = {
    "qoder": _agent_specs(_QODER_HOOKS, _CODE_MODES_INTERACTIVE),
    "qwen": _agent_specs(
        _QWEN_HOOKS, _CODE_MODES_INTERACTIVE, qwen_legacy_pii_enabled=True
    ),
    "codex": _agent_specs(_CODEX_HOOKS, _CODE_MODES_BLOCK_ONLY),
    "cosh": _agent_specs(
        _COSH_HOOKS,
        {"ask"},
        include_timeouts=False,
        include_prompt_mode=False,
    ),
    "openclaw": _agent_specs(
        _OPENCLAW_HOOKS,
        _CODE_MODES_INTERACTIVE,
        include_timeouts=False,
        include_prompt_mode=False,
    ),
    "hermes": _agent_specs(
        _HERMES_HOOKS,
        _CODE_MODES_BLOCK_ONLY,
        include_timeouts=False,
        include_prompt_mode=False,
    ),
}
AGENT_SPECS["cosh"]["skill-ledger"] = CapabilitySpec(
    _COSH_HOOKS["skill-ledger"],
    (
        *AGENT_SPECS["cosh"]["skill-ledger"].env,
        EnvSpec("XDG_DATA_HOME", "~/.local/share", value_kind="anolisa_data_home"),
    ),
    "ask",
)
AGENT_SPECS["hermes"]["pii-check"] = CapabilitySpec(
    _HERMES_HOOKS["pii-check"],
    (
        _hook_enabled("PII_CHECKER_HOOK_ENABLED"),
        _mode("PII_CHECKER_MODE", "observe", _HERMES_NATIVE_MODES),
    ),
)
AGENT_SPECS["hermes"]["skill-ledger"] = CapabilitySpec(
    _HERMES_HOOKS["skill-ledger"],
    (
        _hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
        _mode("SKILL_LEDGER_MODE", "observe", _HERMES_NATIVE_MODES),
    ),
)
AGENT_SPECS["qoder"]["pii-check"] = CapabilitySpec(
    _QODER_HOOKS["pii-check"],
    (
        _hook_enabled("PII_CHECKER_HOOK_ENABLED"),
        _mode("PII_CHECKER_MODE", "observe", _HOOK_POLICIES),
        EnvSpec("PII_CHECKER_INCLUDE_LOW_CONFIDENCE", False, bool_style="broad"),
        _timeout("PII_CHECKER_TIMEOUT", "5"),
    ),
)
AGENT_SPECS["qoder"]["skill-ledger"] = CapabilitySpec(
    _QODER_HOOKS["skill-ledger"],
    (
        _hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
        _mode("SKILL_LEDGER_MODE", "ask", _HOOK_POLICIES),
        _timeout("SKILL_LEDGER_TIMEOUT", "5", "float"),
    ),
    "ask",
)
AGENT_SPECS["qwen"]["pii-check"] = CapabilitySpec(
    _QWEN_HOOKS["pii-check"],
    (
        _hook_enabled("PII_CHECKER_HOOK_ENABLED"),
        EnvSpec("PII_CHECKER_ENABLED", True, bool_style="broad"),
        _mode("PII_CHECKER_MODE", "observe", _HOOK_POLICIES),
        EnvSpec("PII_CHECKER_INCLUDE_LOW_CONFIDENCE", False, bool_style="broad"),
        _timeout("PII_CHECKER_TIMEOUT", "5", "float", 8.0),
    ),
)
AGENT_SPECS["qwen"]["skill-ledger"] = CapabilitySpec(
    _QWEN_HOOKS["skill-ledger"],
    (
        _hook_enabled("SKILL_LEDGER_HOOK_ENABLED"),
        _mode("SKILL_LEDGER_MODE", "ask", _HOOK_POLICIES),
    ),
    "ask",
)
AGENT_SPECS["cosh"]["code-scan"] = CapabilitySpec(
    _COSH_HOOKS["code-scan"],
    (
        _hook_enabled("CODE_SCANNER_HOOK_ENABLED"),
        _mode("CODE_SCANNER_MODE", "ask", {"ask"}),
    ),
    "ask",
)
AGENT_SPECS["cosh"]["prompt-scan"] = CapabilitySpec(
    _COSH_HOOKS["prompt-scan"],
    (
        _hook_enabled("PROMPT_SCANNER_HOOK_ENABLED"),
        _prompt_scan_mode(),
        _l2_model(),
    ),
    "ask",
)

STATIC_DEFAULT_TIMEOUTS = {
    ("qwen", "skill-ledger"): "5",
    ("cosh", "code-scan"): "10",
    ("cosh", "prompt-scan"): "10",
    ("cosh", "pii-check"): "10",
    # The show subprocess budget; the Cosh host allows 10 s for init plus show.
    ("cosh", "skill-ledger"): "5",
    ("openclaw", "code-scan"): "10",
    ("openclaw", "prompt-scan"): "10",
    ("openclaw", "pii-check"): "10",
    ("openclaw", "skill-ledger"): "5",
    ("hermes", "code-scan"): "10",
    ("hermes", "prompt-scan"): "15",
    ("hermes", "pii-check"): "10",
    ("hermes", "skill-ledger"): "5",
}


class CapabilityViewError(ValueError):
    """Raised for invalid capability view filters."""


def query_capabilities(
    agent: str | None = None,
    capability: str | None = None,
    env: Mapping[str, str] | None = None,
) -> list[CapabilityRecord]:
    effective_env = dict(os.environ if env is None else env)
    agent_filter = _normalize_filter(agent, AGENTS, "agent")
    capability_filter = _normalize_filter(
        capability, CANONICAL_CAPABILITIES, "capability"
    )
    records: list[CapabilityRecord] = []
    for agent_name in AGENTS:
        if agent_filter is not None and agent_name != agent_filter:
            continue
        for record in _agent_records(agent_name, effective_env):
            if capability_filter is None or record.capability == capability_filter:
                records.append(record)
    return sorted(
        records, key=lambda item: (item.agent, item.capability, ",".join(item.hooks))
    )


def render_json(records: list[CapabilityRecord]) -> str:
    return json.dumps(
        [record.to_dict() for record in records], ensure_ascii=False, indent=2
    )


def render_table(records: list[CapabilityRecord]) -> str:
    lines: list[str] = []
    for agent_name in _ordered_agents(records):
        agent_records = [record for record in records if record.agent == agent_name]
        rows = [
            (
                record.capability,
                record.effective,
                record.mode,
                record.scan_mode,
                record.timeout,
                ";".join(record.diagnostics) if record.diagnostics else "-",
            )
            for record in agent_records
        ]
        widths = [len(column) for column in _OUTPUT_COLUMNS]
        for row in rows:
            for index, value in enumerate(row):
                widths[index] = max(widths[index], len(value))
        if lines:
            lines.append("")
        lines.append(f"[{agent_name}]")
        lines.append(_format_row(_OUTPUT_COLUMNS, widths))
        lines.append(_format_row(tuple("-" * width for width in widths), widths))
        lines.extend(_format_row(row, widths) for row in rows)
    return "\n".join(lines)


def _ordered_agents(records: list[CapabilityRecord]) -> list[str]:
    ordered: list[str] = []
    for record in records:
        if record.agent not in ordered:
            ordered.append(record.agent)
    return ordered


def _normalize_filter(
    value: str | None, allowed: tuple[str, ...], field_name: str
) -> str | None:
    if value is None:
        return None
    normalized = value.strip().lower()
    if normalized not in allowed:
        display_value = _safe_cli_value(value)
        raise CapabilityViewError(
            f"unknown {field_name}: {display_value}. Allowed values: {', '.join(allowed)}"
        )
    return normalized


def _safe_cli_value(value: str, limit: int = 80) -> str:
    escaped: list[str] = []
    for char in value:
        if char.isprintable():
            escaped.append(char)
        else:
            code_point = ord(char)
            if code_point <= 0xFF:
                escaped.append(f"\\x{code_point:02x}")
            elif code_point <= 0xFFFF:
                escaped.append(f"\\u{code_point:04x}")
            else:
                escaped.append(f"\\U{code_point:08x}")
    display_value = "".join(escaped)
    if len(display_value) > limit:
        return f"{display_value[: limit - 1]}…"
    return display_value


def _agent_records(agent: str, env: Mapping[str, str]) -> list[CapabilityRecord]:
    records: list[CapabilityRecord] = []
    for capability in CANONICAL_CAPABILITIES:
        spec = AGENT_SPECS[agent][capability]
        env_values, diagnostics, enabled = _resolve_env(spec.env, env)
        records.append(
            CapabilityRecord(
                agent=agent,
                capability=capability,
                hooks=list(spec.hooks),
                effective="enabled" if enabled else "disabled",
                source="manifest+env",
                mode=_mode_from_env(env_values, spec.default_mode),
                scan_mode=_scan_mode(capability, env_values),
                timeout=_timeout_from_env_or_default(agent, capability, env_values),
                env=env_values,
                diagnostics=diagnostics,
            )
        )
    return records


def _resolve_env(
    specs: tuple[EnvSpec, ...], env: Mapping[str, str]
) -> tuple[dict[str, Any], list[str], bool]:
    values: dict[str, Any] = {}
    diagnostics: list[str] = []
    for spec in specs:
        raw = env.get(spec.name)
        default: Any = spec.default
        if spec.bool_style is not None:
            effective = _resolve_bool(spec, raw, diagnostics)
        elif spec.name.endswith("_TIMEOUT"):
            effective = _resolve_timeout(spec, raw, diagnostics)
        elif spec.value_kind == "anolisa_data_home":
            effective = raw if valid_anolisa_data_home(raw) else spec.default
            if raw and effective == spec.default:
                diagnostics.append(_fallback_diagnostic(spec))
        elif spec.value_kind == "identifier":
            engine_default, known = _engine_l2_backends()
            default = engine_default or spec.default
            effective = _resolve_identifier(spec, raw, default, known, diagnostics)
        elif raw is None:
            effective = spec.default
        else:
            effective = raw.strip().lower()
            effective = spec.aliases.get(effective, effective)
            if spec.valid_values is not None and effective not in spec.valid_values:
                diagnostics.append(_fallback_diagnostic(spec))
                effective = spec.default
        values[spec.name] = {
            "raw": raw,
            "effective": effective,
            "default": default,
        }
    return values, diagnostics, _enabled_from_values(values)


def _resolve_bool(spec: EnvSpec, raw: str | None, diagnostics: list[str]) -> bool:
    if raw is None:
        return bool(spec.default)
    normalized = raw.strip().lower()
    if spec.bool_style == "strict":
        if normalized in _STRICT_TRUE_FALSE:
            return _STRICT_TRUE_FALSE[normalized]
    else:
        if normalized in _BOOLEAN_TRUE_VALUES:
            return True
        if normalized in _BOOLEAN_FALSE_VALUES:
            return False
    diagnostics.append(_fallback_diagnostic(spec))
    return bool(spec.default)


def _resolve_identifier(
    spec: EnvSpec,
    raw: str | None,
    default: str,
    known: frozenset[str],
    diagnostics: list[str],
) -> str:
    """Resolve an opaque identifier such as an L2 model name.

    Mirrors ``prompt_scanner.cli._resolve_l2_model``: a blank or
    whitespace-only value means "not set" and falls back to the engine
    default. Case is preserved because model names are case-sensitive registry
    paths, and the value is escaped and length-capped because it is the one env
    entry reported close to verbatim instead of as a normalized keyword.

    An unsupported name is reported as configured rather than replaced by the
    default: the engine rejects it at construction, so scans fail loudly and
    the operator needs to see the value that caused it. ``known`` is empty when
    the engine is unavailable, which skips the check instead of guessing.
    """
    if raw is None:
        return default
    text = raw.strip()
    if not text:
        return default
    if known and text.lower() not in known:
        diagnostics.append(
            f"{spec.name} is not a supported L2 backend; prompt scans will fail"
        )
    return _safe_cli_value(text)


def _resolve_timeout(spec: EnvSpec, raw: str | None, diagnostics: list[str]) -> str:
    if raw is None:
        return str(spec.default)
    text = raw.strip()
    try:
        if spec.value_kind == "int":
            value = int(text)
            if value <= 0:
                if spec.require_positive:
                    diagnostics.append(_fallback_diagnostic(spec))
                    return str(spec.default)
                diagnostics.append(
                    f"{spec.name} is nonpositive; the hook subprocess may fail open"
                )
            if spec.max_value is not None:
                value = min(value, int(spec.max_value))
            return str(value)
        value = float(text)
    except (TypeError, ValueError):
        diagnostics.append(_fallback_diagnostic(spec))
        return str(spec.default)
    if not math.isfinite(value) or value <= 0:
        diagnostics.append(_fallback_diagnostic(spec))
        return str(spec.default)
    if spec.max_value is not None:
        value = min(value, spec.max_value)
    return _format_number(value)


def _fallback_diagnostic(spec: EnvSpec) -> str:
    return f"{spec.name} has an invalid value; using {spec.default!r}"


def _format_number(value: float) -> str:
    if value.is_integer():
        return str(int(value))
    return str(value)


def _enabled_from_values(env_values: dict[str, Any]) -> bool:
    hook_enabled = env_values.get("PII_CHECKER_HOOK_ENABLED")
    legacy_enabled = env_values.get("PII_CHECKER_ENABLED")
    if hook_enabled is not None and legacy_enabled is not None:
        if hook_enabled["raw"] is not None:
            return bool(hook_enabled["effective"])
        return bool(legacy_enabled["effective"])
    for name, value in env_values.items():
        if name.endswith("_ENABLED") and value["effective"] is False:
            return False
    return True


def _mode_from_env(env_values: dict[str, Any], default: str = "observe") -> str:
    for name in (
        "CODE_SCANNER_MODE",
        "PROMPT_SCANNER_MODE",
        "PII_CHECKER_MODE",
        "SKILL_LEDGER_MODE",
    ):
        if name in env_values:
            return str(env_values[name]["effective"])
    return default


def _scan_mode(capability: str, env_values: dict[str, Any]) -> str:
    if capability != "prompt-scan":
        return "-"
    value = env_values.get("PROMPT_SCANNER_SCAN_MODE")
    if value is None:
        return "standard"
    return str(value["effective"])


def _timeout_from_env_or_default(
    agent: str, capability: str, env_values: dict[str, Any]
) -> str:
    for name, value in env_values.items():
        if name.endswith("_TIMEOUT"):
            return _format_timeout(value["effective"])
    return STATIC_DEFAULT_TIMEOUTS.get((agent, capability), "-")


def _format_timeout(value: Any) -> str:
    if value is None:
        return "-"
    text = str(value).strip()
    return text if text else "-"


def _format_row(row: tuple[str, ...], widths: list[int]) -> str:
    return "  ".join(value.ljust(widths[index]) for index, value in enumerate(row))
