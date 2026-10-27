"""Resident Claude hosts admit isolated task clients through public SDK methods."""

from __future__ import annotations

import asyncio
from contextlib import aclosing
from dataclasses import replace
import os
from pathlib import Path
import runpy
import signal
import sys
from typing import Any, AsyncIterable, AsyncIterator

import anyio
from claude_agent_sdk import (
    ClaudeAgentOptions,
    ClaudeSDKClient,
    PermissionResultDeny,
    ResultMessage,
)
from aw_claude import Bridge, CallbackFailure, _deny
from aw_scope import Scope
from aw_invocations import Invocations
from aw_model import ModelRelay
from aw_transport import ManagedTransport

_active: set[ManagedClient] = set()
_poisoned = False


def fail_stop() -> None:
    """Kill the AW-owned foreground group when SDK interruption cannot be acknowledged."""
    group = int(os.environ["AW_CLAUDE_RESIDENT_GROUP"])
    if group != os.getpgrp():
        raise RuntimeError("Resident SDK host left its owned process group")
    # main admits only the foreground group created by aw-exec. Forked workers
    # retain that group; escalation terminates their shared host and SDK children.
    os.killpg(group, signal.SIGKILL)


class ResidentBridge(Bridge):
    """A failed managed callback stops delivery and permanently poisons its task."""

    def __init__(self) -> None:
        super().__init__()
        self.scope = Scope(self.config)
        self.failure: str | None = None
        self.mcp_names: set[str] = set()
        self.mcp_servers: set[str] = set()
        self.invocations = Invocations(self)
        self.stop = None
        self.stopped = False
        self.stop_future: asyncio.Task[None] | None = None

    async def _transport(self, hook: dict[str, Any], payload: dict[str, Any]) -> tuple[int, bytes]:
        return await self.scope.invoke(hook, payload)

    def failure_reply(self, event: str, hook: dict[str, Any]) -> dict[str, Any]:
        self.failure = "AW managed task stopped after a callback failure"
        if event == "tool.before":
            return {**_deny(self.failure), "continue_": False, "stopReason": self.failure}
        return {"continue_": False, "stopReason": self.failure}

    async def callback(
        self, payload: dict[str, Any], tool_use_id: str | None, context: dict[str, Any]
    ) -> dict[str, Any]:
        event = "tool.before" if payload.get("hook_event_name") == "PreToolUse" else "tool.after"
        try:
            self.scope.check()
            if self.failure or (
                tool_use_id is not None and payload.get("tool_use_id") != tool_use_id
            ):
                raise CallbackFailure("task identity or lifecycle failure")
            # Managed handlers own both AW phases; preserve caller/native Hooks
            # while avoiding a second invocation with incomplete MCP error data.
            if payload.get("tool_name") in self.mcp_names:
                return {}
            phase = "before" if event == "tool.before" else "after"
            entry = self.invocations.claim(
                tool_use_id, payload["tool_name"], payload["tool_input"], phase
            )
            reply = await super().callback(payload, tool_use_id, context)
            denied = reply.get("hookSpecificOutput", {}).get("permissionDecision") == "deny"
            if phase == "after" or denied:
                await self.invocations.finish(entry)
            else:
                entry.phase = "allowed"
        except Exception:
            reply = self.failure_reply(event, {})
        if self.failure:
            await self.stop_task()
        return reply

    async def stop_task(self) -> None:
        # The CLI cancels its Hook callback when interruption succeeds. Keep the
        # acknowledgement waiter outside that callback task and join it on close.
        self.request_stop()
        await asyncio.shield(self.stop_future)

    def request_stop(self) -> None:
        if self.stop_future is None:
            self.stop_future = asyncio.create_task(self._stop_task())

    async def _stop_task(self) -> None:
        try:
            async with asyncio.timeout(5):
                await self.stop()
            self.stopped = True
        except BaseException as error:
            print(
                f"AW interrupt acknowledgement failed: {type(error).__name__}",
                file=sys.stderr,
                flush=True,
            )
            fail_stop()
            raise


class ManagedClient(ClaudeSDKClient):
    """A public SDK client retains its conversation across drained executions."""

    def __init__(self, options: ClaudeAgentOptions) -> None:
        if sys.version_info < (3, 11):
            raise RuntimeError("AW resident Claude SDK requires Python >=3.11")
        if int(os.environ["AW_CLAUDE_RESIDENT_GROUP"]) != os.getpgrp():
            raise RuntimeError("Resident clients require the AW-owned foreground group")
        self._aw_pid = os.getpid()
        self._aw_bridge = ResidentBridge()
        self._aw_bridge.stop = self.interrupt
        self._aw_cwd = str(Path(options.cwd or os.getcwd()).resolve())
        self._aw_environment = {**os.environ, **options.env}
        self._aw_state = "new"
        self._aw_task_started = False
        self._aw_relay = ModelRelay(self._aw_bridge, self._aw_environment)
        self._aw_query_finished = False
        self._aw_receiving = False
        self._aw_input_task = None
        self._aw_lock = asyncio.Lock()
        self._aw_completion = asyncio.Lock()
        self._aw_connect_task: asyncio.Task[Any] | None = None
        from aw_mcp import claim_servers, snapshot_servers

        composed = self._aw_bridge.options(
            replace(
                options,
                cwd=self._aw_cwd,
                env=dict(options.env),
                mcp_servers=snapshot_servers(options.mcp_servers),
            )
        )
        # Track caller hooks as well as AW hooks: SDK cancellation removes its
        # in-flight index before a callback's asynchronous finally has drained.
        for event in ("PreToolUse", "PostToolUse", "PostToolUseFailure"):
            if not any(
                self._aw_bridge.callback in matcher.hooks
                for matcher in composed.hooks.get(event, [])
            ):
                from claude_agent_sdk import HookMatcher

                composed.hooks.setdefault(event, []).append(
                    HookMatcher(hooks=[self._aw_bridge.callback], timeout=60)
                )

        def tracked(callback: Any, event: str | None = None) -> Any:
            async def invoke(*args: Any, **kwargs: Any) -> Any:
                async with self._aw_bridge.invocations.callback():
                    try:
                        result = await callback(*args, **kwargs)
                        if isinstance(result, PermissionResultDeny) or (
                            event == "PreToolUse"
                            and isinstance(result, dict)
                            and (
                                result.get("decision") == "block"
                                or result.get("hookSpecificOutput", {}).get("permissionDecision")
                                == "deny"
                            )
                        ):
                            self._aw_bridge.invocations.native_denial()
                        return result
                    except Exception:
                        self._aw_bridge.failure_reply("tool.after", {})
                        await self._aw_bridge.stop_task()
                        raise

            return invoke

        composed.hooks = {
            event: [
                replace(matcher, hooks=[tracked(callback, event) for callback in matcher.hooks])
                for matcher in matchers
            ]
            for event, matchers in composed.hooks.items()
        }
        if composed.can_use_tool:
            composed.can_use_tool = tracked(composed.can_use_tool)
        super().__init__(options=composed)
        self._aw_mcp = claim_servers(composed.mcp_servers, self._aw_bridge)

    def _aw_check(self) -> None:
        if self._aw_pid != os.getpid():
            raise RuntimeError("SDK clients must be created after fork")
        if _poisoned or any(client._aw_pid != os.getpid() for client in _active):
            raise RuntimeError("AW worker requires restart after unsafe SDK cleanup or fork")

    async def connect(self, prompt: str | AsyncIterable[dict[str, Any]] | None = None) -> None:
        self._aw_check()
        async with self._aw_lock:
            if self._aw_state != "new":
                raise RuntimeError("AW task clients cannot reconnect; create a new client")
            self._aw_state = "opening"
            try:
                if isinstance(prompt, str) and self.options.can_use_tool:
                    raise ValueError(
                        "can_use_tool callback requires streaming mode when connecting with a prompt"
                    )
                await self._aw_bridge.scope.open(self._aw_cwd, self._aw_environment)
                self._aw_check()
                _active.add(self)
                self._aw_connect_task = asyncio.current_task()
                try:
                    endpoint = await self._aw_relay.start()
                    self.options = replace(
                        self.options,
                        env={
                            **self.options.env,
                            "ANTHROPIC_BASE_URL": endpoint,
                            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                        },
                    )
                    await super().connect(None)
                finally:
                    self._aw_connect_task = None
                self._aw_check()
                self._aw_task_started = False
                self._aw_state = "connected"
                if prompt is not None:
                    await self.query(prompt)
            except BaseException:
                await self._aw_cleanup()
                raise

    async def _connect_inner(
        self,
        prompt: str | AsyncIterable[dict[str, Any]] | None,
        actual_prompt: AsyncIterable[dict[str, Any]],
    ) -> None:
        # SDK 0.2.88 materializes session_store before this pinned seam. Build
        # the transport afterward so SDK mirroring and cleanup keep ownership.
        from claude_agent_sdk._internal.session_resume import apply_materialized_options

        options = self.options
        if self._materialized is not None:
            options = apply_materialized_options(options, self._materialized)
        self._custom_transport = ManagedTransport(options, self._aw_bridge)
        await super()._connect_inner(prompt, actual_prompt)

    async def query(
        self, prompt: str | AsyncIterable[dict[str, Any]], session_id: str = "default"
    ) -> None:
        self._aw_check()
        if self._aw_state != "connected" or self._aw_task_started or self._aw_receiving:
            raise RuntimeError(
                "AW requires the previous response and callbacks to drain before query"
            )
        if self._aw_bridge.failure:
            raise CallbackFailure(self._aw_bridge.failure)
        self._aw_bridge.scope.check()
        self._aw_task_started = True
        self._aw_input_task = asyncio.current_task()
        begun = False
        try:
            if not isinstance(prompt, str):
                from aw_transport import single_input

                message = await single_input(prompt)

                async def inputs() -> AsyncIterator[dict[str, Any]]:
                    yield message

                prompt = inputs()
            self._aw_bridge.invocations.begin()
            begun = True
            self._aw_query_finished = False
            await super().query(prompt, session_id)
        except BaseException:
            if begun:
                self._aw_bridge.failure_reply("tool.before", {})
                await self._aw_bridge.stop_task()
            else:
                self._aw_task_started = False
            raise
        finally:
            self._aw_input_task = None

    async def _aw_complete(self) -> None:
        async with self._aw_completion:
            if not self._aw_task_started or self._aw_query_finished:
                self._aw_bridge.failure_reply("tool.after", {})
                await self._aw_bridge.stop_task()
                raise CallbackFailure("AW received an unsolicited execution result")
            try:
                await self._aw_relay.drain()
                await self._aw_bridge.invocations.drain()
            except BaseException as error:
                self._aw_bridge.failure_reply("tool.after", {})
                await self._aw_bridge.stop_task()
                if isinstance(error, TimeoutError):
                    fail_stop()
                raise
            self._aw_query_finished = True
            self._aw_task_started = False

    async def receive_messages(self) -> AsyncIterator[Any]:
        self._aw_check()
        if self._aw_state != "connected" or not self._aw_task_started or self._aw_receiving:
            raise RuntimeError("AW requires one response consumer for the active task")
        self._aw_receiving = True
        try:
            async with aclosing(super().receive_messages()) as messages:
                async for message in messages:
                    if self._aw_bridge.failure:
                        await self._aw_bridge.stop_task()
                        raise CallbackFailure(self._aw_bridge.failure)
                    if isinstance(message, ResultMessage):
                        await self._aw_complete()
                    yield message
            if self._aw_bridge.failure:
                await self._aw_bridge.stop_task()
                raise CallbackFailure(self._aw_bridge.failure)
            if not self._aw_query_finished:
                raise RuntimeError("Claude SDK response ended without ResultMessage")
        finally:
            self._aw_receiving = False

    async def receive_response(self) -> AsyncIterator[Any]:
        async with aclosing(self.receive_messages()) as messages:
            async for message in messages:
                yield message
                if isinstance(message, ResultMessage):
                    return

    async def interrupt(self) -> None:
        if self._aw_pid != os.getpid():
            raise RuntimeError("A forked worker cannot interrupt an inherited client")
        await super().interrupt()

    async def _aw_cleanup(self) -> None:
        global _poisoned
        self._aw_state = "closing"
        try:
            # Stay in the SDK caller's task and shield teardown from request scopes.
            with anyio.CancelScope(shield=True):
                try:
                    producer = self._aw_input_task
                    if producer is not None and producer is not asyncio.current_task():
                        producer.cancel()
                        async with asyncio.timeout(5):
                            await asyncio.gather(producer, return_exceptions=True)
                    if self._aw_task_started and not self._aw_query_finished:
                        try:
                            await self._aw_bridge.stop_task()
                        except BaseException:
                            fail_stop()
                            raise
                    try:
                        async with asyncio.timeout(12):
                            await super().disconnect()
                    finally:
                        try:
                            await self._aw_relay.close()
                        finally:
                            await self._aw_bridge.invocations.drain()
                finally:
                    try:
                        await self._aw_bridge.scope.close()
                    finally:
                        for server in self._aw_mcp:
                            server.release()
        except BaseException:
            _poisoned = True
            # An undrained callback may still execute caller code. Close the
            # AW-owned process group rather than returning a reusable host.
            fail_stop()
            raise
        else:
            _active.discard(self)
            self._aw_state = "closed"

    async def disconnect(self) -> None:
        if self._aw_pid != os.getpid():
            raise RuntimeError("A forked worker cannot disconnect an inherited client")
        if self._aw_connect_task is asyncio.current_task():
            # SDK connect calls self.disconnect on failure while this task holds
            # the lifecycle lock. The outer connect handler owns all teardown.
            return
        async with self._aw_lock:
            if self._aw_state != "closed":
                await self._aw_cleanup()


def main() -> None:
    """Start a script/module host without creating a model client during readiness."""
    if sys.version_info < (3, 11):
        raise RuntimeError("AW resident Claude SDK requires Python >=3.11")
    args = sys.argv[1:]
    if args[:1] == ["--"]:
        args = args[1:]
    if not args or (args[0] == "-m" and len(args) < 2):
        raise ValueError("Use python -m aw_claude -- script.py or -m application")
    if args[0] != "-m" and not args[0].endswith(".py"):
        raise ValueError("Resident hosts require a Python script or -m module")
    if os.getpid() != os.getpgrp():
        raise RuntimeError("Resident bootstrap requires aw run foreground group ownership")
    os.environ["AW_CLAUDE_RESIDENT_GROUP"] = str(os.getpgrp())
    os.environ["AW_CLAUDE_RESIDENT"] = "1"
    bridge = Bridge()
    if bridge.config["observation_enabled"]:
        raise ValueError("AW resident Claude does not support AgentSight evidence")
    bridge.ready()
    if args[0] == "-m":
        sys.argv = args[1:]
        runpy.run_module(args[1], run_name="__main__", alter_sys=True)
    else:
        sys.argv = args
        sys.path.insert(0, str(Path(args[0]).resolve().parent))
        runpy.run_path(args[0], run_name="__main__")
    if _active or _poisoned:
        raise RuntimeError("Resident host exited with unclosed or failed SDK tasks")
