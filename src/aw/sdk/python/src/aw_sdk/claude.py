"""Cooperative lifecycle for an application-owned Claude Python SDK worker."""

import asyncio
import os
from pathlib import Path
from typing import TYPE_CHECKING
from weakref import WeakSet

from aw_sdk import Instance

if TYPE_CHECKING:
    from claude_agent_sdk import ClaudeAgentOptions, SdkMcpTool
    from aw_sdk._native.claude.aw_mcp import ManagedServer
    from aw_sdk._native.claude.aw_resident import ManagedClient
    from aw_sdk._native.claude.aw_submission import SubmissionDenied

__all__ = ["ResidentClaude", "SubmissionDenied", "create_sdk_mcp_server"]


class ResidentClaude:
    """Publish attach before admitting clients under an immutable AW Task.

    The worker always requires AW: when native interruption or cleanup cannot be
    confirmed, the resident Bridge terminates this worker's process group.
    """

    @classmethod
    def from_config(
        cls,
        config_file: str,
        *,
        target: str | None = None,
        descriptor_file: str | None = None,
        executable: str = "aw",
        attach_timeout_ms: int = 5000,
        reconnect_ms: int = 1000,
    ) -> "ResidentClaude":
        """Select wiring from a trusted descriptor or AW's offline configuration parser."""
        from aw_sdk.configuration import load_configuration
        from aw_sdk._native.claude.aw_sdk_scope import PROFILE

        wiring = load_configuration(
            config_file,
            PROFILE,
            target,
            executable,
            descriptor_file=descriptor_file,
            requires_service=True,
        )
        return cls(
            endpoint=wiring["endpoint"],
            target=wiring["target"],
            config=config_file,
            input_enabled=wiring["input_enabled"],
            attach_timeout_ms=attach_timeout_ms,
            reconnect_ms=reconnect_ms,
        )

    def __init__(
        self,
        *,
        endpoint: str,
        target: str,
        config: str | None = None,
        input_enabled: bool = True,
        attach_timeout_ms: int = 5000,
        reconnect_ms: int = 1000,
    ) -> None:
        from aw_sdk._native.claude.aw_sdk_scope import capabilities, validate_routes

        pid = os.getpid()
        if pid != os.getpgrp() or pid != os.getsid(0):
            raise RuntimeError("Cooperative Claude requires an isolated process started with setsid")
        if type(attach_timeout_ms) is not int or not 100 <= attach_timeout_ms <= 60000:
            raise ValueError("attach_timeout_ms must be 100..60000")
        self.attach_timeout_ms = attach_timeout_ms
        self.pid = pid
        self._clients: WeakSet["ManagedClient"] = WeakSet()
        self._closing: asyncio.Task | None = None
        self.instance = Instance(
            endpoint=endpoint,
            target=target,
            config=Path(config).read_text() if config is not None else None,
            capabilities=capabilities(input_enabled),
            availability="required",
            install=validate_routes,
            reconnect_ms=reconnect_ms,
        )

    def _check_owner(self) -> None:
        if self.pid != os.getpid():
            raise RuntimeError("Create the Claude worker after fork")

    def listen(self, *, automatic: bool = False) -> None:
        """Publish cooperative readiness for the aw connect command."""
        self._check_owner()
        if self._closing is not None:
            raise RuntimeError("Claude worker is closed")
        self.instance.listen(automatic=automatic)

    def create_client(self, options: "ClaudeAgentOptions") -> "ManagedClient":
        """Create a resident native client with AW admission at connect."""
        from aw_sdk._native.claude.aw_resident import ManagedClient
        from aw_sdk._native.claude.aw_sdk_scope import SdkScope

        self._check_owner()
        if self._closing is not None:
            raise RuntimeError("Claude worker is closed")

        def check_open() -> None:
            self._check_owner()
            if self._closing is not None:
                raise RuntimeError("Claude worker is closed")

        client = ManagedClient(
            options,
            scope_factory=lambda install: SdkScope(
                self.instance, self.attach_timeout_ms, install, before_open=check_open
            ),
        )
        self._clients.add(client)
        return client

    async def close(self) -> None:
        """Withdraw the listener after the native client owners finish teardown."""
        self._check_owner()
        if any(client._aw_live for client in self._clients):
            raise RuntimeError("Disconnect Claude clients before closing the worker")
        if self._closing is None:
            self._closing = asyncio.create_task(self.instance.close())
        await asyncio.shield(self._closing)


def create_sdk_mcp_server(
    name: str, version: str = "1.0.0", tools: "list[SdkMcpTool] | None" = None
) -> "ManagedServer":
    """Create an in-process MCP server with the resident Adapter's effect adoption."""
    from aw_sdk._native.claude.aw_mcp import create_sdk_mcp_server as create

    return create(name, version, tools)


def __getattr__(name: str) -> type[Exception]:
    if name == "SubmissionDenied":
        from aw_sdk._native.claude.aw_submission import SubmissionDenied

        return SubmissionDenied
    raise AttributeError(name)
