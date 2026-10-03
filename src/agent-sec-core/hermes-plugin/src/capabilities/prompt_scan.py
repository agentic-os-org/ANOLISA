"""Prompt-scan capability — scans user input for prompt injection / jailbreak via agent-sec-cli."""

import json
import logging
import os
from typing import Any, Callable

from ..cli_runner import call_agent_sec_cli, trace_context
from ..hook_config import env_flag_enabled
from .base import AgentSecCoreCapability

logger = logging.getLogger("agent-sec-core")

_VALID_SCAN_MODES = {"fast", "standard", "strict"}
_USER_INPUT_SOURCE = "user_input"


def _normalize_scan_mode(raw_mode: str | None) -> str:
    scan_mode = (raw_mode or "standard").strip().lower()
    if scan_mode not in _VALID_SCAN_MODES:
        return "standard"
    return scan_mode


class PromptScanCapability(AgentSecCoreCapability):
    """Scan user input for prompt injection / jailbreak attempts (non-blocking, fail-open)."""

    id = "prompt-scan-user-input"
    name = "Prompt Scanner"

    # ------------------------------------------------------------------
    # Lifecycle & registration
    # ------------------------------------------------------------------

    def __init__(self) -> None:
        super().__init__()
        self._hook_enabled: bool = True
        raw_scan_mode = os.environ.get("PROMPT_SCANNER_SCAN_MODE")
        self._scan_mode = _normalize_scan_mode(raw_scan_mode)
        logger.info(
            "Prompt scanner scan mode: raw=%r, effective=%r",
            raw_scan_mode,
            self._scan_mode,
        )

    def _on_register(self, config: dict[str, Any]) -> None:
        """Read prompt-scan specific config."""
        self._hook_enabled = env_flag_enabled("PROMPT_SCANNER_HOOK_ENABLED", True)
        if "warning_ttl_seconds" in config:
            logger.warning(
                "[agent-sec-core] prompt-scan warning_ttl_seconds is ignored; "
                "Hermes synthetic warning delivery was removed"
            )

    def get_hooks_define(self) -> dict[str, Callable[..., Any]]:
        return {"pre_llm_call": self._on_pre_llm_call}

    # ------------------------------------------------------------------
    # Hook handlers
    # ------------------------------------------------------------------

    def _on_pre_llm_call(self, messages: Any = None, **kwargs: Any) -> None:
        """Scan the current user input before the LLM turn starts."""
        if not self._hook_enabled:
            return None

        user_text = self._extract_user_text(messages, kwargs)
        if not user_text.strip():
            return None

        scan = self._scan_text(user_text, trace_context(kwargs))
        if scan is None:
            return None

        verdict = self._safe_string(scan.get("verdict")) or "pass"

        if verdict == "pass":
            logger.info(f"[agent-sec-core] {self.id} PASS")
            return None

        if verdict == "error":
            logger.warning(
                f"[agent-sec-core] {self.id} agent-sec-cli returned verdict=error, fail-open"
            )
            return None

        if verdict not in {"warn", "deny"}:
            logger.warning(
                f"[agent-sec-core] {self.id} UNKNOWN verdict={verdict}, fail-open"
            )
            return None

        threat_type = self._safe_string(scan.get("threat_type")) or "unknown"
        risk_level = self._safe_string(scan.get("risk_level")) or "unknown"
        logger.warning(
            f"[agent-sec-core] {self.id} {verdict.upper()} observed "
            f"threat_type={threat_type[:32]} risk_level={risk_level[:32]}"
        )
        return None

    # ------------------------------------------------------------------
    # CLI invocation
    # ------------------------------------------------------------------

    def _scan_text(
        self,
        text: str,
        security_trace_context: dict[str, str] | None,
    ) -> dict[str, Any] | None:
        """Run agent-sec-cli scan-prompt and parse its JSON output.

        The prompt text is piped via stdin instead of being passed as an
        ``--text`` argv to avoid two issues:
        1. ARG_MAX (~2MB on Linux) — large RAG-injected / multi-turn prompts
           would trigger E2BIG and silently fail-open.
        2. ``ps aux`` / ``/proc/<pid>/cmdline`` leakage — argv is world-readable
           on the same host while the subprocess is alive.
        """
        args = [
            "scan-prompt",
            "--mode",
            self._scan_mode,
            "--format",
            "json",
            "--source",
            _USER_INPUT_SOURCE,
        ]

        result = call_agent_sec_cli(
            args,
            timeout=self._timeout,
            stdin=text,
            trace_context=security_trace_context,
        )
        if result.exit_code != 0:
            logger.warning(
                f"[agent-sec-core] {self.id} agent-sec-cli exit_code={result.exit_code}, fail-open"
            )
            return None

        try:
            scan = json.loads(result.stdout)
        except (json.JSONDecodeError, ValueError):
            logger.warning(
                f"[agent-sec-core] {self.id} agent-sec-cli returned invalid JSON, fail-open"
            )
            return None

        if not isinstance(scan, dict):
            logger.warning(
                f"[agent-sec-core] {self.id} agent-sec-cli returned non-object JSON, fail-open"
            )
            return None
        return scan

    # ------------------------------------------------------------------
    # Input extraction helpers
    # ------------------------------------------------------------------

    def _extract_user_text(self, messages: Any, kwargs: dict[str, Any]) -> str:
        """Extract only the current user input from Hermes hook payloads."""
        for key in ("user_message", "user_input", "prompt"):
            value = kwargs.get(key)
            if isinstance(value, str) and value.strip():
                return value

        if not isinstance(messages, list):
            return ""

        for message in reversed(messages):
            role = self._message_value(message, "role")
            if role != "user":
                continue
            return self._content_to_text(self._message_value(message, "content"))
        return ""

    def _content_to_text(self, content: Any) -> str:
        """Convert common message content shapes to text."""
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            parts: list[str] = []
            for item in content:
                if isinstance(item, str):
                    parts.append(item)
                    continue
                text = self._message_value(item, "text")
                if isinstance(text, str):
                    parts.append(text)
            return "\n".join(parts)
        return ""

    # ------------------------------------------------------------------
    # Misc helpers
    # ------------------------------------------------------------------

    def _message_value(self, message: Any, key: str) -> Any:
        """Read a key from dict-like or object-like messages."""
        if isinstance(message, dict):
            return message.get(key)
        return getattr(message, key, None)

    def _safe_string(self, value: Any) -> str:
        return value if isinstance(value, str) else ""
