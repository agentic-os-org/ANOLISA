"""Code-scan capability — scans terminal/execute_code via agent-sec-cli."""

from __future__ import annotations

import json
import logging
import os

from ..cli_runner import call_agent_sec_cli, trace_context
from ..hook_config import (  # noqa: TID252 - standalone plugin package
    env_flag_enabled,
    normalize_hook_policy,
)
from .base import AgentSecCoreCapability

logger = logging.getLogger("agent-sec-core")

# Mapping: tool_name -> (args_key, language)
_TOOL_LANGUAGE_MAP = {
    "terminal": ("command", "bash"),
    "execute_code": ("code", "python"),
}


class CodeScanCapability(AgentSecCoreCapability):
    """Security capability that scans code before execution.

    Intercepts pre_tool_call for 'terminal' and 'execute_code' tools,
    sends the code to agent-sec-cli scan-code, and blocks on deny/warn verdicts.
    """

    id = "code-scan"
    name = "Code Scanner"

    def _on_register(self, config: dict) -> None:
        """Read code-scan specific config."""
        fallback_policy = "block" if config.get("enable_block", False) else "observe"
        raw_policy = os.environ.get("CODE_SCANNER_MODE")
        policy = normalize_hook_policy(raw_policy, "")
        if raw_policy is not None and policy not in {"observe", "block"}:
            logger.warning(
                "[agent-sec-core] code-scan invalid or unsupported CODE_SCANNER_MODE=%r; using %s",
                raw_policy[:32],
                fallback_policy,
            )
        self._policy = policy if policy in {"observe", "block"} else fallback_policy
        self._hook_enabled = env_flag_enabled("CODE_SCANNER_HOOK_ENABLED", True)

    def get_hooks_define(self) -> dict:
        return {"pre_tool_call": self._on_pre_tool_call}

    def _on_pre_tool_call(self, tool_name, args, **kwargs):
        """Hook handler: scan terminal/execute_code for security risks."""
        if not getattr(self, "_hook_enabled", True):
            return None

        # 1. Only intercept known tools
        tool_info = _TOOL_LANGUAGE_MAP.get(tool_name)
        if tool_info is None:
            return None

        args_key, language = tool_info

        # 2. Extract code content
        code = (args or {}).get(args_key, "").strip()
        if not code:
            return None

        # 3. Call agent-sec-cli scan-code
        result = call_agent_sec_cli(
            ["scan-code", "--code", code, "--language", language],
            timeout=self._timeout,
            trace_context=trace_context(kwargs),
        )

        # 4. Parse result (fail-open on errors)
        if result.exit_code != 0:
            logger.warning(
                f"[agent-sec-core] {self.id} agent-sec-cli exit_code={result.exit_code}, fail-open tool={tool_name} code={code[:120]}"
            )
            return None

        try:
            scan = json.loads(result.stdout)
        except (json.JSONDecodeError, ValueError):
            logger.warning(
                f"[agent-sec-core] {self.id} agent-sec-cli returned invalid JSON, fail-open tool={tool_name} code={code[:120]}"
            )
            return None

        # Self-protect: force block if the command would disable this plugin
        findings = scan.get("findings", [])
        self_protect = next(
            (f for f in findings if f.get("rule_id") == "shell-self-protect-hermes"),
            None,
        )
        if self_protect:
            msg = (
                "[agent-sec-core] 自我保护：该命令将禁用 agent-sec 安全插件。"
                "如果您确实需要禁用，请手动执行以下命令：\n\n"
                f"  {code}\n\n"
                "出于安全原因，AI agent 无法执行此操作。"
            )
            logger.warning(
                f"[agent-sec-core] {self.id} SELF-PROTECT block tool={tool_name} code={code[:120]}"
            )
            return {"action": "block", "message": msg}

        verdict = scan.get("verdict", "pass")

        # warn and deny are separate branches (coding convention), same behavior
        if verdict == "deny":
            msg = self._format_message(scan)
            logger.warning(
                f"[agent-sec-core] {self.id} DENY tool={tool_name} code={code[:120]} | {msg}"
            )
            if self._policy == "block":
                return {"action": "block", "message": msg}
            return None

        if verdict == "warn":
            msg = self._format_message(scan)
            logger.warning(
                f"[agent-sec-core] {self.id} WARN tool={tool_name} code={code[:120]} | {msg}"
            )
            if self._policy == "block":
                return {"action": "block", "message": msg}
            return None

        if verdict == "pass":
            logger.info(
                f"[agent-sec-core] {self.id} PASS tool={tool_name} code={code[:120]}"
            )
            return None

        # verdict == "error" — scanner itself failed, fail-open
        if verdict == "error":
            logger.warning(
                f"[agent-sec-core] {self.id} agent-sec-cli returned verdict=error, fail-open tool={tool_name} code={code[:120]}"
            )
            return None

        # unknown verdict — defensive fallback, fail-open
        logger.warning(
            f"[agent-sec-core] {self.id} UNKNOWN verdict={verdict} tool={tool_name} code={code[:120]}"
        )
        return None

    def _format_message(self, scan: dict) -> str:
        """Format scan-code result into a human-readable block message."""
        summary = scan.get("summary", "")
        findings = scan.get("findings", [])
        lines = [
            (
                f"[agent-sec-core] {summary}"
                if summary
                else "[agent-sec-core] Code scan blocked"
            )
        ]
        for f in findings:
            rule_id = f.get("rule_id", "?")
            desc = f.get("desc_zh") or f.get("desc_en", "")
            lines.append(f"  - {rule_id}: {desc}")
        return "\n".join(lines)
