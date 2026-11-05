"""Bind model-produced calls to one execution and retain only active invocation details."""

from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from contextvars import ContextVar
from dataclasses import dataclass, field
import hashlib
import json
from typing import Any, AsyncIterator
from uuid import uuid4

from aw_claude import CallbackFailure


def digest(value: Any) -> bytes:
    def numbers(item: Any) -> Any:
        # The native CLI parses every JSON number as binary64. Freeze that
        # value, including rounded large integers, without changing tool input.
        if type(item) in (int, float):
            return float(item) if item else 0.0
        if isinstance(item, list):
            return [numbers(child) for child in item]
        if isinstance(item, dict):
            return {key: numbers(child) for key, child in item.items()}
        return item

    return hashlib.sha256(
        json.dumps(numbers(value), sort_keys=True, ensure_ascii=False, allow_nan=False).encode()
    ).digest()


@dataclass
class Invocation:
    call_id: str
    name: str
    execution: int
    sequence: int = 0
    arguments: bytes | None = None
    phase: str = "producing"
    callbacks: set[str] = field(default_factory=set)
    native_denied: bool = False


@dataclass
class Dispatch:
    request_id: str
    call_id: str | None
    execution: int
    entered: bool = False


dispatch: ContextVar[Dispatch | None] = ContextVar("aw_dispatch", default=None)


async def settle(operation: Any) -> None:
    # A downstream disconnect may cancel its producer during service RPC. Join
    # the acknowledgement and local transition before propagating cancellation.
    task = asyncio.create_task(operation)
    try:
        await asyncio.shield(task)
    except asyncio.CancelledError:
        await task
        raise


class Invocations:
    """An unknown identity is always rejected; completed UUIDs need no tombstones."""

    def __init__(self, bridge: Any) -> None:
        self.bridge = bridge
        self.active: dict[str, Invocation] = {}
        self.pending: dict[str, Dispatch] = {}
        self.changed = asyncio.Event()
        self.sequence = 0
        self.execution = 0
        self.running = False
        self.closing = False
        self.issuing = asyncio.Lock()
        self.adopting = asyncio.Lock()

    def check(self) -> None:
        self.bridge.scope.check()
        if self.bridge.failure or not self.running or self.closing:
            raise CallbackFailure("AW execution is unavailable")

    def begin(self) -> None:
        if self.running or self.closing or self.active or self.pending or self.bridge.failure:
            raise CallbackFailure("AW previous execution has not drained")
        self.execution += 1
        self.running = True

    def produce(self, name: str) -> Invocation:
        self.check()
        if not isinstance(name, str) or not name or len(name.encode()) > 512:
            raise CallbackFailure("AW model tool name is invalid")
        if len(self.active) >= self.bridge.scope.invocation_limit:
            raise CallbackFailure("AW active invocation limit exceeded")
        entry = Invocation(str(uuid4()), name, self.execution)
        self.active[entry.call_id] = entry
        return entry

    async def register(self, entry: Invocation, arguments: Any) -> None:
        async with self.issuing:
            self.check()
            if (not isinstance(arguments, dict) or entry.phase != "producing"
                    or self.active.get(entry.call_id) is not entry
                    or entry.execution != self.execution):
                raise CallbackFailure("AW model tool input is invalid or duplicated")
            self.sequence += 1
            if self.sequence >= 2**64:
                raise CallbackFailure("AW invocation sequence exhausted")
            entry.sequence = self.sequence
            entry.arguments = digest(arguments)

            async def admit() -> None:
                await self.bridge.scope.register(entry.sequence, entry.call_id, entry.name, arguments)
                entry.phase = "issued"

            await settle(admit())

    def claim(self, call_id: str, name: str, arguments: Any, phase: str) -> Invocation:
        self.check()
        ticket = dispatch.get()
        entry = self.active.get(call_id)
        if (
            ticket is None
            or ticket.call_id != call_id
            or ticket.execution != self.execution
            or entry is None
            or entry.execution != self.execution
            or entry.name != name
            or entry.arguments != digest(arguments)
            or entry.phase != ("issued" if phase == "before" else "allowed")
        ):
            raise CallbackFailure("AW rejected a stale, duplicate or changed invocation")
        entry.phase = phase
        return entry

    async def finish(self, entry: Invocation) -> None:
        if self.active.get(entry.call_id) is not entry:
            raise CallbackFailure("AW invocation already retired")
        if entry.phase != "adopted" and entry.sequence:
            entry.phase = "closing"

            async def retire() -> None:
                await self.bridge.scope.finish(entry.sequence, entry.call_id)
                entry.phase = "adopted"

            await settle(retire())
        # Native MCP PostTool and caller hooks follow the managed handler. Keep
        # their bounded identity until the next model request or execution drain.
        entry.phase = "adopted"

    def reserve(self, request_id: str, call_id: str | None, callback: str) -> Dispatch:
        self.bridge.scope.check()
        if self.bridge.failure or self.closing or (call_id is not None and not self.running):
            raise CallbackFailure("AW callback arrived outside its execution")
        if (
            request_id in self.pending
            or len(self.pending) >= self.bridge.scope.invocation_limit * 8
        ):
            raise CallbackFailure("AW callback dispatch duplicate or capacity exceeded")
        if call_id is not None:
            entry = self.active.get(call_id)
            if (
                entry is None
                or entry.execution != self.execution
                or callback in entry.callbacks
                or len(entry.callbacks) >= self.bridge.scope.invocation_limit * 8
            ):
                raise CallbackFailure("AW callback identity was retired or already dispatched")
            entry.callbacks.add(callback)
        ticket = Dispatch(request_id, call_id, self.execution)
        self.pending[request_id] = ticket
        return ticket

    def cancel(self, request_id: str | None) -> None:
        ticket = self.pending.get(request_id)
        if ticket is not None and not ticket.entered:
            # The SDK may cancel its task before our context manager runs.
            # Entered callbacks retain their ticket until async cleanup ends;
            # the invocation's callback set still rejects duplicate dispatch.
            del self.pending[request_id]
            self.changed.set()

    def native_denial(self) -> None:
        ticket = dispatch.get()
        entry = self.active.get(ticket.call_id) if ticket is not None else None
        if entry is None or entry.execution != self.execution or not ticket.entered:
            raise CallbackFailure("AW native denial has no live invocation")
        entry.native_denied = True

    @asynccontextmanager
    async def callback(self) -> AsyncIterator[Dispatch]:
        ticket = dispatch.get()
        if (
            ticket is None
            or ticket.entered
            or ticket.execution != self.execution
            or self.pending.get(ticket.request_id) is not ticket
        ):
            raise CallbackFailure("AW callback has no live dispatch identity")
        ticket.entered = True
        try:
            yield ticket
        finally:
            self.pending.pop(ticket.request_id, None)
            self.changed.set()

    async def drain(self) -> None:
        self.closing = True
        async with asyncio.timeout(5):
            # Registration must acknowledge before retirement; adoption owns
            # the same entries and must finish before drain takes them over.
            async with self.issuing, self.adopting:
                while self.pending:
                    self.changed.clear()
                    if self.pending:
                        await self.changed.wait()
                for entry in list(self.active.values()):
                    if entry.phase == "producing":
                        if entry.sequence:
                            raise CallbackFailure("AW invocation registration did not complete")
                    else:
                        await self.finish(entry)
                    del self.active[entry.call_id]
        self.running = False
        self.closing = False

    async def check_model_request(self, body: dict[str, Any]) -> None:
        # Concurrent subagent requests can contain the same completed history.
        # One request owns retirement while the others wait for its outcome.
        async with self.adopting:
            await self._adopt_results(body)

    async def _adopt_results(self, body: dict[str, Any]) -> None:
        self.check()
        # A tool result may enter a later model request only after AW has adopted
        # its final result or denial. Other active calls may belong to subagents.
        for message in body.get("messages", []):
            content = message.get("content")
            if isinstance(content, list):
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        call = block.get("tool_use_id")
                        entry = self.active.get(call)
                        if entry is None:
                            continue
                        async with asyncio.timeout(5):
                            while any(ticket.call_id == call for ticket in self.pending.values()):
                                self.changed.clear()
                                await self.changed.wait()
                        self.check()
                        if entry.native_denied and block.get("is_error") is not True:
                            raise CallbackFailure("AW denied invocation returned a success result")
                        # CLI validation can fail before any Hook, and native
                        # permissions can deny after PreTool without a failure
                        # Hook. Only explicit errors may close those phases;
                        # a failed AW callback is still fenced by check() above.
                        native_error = block.get("is_error") is True and entry.phase in {
                            "issued", "allowed"
                        }
                        if native_error:
                            await self.finish(entry)
                        if entry.phase != "adopted":
                            raise CallbackFailure("AW model request precedes tool result adoption")
                        self.active.pop(call)
