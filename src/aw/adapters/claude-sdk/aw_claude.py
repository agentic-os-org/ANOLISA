"""Explicit Claude SDK factory bridge; native permissions retain their veto."""

from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from dataclasses import replace
from importlib.metadata import version
import json
import logging
import os
from pathlib import Path
import signal
import sys
import tempfile
from typing import Any, AsyncIterator

import claude_agent_sdk
from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient, HookMatcher, PermissionResultDeny

_CLIENT_CREATED = False


class CallbackFailure(RuntimeError):
    """A bounded callback failed independently of the native tool outcome."""


def _deny(reason: str) -> dict[str, Any]:
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }


def _copy_json(value: Any) -> Any:
    # Match the SDK JSON codec's depth support while isolating callback mutations.
    return json.loads(json.dumps(value))


class Bridge:
    """Compose one foreground SDK client with its private AW launch binding."""

    def __init__(self) -> None:
        if version("claude-agent-sdk") != "0.2.88":
            raise RuntimeError("AW requires claude-agent-sdk 0.2.88")
        self.config = json.loads(Path(os.environ["AW_CLAUDE_CONFIG"]).read_text())
        for key in ("executable", "binding", "ready_path", "ready_token"):
            if not isinstance(self.config.get(key), str) or not self.config[key]:
                raise ValueError(f"AW configuration requires {key}")
        hooks = self.config.get("hooks")
        if not isinstance(hooks, list):
            raise ValueError("AW configuration requires hooks")
        for hook in hooks:
            if (
                hook.get("event") not in {"tool.before", "tool.after"}
                or not isinstance(hook.get("step"), str)
                or not hook["step"]
                or hook.get("on_error") not in {"report", "block"}
                or type(hook.get("replace_result")) is not bool
                or type(hook.get("budget_ms")) is not int
                or not 1 <= hook["budget_ms"] <= 55000
            ):
                raise ValueError("Invalid AW hook route")

    async def _invoke(self, hook: dict[str, Any], payload: dict[str, Any]) -> dict[str, Any]:
        encoded = json.dumps(payload, ensure_ascii=False, allow_nan=False).encode()
        if len(encoded) > 1024 * 1024:
            raise CallbackFailure("native_input_limit")
        proc = await asyncio.create_subprocess_exec(
            self.config["executable"],
            "hook",
            "--adapter",
            "claude-sdk",
            "--binding",
            self.config["binding"],
            "--event",
            hook["event"],
            "--step",
            hook["step"],
            "--on-error",
            hook["on_error"],
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            start_new_session=True,
        )
        try:
            stdout, stderr = await asyncio.wait_for(
                proc.communicate(encoded),
                (hook["budget_ms"] + 3000) / 1000,
            )
        except BaseException:
            if proc.returncode is None:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            await proc.wait()
            raise
        if stderr:
            sys.stderr.buffer.write(stderr)
            sys.stderr.buffer.flush()
        if proc.returncode == 2 and hook["event"] == "tool.before":
            reason = json.loads(stdout.strip() or b'"Tool blocked by AW policy"')
            if not isinstance(reason, str):
                raise CallbackFailure("invalid_native_response")
            return _deny(reason)
        if proc.returncode != 0:
            raise CallbackFailure("invalid_native_response")
        result = json.loads(stdout.strip() or b"{}")
        if result == {}:
            return {}
        specific = result.get("hookSpecificOutput", {}) if isinstance(result, dict) else {}
        if not isinstance(specific, dict):
            raise CallbackFailure("invalid_native_response")
        updated = specific.get("updatedToolOutput")
        original = payload.get("tool_response")
        if (
            not hook["replace_result"]
            or payload.get("hook_event_name") != "PostToolUse"
            or payload.get("tool_name") != "Bash"
            or not isinstance(original, dict)
            or not isinstance(updated, dict)
            or not isinstance(updated.get("stdout"), str)
            or {**updated, "stdout": original.get("stdout")} != original
            or result
            != {
                "hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": updated}
            }
        ):
            raise CallbackFailure("invalid_native_response")
        return result

    async def callback(
        self, payload: dict[str, Any], tool_use_id: str | None, context: dict[str, Any]
    ) -> dict[str, Any]:
        """Await configured steps and return a native deny instead of control errors."""
        native = payload.get("hook_event_name")
        event = "tool.before" if native == "PreToolUse" else "tool.after"
        if native not in {"PreToolUse", "PostToolUse", "PostToolUseFailure"}:
            raise ValueError("Unsupported Claude tool event")
        if tool_use_id is not None and payload.get("tool_use_id") != tool_use_id:
            return _deny("AW tool identity mismatch") if event == "tool.before" else {}
        for hook in self.config["hooks"]:
            if hook["event"] != event:
                continue
            try:
                result = await self._invoke(hook, payload)
                if result:
                    return result
            except (
                OSError,
                ValueError,
                RecursionError,
                asyncio.TimeoutError,
                CallbackFailure,
            ) as error:
                logging.getLogger(__name__).error(
                    "AW %s/%s failed: %s; on_error=%s",
                    event,
                    hook["step"],
                    str(error) if isinstance(error, CallbackFailure) else type(error).__name__,
                    hook["on_error"],
                )
                if event == "tool.before" and hook["on_error"] == "block":
                    return _deny("AW hook failed; configured failure action blocked the tool")
        return {}

    def options(self, options: ClaudeAgentOptions) -> ClaudeAgentOptions:
        """Retain existing matchers and vetoes; reject unchecked argument rewrites."""
        if options.setting_sources != [] or options.settings is not None or options.plugins:
            raise ValueError("AW Claude SDK requires setting_sources=[] and no settings/plugins")
        if any(
            key != "strict-mcp-config" or value is not None
            for key, value in options.extra_args.items()
        ):
            raise ValueError(
                "AW Claude SDK extra_args supports only strict-mcp-config without a value"
            )
        if options.cli_path is not None:
            raise ValueError(
                "AW Claude SDK requires its bundled CLI; cli_path overrides unsupported"
            )
        hooks = {event: list(matchers) for event, matchers in (options.hooks or {}).items()}
        before = any(hook["event"] == "tool.before" for hook in self.config["hooks"])
        if before:
            if options.permission_prompt_tool_name is not None:
                raise ValueError("AW tool.before does not support permission_prompt_tool_name")
            if hooks.get("PermissionRequest"):
                raise ValueError("AW tool.before does not support PermissionRequest hooks")
            safe_matchers = []
            for matcher in hooks.get("PreToolUse", []):
                safe_callbacks = []
                for callback in matcher.hooks:

                    async def checked(
                        payload: Any, call_id: Any, context: Any, callback: Any = callback
                    ) -> Any:
                        result = await callback(_copy_json(payload), call_id, context)
                        specific = result.get("hookSpecificOutput", {})
                        if "updatedInput" in specific:
                            return _deny("Tool argument rewriting is unsupported with AW")
                        return result

                    safe_callbacks.append(checked)
                safe_matchers.append(replace(matcher, hooks=safe_callbacks))
            hooks["PreToolUse"] = safe_matchers
        if any(hook["replace_result"] for hook in self.config["hooks"]):
            matchers = []
            for matcher in hooks.get("PostToolUse", []):
                callbacks = []
                for callback in matcher.hooks:

                    async def observed(
                        payload: Any, call_id: Any, context: Any, callback: Any = callback
                    ) -> Any:
                        result = await callback(_copy_json(payload), call_id, context)
                        specific = result.get("hookSpecificOutput", {})
                        if "updatedToolOutput" in specific or "updatedMCPToolOutput" in specific:
                            raise ValueError(
                                "Native output rewriting is unsupported with AW result replacement"
                            )
                        return result

                    callbacks.append(observed)
                matchers.append(replace(matcher, hooks=callbacks))
            hooks["PostToolUse"] = matchers
        for event, native_events in (
            ("tool.before", ["PreToolUse"]),
            ("tool.after", ["PostToolUse", "PostToolUseFailure"]),
        ):
            selected = [hook for hook in self.config["hooks"] if hook["event"] == event]
            if selected:
                timeout = sum(hook["budget_ms"] + 3000 for hook in selected) / 1000 + 1
                for native in native_events:
                    hooks.setdefault(native, []).append(
                        HookMatcher(hooks=[self.callback], timeout=timeout)
                    )
        permission = options.can_use_tool
        if before and permission is not None:
            original_permission = permission

            async def checked_permission(name: Any, tool_input: Any, context: Any) -> Any:
                original = json.dumps(tool_input, sort_keys=True)
                result = await original_permission(name, _copy_json(tool_input), context)
                updated = getattr(result, "updated_input", None)
                if updated is not None and json.dumps(updated, sort_keys=True) != original:
                    return PermissionResultDeny(
                        message="Tool argument rewriting is unsupported with AW"
                    )
                return result

            permission = checked_permission
        bundled = Path(claude_agent_sdk.__file__).parent / "_bundled" / "claude"
        if not bundled.is_file():
            raise ValueError("AW requires the SDK bundled CLI; PATH fallback unsupported")
        return replace(options, hooks=hooks, cli_path=str(bundled), can_use_tool=permission)

    def ready(self) -> None:
        """Publish only after the SDK initialize handshake accepted installed callbacks."""
        path = Path(self.config["ready_path"])
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w", encoding="utf-8", dir=path.parent, prefix=".aw-ready-", delete=False
            ) as output:
                temporary = Path(output.name)
                json.dump(
                    {
                        "version": 1,
                        "adapter": "claude-sdk",
                        "pid": os.getpid(),
                        "token": self.config["ready_token"],
                        "hooks": len(self.config["hooks"]),
                    },
                    output,
                )
            os.replace(temporary, path)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)


@asynccontextmanager
async def create_client(options: ClaudeAgentOptions) -> AsyncIterator[ClaudeSDKClient]:
    """Create one task-scoped SDK client, register AW callbacks and await initialization."""
    global _CLIENT_CREATED
    if _CLIENT_CREATED:
        raise RuntimeError("AW Claude SDK supports one client per foreground worker")
    bridge = Bridge()
    composed = bridge.options(options)
    _CLIENT_CREATED = True
    async with ClaudeSDKClient(options=composed) as client:
        bridge.ready()
        yield client
