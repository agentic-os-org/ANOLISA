"""PII-scan capability for Hermes input and tool lifecycle hooks."""

from __future__ import annotations

import json
import logging
import os
from typing import Any

from ..cli_runner import call_agent_sec_cli, trace_context
from ..hook_config import (  # noqa: TID252 - Hermes loads this as a standalone package.
    env_flag_enabled,
    normalize_hermes_native_policy,
    normalize_hook_policy,
)
from ..pii_text import extract_user_text, value_to_text
from .base import AgentSecCoreCapability

logger = logging.getLogger("agent-sec-core")

_USER_INPUT_SOURCE = "user_input"
_TOOL_INPUT_SOURCE = "tool_input"
_TOOL_OUTPUT_SOURCE = "tool_output"
_MODEL_OUTPUT_SOURCE = "model_output"


class PiiScanCapability(AgentSecCoreCapability):
    """Scan Hermes input and tool boundaries and enforce the configured policy."""

    id = "pii-scan-user-input"
    name = "PII Checker"

    def __init__(self):
        super().__init__()
        self._hook_enabled = True
        self._policy = "observe"
        self._include_low_confidence = False

    def _on_register(self, config: dict) -> None:
        """Read pii-scan specific config."""
        self._hook_enabled = env_flag_enabled("PII_CHECKER_HOOK_ENABLED", True)
        if "PII_CHECKER_MODE" in os.environ:
            raw_policy = os.environ.get("PII_CHECKER_MODE")
            source = "PII_CHECKER_MODE"
        else:
            raw_policy = config.get("policy")
            source = "capability policy"
        self._policy = normalize_hermes_native_policy(raw_policy)
        normalized_policy = normalize_hook_policy(raw_policy, "")
        if raw_policy is not None and normalized_policy not in {"observe", "block"}:
            display_policy = (
                raw_policy[:32] if isinstance(raw_policy, str) else raw_policy
            )
            logger.warning(
                "[agent-sec-core] pii-checker Hermes does not support %s=%r; using observe",
                source,
                display_policy,
            )
        self._include_low_confidence = bool(config.get("include_low_confidence", False))
        if "warning_ttl_seconds" in config:
            logger.warning(
                "[agent-sec-core] pii-checker warning_ttl_seconds is ignored; "
                "Hermes synthetic warning delivery was removed"
            )

    def get_hooks_define(self) -> dict:
        return {
            "pre_llm_call": self._on_pre_llm_call,
            "pre_tool_call": self._on_pre_tool_call,
            "post_tool_call": self._on_post_tool_call,
            "post_llm_call": self._on_post_llm_call,
        }

    def _on_pre_llm_call(self, messages=None, **kwargs):
        """Scan the current user input before the LLM turn starts."""
        if not self._hook_enabled:
            return None

        user_text = extract_user_text(messages, kwargs)
        if not user_text.strip():
            return None

        self._scan_and_handle(
            user_text,
            source=_USER_INPUT_SOURCE,
            can_block=False,
            security_trace_context=trace_context(kwargs),
        )
        return None

    def _on_post_llm_call(
        self,
        assistant_response: Any = None,
        **kwargs: Any,
    ) -> None:
        """Audit the finalized model output without changing its delivery."""
        if not self._hook_enabled:
            return None
        text = self._value_to_text(assistant_response)
        if not text.strip():
            return None
        self._scan_and_handle(
            text,
            source=_MODEL_OUTPUT_SOURCE,
            can_block=False,
            security_trace_context=trace_context(kwargs),
        )
        return None

    def _on_pre_tool_call(
        self,
        *,
        tool_name: Any,
        args: Any,
        **kwargs: Any,
    ) -> dict[str, str] | None:
        """Scan tool arguments before execution."""
        if not self._hook_enabled:
            return None
        text = self._value_to_text(args)
        if not text.strip():
            return None
        data = {"tool_name": tool_name, "args": args, **kwargs}
        return self._scan_and_handle(
            text,
            source=_TOOL_INPUT_SOURCE,
            can_block=True,
            security_trace_context=trace_context(data),
        )

    def _on_post_tool_call(
        self,
        *,
        tool_name: Any,
        args: Any,
        result: Any,
        **kwargs: Any,
    ):
        """Scan tool output after execution."""
        if not self._hook_enabled:
            return None
        text = self._value_to_text(result)
        if not text.strip():
            return None
        data = {"tool_name": tool_name, "args": args, "result": result, **kwargs}
        self._scan_and_handle(
            text,
            source=_TOOL_OUTPUT_SOURCE,
            can_block=False,
            security_trace_context=trace_context(data),
        )
        return None

    def _scan_text(
        self,
        text: str,
        *,
        source: str,
        security_trace_context: dict[str, str] | None,
    ) -> dict[str, Any] | None:
        """Run agent-sec-cli scan-pii and parse its JSON output."""
        args = [
            "scan-pii",
            "--stdin",
            "--format",
            "json",
            "--source",
            source,
        ]
        if self._include_low_confidence:
            args.append("--include-low-confidence")

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

    def _scan_and_handle(
        self,
        text: str,
        *,
        source: str,
        can_block: bool,
        security_trace_context: dict[str, str] | None,
    ) -> dict[str, str] | None:
        """Scan text and return a native block where Hermes supports one."""
        scan = self._scan_text(
            text,
            source=source,
            security_trace_context=security_trace_context,
        )
        if scan is None:
            return

        verdict = self._safe_string(scan.get("verdict")) or "pass"
        findings = self._as_list(scan.get("findings"))

        if verdict == "pass" or not findings:
            logger.info(f"[agent-sec-core] {self.id} PASS source={source}")
            return

        if verdict not in {"warn", "deny"}:
            logger.warning(
                f"[agent-sec-core] {self.id} UNKNOWN verdict={verdict}, fail-open"
            )
            return

        if self._policy == "observe":
            logger.info(
                f"[agent-sec-core] {self.id} {verdict.upper()} observed source={source}"
            )
            return

        if can_block and self._policy == "block" and verdict == "deny":
            message = self._format_pii_message(
                verdict,
                findings,
                outcome="当前策略已阻断本次工具调用。",
                summary=scan.get("summary"),
            )
            logger.warning(
                f"[agent-sec-core] {self.id} {verdict.upper()} blocked source={source}"
            )
            return {"action": "block", "message": message}

        logger.warning(
            f"[agent-sec-core] {self.id} {verdict.upper()} observed at non-blocking "
            f"boundary source={source}"
        )
        return None

    def _value_to_text(self, value: Any) -> str:
        """Convert arbitrary hook values into scan text."""
        return value_to_text(value)

    def _format_pii_message(
        self, verdict: str, findings: list[Any], *, outcome: str, summary: Any = None
    ) -> str:
        """Build a concise block message without exposing scanner-internal fields."""
        typed_findings = [item for item in findings if isinstance(item, dict)]
        return f"[pii-checker] {self._risk_summary(verdict, typed_findings, summary=summary)}；{outcome}"

    def _risk_summary(
        self, verdict: str, findings: list[dict[str, Any]], summary: Any = None
    ) -> str:
        """Summarize per-finding risk without exposing internal labels."""
        if isinstance(summary, dict) and summary.get("findings_truncated") is True:
            counts = summary.get("by_severity")
            if isinstance(counts, dict):
                high, general = counts.get("deny", 0), counts.get("warn", 0)
                total = summary.get("total")
                if (
                    all(
                        type(value) is int and value >= 0
                        for value in (high, general, total)
                    )
                    and total == high + general
                ):
                    return (
                        f"检测到 {total} 项敏感信息"
                        f"（高风险 {high}、一般风险 {general}；明细已省略）"
                    )
        high_count = sum(
            1 for finding in findings if self._finding_risk(finding, verdict) == "high"
        )
        general_count = len(findings) - high_count

        if high_count and general_count:
            return (
                f"检测到 {len(findings)} 项敏感信息"
                f"（高风险 {high_count}、一般风险 {general_count}）"
            )
        if high_count:
            return f"检测到 {high_count} 项高风险敏感信息"
        return f"检测到 {general_count} 项一般风险敏感信息"

    def _finding_risk(self, finding: dict[str, Any], verdict: str) -> str:
        """Map a finding severity to its user-facing risk bucket."""
        severity = self._safe_string(finding.get("severity"))
        if severity == "deny":
            return "high"
        if severity == "warn":
            return "general"
        return "high" if verdict == "deny" else "general"

    def _as_list(self, value) -> list[Any]:
        return value if isinstance(value, list) else []

    def _safe_string(self, value) -> str:
        return value if isinstance(value, str) else ""
