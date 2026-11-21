"""Resident Claude hosts admit isolated task clients through public SDK methods."""

from __future__ import annotations

import asyncio
from contextlib import aclosing
from dataclasses import replace
import os
import re
from pathlib import Path
from uuid import uuid4
import runpy
import signal
import sys
from typing import Any, AsyncIterable, AsyncIterator, Callable

import anyio
from claude_agent_sdk import (
    ClaudeAgentOptions,
    ClaudeSDKClient,
    PermissionResultDeny,
    ResultMessage,
)
if __package__:
    from .aw_claude import Bridge, CallbackFailure, _deny
    from .aw_scope import Scope
    from .aw_invocations import Invocations, digest
    from .aw_model import ModelRelay
    from .aw_transport import ManagedTransport
else:
    from aw_claude import Bridge, CallbackFailure, _deny
    from aw_scope import Scope
    from aw_invocations import Invocations, digest
    from aw_model import ModelRelay
    from aw_transport import ManagedTransport

_active: set[ManagedClient] = set()
_poisoned = False


def fail_stop(group: int) -> None:
    """Terminate the resident worker group when native cleanup cannot be confirmed."""
    if group != os.getpgrp():
        raise RuntimeError("Resident SDK host left its owned process group")
    # aw run owns its foreground group. Cooperative workers must start a private
    # session so escalation cannot reach the launcher or sibling workers.
    os.killpg(group, signal.SIGKILL)


class ResidentBridge(Bridge):
    """A failed managed callback stops delivery and permanently poisons its task."""

    def __init__(self, group: int, scope: Any = None) -> None:
        super().__init__(
            None if scope is None else {"hooks": [], "resident_policy_reload": True},
            launch_bound=scope is None,
        )
        self.group = group
        self.scope = Scope(self.config) if scope is None else scope
        self.scope.on_failure = self.scope_failed
        self.failure: str | None = None
        self.mcp_names: set[str] = set()
        self.mcp_servers: set[str] = set()
        self.invocations = Invocations(self)
        self.stop = None
        self.stopped = False
        self.stop_future: asyncio.Task[None] | None = None

    def scope_failed(self, error: BaseException) -> None:
        self.failure = "AW managed task stopped after bridge validity failure"
        if self.stop is not None:
            self.request_stop()

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
                updated = reply.get("hookSpecificOutput", {}).get("updatedInput")
                if updated is not None:
                    entry.arguments = digest(updated)
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
            fail_stop(self.group)
            raise


class ManagedClient(ClaudeSDKClient):
    """A public SDK client retains its conversation across drained executions."""

    _aw_observe_sessions = False

    def __init__(
        self,
        options: ClaudeAgentOptions,
        *,
        scope_factory: Callable[[Callable[[dict], bool]], Any] | None = None,
    ) -> None:
        if sys.version_info < (3, 11):
            raise RuntimeError("AW resident Claude SDK requires Python >=3.11")
        self._aw_pid = os.getpid()
        group = os.getpgrp()
        if scope_factory is None:
            if int(os.environ["AW_CLAUDE_RESIDENT_GROUP"]) != group:
                raise RuntimeError("Resident clients require the AW-owned foreground group")
        elif self._aw_pid != group or os.getsid(0) != group:
            raise RuntimeError("Cooperative Claude requires an isolated process started with setsid")
        self._aw_cooperative = scope_factory is not None
        self._aw_bridge = ResidentBridge(
            group, None if scope_factory is None else scope_factory(self._aw_install_routes)
        )
        self._aw_bridge.stop = self._aw_stop_native
        self._aw_cwd = str(Path(options.cwd or os.getcwd()).resolve())
        self._aw_environment = {**os.environ, **options.env}
        self._aw_state = "new"
        self._aw_task_started = False
        self._aw_relay: ModelRelay | None = None
        self._aw_query_finished = False
        self._aw_receiving = False
        self._aw_input_task = None
        self._aw_lock = asyncio.Lock()
        self._aw_completion = asyncio.Lock()
        self._aw_connect_task: asyncio.Task[Any] | None = None
        if __package__:
            from .aw_mcp import snapshot_servers
        else:
            from aw_mcp import snapshot_servers

        self._aw_options = replace(options, cwd=self._aw_cwd, env=dict(options.env),
                                   mcp_servers=snapshot_servers(options.mcp_servers))
        self._aw_mcp = []
        super().__init__(options=self._aw_options)
        if not self._aw_bridge.config.get("resident_policy_reload"):
            self._aw_compose(self._aw_options)

    def _aw_install_routes(self, snapshot: dict) -> bool:
        if (
            not isinstance(snapshot, dict)
            or re.fullmatch(r"[a-f0-9]{64}", snapshot.get("policy_revision", "")) is None
        ):
            raise ValueError("AW resident policy handoff is malformed")
        self._aw_bridge.set_routes(snapshot["hooks"])
        self._aw_compose(self._aw_options)
        return True

    def _aw_compose(self, options: ClaudeAgentOptions) -> None:
        if __package__:
            from .aw_mcp import claim_servers
        else:
            from aw_mcp import claim_servers

        self._aw_observe_sessions = any(
            hook["event"] in {"session.start", "session.end"} for hook in self._aw_bridge.config["hooks"]
        )
        composed = self._aw_bridge.options(options)
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
        self.options = composed
        self._aw_mcp = claim_servers(composed.mcp_servers, self._aw_bridge)

    def _aw_check(self) -> None:
        if self._aw_pid != os.getpid():
            raise RuntimeError("SDK clients must be created after fork")
        if _poisoned or any(client._aw_pid != os.getpid() for client in _active):
            raise RuntimeError("AW worker requires restart after unsafe SDK cleanup or fork")

    async def connect(self, prompt: str | AsyncIterable[dict[str, Any]] | None = None) -> None:
        self._aw_check()
        checked_input = False
        async with self._aw_lock:
            if self._aw_state != "new":
                raise RuntimeError("AW task clients cannot reconnect; create a new client")
            self._aw_state = "opening"
            try:
                if isinstance(prompt, str) and self.options.can_use_tool:
                    raise ValueError(
                        "can_use_tool callback requires streaming mode when connecting with a prompt"
                    )
                reloadable = self._aw_bridge.config.get("resident_policy_reload", False)
                if reloadable and not self._aw_cooperative:
                    await self._aw_bridge.scope.reserve()
                await self._aw_bridge.scope.open(self._aw_cwd, self._aw_environment,
                                                 acknowledge=not reloadable)
                if reloadable and not self._aw_cooperative:
                    self._aw_install_routes(await self._aw_bridge.scope.policy())
                checked_input = any(hook["event"] == "input.submit"
                                    for hook in self._aw_bridge.config["hooks"])
                self._aw_check()
                _active.add(self)
                self._aw_connect_task = asyncio.current_task()
                try:
                    self.options = replace(
                        self.options,
                        env={
                            **self.options.env,
                            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                        },
                    )
                    if reloadable:
                        await self._aw_bridge.scope.acknowledge()
                    await super().connect(None)
                    self._aw_bridge.bind_stop_deliveries(getattr(self, "_query", None))
                    async with asyncio.timeout(5):
                        transport = self._custom_transport
                        environment = await transport.model_environment(self._query)
                        self._aw_relay = ModelRelay(
                            self._aw_bridge, {**self._aw_environment, **environment}
                        )
                        endpoint = await self._aw_relay.start()
                        await transport.route_model(self._query, endpoint)
                    self.options = replace(
                        self.options, env={**self.options.env, "ANTHROPIC_BASE_URL": endpoint}
                    )
                finally:
                    self._aw_connect_task = None
                self._aw_check()
                if self._aw_bridge.failure:
                    if self._aw_bridge.stop_future is not None:
                        await asyncio.shield(self._aw_bridge.stop_future)
                    raise CallbackFailure(self._aw_bridge.failure)
                self._aw_bridge.scope.check()
                self._aw_task_started = False
                self._aw_state = "connected"
                if prompt is not None and not checked_input:
                    await self.query(prompt)
            except BaseException:
                await self._aw_cleanup()
                raise
        # A denied initial input leaves this initialized client available.
        if prompt is not None and checked_input:
            await self.query(prompt)

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
        turn_id = str(uuid4())
        try:
            hooks = [hook for hook in self._aw_bridge.config["hooks"]
                     if hook["event"] == "input.submit"]
            if hooks:
                if __package__:
                    from .aw_submission import check, prepare, SubmissionDenied
                else:
                    from aw_submission import check, prepare, SubmissionDenied

                prompt, payload = await prepare(
                    prompt, session_id, self._aw_cwd, hooks[0]["budget_ms"],
                )
                try:
                    turn_id = await check(self._aw_bridge, payload)
                except SubmissionDenied:
                    raise
                except asyncio.CancelledError:
                    raise
                except Exception:
                    self._aw_bridge.failure_reply("input.submit", {})
                    raise
                self._aw_bridge.scope.check()
                if self._aw_bridge.failure:
                    raise CallbackFailure(self._aw_bridge.failure)
            elif not isinstance(prompt, str):
                if __package__:
                    from .aw_transport import single_input
                else:
                    from aw_transport import single_input

                message = await single_input(prompt)

                async def inputs() -> AsyncIterator[dict[str, Any]]:
                    yield message

                prompt = inputs()
            self._aw_bridge.invocations.begin()
            begun = True
            self._aw_query_finished = False
            self._aw_bridge.turn_id = turn_id
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
                    fail_stop(self._aw_bridge.group)
                raise
            self._aw_query_finished = True
            self._aw_task_started = False
            self._aw_bridge.turn_id = None

    async def receive_messages(self) -> AsyncIterator[Any]:
        self._aw_check()
        if (self._aw_state != "connected" or not self._aw_task_started or self._aw_receiving
                or self._aw_input_task is not None):
            raise RuntimeError("AW requires one response consumer for the active task")
        self._aw_receiving = True
        try:
            async with aclosing(super().receive_messages()) as messages:
                async for message in messages:
                    if self._aw_bridge.failure:
                        await self._aw_bridge.stop_task()
                        raise CallbackFailure(self._aw_bridge.failure)
                    if self._aw_observe_sessions:
                        await self._aw_bridge.session_initialized(message, self._aw_cwd)
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

    async def _aw_stop_native(self) -> None:
        if self._aw_pid != os.getpid():
            raise RuntimeError("A forked worker cannot interrupt an inherited client")
        # Before super().connect(None) binds _query (or after a drained disconnect),
        # no native work can be running; SDK "Not connected" is a confirmed no-op.
        if getattr(self, "_query", None) is None and (
            not self._aw_task_started or self._aw_query_finished
        ):
            return
        await self.interrupt()

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
                    reason = "disconnected"
                    if self._aw_task_started and not self._aw_query_finished:
                        try:
                            await self._aw_bridge.stop_task()
                            reason = "cancelled"
                        except BaseException:
                            fail_stop(self._aw_bridge.group)
                            raise
                    try:
                        async with asyncio.timeout(12):
                            await super().disconnect()
                    finally:
                        try:
                            if self._aw_relay is not None:
                                await self._aw_relay.close()
                        finally:
                            await self._aw_bridge.invocations.drain()
                    if self._aw_observe_sessions:
                        await self._aw_bridge.session_disconnected(self._aw_cwd, reason)
                finally:
                    try:
                        await self._aw_bridge.scope.close()
                    finally:
                        for server in self._aw_mcp:
                            server.release()
                if self._aw_bridge.config.get("resident_policy_reload") and not self._aw_cooperative:
                    await self._aw_bridge.scope.release()
        except BaseException:
            _poisoned = True
            # An undrained callback may still execute caller code. Close the
            # AW-owned process group rather than returning a reusable host.
            fail_stop(self._aw_bridge.group)
            raise
        else:
            _active.discard(self)
            self._aw_state = "closed"

    @property
    def _aw_live(self) -> bool:
        return self._aw_state not in {"new", "closed"}

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


async def prepare_native() -> None:
    """Confirm a prepared native host before its first managed SDK client."""
    if sys.version_info < (3, 11):
        raise RuntimeError("AW resident Claude SDK requires Python >=3.11")
    if os.getpid() != os.getpgrp():
        raise RuntimeError("Native Claude host requires its own process group; start it with setsid")
    bridge = Bridge()
    if not bridge.config.get("resident_policy_reload"):
        raise RuntimeError("Native Claude startup requires aw instance prepare")
    os.environ["AW_CLAUDE_RESIDENT_GROUP"] = str(os.getpgrp())
    os.environ["AW_CLAUDE_RESIDENT"] = "1"
    await Scope(bridge.config).ready()
    bridge.ready()


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
    if bridge.config.get("resident_policy_reload"):
        asyncio.run(Scope(bridge.config).ready())
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
