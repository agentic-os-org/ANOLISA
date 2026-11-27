"""Pin the SDK subprocess transport and carry dispatch identity into its callback tasks."""

from __future__ import annotations

import asyncio
from dataclasses import replace
import os
from typing import Any, AsyncGenerator, AsyncIterable, AsyncIterator
from urllib.parse import SplitResult, urlsplit

from claude_agent_sdk import ClaudeAgentOptions, Transport
from claude_agent_sdk._internal.transport.subprocess_cli import SubprocessCLITransport

if __package__:
    from .aw_claude import CallbackFailure
    from .aw_invocations import digest, dispatch
else:
    from aw_claude import CallbackFailure
    from aw_invocations import digest, dispatch


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


def _cli_bypasses_relay(relay: SplitResult, exclusions: str) -> bool:
    # The bundled CLI accepts host suffixes at a dot boundary. With a port,
    # only the full authority matches. A single leading dot is optional.
    host = relay.hostname
    if not host:
        return False
    for entry in exclusions.split(","):
        rule = entry.strip(" \t\r\n\v\f")
        if rule == "*":
            return True
        rule = rule.removeprefix(".")
        if rule in (relay.netloc, host) or host.endswith("." + rule):
            return True
    return False


class ManagedTransport(Transport):
    """Pin native transport, settings controls and callback dispatch identity."""

    def __init__(self, options: ClaudeAgentOptions, bridge: Any) -> None:
        if options.can_use_tool:
            options = replace(options, permission_prompt_tool_name="stdio")
        self.delegate = SubprocessCLITransport(prompt=empty_input(), options=options)
        self.bridge = bridge
        self._reader: AsyncGenerator[dict[str, Any], None] | None = None
        self._closed = False

    async def model_environment(self, query: Any) -> dict[str, str]:
        # Let the CLI resolve inline/file, filesystem and managed source precedence.
        response = await query._send_control_request({"subtype": "get_settings"})
        settings = response.get("effective") if isinstance(response, dict) else None
        environment = settings.get("env", {}) if isinstance(settings, dict) else None
        if not isinstance(environment, dict) or any(
            not isinstance(key, str) or not isinstance(value, str)
            for key, value in environment.items()
        ):
            raise CallbackFailure("AW requires the CLI effective settings environment")
        return dict(environment)

    async def route_model(
        self, query: Any, endpoint: str, base_environment: dict[str, str] | None = None,
        effective_environment: dict[str, str] | None = None,
    ) -> None:
        base = os.environ if base_environment is None else base_environment
        if effective_environment is None:
            effective_environment = await self.model_environment(query)
        relay = urlsplit(endpoint)
        flags = {
            "ANTHROPIC_BASE_URL": endpoint,
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        }
        # The bundled CLI accepts a port-specific exemption on a live connection.
        # Preserve both sources even when native settings shadow an inherited list.
        rules = dict.fromkeys(
            rule.strip() for source in (base, effective_environment)
            for name in ("NO_PROXY", "no_proxy")
            for rule in source.get(name, "").split(",") if rule.strip()
        )
        rules[relay.netloc] = None
        flags["NO_PROXY"] = flags["no_proxy"] = ",".join(rules)
        await query._send_control_request({
            "subtype": "apply_flag_settings", "settings": {"env": flags},
        })
        environment = await self.model_environment(query)
        if environment.get("ANTHROPIC_BASE_URL") != endpoint:
            raise CallbackFailure("Native settings override the required AW model relay")
        adopted = {**base, **environment}
        # Native model requests select HTTP(S) proxies and ignore ALL_PROXY.
        if any(adopted.get(key) for key in ("http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY")):
            bypass = adopted.get("no_proxy") or adopted.get("NO_PROXY", "")
            if not _cli_bypasses_relay(relay, bypass):
                raise CallbackFailure("Native settings override the required AW model relay proxy bypass")

    async def connect(self) -> None:
        if self._closed:
            raise RuntimeError("AW transports cannot reconnect; create a new client")
        await self.delegate.connect()

    async def write(self, data: str) -> None:
        self.bridge.invocations.settle_mcp_validation_error(dispatch.get(), data)
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
                        version = message.get("claude_code_version")
                        if (not isinstance(version, str) or not version or len(version) > 128
                                or not version[0].isascii() or not version[0].isdigit()
                                or any(not char.isascii() or not (char.isalnum() or char in ".+-_!")
                                       for char in version)):
                            raise CallbackFailure("AW requires Claude CLI version metadata in its init frame")
                    if message.get("type") == "control_request":
                        request = message["request"]
                        kind = request.get("subtype")
                        call_id = None
                        callback = kind
                        tracked = kind in {"hook_callback", "can_use_tool"}
                        stop_fingerprint = None
                        if kind == "hook_callback":
                            callback = "hook:" + request["callback_id"]
                            payload = request.get("input", {})
                            # UserPromptSubmit carries a native input identifier in
                            # the callback envelope, outside the tool-call ledger.
                            if payload.get("hook_event_name") == "Stop":
                                stop_fingerprint = digest(request)
                            elif payload.get("hook_event_name") != "UserPromptSubmit":
                                call_id = request.get("tool_use_id") or payload.get("tool_use_id")
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
                        ledger = self.bridge.invocations
                        if tracked and call_id is not None and ledger.direct:
                            call_id = await ledger.native_dispatch_id(call_id, request)
                        if tracked and stop_fingerprint is not None:
                            ticket = self.bridge.invocations.reserve(
                                message["request_id"], call_id, callback,
                                stop_fingerprint=stop_fingerprint,
                            )
                            if ticket is None:
                                continue
                        elif tracked:
                            ticket = self.bridge.invocations.reserve(message["request_id"], call_id, callback)
                    elif message.get("type") == "user" and self.bridge.invocations.direct:
                        await self.bridge.invocations.native_results(message.get("message", {}).get("content"))
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
