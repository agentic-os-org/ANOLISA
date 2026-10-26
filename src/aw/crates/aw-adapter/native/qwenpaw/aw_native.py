"""Verified pinned native seams for installation, dispatch and finalizer ownership."""

from __future__ import annotations

import asyncio
from collections import OrderedDict
from copy import copy
from dataclasses import replace
from contextvars import ContextVar
from functools import wraps
from types import MethodType

from agentscope.tool import ToolResponse

from .aw_resident import ResidentFailure, ResidentMiddleware, current_call, frozen, settle


current_parse = ContextVar("aw_qwenpaw_native_parser", default=None)


def invocation():
    call = current_call.get()
    if call is None:
        raise ResidentFailure("AW Coordinator callback has no native dispatch identity")
    call.check()
    return call


def track_hook(callback, call, *, after=False):
    def start(*args, **kwargs):
        async def run():
            try:
                call.check()
                if after:
                    response, *tail = args
                    response = ToolResponse.model_validate(
                        call.owner.lease.redact(response.model_dump(mode="json"))
                    )
                    return await callback(response, *tail, **kwargs)
                return await callback(*args, **kwargs)
            except BaseException:
                call.failure = True
                call.owner.failed = True
                raise ResidentFailure("AW native Hook failed") from None
        task = asyncio.create_task(run())
        call.hooks.add(task)
        return task
    return start


class HookRegistry:
    """Retain native shielded Hook tasks until their actual finalizers finish."""

    def __init__(self, native):
        self.native = native

    def __getattr__(self, name):
        return getattr(self.native, name)

    def get(self, name):
        pair = self.native.get(name)
        call = current_call.get()
        if call is None:
            return pair
        return replace(pair,
                       before=track_hook(pair.before, call) if pair.before else None,
                       after=track_hook(pair.after, call, after=True) if pair.after else None)


class StreamGate:
    """Keep strict output out of every native subscriber until final adoption."""

    def __init__(self, native, strict):
        self.native = native
        self.strict = strict

    async def append(self, chunk):
        if not self.strict:
            await self.native.append(chunk)

    async def close(self):
        # The outer owner closes the real stream after Hooks and cleanup.
        pass


def bind_toolkit(agent):
    """Corroborate the effective input of this Agent's admitted native dispatch."""
    from agentscope.tool import _toolkit
    parser = _toolkit._json_loads_with_repair
    if not getattr(parser, "_aw_dispatch_guard", False):
        @wraps(parser)
        def checked_input(raw, schema=None):
            call = current_parse.get()
            # Consume the dispatch guard before the native tool body can call
            # another Toolkit with its own arguments in this same task.
            current_parse.set(None)
            arguments = parser(raw, schema)
            if call is not None:
                call.check()
                if frozen(arguments) != call.arguments:
                    call.failure = True
                    call.owner.failed = True
                    raise ResidentFailure("AW native effective arguments changed after authorization")
                call.native_input_checked = True
            return arguments
        checked_input._aw_dispatch_guard = True
        _toolkit._json_loads_with_repair = checked_input
    toolkit_call = getattr(agent.toolkit, "call_tool", None)
    if not callable(toolkit_call):
        raise ResidentFailure("AW requires the pinned Toolkit dispatch seam")
    if not getattr(toolkit_call, "_aw_parser_bound", False):
        @wraps(toolkit_call)
        async def guarded_toolkit(tool_call, state):
            call = current_call.get()
            owns = (call is not None and not call.native_input_checked
                    and call.owner.agent.toolkit is agent.toolkit and tool_call.id == call.call.id)
            stream = toolkit_call(tool_call, state)
            first = True
            try:
                while True:
                    token = current_parse.set(call if owns and first else None)
                    try:
                        item = await anext(stream)
                    except StopAsyncIteration:
                        break
                    finally:
                        current_parse.reset(token)
                    first = False
                    yield item
            finally:
                await settle(stream.aclose())
        guarded_toolkit._aw_parser_bound = True
        agent.toolkit.call_tool = guarded_toolkit
    agent._aw_managed_parser = _toolkit._json_loads_with_repair
    agent._aw_managed_toolkit = agent.toolkit
    agent._aw_managed_toolkit_call = getattr(agent.toolkit, "call_tool", None)


def bind_agent(agent):
    """Corroborate dispatch after all native and application acting middleware."""
    bind_toolkit(agent)
    original = agent._acting_impl

    @wraps(original)
    async def checked(tool_call):
        call = invocation()
        call.check(tool_call)
        stream = original(tool_call)
        try:
            async for item in stream:
                try:
                    call.check(tool_call)
                except ResidentFailure:
                    # Toolkit catches parser errors and yields its final response
                    # from finally. Exhaust that owned stream without delivery;
                    # closing it at the error chunk strands a yielding finalizer.
                    async def discard():
                        async for _ in stream:
                            pass
                    await settle(discard())
                    raise
                yield item
        finally:
            await settle(stream.aclose())

    agent._acting_impl = checked
    agent._aw_managed_acting_impl = checked

    resident = [mw for mw in agent._reply_middlewares if isinstance(mw, ResidentMiddleware)]
    if len(resident) != 1:
        raise ResidentFailure("AW public reply requires one resident owner")
    middleware = resident[0]
    reply_stream = agent.reply_stream
    reply = agent.reply
    close = agent.close

    async def close_request():
        # AgentScope's public generator does not close its nested middleware
        # chain. Retain and close the actual owner before returning to callers.
        if middleware.reply_stream is not None:
            task = middleware.reply_task
            if middleware.reply_stream.ag_running and task is not asyncio.current_task():
                middleware.failed = True
                task.cancel()
                # Coordinator shields its drain task from parent cancellation.
                # Stop the owned task as well before joining the public pull.
                native_tasks = [call.entry.background_task for call in middleware.active.values()
                                if call.entry is not None and call.entry.background_task is not None]
                for native_task in native_tasks:
                    native_task.cancel()
                await settle(asyncio.gather(*native_tasks, return_exceptions=True))
                await settle(asyncio.gather(task, return_exceptions=True))
            await settle(middleware.reply_stream.aclose())

    @wraps(reply_stream)
    async def owned_reply_stream(*args, **kwargs):
        stream = reply_stream(*args, **kwargs)
        try:
            async for item in stream:
                yield item
        finally:
            try:
                await settle(close_request())
            finally:
                await settle(stream.aclose())

    @wraps(reply)
    async def owned_reply(*args, **kwargs):
        try:
            return await reply(*args, **kwargs)
        finally:
            await settle(close_request())

    @wraps(close)
    async def owned_close(*args, **kwargs):
        try:
            await settle(close_request())
        finally:
            await settle(close(*args, **kwargs))

    agent.close = owned_close
    agent.reply_stream = owned_reply_stream
    agent.reply = owned_reply


def bind_coordinator(coordinator):
    if getattr(coordinator, "_aw_resident_bound", False):
        return
    names = ("execute", "_run_tool_with_hooks", "_supervise", "_begin_offload", "_finalize_completed", "shutdown")
    original = {name: getattr(coordinator, name, None) for name in names}
    if not all(callable(method) for method in original.values()):
        raise ResidentFailure("AW requires the pinned Coordinator lifecycle seams")
    if any(not callable(getattr(original[name], "__func__", None))
           for name in ("_supervise", "_finalize_completed")):
        raise ResidentFailure("AW requires the pinned native publication methods")
    supervisors = set()
    pending_results = {}
    offload_drains = {}

    def protect_callback(callback):
        @wraps(callback)
        async def protected(*args, **kwargs):
            call = current_call.get()
            if call is None:
                raise ResidentFailure("AW native callback has no dispatch identity")
            try:
                call.check()
                return await callback(*args, **kwargs)
            except BaseException:
                call.failure = True
                call.owner.failed = True
                raise ResidentFailure("AW native lifecycle callback failed") from None
        return protected

    # Native logs and swallows these callbacks separately from tool Hooks.
    # Cover both callbacks installed before binding and future registration.
    for event in ("completion", "offloaded"):
        handlers = getattr(coordinator, "_" + event + "_handlers")
        handlers[:] = [protect_callback(callback) for callback in handlers]
        register = getattr(coordinator, "on_" + event)
        def checked_registration(callback, register=register):
            return register(protect_callback(callback))
        setattr(coordinator, "on_" + event, checked_registration)

    async def execute(self, *args, **kwargs):
        call = invocation()
        stream = original["execute"](*args, **kwargs)
        call.coordinator_stream = stream
        try:
            async for item in stream:
                yield item
        finally:
            try:
                await settle(call.close_coordinator())
            finally:
                if not call.offloaded and call.native_done:
                    pending_results.pop(call.identity, None)

    async def run(self, next_handler, tool_call, entry):
        call = current_call.get()
        if call is None:
            raise ResidentFailure("AW native execution has no dispatch identity")
        call.entry = entry
        strict = call.owner.withhold(tool_call)
        private = replace(entry, final_response=entry.final_response.model_copy(deep=True),
                          stream=StreamGate(entry.stream, strict))

        async def guarded(**kwargs):
            call.check(kwargs["tool_call"])
            stream = next_handler(**kwargs)
            try:
                async for item in stream:
                    call.check(kwargs["tool_call"])
                    # Redact before native Hooks, pruning and external observers.
                    yield type(item).model_validate(call.owner.lease.redact(item.model_dump(mode="json")))
            except BaseException:
                call.failure = True
                call.owner.failed = True
                raise ResidentFailure("AW native tool or result processing failed") from None
            finally:
                try:
                    await settle(stream.aclose())
                except BaseException:
                    call.failure = True
                    call.owner.failed = True
                    raise ResidentFailure("AW native generator cleanup failed") from None

        try:
            call.check(tool_call)
            await original["_run_tool_with_hooks"](guarded, tool_call, private)
            # Native shields after_call and may swallow cancellation. Join its
            # real task before adopting a response or declaring the stream done.
            if call.hooks:
                await settle(asyncio.gather(*call.hooks))
            call.check()
            response = ToolResponse.model_validate(
                call.owner.lease.redact(private.final_response.model_dump(mode="json"))
            )
            response = await call.owner.hooks(call, "tool.after", response)
            # HTTP reads hot entries too. Keep the result private until native
            # scheduling chooses foreground finalization or background delivery.
            pending_results[call.identity] = (response, private.end_state)
        except BaseException:
            call.failure = True
            call.owner.failed = True
            # Coordinator task exceptions can bypass its own cancellation
            # finalizer. Persist failure and let the verified finalize/delivery
            # guards reject output after native cleanup has been joined.
        finally:
            if call.hooks:
                await settle(asyncio.gather(*call.hooks, return_exceptions=True))
            call.hooks.clear()
            call.native_done = True
            await entry.stream.close()
            if call.failure and self._entries.get(entry.ctx.tool_call_id) is entry:
                self._entries.pop(entry.ctx.tool_call_id)

    async def offload(self, entry, processor):
        call = invocation()
        if call.entry is not entry:
            raise ResidentFailure("AW offload identity mismatch")
        call.offloaded = True
        drained = asyncio.get_running_loop().create_future()
        offload_drains[call.identity] = drained
        try:
            result = await original["_begin_offload"](entry, processor)
            call.check()
            drained.set_result(True)
            return result
        finally:
            # Native starts supervision before awaiting offloaded handlers.
            # Wake its publication gate on both success and failure/cancellation.
            if not drained.done():
                drained.set_result(False)

    async def finalize(self, entry):
        call = invocation()
        if call.entry is not entry or not call.native_done:
            raise ResidentFailure("AW native finalization preceded callback drain")
        entry.final_response, entry.end_state = pending_results.pop(call.identity)
        return await original["_finalize_completed"](entry)

    async def supervise(self, entry, processor):
        call = current_call.get()
        task = asyncio.current_task()
        supervisors.add(task)
        if call is not None:
            call.supervisor = task
        try:
            if call is None or call.entry is not entry:
                raise ResidentFailure("AW background callback identity mismatch")
            async def checked(response):
                call.check()
                try:
                    result = await processor(response)
                    return ToolResponse.model_validate(call.owner.lease.redact(result.model_dump(mode="json")))
                except BaseException:
                    call.failure = True
                    call.owner.failed = True
                    # Native catches Exception here. Cancellation aborts the
                    # verified delivery path, so the original cannot become a hint.
                    raise asyncio.CancelledError("AW background result rejected")
            # Run the pinned native supervisor unchanged, but stage its early
            # cache/hint writes. Its task, cancellation context and stream stay
            # shared with the real entry so native offload controls still work.
            staged = copy(self)
            private = replace(entry, final_response=entry.final_response.model_copy(deep=True))
            staged._entries = {entry.ctx.tool_call_id: private}
            staged._completed_cache = OrderedDict()
            staged._pending_hints = {}
            staged._hints_lock = asyncio.Lock()

            async def stage_finalization(candidate):
                call.check()
                if candidate is not private or not call.native_done:
                    raise ResidentFailure("AW background finalization preceded callback drain")
                candidate.final_response, candidate.end_state = pending_results.pop(call.identity)
                return await original["_finalize_completed"].__func__(staged, candidate)

            staged._finalize_completed = stage_finalization
            await original["_supervise"].__func__(staged, private, checked if processor else None)
            if not await asyncio.shield(offload_drains[call.identity]):
                raise ResidentFailure("AW offload callbacks did not complete successfully")
            call.check()
            # Completion callbacks receive the private entry, with the original
            # context, task and stream. External getters retain the empty public
            # entry until every required callback has returned successfully.
            # Promote through the native bounded cache without suspending.
            async with self._hints_lock:
                call.check()
                entry.final_response = private.final_response
                entry.end_state = private.end_state
                entry.force_cancelled = private.force_cancelled
                entry.status = private.status
                self._entries.pop(entry.ctx.tool_call_id, None)
                self._store_completed(entry)
                for session, hints in staged._pending_hints.items():
                    self._pending_hints.setdefault(session, []).extend(hints)
        except BaseException:
            if call is not None:
                call.failure = True
                call.owner.failed = True
            # The supervisor is a native detached task. Persist failure on its
            # owner so future dispatch/delivery rejects it without an orphaned
            # task exception containing tool data.
        finally:
            try:
                if call is not None:
                    pending_results.pop(call.identity, None)
                    offload_drains.pop(call.identity, None)
                    if call.failure and self._entries.get(entry.ctx.tool_call_id) is entry:
                        self._entries.pop(entry.ctx.tool_call_id)
                    call.background_done = True
                if call is not None and call.native_done and call.foreground_done:
                    await settle(call.owner.retire(call))
            finally:
                supervisors.discard(task)

    async def shutdown(self):
        async def drain():
            await original["shutdown"]()
            tasks = [entry.background_task for entry in self._entries.values()
                     if entry.background_task is not None]
            tasks.extend(supervisors)
            if tasks:
                await asyncio.gather(*tasks, return_exceptions=True)
        await settle(drain(), timeout=10)

    for name, method in (("execute", execute), ("_run_tool_with_hooks", run), ("_begin_offload", offload),
                         ("_finalize_completed", finalize), ("_supervise", supervise),
                         ("shutdown", shutdown)):
        setattr(coordinator, name, MethodType(method, coordinator))
    coordinator.hooks = HookRegistry(coordinator.hooks)
    coordinator._aw_resident_bound = True


def install_guard():
    """Check actual native assembly even when its plugin factory swallows errors."""
    from qwenpaw.runtime.builder import AgentBuilder
    from qwenpaw.tool_calls import ToolCoordinatorMiddleware

    if getattr(AgentBuilder, "_aw_resident_guard", False):
        return
    build_middlewares = AgentBuilder._build_middlewares
    build = AgentBuilder.build

    @wraps(build_middlewares)
    def checked_middlewares(ctx, agent_config):
        middlewares = build_middlewares(ctx, agent_config)
        resident = [mw for mw in middlewares if isinstance(mw, ResidentMiddleware)]
        coordinators = [mw for mw in middlewares if isinstance(mw, ToolCoordinatorMiddleware)]
        if len(resident) != 1 or len(coordinators) != 1:
            raise ResidentFailure("AW requires one installed resident middleware and native Coordinator per Agent")
        bind_coordinator(coordinators[0]._coordinator)
        # Run admission before pruning, Coordinator offload or model callbacks.
        return [resident[0], *[mw for mw in middlewares if mw is not resident[0]]]

    @wraps(build)
    async def checked_build(self, *args, **kwargs):
        agent = await build(self, *args, **kwargs)
        resident = [mw for mw in agent._reply_middlewares if isinstance(mw, ResidentMiddleware)]
        if len(resident) != 1 or resident[0] not in agent._acting_middlewares:
            raise ResidentFailure("AW Agent did not install required lifecycle middleware")
        bind_agent(agent)
        return agent

    AgentBuilder._build_middlewares = staticmethod(checked_middlewares)
    AgentBuilder.build = checked_build
    AgentBuilder._aw_resident_guard = True
