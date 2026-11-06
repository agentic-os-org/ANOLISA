"""AgentScope factory bridge; native tool scheduling retains authority."""

from __future__ import annotations

import asyncio
import atexit
import io
import json
import os
import signal
import stat
import sys
import tempfile
from contextvars import ContextVar
from pathlib import Path
from typing import Any, AsyncGenerator, Callable
from uuid import uuid4

from agentscope._utils._common import _json_loads_with_repair
from agentscope.exception import AgentOrientedException
from agentscope.message import TextBlock, ToolResultState
from agentscope.middleware import MiddlewareBase
from agentscope.tool import ToolChunk, ToolResponse

_invocation_id: ContextVar[str] = ContextVar("aw_agentscope_invocation_id")
_pending_closes: ContextVar[dict[str, tuple[AwToolMiddleware, dict[str, Any], float]]] = ContextVar(
    "aw_agentscope_pending_closes"
)
_withheld: ContextVar[bool] = ContextVar("aw_agentscope_withheld", default=False)


def _withheld_response() -> ToolResponse:
    """Placeholder delivered to the model when a strict after check cannot run."""
    return ToolResponse(
        state=ToolResultState.ERROR,
        content=[
            TextBlock(
                type="text",
                text="AW post-tool check failed; result withheld by policy",
            )
        ],
    )


from aw_scope_ledger import (  # noqa: E402
    AwTaskScope,
    CallbackFailure,
    _ensure_task_scope,
    turn_scope,
)

_invocation_scope: ContextVar[AwTaskScope | None] = ContextVar(
    "aw_agentscope_invocation_scope", default=None
)


class AwInvocationMiddleware(MiddlewareBase):
    """Share a fresh correlation across this invocation's before/after wrappers."""

    def __init__(self, config: dict[str, Any] | None = None) -> None:
        self.config = config
        self._strict_output = any(
            hook["event"] == "tool.after" and hook["on_error"] == "withhold_result"
            for hook in (config or {}).get("hooks", [])
        )

    @staticmethod
    def _effective_input(raw_input: Any) -> Any | None:
        """Decode the SDK-effective arguments, or None to skip registration.

        Uses the same bound and parser as the tool middleware's evaluation so
        the registered digest and the normalized hook input always agree; an
        undecodable or out-of-domain call stays unregistered and keeps the
        existing parser failure policy instead of minting a ledger identity.
        """
        try:
            _encode_payload({"input": raw_input})
            effective = _json_loads_with_repair(raw_input)
            # Bound the decoded object exactly like the hook payload: a
            # registration must stay inside the wire domain its invoke uses.
            _encode_payload({"input": effective})
            json.dumps(effective, allow_nan=False)
            return effective
        except (AgentOrientedException, RecursionError, ValueError, TypeError,
                CallbackFailure):
            return None

    async def on_acting(
        self, agent: Any, input_kwargs: dict[str, Any], next_handler: Any
    ) -> AsyncGenerator[Any, None]:
        token = _invocation_id.set(uuid4().hex)
        invocation = _invocation_id.get()
        pending: dict[str, tuple[AwToolMiddleware, dict[str, Any], float]] = {}
        cleanup_token = _pending_closes.set(pending)
        withheld_token = _withheld.set(False)
        scope_token = _invocation_scope.set(None)
        registered = False
        final_response = None
        owner = turn_scope.get()
        task = asyncio.current_task()
        if owner is not None and task is not None and task is not owner.owner_task:
            owner.tasks.add(task)
        call = input_kwargs.get("tool_call")
        try:
            # Enable the ledger before the first OpenHookEvent. The process
            # scope is keyed by the same invocation id the native payload
            # normalizes into tool.call_id.
            if self.config is not None:
                scope = await _ensure_task_scope(self.config)
                _invocation_scope.set(scope)
                if call is not None:
                    effective_input = self._effective_input(call.input)
                    if effective_input is not None:
                        # The ledger digests the SDK-effective arguments the
                        # hook payload normalizes, not the raw model string.
                        # A cancelled registration is booked as committed in
                        # the scope; the drain retires whatever is booked.
                        await scope.register(invocation, call.name, effective_input)
                        registered = True
            async for item in next_handler():
                # Only the checked final content may reach native stream consumers.
                if self._strict_output and isinstance(item, ToolChunk):
                    continue
                # This wrapper is the outermost AW middleware, so strict
                # withholding lands here after every after hook has observed
                # the original response it was correlated against.
                if isinstance(item, ToolResponse) and _withheld.get():
                    item = _withheld_response()
                if (owner is not None or self._strict_output) and isinstance(item, ToolResponse):
                    # Sequential interruption stops native iteration at the final
                    # result. Retire this invocation before publishing that result.
                    final_response = item
                else:
                    yield item
        finally:
            try:

                async def drain() -> None:
                    # Pending phases close first, while the ledger still holds
                    # the invocation identity; finish then retires both phases
                    # and the identity server-side. A call booked in the scope
                    # — committed or cancelled-during-registration — always
                    # retires; an unregistered call under a usable scope never
                    # opened a correlated event, so its cleanup stays inert.
                    scope = _invocation_scope.get()
                    booked = scope is not None and invocation in scope.invocations
                    if (
                        scope is not None
                        and not scope.closed
                        and not scope.failed
                        and not registered
                        and not booked
                    ):
                        return
                    for middleware, payload, _ in pending.values():
                        await middleware._close_event(payload)
                    if booked and scope is not None:
                        await scope.finish(invocation)

                # A cancelled registration is booked in the scope and must
                # retire even with no pending phase and registered unset.
                scope_now = _invocation_scope.get()
                booked_now = scope_now is not None and invocation in scope_now.invocations
                if registered or pending or booked_now:
                    cleanup = asyncio.create_task(drain())
                    interrupted = None
                    # Retirement and phase closes are bounded; cancellation
                    # must not abandon the owned task or skip a correlation.
                    while not cleanup.done():
                        try:
                            await asyncio.shield(cleanup)
                        except asyncio.CancelledError as error:
                            if interrupted is None:
                                interrupted = error
                    cleanup.result()
                    if interrupted is not None:
                        raise interrupted
            finally:
                if owner is not None and task is not None:
                    owner.tasks.discard(task)
                _withheld.reset(withheld_token)
                _pending_closes.reset(cleanup_token)
                _invocation_scope.reset(scope_token)
                _invocation_id.reset(token)
        if final_response is not None:
            if self._strict_output:
                yield ToolChunk(**final_response.model_dump(), is_last=True)
            yield final_response


def _encode_payload(payload: dict[str, Any]) -> bytearray:
    limit = 1024 * 1024
    remaining = limit

    def check(value: Any) -> None:
        nonlocal remaining
        # iterencode can emit a whole escaped string as one chunk. Bound those
        # allocations first, including dictionary keys and Unicode expansion.
        remaining -= 1
        if remaining < 0 or (isinstance(value, str) and len(value) > remaining):
            raise CallbackFailure("native_input_limit")
        if isinstance(value, str):
            for start in range(0, len(value), 4096):
                chunk = value[start : start + 4096]
                try:
                    chunk.encode("utf-8")
                except UnicodeEncodeError as error:
                    raise CallbackFailure("native_input_invalid_unicode") from error
                remaining -= len(json.dumps(chunk, ensure_ascii=True)) - 2
                if remaining < 0:
                    raise CallbackFailure("native_input_limit")
        elif isinstance(value, dict):
            for key, item in value.items():
                check(key)
                check(item)
        elif isinstance(value, (list, tuple)):
            for item in value:
                check(item)
        elif isinstance(value, int) and not -(2**63) <= value < 2**64:
            # serde_json must not round policy input away from SDK execution.
            raise CallbackFailure("native_integer_out_of_range")

    check(payload)
    encoded = bytearray()
    for chunk in json.JSONEncoder(ensure_ascii=True, sort_keys=True).iterencode(payload):
        if len(chunk) > limit - len(encoded):
            raise CallbackFailure("native_input_limit")
        encoded.extend(chunk.encode("ascii"))
    return encoded


class AwToolMiddleware(MiddlewareBase):
    """Apply bounded native tool policies and model-visible result replacements."""

    def __init__(
        self, config: dict[str, Any], hook: dict[str, Any], *, last_before: bool = False
    ) -> None:
        self.config = config
        self.hook = hook
        self._last_before = last_before

    async def _invoke(self, payload: dict[str, Any]) -> tuple[int, bytes, bytes]:
        # Report-mode exit 0 cannot prove that the service accepted the step.
        # Keep the first snapshot and retire every entered phase at scope exit.
        _, _, deadline = _pending_closes.get().setdefault(
            self.hook["event"],
            (
                self,
                payload,
                asyncio.get_running_loop().time() + (self.hook["budget_ms"] + 3000) / 1000,
            ),
        )
        return await self._transport(payload, deadline=deadline)

    async def _transport(
        self, payload: dict[str, Any], *, deadline: float, close_event: bool = False
    ) -> tuple[int, bytes, bytes]:
        # Encoding failures follow Hook policy before creating a transport.
        try:
            encoded = _encode_payload(payload)
        except RecursionError as error:
            raise CallbackFailure("native_input_too_deep") from error
        if asyncio.get_running_loop().time() >= deadline:
            raise TimeoutError
        # Keep every phase on the scope that registered this invocation. A
        # failed owner fails this call explicitly instead of rerouting its old
        # identity to a root binding or a replacement scope.
        scope = _invocation_scope.get()
        if scope is not None:
            return await scope.invoke(
                self.hook,
                payload,
                deadline - asyncio.get_running_loop().time(),
                close_event=close_event,
            )
        proc = None
        try:
            async with asyncio.timeout_at(deadline):
                proc = await asyncio.create_subprocess_exec(
                    self.config["executable"],
                    "hook",
                    "--adapter",
                    "agentscope",
                    "--binding",
                    self.config["binding"],
                    "--event",
                    self.hook["event"],
                    "--step",
                    self.hook["step"],
                    "--on-error",
                    self.hook["on_error"],
                    *(["--close-event", "true"] if close_event else []),
                    stdin=asyncio.subprocess.PIPE,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                    start_new_session=True,
                )
                stdout, stderr = await proc.communicate(encoded)
        except BaseException:
            if proc is not None:
                if proc.returncode is None:
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                await proc.wait()
            raise
        return proc.returncode, stdout, stderr

    async def _close_event(self, payload: dict[str, Any]) -> None:
        # An early phase close retires the event on the scope's own instance
        # while the ledger keeps the invocation identity until finish; the
        # legacy root-binding path is only used without a usable scope.
        try:
            code, _, diagnostic = await self._transport(
                payload, deadline=asyncio.get_running_loop().time() + 3, close_event=True
            )
            if code != 0:
                raise CallbackFailure(
                    f"event_close_exit_{code}: {diagnostic.decode(errors='replace')}"
                )
        except (OSError, TimeoutError, CallbackFailure) as error:
            self._log_failure(f"event_cleanup_failed: {error}")

    @staticmethod
    def _diagnostic(data: bytes) -> None:
        if not data or sys.stderr is None:
            return
        try:
            data = data[:4096]
            sink = getattr(sys.stderr, "buffer", None)
            if sink is None:
                sink = sys.stderr
            if type(sink) is io.StringIO:
                sink.write(data.decode(errors="replace"))
            elif type(sink) is io.BytesIO:
                sink.write(data)
            else:
                fd = sink.fileno()
                if stat.S_ISREG(os.fstat(fd).st_mode):
                    os.write(fd, data)
                else:
                    # Reopen independently: setting O_NONBLOCK on a dup would
                    # also change the application's inherited descriptor flags.
                    diagnostic = os.open(f"/proc/self/fd/{fd}", os.O_WRONLY | os.O_NONBLOCK)
                    try:
                        os.write(diagnostic, data)
                    finally:
                        os.close(diagnostic)
        except (OSError, ValueError, AttributeError, io.UnsupportedOperation):
            # Full/closed/unsupported sinks may drop diagnostics, never policy.
            pass

    def _log_failure(self, code: str) -> None:
        self._diagnostic(
            (
                f"AW {self.hook['event']} step {self.hook['step']} failed: {code}; "
                f"on_error={self.hook['on_error']}\n"
            ).encode(errors="replace")
        )

    async def _run(self, payload: dict[str, Any]) -> ToolResponse | None:
        try:
            return await self._evaluate(payload)
        except (OSError, TimeoutError, CallbackFailure) as error:
            code = str(error) if isinstance(error, CallbackFailure) else type(error).__name__
            self._log_failure(code)
            if self.hook["event"] == "tool.before" and self.hook["on_error"] == "block":
                return ToolResponse(
                    state=ToolResultState.DENIED,
                    content=[
                        TextBlock(text="AW hook failed; configured failure action blocked the tool")
                    ],
                )
            # A strict after step must not deliver an unchecked native result,
            # but yielding the placeholder here would change the payload outer
            # after hooks observe under the same invocation. Flag it and let
            # the invocation wrapper replace the completed response instead.
            if self.hook["event"] == "tool.after" and self.hook["on_error"] == "withhold_result":
                _withheld.set(True)
                return None
            return None

    async def _evaluate(self, payload: dict[str, Any]) -> ToolResponse | None:
        try:
            # Bound raw argument repair with the same wire cap before parsing.
            _encode_payload({"input": payload["tool_call"]["input"]})
            # Toolkit reparses without a schema at execution; retain its exact
            # pinned parser semantics without changing the native call string.
            payload = {
                **payload,
                "effective_input": _json_loads_with_repair(payload["tool_call"]["input"]),
            }
        except (AgentOrientedException, RecursionError, ValueError) as error:
            raise CallbackFailure("native_arguments_invalid") from error
        code, stdout, stderr = await self._invoke(payload)
        self._diagnostic(stderr)
        if code == 2 and self.hook["event"] == "tool.before":
            reason = (stderr or stdout).decode(errors="replace").strip()
            return ToolResponse(
                state=ToolResultState.DENIED,
                content=[TextBlock(text=reason or "Denied by AW")],
            )
        if code != 0:
            raise CallbackFailure(f"native_exit_{code}")
        try:
            directive = json.loads(stdout) if stdout.strip() else None
        except RecursionError as error:
            raise CallbackFailure("native_output_too_deep") from error
        except ValueError as error:
            # Strict after treats an undecodable reply as a failed check; report
            # steps keep treating it as a neutral no-directive reply.
            if self.hook["event"] == "tool.after" and self.hook["on_error"] == "withhold_result":
                raise CallbackFailure("native_output_invalid") from error
            directive = None
        if isinstance(directive, dict) and (
            directive.get("action") in ("ask", "approve") or directive.get("decision") == "ask"
        ):
            raise CallbackFailure("unsupported_approval")
        if (
            self.hook["event"] == "tool.after"
            and self.hook.get("replace_result") is True
            and isinstance(directive, dict)
            and "aw_replacement" in directive
        ):
            try:
                return ToolResponse.model_validate(directive["aw_replacement"])
            except (ValueError, TypeError) as error:
                raise CallbackFailure("invalid_result_replacement") from error
        return None

    async def on_acting(
        self,
        agent: Any,
        input_kwargs: dict[str, Any],
        next_handler: Callable[..., AsyncGenerator[Any, None]],
    ) -> AsyncGenerator[Any, None]:
        """Run final checks inside the invocation's optional strict stream gate."""
        call = input_kwargs["tool_call"]
        session_id = agent.state.session_id
        payload = {
            "event": self.hook["event"],
            "invocation_id": _invocation_id.get(),
            "session_id": session_id,
            "tool_call": call.model_dump(mode="json"),
        }
        if self.hook["event"] == "tool.before":
            denied = await self._run(payload)
            if denied is not None or self._last_before:
                pending = _pending_closes.get().get("tool.before")
                if pending is not None:
                    middleware, snapshot, _ = pending
                    await middleware._close_event(snapshot)
                    _pending_closes.get().pop("tool.before")
            if denied is not None:
                yield denied
                return
        async for item in next_handler():
            # ToolChunk is intermediate; ToolResponse is the completed native result.
            # The outer invocation wrapper gates chunks for strict after routes.
            if self.hook["event"] == "tool.after" and isinstance(item, ToolResponse):
                replacement = await self._run(
                    {**payload, "tool_response": item.model_dump(mode="json")}
                )
                if replacement is not None:
                    item = replacement
            yield item


class AwBridge:
    """Construct public Agent objects with explicit AW middleware composition.

    Existing middleware runs first, so trusted argument injection and native
    delegation wrappers retain their ordering. Readiness is published only after
    a real Agent has been constructed with the AW middleware.
    """

    def __init__(self) -> None:
        self.config = json.loads(Path(os.environ["AW_NATIVE_CONFIG"]).read_text())
        for key in ("executable", "binding", "ready_path", "ready_token"):
            if not isinstance(self.config.get(key), str) or not self.config[key]:
                raise ValueError(f"AW bridge configuration requires {key}")
        hooks = self.config.get("hooks")
        if not isinstance(hooks, list):
            raise ValueError("AW bridge configuration requires hooks")
        for hook in hooks:
            if hook.get("event") not in {"input.submit", "tool.before", "tool.after"}:
                raise ValueError("Unsupported AW native event")
            if not isinstance(hook.get("step"), str) or not hook["step"]:
                raise ValueError("AW step must be a nonempty string")
            if hook.get("on_error") not in {"report", "block", "withhold_result"}:
                raise ValueError("Invalid AW failure policy")
            if hook["on_error"] == "withhold_result" and hook["event"] != "tool.after":
                raise ValueError("AW result withholding requires tool.after")
            if type(hook.get("replace_result")) is not bool:
                raise ValueError("AW result replacement capability must be a boolean")
            if hook["replace_result"] and hook["event"] != "tool.after":
                raise ValueError("AW result replacement requires tool.after")
            if type(hook.get("budget_ms")) is not int or not 1 <= hook["budget_ms"] <= 55000:
                raise ValueError("AW native budget must be 1..55000 ms")
        self._ready = False

    def create_agent(
        self, *args: Any, middlewares: list[MiddlewareBase] | None = None, **kwargs: Any
    ) -> Any:
        """Use this entry in every new/resumed Agent factory, before serving turns."""
        from agentscope.agent import Agent

        last_before = next(
            (hook for hook in reversed(self.config["hooks"]) if hook["event"] == "tool.before"),
            None,
        )
        additions = [
            AwToolMiddleware(self.config, hook, last_before=hook is last_before)
            for hook in self.config["hooks"] if hook["event"] != "input.submit"
        ]
        factory = Agent
        extra = {}
        if any(hook["event"] == "input.submit" for hook in self.config["hooks"]):
            from aw_submission import AwSubmissionAgent, AwSubmissionMiddleware

            factory = AwSubmissionAgent
            extra["aw_config"] = self.config
            additions.insert(0, AwSubmissionMiddleware(self.config))
        agent = factory(
            *args,
            middlewares=[*(middlewares or []), AwInvocationMiddleware(self.config), *additions],
            **extra,
            **kwargs,
        )
        if not self._ready:
            self._publish_ready()
            self._ready = True
        return agent

    def _publish_ready(self) -> None:
        path = Path(self.config["ready_path"])
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w",
                encoding="utf-8",
                dir=path.parent,
                prefix=".aw-ready-",
                delete=False,
            ) as output:
                temporary = Path(output.name)
                json.dump(
                    {
                        "version": 1,
                        "adapter": "agentscope",
                        "pid": os.getpid(),
                        "token": self.config["ready_token"],
                        "hooks": len(self.config["hooks"]),
                    },
                    output,
                )
                output.write("\n")
            os.replace(temporary, path)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
