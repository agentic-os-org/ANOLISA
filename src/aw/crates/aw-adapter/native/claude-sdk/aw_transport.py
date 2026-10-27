"""Pin the SDK subprocess transport and carry dispatch identity into its callback tasks."""

from __future__ import annotations

import asyncio
from dataclasses import replace
from typing import Any, AsyncGenerator, AsyncIterable, AsyncIterator

from claude_agent_sdk import ClaudeAgentOptions, Transport
from claude_agent_sdk._internal.transport.subprocess_cli import SubprocessCLITransport

from aw_claude import CallbackFailure
from aw_invocations import dispatch


async def empty_input() -> AsyncIterator[dict[str, Any]]:
    # Custom transport still selects the SDK's streaming control protocol.
    for message in ():
        yield message


async def single_input(prompt: AsyncIterable[dict[str, Any]]) -> dict[str, Any]:
    # Native CLI batches can coalesce several inputs into one result. Admit a
    # complete one-message input before writing anything to the native process.
    iterator = aiter(prompt)
    try:
        try:
            message = await anext(iterator)
        except StopAsyncIteration:
            raise ValueError("AW query requires one user input message") from None
        try:
            await anext(iterator)
        except StopAsyncIteration:
            return message
        raise ValueError("AW query requires one user input message; submit sequential queries")
    finally:
        close = getattr(iterator, "aclose", None)
        if close is not None:
            async with asyncio.timeout(5):
                await close()


class ManagedTransport(Transport):
    """The pinned transport interface is the only SDK-private integration seam."""

    def __init__(self, options: ClaudeAgentOptions, bridge: Any) -> None:
        if options.can_use_tool:
            options = replace(options, permission_prompt_tool_name="stdio")
        self.delegate = SubprocessCLITransport(prompt=empty_input(), options=options)
        self.bridge = bridge
        self._reader: AsyncGenerator[dict[str, Any], None] | None = None
        self._closed = False

    async def connect(self) -> None:
        if self._closed:
            raise RuntimeError("AW transports cannot reconnect; create a new client")
        await self.delegate.connect()

    async def write(self, data: str) -> None:
        await self.delegate.write(data)

    async def close(self) -> None:
        self._closed = True
        try:
            if self._reader is not None:
                await self._reader.aclose()
        finally:
            await self.delegate.close()

    async def end_input(self) -> None:
        await self.delegate.end_input()

    def is_ready(self) -> bool:
        return self.delegate.is_ready()

    def read_messages(self) -> AsyncIterator[dict[str, Any]]:
        if self._closed:
            raise RuntimeError("AW transport is closed; create a new client")
        # One reader per transport; a new client owns the next connection.
        # SDK Query.close joins its reader task before closing this transport,
        # but does not close the iterator when that task stops outside anext().
        if self._reader is None:
            self._reader = self._read_messages()
        return self._reader

    async def _read_messages(self) -> AsyncGenerator[dict[str, Any], None]:
        owner = asyncio.current_task()
        messages = self.delegate.read_messages()
        try:
            async for message in messages:
                ticket = None
                try:
                    if message.get("type") == "system" and message.get("subtype") == "init":
                        if message.get("claude_code_version") != "2.1.161":
                            raise CallbackFailure("AW managed identities require Claude CLI 2.1.161")
                    if message.get("type") == "control_request":
                        request = message["request"]
                        kind = request.get("subtype")
                        call_id = None
                        callback = kind
                        tracked = kind in {"hook_callback", "can_use_tool"}
                        if kind == "hook_callback":
                            callback = "hook:" + request["callback_id"]
                            call_id = request.get("tool_use_id") or request.get("input", {}).get(
                                "tool_use_id"
                            )
                        elif kind == "can_use_tool":
                            call_id = request.get("tool_use_id")
                        elif kind == "mcp_message":
                            mcp = request.get("message", {})
                            server = request.get("server_name")
                            tracked = (
                                mcp.get("method") == "tools/call" and server in self.bridge.mcp_servers
                            )
                            if tracked:
                                callback = "mcp:" + server
                                call_id = (
                                    mcp.get("params", {}).get("_meta", {}).get("claudecode/toolUseId")
                                )
                                if not isinstance(call_id, str):
                                    raise CallbackFailure(
                                        "AW managed MCP requires native tool-use identity"
                                    )
                        if tracked:
                            ticket = self.bridge.invocations.reserve(
                                message["request_id"], call_id, callback
                            )
                    elif message.get("type") == "control_cancel_request":
                        self.bridge.invocations.cancel(message.get("request_id"))
                except Exception:
                    self.bridge.failure_reply("tool.after", {})
                    # Keep the reader available for interrupt acknowledgements.
                    # A rejected frame never reaches native business code.
                    self.bridge.request_stop()
                    continue
                token = dispatch.set(ticket)
                try:
                    # SDK 0.2.88 starts its callback task in this context before
                    # asking for another frame. Closure must never resume dispatch.
                    yield message
                finally:
                    if asyncio.current_task() is owner:
                        dispatch.reset(token)
        finally:
            await _cancel_reader(messages)


async def _cancel_reader(messages: AsyncGenerator[dict[str, Any], None]) -> None:
    # SDK 0.2.88 swallows GeneratorExit and waits for the still-open CLI.
    # Cancellation closes its generator without that wait; transport.close owns
    # process termination. Consume only our injected cancellation, never a fault.
    cancellation = asyncio.CancelledError()
    try:
        await messages.athrow(cancellation)
    except asyncio.CancelledError as error:
        if error is not cancellation:
            raise
    except StopAsyncIteration:
        pass
    if messages.ag_frame is not None:
        raise RuntimeError("Claude transport reader yielded during cancellation")
