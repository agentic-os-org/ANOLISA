"""Real SDK startup errors and cancellation must reclaim tasks without poisoning the host."""

import asyncio
import json
import os
from pathlib import Path
import subprocess
import time
from typing import Any, Awaitable
from unittest.mock import patch

import anyio
import aw_resident
import aw_scope
from aw_claude import create_client, create_sdk_mcp_server
from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient, PermissionResultAllow
from claude_agent_sdk._internal.query import Query


async def bounded(operation: Awaitable[Any]) -> Any:
    task = asyncio.ensure_future(operation)
    try:
        done, _ = await asyncio.wait({task}, timeout=8)
        assert done, "SDK startup or cleanup exceeded its test deadline"
        return await task
    finally:
        # A regressed SDK error handler can consume the first cancellation.
        for _ in range(2):
            if task.done():
                break
            task.cancel()
            await asyncio.wait({task}, timeout=3)
        assert task.done(), "SDK task survived bounded test cleanup"


def options(root: Path, name: str) -> ClaudeAgentOptions:
    directory = root / name
    directory.mkdir()
    return ClaudeAgentOptions(
        cwd=str(directory),
        setting_sources=[],
        tools=[],
        env={
            "CLAUDE_CONFIG_DIR": str(directory / "profile"),
            "ANTHROPIC_BASE_URL": "http://127.0.0.1:1",
            "ANTHROPIC_API_KEY": "local-fixture",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        },
    )


def record_process(client: ClaudeSDKClient) -> int:
    # Private SDK inspection is confined to native process-cleanup evidence.
    pid = client._transport.delegate._process.pid
    directory = Path(client.options.cwd)
    (directory / "sdk-process.json").write_text(
        json.dumps(
            {
                "pid": pid,
                "owner": os.getpid(),
                "cwd": str(directory),
                "ports": [],
                "lifetime_seconds": 8,
                "stop": f"kill -TERM -- {pid}",
            }
        )
    )
    return pid


def assert_released(client: Any, root: Path, config: dict[str, Any]) -> None:
    assert client._aw_state == "closed"
    assert client._aw_bridge.scope.closed
    assert client._aw_bridge.scope.release_error is None
    assert client not in aw_resident._active
    assert not aw_resident._poisoned
    status = json.loads(
        subprocess.check_output(
            [
                config["executable"],
                "status",
                "--config",
                str(root / "aw.json"),
            ],
            timeout=5,
        )
    )["result"]
    assert status["bindings"] == 1 and status["events"] == 0, status


async def recover(client: Any, root: Path, config: dict[str, Any], name: str) -> None:
    assert_released(client, root, config)
    replacement = create_client(options(root, name + "-recovery"))

    async def connect_and_close() -> None:
        try:
            await replacement.connect()
            pid = record_process(replacement)
        finally:
            await replacement.disconnect()
        assert not Path(f"/proc/{pid}").exists()

    await bounded(connect_and_close())
    assert_released(replacement, root, config)


async def validation_error(root: Path, config: dict[str, Any]) -> None:
    async def permission(
        name: str, arguments: dict[str, Any], context: Any
    ) -> PermissionResultAllow:
        return PermissionResultAllow()

    settings = options(root, "validation-error")
    settings.can_use_tool = permission
    client = create_client(settings)
    try:
        await bounded(client.connect("invalid string prompt with can_use_tool"))
    except ValueError as error:
        assert "can_use_tool callback requires streaming mode" in str(error)
    else:
        raise AssertionError("Native SDK validation error was lost")
    await recover(client, root, config, "validation-error")


async def mcp_constructor_claims(root: Path, config: dict[str, Any], mode: str) -> None:
    fresh = create_sdk_mcp_server("fresh")
    settings = options(root, "mcp-" + mode)
    settings.mcp_servers = {"local": fresh}
    owner = None
    try:
        if mode == "owned-server":
            owned = create_sdk_mcp_server("owned")
            owner_settings = options(root, "mcp-existing-owner")
            owner_settings.mcp_servers = {"owned": owned}
            owner = create_client(owner_settings)
            settings.mcp_servers["owned"] = dict(owned)
        elif mode == "duplicate-alias":
            settings.mcp_servers["alias"] = dict(fresh)
        if mode == "sdk-error":
            with patch.object(ClaudeSDKClient, "__init__", side_effect=RuntimeError("injected SDK constructor failure")):
                try:
                    create_client(settings)
                except RuntimeError as error:
                    assert str(error) == "injected SDK constructor failure"
                else:
                    raise AssertionError("SDK constructor failure was lost")
        else:
            try:
                create_client(settings)
            except ValueError as error:
                assert "fresh AW MCP server" in str(error)
            else:
                raise AssertionError("Invalid MCP ownership was admitted")
        assert not fresh.claimed and fresh.bridge is None
        if owner is not None:
            assert owned.bridge is owner._aw_bridge
            assert owner._aw_mcp == [owned]
        settings.mcp_servers = {"local": fresh}
        retry = create_client(settings)
        assert fresh.bridge is retry._aw_bridge
        async def connect_and_close() -> None:
            try:
                await retry.connect()
                pid = record_process(retry)
            finally:
                await retry.disconnect()
            assert not Path(f"/proc/{pid}").exists()
        await bounded(connect_and_close())
        assert_released(retry, root, config)
        assert fresh.claimed and fresh.bridge is None
        try:
            create_client(settings)
        except ValueError:
            pass
        else:
            raise AssertionError("Closed MCP server was reused")
    finally:
        if owner is not None:
            await bounded(owner.disconnect())
            assert_released(owner, root, config)


async def initialization(root: Path, config: dict[str, Any], cancel: bool) -> None:
    name = "initialize-cancel" if cancel else "initialize-error-concurrent-close"
    client = create_client(options(root, name))
    entered, finish = asyncio.Event(), asyncio.Event()
    pids, native_closes = [], []
    original_disconnect = ClaudeSDKClient.disconnect

    async def initialize(query: Query) -> None:
        pids.append(record_process(client))
        entered.set()
        await finish.wait()
        raise RuntimeError("injected SDK initialization failure")

    async def disconnect(sdk_client: ClaudeSDKClient) -> None:
        native_closes.append(sdk_client)
        await original_disconnect(sdk_client)

    with patch.object(Query, "initialize", initialize), patch.object(
        ClaudeSDKClient, "disconnect", disconnect
    ):

        async def exercise() -> None:
            connecting = asyncio.create_task(client.connect())
            closing = None
            try:
                await entered.wait()
                if cancel:
                    connecting.cancel()
                else:
                    closing = asyncio.create_task(client.disconnect())
                    await asyncio.sleep(0)
                    assert not closing.done(), "A different task bypassed the lifecycle lock"
                    finish.set()
                results = await asyncio.gather(
                    connecting, *([closing] if closing else []), return_exceptions=True
                )
                expected = asyncio.CancelledError if cancel else RuntimeError
                assert isinstance(results[0], expected), results
                if not cancel:
                    assert str(results[0]) == "injected SDK initialization failure"
                    assert results[1] is None
            finally:
                finish.set()
                for task in (connecting, closing):
                    if task is not None and not task.done():
                        await bounded(task)

        await bounded(exercise())
    assert native_closes == [client], native_closes
    assert pids and all(not Path(f"/proc/{pid}").exists() for pid in pids)
    await recover(client, root, config, name)


async def acknowledge_cancel(root: Path, config: dict[str, Any]) -> None:
    client = create_client(options(root, "acknowledge-cancel"))
    original = aw_scope.command
    acknowledged = False

    async def command(
        configuration: dict[str, Any], action: str, value: Any, *args: Any, **kwargs: Any
    ) -> tuple[int, bytes]:
        nonlocal acknowledged
        response = await original(configuration, action, value, *args, **kwargs)
        if action == "acknowledge":
            acknowledged = True
            cancellation.cancel()
            await anyio.sleep(0)
        return response

    with patch.object(aw_scope, "command", command):
        with anyio.fail_after(8):
            with anyio.CancelScope() as cancellation:
                await client.connect()
    assert acknowledged and cancellation.cancel_called
    await recover(client, root, config, "acknowledge-cancel")


async def acknowledge_expired(root: Path, config: dict[str, Any]) -> None:
    client = create_client(options(root, "acknowledge-expired"))
    original = aw_scope.command

    async def command(
        configuration: dict[str, Any], action: str, value: Any, *args: Any, **kwargs: Any
    ) -> tuple[int, bytes]:
        if action == "acknowledge":
            # Let actual service maintenance reclaim the unacknowledged handle.
            deadline = time.monotonic() + 7
            while True:
                status = json.loads(
                    subprocess.check_output(
                        [
                            config["executable"],
                            "status",
                            "--config",
                            str(root / "aw.json"),
                        ],
                        timeout=3,
                    )
                )["result"]
                if status["bindings"] == 1:
                    assert status["events"] == 0, status
                    break
                assert time.monotonic() < deadline, "Expired handoff was not reclaimed"
                await asyncio.sleep(0.05)
        return await original(configuration, action, value, *args, **kwargs)

    with patch.object(aw_scope, "command", command):
        try:
            await bounded(client.connect())
        except RuntimeError as error:
            assert str(error) == "AW task handoff acknowledgement failed", error
        else:
            raise AssertionError("Expired task was admitted")
    await recover(client, root, config, "acknowledge-expired")


async def run(root: Path, config: dict[str, Any]) -> None:
    cases = {
        "validation-error": lambda: validation_error(root, config),
        "mcp-owned-server": lambda: mcp_constructor_claims(root, config, "owned-server"),
        "mcp-duplicate-alias": lambda: mcp_constructor_claims(root, config, "duplicate-alias"),
        "mcp-sdk-error": lambda: mcp_constructor_claims(root, config, "sdk-error"),
        "initialize-error-concurrent-close": lambda: initialization(root, config, False),
        "initialize-cancel": lambda: initialization(root, config, True),
        "acknowledge-cancel": lambda: acknowledge_cancel(root, config),
        "acknowledge-expired": lambda: acknowledge_expired(root, config),
    }
    selected = os.environ.get("AW_TEST_STARTUP_CASE")
    if selected is not None:
        cases = {selected: cases[selected]}
    results = []
    for name, case in cases.items():
        await case()
        results.append({"case": name, "passed": True})
    (root / "result.json").write_text(json.dumps({"cases": results, "passed": True}))
