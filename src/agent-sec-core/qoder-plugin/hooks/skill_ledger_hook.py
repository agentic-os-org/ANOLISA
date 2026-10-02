#!/usr/bin/env python3
"""Validate local Qoder Skills with Skill Ledger before invocation."""

import json
import os
import re
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from qoder_hook_common import (
    dumps_hook_output,
    env_flag_enabled,
    env_hook_policy,
    jsonish_value,
    load_hook_input,
    normalize_hook_policy,
    pre_tool_decision_output,
    with_trace_context,
)

_TOOL_NAME = "Skill"
_DEFAULT_POLICY = "ask"
_HOOK_ENABLED = env_flag_enabled("SKILL_LEDGER_HOOK_ENABLED", True)
_DEFAULT_TIMEOUT = 5.0
_VALID_STATUSES = frozenset(
    {"pass", "none", "drifted", "warn", "deny", "tampered", "error"}
)
_SKILL_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,127}")
_FRONTMATTER_NAME_RE = re.compile(r"^name[ \t]*:(.*)$")
_YAML_PLAIN_NON_STRING_RE = re.compile(
    r"(?:~|null|true|false|yes|no|on|off|[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[-+]?\d+)?)",
    re.IGNORECASE,
)
_MAX_SKILL_MD_CHARS = 65_536


@dataclass(frozen=True)
class ResolvedSkill:
    """A local Qoder Skill resolved to a canonical directory."""

    skill_name: str
    skill_dir: Path
    source: str


@dataclass(frozen=True)
class ResolutionIssue:
    """A local resolution failure that prevents trustworthy validation."""

    skill_name: str
    code: str


@dataclass(frozen=True)
class ResolutionResult:
    """Result of resolving one Qoder Skill invocation."""

    skill_name: str
    resolved: ResolvedSkill | None = None
    issue: ResolutionIssue | None = None
    not_found: bool = False


@dataclass(frozen=True)
class ParsedSkillName:
    """A supported frontmatter name or a safe resolution failure."""

    name: str | None = None
    issue_code: str | None = None


@dataclass(frozen=True)
class CheckOutcome:
    """Sanitized result of one Skill Ledger CLI invocation."""

    status: str
    data: dict[str, Any]
    error_code: str | None = None


def _debug(message: str) -> None:
    """Write a single-line diagnostic without exposing raw CLI output."""
    print(f"[skill-ledger debug] {' '.join(message.split())}", file=sys.stderr)


def _read_policy() -> str:
    """Return the configured four-level Skill Ledger mode."""
    raw = os.environ.get("SKILL_LEDGER_MODE")
    policy = env_hook_policy("SKILL_LEDGER_MODE", _DEFAULT_POLICY)
    if "SKILL_LEDGER_MODE" in os.environ and normalize_hook_policy(raw, "") == "":
        _debug("invalid SKILL_LEDGER_MODE; using ask")
    return policy


def _read_timeout() -> float:
    """Return a positive Skill Ledger subprocess timeout."""
    try:
        timeout = float(os.environ.get("SKILL_LEDGER_TIMEOUT", str(_DEFAULT_TIMEOUT)))
    except (TypeError, ValueError):
        return _DEFAULT_TIMEOUT
    return timeout if timeout > 0 else _DEFAULT_TIMEOUT


def _normalize_skill_name(value: Any) -> str | None:
    """Return a supported Qoder Skill name without treating it as a path."""
    if not isinstance(value, str):
        return None
    name = value.strip()
    return name if _SKILL_NAME_RE.fullmatch(name) else None


def _parse_name_scalar(raw_value: str) -> ParsedSkillName:
    """Parse the safe YAML scalar subset supported by the standalone hook."""
    value = raw_value.strip()
    if not value:
        return ParsedSkillName(issue_code="skill_manifest_invalid")

    if value[0] in {'"', "'"}:
        quote = value[0]
        closing = value.find(quote, 1)
        if closing < 0:
            return ParsedSkillName(issue_code="skill_manifest_invalid")
        candidate = value[1:closing]
        suffix = value[closing + 1 :]
        if suffix.strip() and not re.fullmatch(r"[ \t]+#.*", suffix):
            return ParsedSkillName(issue_code="skill_manifest_name_unsupported")
        if quote == '"' and "\\" in candidate:
            return ParsedSkillName(issue_code="skill_manifest_name_unsupported")
    else:
        comment = re.search(r"[ \t]+#", value)
        candidate = value[: comment.start()].rstrip() if comment else value
        if _YAML_PLAIN_NON_STRING_RE.fullmatch(candidate) or (
            not re.search(r"[A-Za-z]", candidate)
            and re.fullmatch(r"[-+0-9_.:]+", candidate)
        ):
            return ParsedSkillName(issue_code="skill_manifest_name_unsupported")

    name = _normalize_skill_name(candidate)
    if name is None or name != candidate:
        return ParsedSkillName(issue_code="skill_manifest_name_unsupported")
    return ParsedSkillName(name=name)


def _parse_skill_name(skill_md: Path) -> ParsedSkillName:
    """Read a supported top-level ``name`` from SKILL.md frontmatter."""
    with skill_md.open(encoding="utf-8-sig", errors="replace") as handle:
        text = handle.read(_MAX_SKILL_MD_CHARS + 1)
    truncated = len(text) > _MAX_SKILL_MD_CHARS
    if truncated:
        text = text[:_MAX_SKILL_MD_CHARS]
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return ParsedSkillName()

    name_values: list[str] = []
    for line in lines[1:]:
        if line.strip() == "---":
            if not name_values:
                return ParsedSkillName(issue_code="skill_manifest_invalid")
            if len(name_values) > 1:
                return ParsedSkillName(issue_code="skill_manifest_name_ambiguous")
            return _parse_name_scalar(name_values[0])
        match = _FRONTMATTER_NAME_RE.fullmatch(line)
        if match:
            name_values.append(match.group(1))

    return ParsedSkillName(issue_code="skill_manifest_invalid")


def _root_match(
    root: Path, source: str, skill_name: str
) -> tuple[ResolvedSkill | None, ResolutionIssue | None]:
    """Resolve a Skill within one Qoder root without crossing its boundary."""
    try:
        if not root.exists():
            return None, None
        if not root.is_dir():
            return None, ResolutionIssue(skill_name, f"{source}_root_invalid")
        resolved_root = root.resolve(strict=True)
        entries = sorted(root.iterdir(), key=lambda path: path.name)
    except (OSError, RuntimeError, ValueError):
        return None, ResolutionIssue(skill_name, f"{source}_root_unreadable")

    matches: list[Path] = []
    catalog_issue_code: str | None = None
    for entry in entries:
        try:
            if not entry.is_dir():
                continue
            resolved_entry = entry.resolve(strict=True)
        except (OSError, RuntimeError, ValueError):
            if entry.name == skill_name:
                return None, ResolutionIssue(skill_name, "skill_path_unresolvable")
            continue

        if not resolved_entry.is_relative_to(resolved_root):
            try:
                if (resolved_entry / "SKILL.md").is_file():
                    catalog_issue_code = "skill_symlink_escape"
            except OSError:
                catalog_issue_code = "skill_symlink_escape"
            continue

        skill_md = resolved_entry / "SKILL.md"
        try:
            if not skill_md.is_file():
                continue
            if skill_md.is_symlink():
                catalog_issue_code = "skill_manifest_symlink"
                continue
            parsed_name = _parse_skill_name(skill_md)
        except OSError:
            catalog_issue_code = "skill_manifest_unreadable"
            continue

        if parsed_name.issue_code is not None:
            catalog_issue_code = parsed_name.issue_code
            continue

        catalog_name = parsed_name.name or entry.name
        if catalog_name == skill_name:
            matches.append(resolved_entry)

    if catalog_issue_code is not None:
        return None, ResolutionIssue(skill_name, catalog_issue_code)
    if len(matches) > 1:
        return None, ResolutionIssue(skill_name, f"{source}_name_ambiguous")
    if matches:
        return ResolvedSkill(skill_name, matches[0], source), None
    return None, None


def _resolve_skill(input_data: dict[str, Any]) -> ResolutionResult:
    """Resolve a Qoder Skill name through user then project catalogs."""
    tool_input = jsonish_value(input_data.get("tool_input"))
    if not isinstance(tool_input, dict):
        issue = ResolutionIssue("<unknown>", "tool_input_invalid")
        return ResolutionResult(issue.skill_name, issue=issue)

    raw_name = tool_input.get("skill")
    skill_name = _normalize_skill_name(raw_name)
    if skill_name is None:
        issue = ResolutionIssue("<invalid>", "skill_name_invalid")
        return ResolutionResult(issue.skill_name, issue=issue)

    cwd_value = input_data.get("cwd")
    if not isinstance(cwd_value, str):
        issue = ResolutionIssue(skill_name, "cwd_invalid")
        return ResolutionResult(skill_name, issue=issue)
    cwd = Path(cwd_value)
    try:
        if not cwd.is_absolute() or not cwd.is_dir():
            issue = ResolutionIssue(skill_name, "cwd_invalid")
            return ResolutionResult(skill_name, issue=issue)
        cwd = cwd.resolve(strict=True)
    except (OSError, RuntimeError, ValueError):
        issue = ResolutionIssue(skill_name, "cwd_unresolvable")
        return ResolutionResult(skill_name, issue=issue)

    roots = (
        (Path.home() / ".qoder" / "skills", "user"),
        (cwd / ".qoder" / "skills", "project"),
    )
    for root, source in roots:
        resolved, issue = _root_match(root, source, skill_name)
        if issue is not None:
            return ResolutionResult(skill_name, issue=issue)
        if resolved is not None:
            return ResolutionResult(skill_name, resolved=resolved)

    return ResolutionResult(skill_name, not_found=True)


def _run_check(input_data: dict[str, Any], resolved: ResolvedSkill) -> CheckOutcome:
    """Run ``skill-ledger check`` and parse JSON even for non-zero exits."""
    args = with_trace_context(
        ["agent-sec-cli", "skill-ledger", "check", str(resolved.skill_dir)],
        input_data,
    )
    try:
        proc = subprocess.run(
            args,
            capture_output=True,
            check=False,
            text=True,
            timeout=_read_timeout(),
        )
    except subprocess.TimeoutExpired:
        return CheckOutcome("error", {}, "timeout")
    except FileNotFoundError:
        return CheckOutcome("error", {}, "cli_unavailable")
    except (OSError, subprocess.SubprocessError):
        return CheckOutcome("error", {}, "cli_failed")

    try:
        data = json.loads(proc.stdout)
    except (json.JSONDecodeError, TypeError, ValueError):
        return CheckOutcome("error", {}, "invalid_json")
    if not isinstance(data, dict):
        return CheckOutcome("error", {}, "invalid_json")

    status_value = data.get("status")
    status = status_value.strip().lower() if isinstance(status_value, str) else ""
    if status not in _VALID_STATUSES:
        return CheckOutcome("unknown", {}, "unknown_status")
    return CheckOutcome(status, data, "check_error" if status == "error" else None)


def _list_size(value: Any) -> int:
    """Return the number of items in a JSON array."""
    return len(value) if isinstance(value, list) else 0


def _format_check_notice(resolved: ResolvedSkill, outcome: CheckOutcome) -> str:
    """Build an actionable notice without copying findings or stderr."""
    name = resolved.skill_name
    status = outcome.status
    prefix = f"[skill-ledger] Skill '{name}' status: {status}."

    if status == "none":
        command = shlex.join(
            ["agent-sec-cli", "skill-ledger", "scan", str(resolved.skill_dir)]
        )
        return f"{prefix} No signed scan is available. Review the Skill, then run: {command}"
    if status == "drifted":
        added = _list_size(outcome.data.get("added"))
        removed = _list_size(outcome.data.get("removed"))
        modified = _list_size(outcome.data.get("modified"))
        return (
            f"{prefix} Files changed after signing "
            f"(added={added}, removed={removed}, modified={modified}). "
            "Review the changes and scan the Skill again."
        )
    if status in {"warn", "deny"}:
        findings = _list_size(outcome.data.get("findings"))
        level = "security warnings" if status == "warn" else "blocking findings"
        return f"{prefix} The signed scan contains {findings} {level}. Review the scan findings."
    if status == "tampered":
        return f"{prefix} Signed metadata verification failed. Review the Skill source before rescanning."

    error_messages = {
        "timeout": "The Skill Ledger check timed out.",
        "cli_unavailable": "The agent-sec-cli executable is unavailable.",
        "cli_failed": "The Skill Ledger process could not be started.",
        "invalid_json": "Skill Ledger returned an unreadable result.",
        "unknown_status": "Skill Ledger returned an unsupported status.",
        "check_error": "Skill Ledger could not complete the check.",
    }
    message = error_messages.get(
        outcome.error_code, "Skill Ledger could not validate the Skill."
    )
    return f"{prefix} {message} Verify the installation and retry."


def _format_resolution_notice(issue: ResolutionIssue) -> str:
    """Build a sanitized notice for an untrustworthy local resolution."""
    messages = {
        "tool_input_invalid": "the Skill tool input is not an object",
        "skill_name_invalid": "the Skill name is missing or invalid",
        "cwd_invalid": "the Qoder working directory is not a valid absolute directory",
        "cwd_unresolvable": "the Qoder working directory cannot be resolved",
        "project_root_invalid": "the project Skill root is not a directory",
        "project_root_unreadable": "the project Skill root cannot be inspected",
        "user_root_invalid": "the user Skill root is not a directory",
        "user_root_unreadable": "the user Skill root cannot be inspected",
        "skill_path_unresolvable": "the matching Skill directory cannot be resolved",
        "skill_symlink_escape": "a Skill directory escapes its trusted root",
        "skill_manifest_symlink": "a SKILL.md in the selected root is a symbolic link",
        "skill_manifest_unreadable": "a SKILL.md in the selected root cannot be read",
        "skill_manifest_invalid": "a SKILL.md frontmatter is invalid or missing its name",
        "skill_manifest_name_ambiguous": "a SKILL.md declares the name field more than once",
        "skill_manifest_name_unsupported": "a SKILL.md uses an unsupported name scalar",
        "project_name_ambiguous": "multiple project Skills declare the same name",
        "user_name_ambiguous": "multiple user Skills declare the same name",
    }
    detail = messages.get(issue.code, "the local Skill path cannot be resolved safely")
    return f"[skill-ledger] Skill '{issue.skill_name}' resolution failed: {detail}."


def _policy_output(policy: str, notice: str) -> str | None:
    """Map a risky result to Qoder's four supported policy behaviors."""
    if policy in {"debug", "observe"}:
        _debug(notice)
        return None
    if policy == "warn":
        return dumps_hook_output({"decision": "allow", "systemMessage": notice})
    if policy == "block":
        return pre_tool_decision_output("deny", notice)
    return pre_tool_decision_output("ask", notice)


def main() -> None:
    """Validate a local Qoder Skill before the Skill tool executes."""
    if not _HOOK_ENABLED:
        return
    input_data = load_hook_input()
    if input_data is None:
        return
    if input_data.get("hook_event_name") != "PreToolUse":
        return
    if input_data.get("tool_name") != _TOOL_NAME:
        return

    policy = _read_policy()
    resolution = _resolve_skill(input_data)
    if resolution.issue is not None:
        output = _policy_output(policy, _format_resolution_notice(resolution.issue))
        if output:
            print(output)
        return
    if resolution.not_found:
        _debug(
            f"Skill '{resolution.skill_name}' was not found in user or project roots; "
            "assuming a built-in, plugin, or remote source"
        )
        return
    if resolution.resolved is None:
        return

    outcome = _run_check(input_data, resolution.resolved)
    if outcome.status == "pass":
        return
    output = _policy_output(policy, _format_check_notice(resolution.resolved, outcome))
    if output:
        print(output)


if __name__ == "__main__":
    main()
