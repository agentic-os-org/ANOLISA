"""Bind public SDK MCP handlers to one client without guessing native tool-use IDs."""

from __future__ import annotations

from dataclasses import dataclass, replace
import json
import re
from typing import Any
import weakref

from claude_agent_sdk import SdkMcpTool, create_sdk_mcp_server as sdk_server
from aw_claude import CallbackFailure, _copy_json

SOURCE = "claude-sdk-mcp/v1"
_servers = weakref.WeakKeyDictionary()


def identifier(value: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"[A-Za-z0-9_-]{1,128}", value) is None:
        raise ValueError(
            "AW MCP names require 1..128 ASCII letters, digits, underscores or hyphens"
        )
    return value


@dataclass(frozen=True)
class _Claim:
    key: str
    bridge: Any


class ManagedServer(dict):
    """A dict-compatible public SDK configuration, claimed once by one task client."""

    def __init__(self, name: str, version: str, tools: list[SdkMcpTool[Any]]) -> None:
        self._claim: _Claim | None = None
        tools = [replace(tool) for tool in tools]
        names = [identifier(tool.name) for tool in tools]
        if len(set(names)) != len(names):
            raise ValueError("AW MCP tool names must be unique")
        self._names = tuple(names)
        wrapped = []
        for tool in tools:

            async def handler(arguments: dict[str, Any], tool: SdkMcpTool[Any] = tool) -> Any:
                return await self.invoke(tool, arguments)

            wrapped.append(replace(tool, handler=handler))
        super().__init__(sdk_server(name, version, wrapped))
        _servers[self["instance"]] = weakref.ref(self)

    @property
    def claimed(self) -> bool:
        return self._claim is not None

    @property
    def key(self) -> str | None:
        return self._claim.key if self._claim is not None else None

    @property
    def bridge(self) -> Any:
        return self._claim.bridge if self._claim is not None else None

    @property
    def names(self) -> tuple[str, ...]:
        return self._names

    def claim(self, key: str, bridge: Any) -> set[str]:
        if self.claimed:
            raise ValueError("Create a fresh AW MCP server for each task client")
        self._claim = _Claim(identifier(key), bridge)
        bridge.mcp_servers.add(self._claim.key)
        return {f"mcp__{self._claim.key}__{name}" for name in self._names}

    def release(self) -> None:
        if self._claim is not None:
            self._claim = replace(self._claim, bridge=None)

    async def steps(self, payload: dict[str, Any]) -> dict[str, Any] | None:
        bridge = self.bridge
        if bridge is None:
            raise CallbackFailure("MCP server has no live task client")
        try:
            bridge.scope.check()
            if bridge.failure:
                raise CallbackFailure("MCP task has already stopped")
            for hook in bridge.config["hooks"]:
                if hook["event"] != payload["event"]:
                    continue
                code, output = await bridge.scope.invoke(hook, payload)
                if code == 2 and payload["event"] == "tool.before":
                    reason = json.loads(output or b'"Blocked by AW policy"')
                    if not isinstance(reason, str):
                        raise CallbackFailure("MCP denial reason is invalid")
                    return {"content": [{"type": "text", "text": reason}], "is_error": True}
                if code != 0:
                    raise CallbackFailure("MCP Hook execution failed")
                directive = json.loads(output or b"{}")
                if directive == {}:
                    continue
                if not hook["replace_result"] or set(directive) != {"aw_mcp_result"}:
                    raise CallbackFailure("MCP Hook returned an unsupported directive")
                original = payload["tool_result"]
                result = directive["aw_mcp_result"]
                if payload["is_error"] or "structuredContent" in original:
                    raise CallbackFailure("MCP result is not eligible for replacement")
                blocks = result.get("content") if isinstance(result, dict) else None
                if (
                    not isinstance(blocks, list)
                    or len(blocks) != 1
                    or blocks[0].get("type") != "text"
                    or not isinstance(blocks[0].get("text"), str)
                ):
                    raise CallbackFailure("MCP replacement requires one text block")
                compared = _copy_json(result)
                compared["content"][0]["text"] = original["content"][0]["text"]
                if compared != original:
                    raise CallbackFailure("MCP replacement changed the native result envelope")
                return result
            return None
        except Exception:
            bridge.failure_reply(payload["event"], {})
            await bridge.stop_task()
            raise CallbackFailure("AW stopped the MCP task after a callback failure") from None

    async def invoke(self, tool: SdkMcpTool[Any], arguments: dict[str, Any]) -> dict[str, Any]:
        claim = self._claim
        bridge = claim.bridge if claim is not None else None
        if bridge is None:
            raise CallbackFailure("MCP server must belong to a connected AW task client")
        bridge.scope.check()
        async with bridge.invocations.callback() as ticket:
            entry = None
            try:
                entry = bridge.invocations.claim(
                    ticket.call_id, f"mcp__{claim.key}__{tool.name}", arguments, "before"
                )
                return await self.execute(entry, claim, tool, arguments)
            except Exception:
                if entry is None or entry.phase != "adopted":
                    bridge.failure_reply("tool.after", {})
                    await bridge.stop_task()
                raise

    async def execute(
        self, entry: Any, claim: _Claim, tool: SdkMcpTool[Any], arguments: dict[str, Any]
    ) -> dict[str, Any]:
        bridge = claim.bridge
        payload = {
            "aw_source": SOURCE,
            "event": "tool.before",
            "scope_id": bridge.scope.handle["binding"]["instance_id"],
            "invocation_id": entry.call_id,
            "server": claim.key,
            "tool_name": tool.name,
            "tool_input": _copy_json(arguments),
        }
        denial = await self.steps(payload)
        if denial is not None:
            await bridge.invocations.finish(entry)
            return denial
        entry.phase = "allowed"
        try:
            result = await tool.handler(_copy_json(arguments))
        except Exception as error:
            bridge.invocations.claim(entry.call_id, entry.name, arguments, "after")
            await self.steps(
                {
                    **payload,
                    "event": "tool.after",
                    "is_error": True,
                    "tool_result": {
                        "content": [{"type": "text", "text": str(error)}],
                        "is_error": True,
                    },
                }
            )
            await bridge.invocations.finish(entry)
            raise
        try:
            snapshot = _copy_json(result)
            if (
                not isinstance(snapshot, dict)
                or "isError" in snapshot
                or type(snapshot.get("is_error", False)) is not bool
            ):
                raise CallbackFailure("SDK MCP results require the public boolean is_error field")
        except Exception:
            bridge.failure_reply("tool.after", {})
            await bridge.stop_task()
            raise CallbackFailure(
                "AW stopped the MCP task after an invalid handler result"
            ) from None
        bridge.invocations.claim(entry.call_id, entry.name, arguments, "after")
        replacement = await self.steps(
            {
                **payload,
                "event": "tool.after",
                "tool_result": snapshot,
                "is_error": snapshot.get("is_error", False),
            }
        )
        await bridge.invocations.finish(entry)
        return replacement if replacement is not None else snapshot


def create_sdk_mcp_server(
    name: str, version: str = "1.0.0", tools: list[SdkMcpTool[Any]] | None = None
) -> ManagedServer:
    """Keep the SDK tool catalog and closures; attach the server in create_client options."""
    return ManagedServer(identifier(name), version, list(tools or []))


def snapshot_servers(servers: Any) -> Any:
    """Freeze server selection while retaining the SDK instances and business closures."""
    if not isinstance(servers, dict):
        return servers
    return {
        name: dict(server) if isinstance(server, dict) else server
        for name, server in servers.items()
    }


def claim_servers(servers: Any, bridge: Any) -> list[ManagedServer]:
    pending = []
    seen = set()
    if isinstance(servers, dict):
        # Delimiter-bearing server keys can alias another server's tool namespace.
        managed_present = any(
            isinstance(value, dict)
            and value.get("type") == "sdk"
            and _servers.get(value.get("instance")) is not None
            for value in servers.values()
        )
        if managed_present:
            for key in servers:
                identifier(key)
                if "__" in key:
                    raise ValueError(
                        "Managed MCP configurations forbid ambiguous server keys containing __"
                    )
        for key, server in servers.items():
            if isinstance(server, dict) and server.get("type") == "sdk":
                reference = _servers.get(server.get("instance"))
                managed = reference() if reference is not None else None
                if managed is not None:
                    if managed.claimed or id(managed) in seen:
                        raise ValueError("Create a fresh AW MCP server for each task client")
                    seen.add(id(managed))
                    pending.append((key, managed))
    # Complete synchronous validation before consuming any server's one-use claim.
    for key, managed in pending:
        bridge.mcp_names.update(managed.claim(key, bridge))
    return [managed for _, managed in pending]
