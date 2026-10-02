"""Skill-ledger capability for Hermes skill_view calls."""

from __future__ import annotations

import json
import logging
import os
from pathlib import Path
from typing import Any

from ..cli_runner import call_agent_sec_cli, trace_context
from ..hook_config import (  # noqa: TID252 - Hermes loads this as a standalone package.
    env_flag_enabled,
    normalize_hermes_native_policy,
    normalize_hook_policy,
)
from .base import AgentSecCoreCapability

logger = logging.getLogger("agent-sec-core")

_TOOL_NAME = "skill_view"
_SKILL_MANIFEST = "SKILL.md"
_DEFAULT_HERMES_SKILLS_DIR = Path("~/.hermes/skills")
_POLICY_DEBUG = "observe"
_POLICY_BLOCK = "block"
_DEFAULT_POLICY = _POLICY_DEBUG
_WARNING_LOG_STATUSES = frozenset({"deny", "tampered"})
_WARNING_LOG_REASON_CODES = frozenset({"tampered"})
_SKIP_DIRS = frozenset({".git", ".github", ".hub", ".archive", ".skill-meta"})
_UNSUPPORTED_HERMES_NOTICE = "暂不支持Hermes场景，请自行关注skill安全性。"
_REMOVED_WARNING_CONFIGS = ("max_warnings_per_turn", "max_warning_contexts")


class _UnsupportedHermesSkillRoot(Exception):
    """Internal signal for unsupported Hermes skill root views."""

    def __init__(self, root: Path, reason: str):
        super().__init__(reason)
        self.root = root
        self.reason = reason


class SkillLedgerCapability(AgentSecCoreCapability):
    """Check Hermes skills with skill-ledger before skill_view reads them."""

    id = "skill-ledger"
    name = "Skill Ledger"

    def _on_register(self, config: dict) -> None:
        """Read skill-ledger specific config."""
        self._hook_enabled = env_flag_enabled("SKILL_LEDGER_HOOK_ENABLED", True)
        self._policy = self._read_policy(config)
        self._skills_dir = _DEFAULT_HERMES_SKILLS_DIR
        removed_configs = [key for key in _REMOVED_WARNING_CONFIGS if key in config]
        if removed_configs:
            logger.warning(
                "[agent-sec-core] skill-ledger config %s ignored; "
                "Hermes synthetic warning delivery was removed",
                ", ".join(removed_configs),
            )

    def get_hooks_define(self) -> dict:
        return {"pre_tool_call": self._on_pre_tool_call}

    def _on_pre_tool_call(self, tool_name, args, **kwargs):
        """Run skill-ledger exposure summary before Hermes reads a skill."""
        if not self._hook_enabled:
            return None
        if tool_name != _TOOL_NAME:
            return None
        if not isinstance(args, dict):
            self._diagnostic("[agent-sec-core] skill-ledger missing args, fail-open")
            return None

        root = self._resolved_skills_dir()
        try:
            skill_dir = self._resolve_skill_dir(args, root=root)
        except _UnsupportedHermesSkillRoot as exc:
            self._handle_unsupported_hermes(exc.root, exc.reason)
            return None

        if skill_dir is None:
            self._diagnostic(
                "[agent-sec-core] skill-ledger could not resolve skill_dir, fail-open"
            )
            return None
        result = call_agent_sec_cli(
            ["skill-ledger", "show", str(skill_dir)],
            timeout=self._timeout,
            trace_context=trace_context(kwargs),
        )
        if not result.stdout.strip():
            self._diagnostic(
                "[agent-sec-core] skill-ledger empty CLI output, fail-open skill_dir=%s exit_code=%s",
                skill_dir,
                result.exit_code,
            )
            return None

        try:
            summary = json.loads(result.stdout)
        except (json.JSONDecodeError, ValueError):
            self._diagnostic(
                "[agent-sec-core] skill-ledger invalid CLI JSON, fail-open skill_dir=%s exit_code=%s",
                skill_dir,
                result.exit_code,
            )
            return None

        if not isinstance(summary, dict):
            self._diagnostic(
                "[agent-sec-core] skill-ledger CLI JSON is not an object, fail-open skill_dir=%s",
                skill_dir,
            )
            return None

        message = summary.get("message")
        if not isinstance(message, str) or not message.strip():
            return None

        skill_name = str(summary.get("skillName") or skill_dir.name)
        message = f"Skill '{skill_name}': {message}"
        if self._policy == _POLICY_DEBUG:
            latest_status = summary.get("latestStatus")
            normalized_status = (
                latest_status.strip().lower() if isinstance(latest_status, str) else ""
            )
            reason_code = summary.get("reasonCode")
            normalized_reason_code = (
                reason_code.strip().lower() if isinstance(reason_code, str) else ""
            )
            log = (
                logger.warning
                if normalized_status in _WARNING_LOG_STATUSES
                or normalized_reason_code in _WARNING_LOG_REASON_CODES
                else logger.info
            )
            log("[agent-sec-core] skill-ledger %s", message)
            return None

        logger.warning("[agent-sec-core] skill-ledger %s", message)
        return {"action": "block", "message": message}

    def _resolve_skill_dir(
        self, args: dict[str, Any], *, root: Path | None = None
    ) -> Path | None:
        """Resolve a Hermes skill_view call to a local skill directory."""
        skill_name = self._extract_string(args, "name", "skill", "skill_name")
        if not skill_name:
            return None
        return self._resolve_skill_dir_from_name(skill_name, root=root)

    def _resolve_skill_dir_from_name(
        self, skill_name: str, *, root: Path | None = None
    ) -> Path | None:
        """Resolve by Hermes local directory name or category/name."""
        wanted = skill_name.strip()
        if not wanted:
            return None
        if ":" in wanted:
            logger.debug(
                "[agent-sec-core] skill-ledger skips qualified/plugin skill name: %s",
                wanted,
            )
            return None

        if root is None:
            root = self._resolved_skills_dir()
        if root is None:
            return None
        try:
            if not root.is_dir():
                return None
        except OSError as exc:
            raise _UnsupportedHermesSkillRoot(root, str(exc)) from exc
        try:
            resolved_root = root.resolve()
        except (OSError, ValueError) as exc:
            raise _UnsupportedHermesSkillRoot(root, str(exc)) from exc

        candidates: list[Path] = []
        seen: set[Path] = set()

        def record(skill_dir: Path, skill_file: Path) -> None:
            try:
                resolved_file = skill_file.resolve()
            except OSError as exc:
                raise _UnsupportedHermesSkillRoot(root, str(exc)) from exc
            except ValueError:
                return
            if not self._is_under_root(resolved_file, resolved_root):
                return
            if resolved_file in seen:
                return
            seen.add(resolved_file)
            candidates.append(skill_dir)

        relative_name = self._safe_relative_name(wanted)
        if relative_name is not None:
            direct_path = root / relative_name
            if "/" in wanted:
                try:
                    resolved_direct_path = direct_path.resolve(strict=False)
                except (OSError, ValueError) as exc:
                    raise _UnsupportedHermesSkillRoot(root, str(exc)) from exc
                if self._is_ignored_path(direct_path, root):
                    return None
                if not self._is_under_root(resolved_direct_path, resolved_root):
                    return None
                return direct_path
            direct_skill_file = direct_path / _SKILL_MANIFEST
            try:
                is_direct_skill = direct_path.is_dir() and direct_skill_file.is_file()
            except OSError as exc:
                raise _UnsupportedHermesSkillRoot(root, str(exc)) from exc
            if is_direct_skill:
                record(direct_path, direct_skill_file)

        if "/" not in wanted:
            for skill_file in self._iter_skill_files(root):
                if skill_file.parent.name == wanted:
                    record(skill_file.parent, skill_file)

        if len(candidates) > 1:
            self._diagnostic(
                "[agent-sec-core] skill-ledger ambiguous Hermes skill name=%s matches=%s, fail-open",
                wanted,
                [str(path) for path in candidates],
            )
            return None
        return candidates[0] if candidates else None

    def _resolved_skills_dir(self) -> Path | None:
        try:
            expanded = self._skills_dir.expanduser()
            return Path(os.path.abspath(os.path.normpath(os.fspath(expanded))))
        except (OSError, ValueError):
            self._diagnostic(
                "[agent-sec-core] skill-ledger invalid Hermes skills dir: %s",
                self._skills_dir,
            )
            return None

    def _iter_skill_files(self, root: Path):
        """Yield SKILL.md files under the default Hermes local skills dir."""
        try:
            skill_files = sorted(root.rglob(_SKILL_MANIFEST))
        except OSError as exc:
            raise _UnsupportedHermesSkillRoot(root, str(exc)) from exc

        for skill_file in skill_files:
            if self._is_ignored_path(skill_file, root):
                continue
            yield skill_file

    def _handle_unsupported_hermes(self, root: Path, reason: str) -> None:
        log_message = "[agent-sec-core] skill-ledger %s root=%s reason=%s"
        if self._policy == _POLICY_DEBUG:
            logger.debug(log_message, _UNSUPPORTED_HERMES_NOTICE, root, reason)
            return

        logger.warning(log_message, _UNSUPPORTED_HERMES_NOTICE, root, reason)

    @staticmethod
    def _is_ignored_path(path: Path, root: Path) -> bool:
        try:
            parts = path.relative_to(root).parts
        except ValueError:
            return True
        return any(part in _SKIP_DIRS for part in parts)

    @staticmethod
    def _is_under_root(path: Path, root: Path) -> bool:
        try:
            path.relative_to(root)
        except ValueError:
            return False
        return True

    @staticmethod
    def _safe_relative_name(skill_name: str) -> Path | None:
        path = Path(skill_name)
        if path.is_absolute() or ".." in path.parts:
            return None
        return path

    @staticmethod
    def _extract_string(args: dict[str, Any], *keys: str) -> str | None:
        for key in keys:
            value = args.get(key)
            if isinstance(value, str) and value.strip():
                return value.strip()
        return None

    @staticmethod
    def _read_policy(config: dict) -> str:
        if "SKILL_LEDGER_MODE" in os.environ:
            raw_policy = os.environ.get("SKILL_LEDGER_MODE")
            return SkillLedgerCapability._native_policy(
                raw_policy, source="SKILL_LEDGER_MODE"
            )

        raw_policy = config.get("policy")
        if isinstance(raw_policy, str) and raw_policy.strip():
            return SkillLedgerCapability._native_policy(
                raw_policy, source="capability policy"
            )

        if "enable_block" in config:
            return _POLICY_BLOCK if bool(config.get("enable_block")) else _POLICY_DEBUG

        return _DEFAULT_POLICY

    @staticmethod
    def _native_policy(raw_policy: object, *, source: str) -> str:
        policy = normalize_hermes_native_policy(raw_policy)
        normalized_policy = normalize_hook_policy(raw_policy, "")
        if normalized_policy not in {_POLICY_DEBUG, _POLICY_BLOCK}:
            display_policy = (
                raw_policy[:32] if isinstance(raw_policy, str) else raw_policy
            )
            logger.warning(
                "[agent-sec-core] skill-ledger Hermes does not support %s=%r; using observe",
                source,
                display_policy,
            )
        return policy

    def _diagnostic(self, message: str, *args: Any) -> None:
        if self._policy == _POLICY_DEBUG:
            logger.debug(message, *args)
        else:
            logger.warning(message, *args)
