"""Per-Agent admission and native invocation ownership over the shared AW ledger."""

from __future__ import annotations

import asyncio
from contextvars import ContextVar
from dataclasses import dataclass, field
import json
import os
import re
from typing import Any
from uuid import uuid4

from agentscope.middleware import MiddlewareBase
from agentscope._utils._common import _json_loads_with_repair
from agentscope.tool import ToolResponse, _toolkit

from .aw_private import clear_execution, current_execution, public_execution, validate_execution
from .aw_scope import Scope


class ResidentFailure(RuntimeError):
    """Fail closed at the verified native dispatch and delivery boundaries."""


current_call: ContextVar[Any] = ContextVar("aw_qwenpaw_invocation", default=None)


def frozen(value: Any) -> str:
    """Preserve Python integers exactly before the shared binary64 wire digest."""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, allow_nan=False)


CLEANUP_TIMEOUT = 60
CANCEL_TIMEOUT = 1
pending_cleanup: set[asyncio.Future] = set()
cleanup_failed = False
cleanup_deadline: ContextVar[Any] = ContextVar("aw_qwenpaw_cleanup_deadline", default=None)


def check_cleanup():
    if cleanup_failed:
        raise ResidentFailure("AW native cleanup failed; worker restart required")


def retain_cleanup(task):
    """Retain timed-out original work and retrieve its eventual exception."""
    pending_cleanup.add(task)
    def finished(done):
        pending_cleanup.discard(done)
        if not done.cancelled():
            done.exception()
    task.add_done_callback(finished)


def fence_cleanup():
    """Prevent another Agent from inheriting uncertain process cleanup state."""
    global cleanup_failed
    cleanup_failed = True


async def settle(operation: Any, timeout: float | None = None) -> Any:
    """Join owned work within one inherited budget, including cancellation drain."""
    loop = asyncio.get_running_loop()
    budget = CLEANUP_TIMEOUT if timeout is None else timeout
    deadline = loop.time() + budget
    inherited = cleanup_deadline.get()
    if inherited is not None:
        deadline = min(deadline, inherited)
    token = cleanup_deadline.set(deadline)
    task = asyncio.ensure_future(operation)
    cleanup_deadline.reset(token)
    cancelled = False
    cancel_budget = min(CANCEL_TIMEOUT, max(0, deadline - loop.time()) / 10)
    while not task.done() and loop.time() < deadline - cancel_budget:
        try:
            await asyncio.wait({task}, timeout=max(0, deadline - cancel_budget - loop.time()))
        except asyncio.CancelledError:
            cancelled = True
    if not task.done():
        fence_cleanup()
        retain_cleanup(task)
        task.cancel()
        # Retain cancellation-resistant original work until its actual exit.
        # All nested cleanup and this join share the caller's absolute deadline.
        while not task.done() and loop.time() < deadline:
            try:
                await asyncio.wait({task}, timeout=max(0, deadline - loop.time()))
            except asyncio.CancelledError:
                cancelled = True
        raise ResidentFailure("AW native cleanup deadline exceeded; worker restart required")
    result = task.result()
    if cancelled:
        raise asyncio.CancelledError()
    return result


@dataclass(eq=False)
class Invocation:
    owner: Any
    call: Any
    sequence: int
    arguments: str
    identity: str = field(default_factory=lambda: str(uuid4()))
    hooks: set[asyncio.Task] = field(default_factory=set)
    native_done: bool = False
    native_input_checked: bool = False
    offloaded: bool = False
    foreground_done: bool = False
    background_done: bool = False
    failure: bool = False
    entry: Any = None
    supervisor: Any = None
    coordinator_stream: Any = None
    coordinator_closing: Any = field(default_factory=asyncio.Lock)
    coordinator_closed: bool = False
    retirement: Any = None
    retired: bool = False

    def drained(self):
        task = self.entry.background_task if self.entry is not None else None
        return (self.native_done and (task is None or task.done())
                and all(hook.done() for hook in self.hooks)
                and (self.coordinator_stream is None or self.coordinator_closed)
                and (not self.offloaded or self.background_done))

    async def close_coordinator(self):
        async with self.coordinator_closing:
            if self.coordinator_stream is not None:
                try:
                    await self.coordinator_stream.aclose()
                    self.coordinator_closed = True
                except ResidentFailure:
                    # Native parent cancellation attempts final delivery again.
                    # Once cleanup is confirmed, keep the failed owner fenced
                    # while allowing that already-rejected generator to close.
                    if not self.native_done or not (self.failure or self.owner.failed):
                        raise
                except BaseException:
                    self.failure = self.owner.failed = True
                    fence_cleanup()
                    raise
                finally:
                    # A raising async-generator finalizer may still have exited.
                    # Unknown streams retain their lease until closure is proven.
                    if getattr(self.coordinator_stream, "ag_frame", False) is None:
                        self.coordinator_closed = True

    def check(self, call: Any = None) -> None:
        self.owner.check()
        if self.failure or self.owner.active.get(self.identity) is not self:
            raise ResidentFailure("AW invocation is failed or retired")
        if call is not None and (
            call.id != self.call.id or call.name != self.call.name
            or frozen(json.loads(call.input)) != self.arguments
        ):
            self.failure = True
            self.owner.failed = True
            raise ResidentFailure("AW native invocation identity or arguments changed")

    def payload(self, event: str, response: Any = None) -> dict[str, Any]:
        result = {
            "event": event, "session_id": self.owner.session,
            "aw_invocation_id": self.identity,
            "tool_call": self.call.model_dump(mode="json"),
        }
        if getattr(self, "mcp_dispatch", None) is not None:
            result["aw_mcp_context"] = self.mcp_dispatch.payload()
        if response is not None:
            result["tool_response"] = response.model_dump(mode="json")
        # Redaction is mandatory before crossing any external Hook boundary.
        visible_call = dict(result["tool_call"])
        if frozen(self.owner.lease.redact(json.loads(visible_call["input"]))) != self.arguments:
            raise ResidentFailure("AW credentials must stay outside authorization-bearing tool input")
        result["tool_call"] = visible_call
        if response is not None:
            result["tool_response"] = self.owner.lease.redact(result["tool_response"])
        return result


class ResidentMiddleware(MiddlewareBase):
    """One request scope survives until all its Coordinator work has drained."""

    def __init__(self, config: dict, evaluate: Any, context: Any) -> None:
        self.root_config = {**config, "adapter": "qwenpaw"}
        self.config = dict(self.root_config)
        self.evaluate = evaluate
        self.context = context
        self.scope = Scope(self.config)
        self.active: dict[str, Invocation] = {}
        self.sequence = 0
        self.failed = False
        self.request_done = False
        self.opened = False
        self.issuing = asyncio.Lock()
        self.closing = asyncio.Lock()
        self.lease = None
        self.private = False
        self.lease_context = None
        self.lease_entered = False
        self.acquisition_finished = True
        self.request_released = False
        self.session = None
        self.agent = None
        self.reply_stream = None
        self.reply_task = None

    def check(self) -> None:
        check_cleanup()
        self.scope.check()
        if self.failed or self.agent is None:
            raise ResidentFailure("AW Agent admission or native execution failed")
        # Native HTTP readiness can precede the plugin's assembly acknowledgement.
        if not all(hasattr(self.agent, name) for name in (
                "_aw_managed_acting_impl", "_aw_managed_parser",
                "_aw_managed_toolkit", "_aw_managed_toolkit_call")):
            self.failed = True
            raise ResidentFailure("AW Agent native assembly is incomplete")
        if (self not in self.agent._reply_middlewares
                or self not in self.agent._acting_middlewares
                or self.agent._acting_impl is not self.agent._aw_managed_acting_impl
                or _toolkit._json_loads_with_repair is not self.agent._aw_managed_parser
                or self.agent.toolkit is not self.agent._aw_managed_toolkit
                or getattr(self.agent.toolkit, "call_tool", None) is not self.agent._aw_managed_toolkit_call):
            self.failed = True
            raise ResidentFailure("AW required Agent middleware was removed")

    def on_reply(self, agent, input_kwargs, next_handler):
        if self.reply_stream is not None and self.reply_stream.ag_frame is not None:
            raise ResidentFailure("AW concurrent replies require separate Agent instances")
        stream = self._on_reply(agent, input_kwargs, next_handler)
        self.reply_stream = stream
        return stream

    async def _on_reply(self, agent, input_kwargs, next_handler):
        check_cleanup()
        self.reply_task = asyncio.current_task()
        if self.opened:
            if not self.request_released or self.failed:
                raise ResidentFailure("AW previous request has not completed cleanup")
            reply_stream = self.reply_stream
            self.__init__(self.root_config, self.evaluate, self.context)
            self.reply_stream = reply_stream
            self.reply_task = asyncio.current_task()
        self.opened = True
        self.agent = agent
        self.session = agent.state.session_id
        factory = getattr(self.context, "aw_execution_factory", public_execution)
        if factory is not public_execution and getattr(self.context, "aw_environment_version", None) != 1:
            raise ResidentFailure("AW task credentials require the native environment extension")
        self.private = factory is not public_execution
        stream = None
        try:
            cwd = str(getattr(self.context, "workspace_dir", os.getcwd()))
            if self.root_config.get("resident_policy_reload"):
                await settle(self.scope.reserve())
            self.lease_context = factory()
            self.acquisition_finished = False
            await settle(self.acquire_lease())
            if self.root_config.get("resident_policy_reload"):
                await self.scope.open(cwd, self.lease.hook_environment, acknowledge=False)
                from .plugin import validate_hooks
                policy = await self.scope.policy()
                if not isinstance(policy, dict) or not isinstance(policy.get("policy_revision"), str) or not re.fullmatch(r"[a-f0-9]{64}", policy["policy_revision"]):
                    raise ResidentFailure("AW resident policy handoff is malformed")
                validate_hooks(policy)
                # Keep launch identity and executable fields outside the snapshot.
                self.config = {**self.root_config, "policy_revision": policy["policy_revision"],
                               "hooks": json.loads(json.dumps(policy["hooks"]))}
                await self.scope.acknowledge()
            else:
                await self.scope.open(cwd, self.lease.hook_environment)
            self.check()
            stream = next_handler(**input_kwargs)
            while True:
                self.reply_task = asyncio.current_task()
                try:
                    value = await anext(stream)
                except StopAsyncIteration:
                    break
                self.check()
                yield value
        except BaseException:
            self.failed = True
            raise
        finally:
            try:
                if stream is not None:
                    await settle(stream.aclose())
            except BaseException:
                self.failed = True
                fence_cleanup()
                raise
            finally:
                self.request_done = True
                try:
                    await settle(self.drain_foreground())
                except BaseException:
                    self.failed = True
                    fence_cleanup()
                    raise
                finally:
                    await settle(self.close_if_drained())

    async def acquire_lease(self):
        # Keep the original acquisition owned when a cancelled caller stops
        # waiting. A late lease is retired without entering native execution.
        try:
            self.lease = await self.lease_context.__aenter__()
            self.lease_entered = True
            validate_execution(self.lease)
        except asyncio.CancelledError:
            raise
        except Exception:
            raise ResidentFailure("AW private execution acquisition failed") from None
        finally:
            self.acquisition_finished = True
            if self.request_done:
                await self.close_if_drained()

    async def drain_foreground(self):
        # Closing an outer AgentScope generator does not close its nested
        # children. Join the exact Coordinator tasks before retiring leases.
        async with self.issuing:
            # Foreground failures invalidate previously healthy offloads too.
            # Defer them until foreground cleanup settles, regardless of insertion order.
            calls = sorted(self.active.values(), key=lambda call: call.offloaded)
        failure = None
        for call in calls:
            if call.offloaded and not self.failed:
                continue
            try:
                call.foreground_done = True
                task = call.entry.background_task if call.entry is not None else None
                if task is not None:
                    if not task.done():
                        task.cancel()
                    await settle(asyncio.gather(task, return_exceptions=True))
                else:
                    call.native_done = True
                if call.coordinator_stream is not None:
                    await settle(self.owned(call, call.close_coordinator()))
                if call.supervisor is not None:
                    await settle(asyncio.gather(call.supervisor, return_exceptions=True))
                if self.active.get(call.identity) is call and call.native_done:
                    await self.retire(call)
            except BaseException as error:
                # One failed acknowledgement must not strand another invocation's
                # original task. Every join retains the inherited cleanup deadline.
                self.failed = True
                fence_cleanup()
                if failure is None:
                    failure = error
        if failure is not None:
            raise failure

    async def owned(self, invocation, operation):
        token = current_call.set(invocation)
        private_token = current_execution.set(self.lease)
        try:
            return await operation
        finally:
            current_execution.reset(private_token)
            current_call.reset(token)

    async def close_if_drained(self):
        async with self.closing:
            if not self.request_done or not self.acquisition_finished or self.request_released:
                return
            if self.active and not (self.failed and all(call.drained() for call in self.active.values())):
                return
            try:
                await self.scope.close()
            except BaseException:
                self.failed = True
                fence_cleanup()
                raise
            finally:
                try:
                    if self.lease is not None or self.lease_entered:
                        await self.lease_context.__aexit__(None, None, None)
                except asyncio.CancelledError:
                    self.failed = True
                    fence_cleanup()
                    raise
                except Exception:
                    self.failed = True
                    fence_cleanup()
                    raise ResidentFailure("AW private cleanup failed; worker restart required") from None
                finally:
                    clear_execution(self.lease)
                    self.lease = None
                    self.lease_entered = False
            # The task stays busy until both the child scope and application-owned
            # credential finalizer have acknowledged their complete cleanup.
            if self.root_config.get("resident_policy_reload"):
                check_cleanup()
                try:
                    await self.scope.release()
                except BaseException:
                    self.failed = True
                    fence_cleanup()
                    raise
            self.request_released = True

    def selected(self, hook, call):
        def matches(selectors):
            return any(value in ("*", call.name, "native:qwenpaw:" + call.name)
                       for value in selectors)
        return matches(hook.get("event_tools", ["*"])) and matches(hook.get("step_tools", ["*"]))

    def withhold(self, call):
        # A task-private redactor may need adjacent chunks to recognize a secret.
        # Withhold native/UI chunks and redact the accumulated response instead.
        return self.private or any(h["event"] == "tool.after"
                                   and h["on_error"] == "withhold_result"
                                   and self.selected(h, call)
                                   for h in self.config["hooks"])

    async def hooks(self, invocation, event, response=None):
        invocation.check()
        hooks = [h for h in self.config["hooks"] if h["event"] == event]
        if event == "tool.after":
            hooks.reverse()
        for hook in hooks:
            middleware = self.evaluate(self.config, hook)
            middleware.scope = self.scope
            replacement = await middleware._run(invocation.payload(event, response))
            if replacement is not None:
                if event == "tool.before":
                    return replacement
                response = replacement
        return response

    async def retire(self, invocation):
        # Foreground close, background supervision and request drain can race.
        # Share the one acknowledgement; never retry an uncertain finish RPC.
        if invocation.retirement is None:
            invocation.retirement = asyncio.create_task(self._retire(invocation))
            retain_cleanup(invocation.retirement)
        await asyncio.shield(invocation.retirement)

    async def _retire(self, invocation):
        if invocation.retired:
            return
        if self.active.get(invocation.identity) is not invocation:
            raise ResidentFailure("AW native invocation ownership lost")
        try:
            await invocation.close_coordinator()
            if not invocation.drained():
                raise ResidentFailure("AW retirement preceded confirmed native drain")
            await self.scope.finish(invocation.sequence, invocation.identity)
            invocation.retired = True
            del self.active[invocation.identity]
        except BaseException:
            invocation.failure = self.failed = True
            fence_cleanup()
            raise
        finally:
            await self.close_if_drained()

    async def on_acting(self, agent, input_kwargs, next_handler):
        self.check()
        call = input_kwargs["tool_call"]
        tool = await agent.toolkit.check_tool_available(call.name, agent.state.tool_context.activated_groups)
        arguments = _json_loads_with_repair(call.input, tool.input_schema)
        # Freeze the native parser's effective input before policy/registration.
        # Native dispatch reparses this canonical input under an exact guard.
        call.input = frozen(arguments)
        async with self.issuing:
            self.check()
            if len(self.active) >= self.scope.invocation_limit:
                raise ResidentFailure("AW active invocation capacity exceeded")
            self.sequence += 1
            invocation = Invocation(self, call.model_copy(deep=True), self.sequence, frozen(arguments))
            if getattr(tool, "is_mcp", False):
                from .aw_mcp import bind_mcp_invocation
                bind_mcp_invocation(tool, invocation)
            invocation.payload("tool.before")
            self.active[invocation.identity] = invocation
            try:
                await settle(self.scope.register(invocation.sequence, invocation.identity, call.name, json.loads(invocation.payload("tool.before")["tool_call"]["input"])))
            except BaseException:
                self.failed = True
                self.active.pop(invocation.identity)
                raise
        stream = None
        terminal = None
        try:
            denied = await self.hooks(invocation, "tool.before")
            if denied is not None:
                response = await self.hooks(invocation, "tool.after", denied)
                invocation.native_done = True
                terminal = response
            else:
                stream = next_handler(**input_kwargs)
                while True:
                    try:
                        value = await self.owned(invocation, anext(stream))
                    except StopAsyncIteration:
                        break
                    invocation.check()
                    if terminal is not None:
                        raise ResidentFailure("AW native output followed a terminal response")
                    if isinstance(value, ToolResponse):
                        if not invocation.offloaded and not invocation.native_done:
                            raise ResidentFailure("AW result preceded native cleanup")
                        terminal = value
                    elif not self.withhold(call):
                        yield value
        except BaseException:
            if invocation.entry is None:
                invocation.native_done = True
            invocation.failure = True
            self.failed = True
            raise
        finally:
            try:
                if stream is not None:
                    await settle(self.owned(invocation, stream.aclose()))
            finally:
                invocation.foreground_done = True
                if (self.active.get(invocation.identity) is invocation and invocation.native_done
                        and (not invocation.offloaded or invocation.background_done)):
                    try:
                        await settle(self.owned(invocation, invocation.close_coordinator()))
                    finally:
                        if invocation.drained():
                            await settle(self.retire(invocation))
        if terminal is not None:
            if invocation.failure or self.failed:
                raise ResidentFailure("AW rejected unverified native delivery")
            yield terminal
