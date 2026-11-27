"""Explicit Claude SDK factory bridge; native permissions retain their veto."""

from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from contextvars import ContextVar
from dataclasses import replace
from hashlib import sha256
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
_stop_delivery: ContextVar[str | None] = ContextVar("aw_claude_stop_delivery", default=None)


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

    def __init__(
        self, config: dict[str, Any] | None = None, *, launch_bound: bool = True
    ) -> None:
        self.config = config if config is not None else json.loads(Path(os.environ["AW_CLAUDE_CONFIG"]).read_text())
        if launch_bound:
            self._validate_launch()
        self.set_routes(self.config.get("hooks"))
        self.turn_id: str | None = None
        self._stop_turn: str | None = None
        self._stop_replies: dict[str, tuple[bytes, asyncio.Future[dict[str, Any]]]] = {}
        self.session_id: str | None = None
        self.final = None
        if self.config.get("final_profile"):
            from aw_final import FinalGate

            self.final = FinalGate(self.config)

    def _validate_launch(self) -> None:
        for key in ("executable", "binding", "ready_path", "ready_token"):
            if not isinstance(self.config.get(key), str) or not self.config[key]:
                raise ValueError(f"AW configuration requires {key}")
    def set_routes(self, hooks: list[dict[str, Any]]) -> None:
        """Install the immutable Adapter projection for this client."""
        if not isinstance(hooks, list):
            raise ValueError("AW configuration requires hooks")
        for hook in hooks:
            if (
                hook.get("event") not in {"session.start", "input.submit", "tool.before", "tool.after", "security.violation",
                                          "turn.stop", "session.end"}
                or not isinstance(hook.get("step"), str)
                or not hook["step"]
                or hook.get("on_error") not in {"report", "block"}
                or type(hook.get("replace_result")) is not bool
                or type(hook.get("budget_ms")) is not int
                or not 1 <= hook["budget_ms"] <= 55000
            ):
                raise ValueError("Invalid AW hook route")
        self.config = {**self.config, "hooks": hooks}

    async def _transport(self, hook: dict[str, Any], payload: dict[str, Any]) -> tuple[int, bytes]:
        encoded = json.dumps(payload, ensure_ascii=False, allow_nan=False).encode()
        if len(encoded) > 1024 * 1024:
            raise CallbackFailure("native_input_limit")
        close = ["--close-event", "true"] if hook.get("close_event") else []
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
            *close,
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
        return proc.returncode, stdout

    async def _invoke(self, hook: dict[str, Any], payload: dict[str, Any]) -> dict[str, Any]:
        code, stdout = await self._transport(hook, payload)
        if code == 2 and hook["event"] == "tool.before":
            reason = json.loads(stdout.strip() or b'"Tool blocked by AW policy"')
            if not isinstance(reason, str):
                raise CallbackFailure("invalid_native_response")
            return _deny(reason)
        if code != 0:
            raise CallbackFailure("invalid_native_response")
        result = json.loads(stdout.strip() or b"{}")
        if result == {}:
            return {}
        specific = result.get("hookSpecificOutput", {}) if isinstance(result, dict) else {}
        if not isinstance(specific, dict):
            raise CallbackFailure("invalid_native_response")
        updated_input = specific.get("updatedInput")
        if (hook["event"] == "tool.before" and isinstance(updated_input, dict)
                and result == {"hookSpecificOutput": {
                    "hookEventName": "PreToolUse", "updatedInput": updated_input}}):
            return result
        updated = specific.get("updatedToolOutput")
        original = payload.get("tool_response")
        # Native MCP tools adopt a string replacement of the whole
        # model-visible projection; the CLI keeps its own error state.
        if (
            not hook["replace_result"]
            or payload.get("hook_event_name") != "PostToolUse"
            or result
            != {
                "hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": updated}
            }
        ):
            raise CallbackFailure("invalid_native_response")
        if str(payload.get("tool_name", "")).startswith("mcp__") and isinstance(
            original, (str, list)
        ):
            if not isinstance(updated, str):
                raise CallbackFailure("invalid_native_response")
            return result
        if (
            payload.get("tool_name") != "Bash"
            or not isinstance(original, dict)
            or not isinstance(updated, dict)
            or not isinstance(updated.get("stdout"), str)
            or {**updated, "stdout": original.get("stdout")} != original
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
        if self.final is not None and event == "tool.after":
            self.final.record(payload)
        if (self.final is not None and self.final.enforcing and event == "tool.before"
                and not self.final.supports_tool(payload)):
            return _deny("AW final gate does not cover this tool delivery path")
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
                # A transport exception or malformed result carries no completed
                # post-tool decision. Resident clients must fence that invocation.
                result = self.failure_reply(event, {} if event == "tool.after" else hook)
                if result:
                    return result
        return {}

    async def _observe_session(self, event: str, native: str, cwd: str, **fields: str) -> None:
        payload = {"hook_event_name": native, "session_id": self.session_id, "cwd": cwd, **fields}
        for hook in self.config["hooks"]:
            if hook["event"] == event:
                try:
                    code, stdout = await self._transport(hook, payload)
                    if code != 0 or stdout.strip() not in (b"", b"{}"):
                        raise CallbackFailure("invalid_native_response")
                except (OSError, ValueError, TimeoutError, RuntimeError) as error:
                    logging.getLogger(__name__).error("AW %s/%s failed: %s", event, hook["step"], error)

    async def session_initialized(self, message: Any, cwd: str) -> None:
        if self.session_id is not None:
            return
        from claude_agent_sdk import SystemMessage

        if isinstance(message, SystemMessage) and message.subtype == "init":
            session = message.data.get("session_id")
            if isinstance(session, str) and session:
                self.session_id = session
                await self._observe_session("session.start", "AWSessionInit", cwd)

    async def session_disconnected(self, cwd: str, reason: str) -> None:
        if self.session_id is not None:
            await self._observe_session("session.end", "AWSessionDisconnect", cwd, reason=reason)
            self.session_id = None

    def bind_stop_deliveries(self, query: Any) -> None:
        if not any(hook["event"] == "turn.stop" for hook in self.config["hooks"]):
            return
        # Claude SDK 0.2.88 drops the control request ID before calling HookMatcher.
        # Preserve it on this unshared Query, after connection and before any prompt.
        handler = getattr(query, "_handle_control_request", None)
        if not callable(handler):
            raise CallbackFailure("Claude SDK cannot expose native Stop delivery identity")

        async def with_delivery(frame: dict[str, Any]) -> Any:
            request = frame["request"]
            if (
                request["subtype"] != "hook_callback"
                or (request.get("input") or {}).get("hook_event_name") != "Stop"
            ):
                return await handler(frame)
            token = _stop_delivery.set(frame.get("request_id"))
            try:
                return await handler(frame)
            finally:
                _stop_delivery.reset(token)

        query._handle_control_request = with_delivery

    def _stop_failure(self) -> dict[str, Any]:
        if any(hook["event"] == "turn.stop" and hook["on_error"] == "block"
               for hook in self.config["hooks"]):
            return {"decision": "block", "reason": "AW turn stop check failed"}
        return {}

    async def stop_callback(
        self, payload: dict[str, Any], _tool_use_id: str | None, _context: dict[str, Any]
    ) -> dict[str, Any]:
        """Return the Stop decision to Claude before the terminal result is emitted."""
        delivery = _stop_delivery.get()
        turn = self.turn_id
        if turn is None or not isinstance(delivery, str) or not delivery or len(delivery) > 512:
            logging.getLogger(__name__).error("AW Stop has no active turn or SDK delivery ID")
            return self._stop_failure()
        if self._stop_turn != turn:
            self._stop_turn = turn
            self._stop_replies.clear()
        fingerprint = sha256(json.dumps(payload, sort_keys=True, allow_nan=False).encode()).digest()
        previous = self._stop_replies.get(delivery)
        if previous is not None:
            original, reply = previous
            if original != fingerprint:
                return self._stop_failure()
            return dict(await asyncio.shield(reply))
        # The durable receipt fences replay after a completed cache entry is evicted.
        if len(self._stop_replies) >= 64:
            completed = next((key for key, (_, reply) in self._stop_replies.items()
                              if reply.done()), None)
            if completed is None:
                return self._stop_failure()
            del self._stop_replies[completed]
        reply = asyncio.get_running_loop().create_future()
        self._stop_replies[delivery] = (fingerprint, reply)
        native = {**payload, "turn_id": turn, "attempt_id": delivery,
                  "delivery_id": delivery, "reason": "stop_requested"}
        try:
            result = await self._check_stop(native)
        except BaseException:
            reply.set_result(self._stop_failure())
            raise
        reply.set_result(result)
        return dict(result)

    async def _check_stop(self, native: dict[str, Any]) -> dict[str, Any]:
        for hook in self.config["hooks"]:
            if hook["event"] != "turn.stop":
                continue
            try:
                code, stdout = await self._transport(hook, native)
                if code == 2:
                    reason = json.loads(stdout or b'"AW policy requires the turn to continue"')
                    if not isinstance(reason, str) or not reason:
                        raise CallbackFailure("invalid_native_response")
                    return {"decision": "block", "reason": reason}
                if code != 0 or stdout.strip() not in (b"", b"{}"):
                    raise CallbackFailure("invalid_native_response")
            except (OSError, ValueError, TimeoutError, RuntimeError) as error:
                logging.getLogger(__name__).error("AW turn.stop/%s failed: %s", hook["step"], error)
                if hook["on_error"] == "block":
                    return {"decision": "block", "reason": "AW turn stop check failed"}
        return {}

    def failure_reply(self, event: str, hook: dict[str, Any]) -> dict[str, Any]:
        if event == "tool.before" and hook["on_error"] == "block":
            return _deny("AW hook failed; configured failure action blocked the tool")
        return {}

    def options(self, options: ClaudeAgentOptions) -> ClaudeAgentOptions:
        """Compose SDK callbacks; CLI-loaded Hooks retain their native scheduling."""
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
        final_enforcing = self.final is not None and self.final.enforcing
        before = final_enforcing or any(hook["event"] == "tool.before" for hook in self.config["hooks"])
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
            if selected or final_enforcing or (event == "tool.after" and self.final is not None):
                timeout = sum(hook["budget_ms"] + 3000 for hook in selected) / 1000 + 1
                for native in native_events:
                    hooks.setdefault(native, []).append(
                        HookMatcher(hooks=[self.callback], timeout=timeout)
                    )
        stops = [hook for hook in self.config["hooks"] if hook["event"] == "turn.stop"]
        if stops:
            timeout = sum(hook["budget_ms"] + 3000 for hook in stops) / 1000 + 1
            hooks.setdefault("Stop", []).append(HookMatcher(hooks=[self.stop_callback], timeout=timeout))
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
        composed = replace(options, hooks=hooks, cli_path=str(bundled), can_use_tool=permission)
        return self.final.options(composed, bundled) if self.final is not None else composed

    def ready(self) -> None:
        """Publish only after the SDK initialize handshake accepted installed callbacks."""
        if self.final is not None:
            self.final.ready()
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
async def _create_task_client(options: ClaudeAgentOptions) -> AsyncIterator[ClaudeSDKClient]:
    """Create one task-scoped SDK client, register AW callbacks and await initialization."""
    global _CLIENT_CREATED
    if _CLIENT_CREATED:
        raise RuntimeError("AW Claude SDK supports one client per foreground worker")
    bridge = Bridge()
    try:
        composed = bridge.options(options)
        _CLIENT_CREATED = True
        if any(hook["event"] in {"session.start", "input.submit", "turn.stop", "session.end"}
               for hook in bridge.config["hooks"]):
            from aw_foreground import CheckedClient
            client = CheckedClient(composed, bridge)
        else:
            client = ClaudeSDKClient(options=composed)
        async with client:
            bridge.ready()
            yield client
    finally:
        if bridge.final is not None:
            bridge.final.close()


def create_client(options: ClaudeAgentOptions) -> Any:
    """Use the public client factory in task workers and resident AW hosts."""
    if os.environ.get("AW_CLAUDE_RESIDENT") == "1":
        if __package__:
            from .aw_resident import ManagedClient
        else:
            from aw_resident import ManagedClient
        return ManagedClient(options)
    return _create_task_client(options)


def create_sdk_mcp_server(name: str, version: str = "1.0.0", tools: Any = None) -> Any:
    """Create an explicitly managed in-process MCP server for a resident task client."""
    if os.environ.get("AW_CLAUDE_RESIDENT") != "1":
        raise RuntimeError("Managed MCP requires the AW resident entrypoint")
    if __package__:
        from .aw_mcp import create_sdk_mcp_server as factory
    else:
        from aw_mcp import create_sdk_mcp_server as factory
    return factory(name, version, tools)


if __name__ == "__main__":
    if __package__:
        from .aw_resident import main
    else:
        from aw_resident import main
    main()
