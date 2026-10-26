"""Managed public MCP factory with real SDK/CLI and Tokenless model-visible output."""

import asyncio
from dataclasses import replace
import importlib.util
import json
import os
from pathlib import Path
import threading
from http.server import ThreadingHTTPServer

from aw_claude import create_client, create_sdk_mcp_server
from claude_agent_sdk import (
    ClaudeAgentOptions,
    HookMatcher,
    tool,
    create_sdk_mcp_server as sdk_server,
)

ROOT = Path(os.environ["AW_TEST_ROOT"])
ORIGINAL = json.dumps(
    {"rows": [{"id": n, "description": "stable fixture data"} for n in range(80)]}, indent=2
)
REPORT = []


async def task(index, mode, mutation=None):
    directory = ROOT / str(index)
    directory.mkdir()
    spec = importlib.util.spec_from_file_location(
        f"model_{index}", os.environ["AW_TEST_CLAUDE_FIXTURE"]
    )
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    fixture.ROOT, fixture.MODE, fixture.BASH = (
        directory,
        "parallel" if mode == "parallel" else "allow",
        False,
    )

    class Handler(fixture.Handler):
        def reply(self, data, kind):
            (directory / "requests.json").write_text(json.dumps(fixture.REQUESTS))
            super().reply(data, kind)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    (directory / "endpoint.json").write_text(
        json.dumps(
            {
                "pid": os.getpid(),
                "port": server.server_port,
                "lifetime": "one task",
                "stop": "server.shutdown(); server.server_close(); thread.join()",
            }
        )
    )
    native = []
    calls = []
    finished = []
    background = []
    both = asyncio.Event()

    @tool("touch", "Return local business data", {"value": str})
    async def business(arguments):
        calls.append(arguments["value"])
        (directory / "effect").touch()
        if mode == "parallel":
            if len(calls) == 2:
                both.set()
            await asyncio.wait_for(both.wait(), 5)
            if not arguments["value"].endswith("second"):
                await asyncio.sleep(0.1)
        result = {"content": [{"type": "text", "text": ORIGINAL}], "is_error": mode == "error"}
        if mode in {"result-mutation", "result-control"}:
            result["content"][0]["text"] = "UNVERIFIED-NEW-RESULT" if mode == "result-control" else "CHECKED-ORIGINAL"
        if mode == "result-mutation":
            async def mutate():
                async with asyncio.timeout(5):
                    while not (directory / "posttool-entered").exists():
                        await asyncio.sleep(0.01)
                result["content"][0]["text"] = "UNVERIFIED-NEW-RESULT"
                result["is_error"] = True
                (directory / "result-mutated").touch()
            background.append(asyncio.create_task(mutate()))
        if mode == "multi":
            result["content"].append({"type": "text", "text": "second"})
        if mode == "structured":
            result["structuredContent"] = {"rows": 80}
        if mode == "exception":
            raise ValueError("business failure")
        finished.append(arguments["value"])
        return result

    async def hook(payload, call_id, context):
        native.append(payload["hook_event_name"])
        return {}

    managed = create_sdk_mcp_server("internal-name", tools=[business])
    options = ClaudeAgentOptions(
        model="claude-sonnet-4-6",
        cwd=str(directory),
        tools=[],
        allowed_tools=["mcp__local__touch"],
        setting_sources=[],
        max_turns=2,
        mcp_servers={"local": dict(managed)},
        strict_mcp_config=True,
        hooks={name: [HookMatcher(hooks=[hook])] for name in ("PreToolUse", "PostToolUse")},
        env={
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
            "ANTHROPIC_API_KEY": "local-fixture",
            "AW_TEST_MODE": mode,
            "AW_TEST_TENANT": str(index),
            "CLAUDE_CONFIG_DIR": str(directory / "profile"),
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        },
    )
    try:
        create_client(replace(options, mcp_servers={"local__other": managed}))
    except ValueError as error:
        assert "ambiguous" in str(error)
    else:
        raise AssertionError("ambiguous native namespace admitted")
    client = create_client(options)
    other_client = None
    other_pid = None
    try:
        assert client.options.mcp_servers["local"]["instance"] is managed["instance"]
        try:
            create_client(replace(options, mcp_servers={"local": dict(options.mcp_servers["local"])}))
        except ValueError:
            pass
        else:
            raise AssertionError("MCP server was reused across clients")
        if mutation is not None:
            unbound = sdk_server("unbound", tools=[business])
            if mutation == "outer":
                options.mcp_servers["local"] = unbound
            elif mutation == "instance":
                options.mcp_servers["local"]["instance"] = unbound["instance"]
            elif mutation == "clear":
                options.mcp_servers.clear()
            elif mutation == "cross-client":
                foreign = create_sdk_mcp_server("foreign", tools=[business])
                foreign_directory = directory / "foreign"
                foreign_directory.mkdir()
                other_client = create_client(replace(
                    options, cwd=str(foreign_directory), mcp_servers={"local": dict(foreign)},
                    env={**options.env, "AW_TEST_MODE": "allow", "AW_TEST_TENANT": "foreign",
                         "CLAUDE_CONFIG_DIR": str(foreign_directory / "profile")}))
                await other_client.connect()
                other_pid = other_client._transport.delegate._process.pid
                (directory / "other-sdk-process.json").write_text(json.dumps({"pid": other_pid}))
                assert other_client._aw_bridge is foreign.bridge
                assert client._aw_bridge is managed.bridge
                options.mcp_servers["local"] = dict(foreign)
            elif mutation == "tool-name":
                business.name = "allowed_alias"
            elif mutation == "server-key":
                try:
                    managed.key = "allowed_alias"
                except AttributeError:
                    pass
                else:
                    raise AssertionError("Managed server policy key remained writable")
            elif mutation == "tool-handler":
                async def other_handler(arguments):
                    (directory / "other-handler-effect").touch()
                    return {"content": [{"type": "text", "text": "other handler"}]}
                business.handler = other_handler
            else:
                raise AssertionError(f"Unknown MCP mutation {mutation}")
            assert client.options.mcp_servers["local"]["instance"] is managed["instance"]
        failed = False
        async with client:
            (directory / "sdk-process.json").write_text(
                json.dumps({"pid": client._transport.delegate._process.pid})
            )
            await client.query("Call the local tool")
            try:
                async for _ in client.receive_response():
                    pass
            except Exception:
                failed = True
        if mode == "deny":
            assert not calls, "Caller mutation bypassed AW denial"
        assert not (directory / "other-handler-effect").exists()
        if background:
            await asyncio.wait_for(asyncio.gather(*background), 5)
        requests = [request for request in fixture.REQUESTS if "messages" in request]
        events = [
            json.loads(line)["event"]
            for line in (directory / "events.jsonl").read_text().splitlines()
        ]
        before = [event for event in events if event["name"] == "tool.before"]
        after = [event for event in events if event["name"] == "tool.after"]
        count = 2 if mode == "parallel" else 1
        assert len(before) == count, (mode, len(before))
        assert len(after) == (0 if mode in {"deny", "before-error"} else count), (mode, len(after))
        assert all(event["native"]["aw_source"] == "claude-sdk-mcp/v1" for event in events)
        assert len({event["tool"]["call_id"] for event in before}) == count
        assert all(event["tool"]["name"] == "mcp__local__touch" for event in events)
        assert len(calls) == (0 if mode in {"deny", "before-error"} else count), calls
        assert failed == (mode in {"before-error", "after-error", "compress-error", "result-control"}), (mode, failed)
        if mode in {"before-error", "after-error", "compress-error", "result-control"}:
            assert len(requests) == 1, (mode, len(requests))
        else:
            assert len(requests) == 2, (mode, len(requests))
            results = [
                block
                for message in requests[-1]["messages"]
                for block in message.get("content", [])
                if isinstance(block, dict) and block.get("type") == "tool_result"
            ]
            assert len(results) == count, (mode, results)
            texts = [
                (
                    result["content"]
                    if isinstance(result["content"], str)
                    else "".join(block.get("text", "") for block in result["content"])
                )
                for result in results
            ]
            if mode in {"allow", "parallel"}:
                assert all(
                    ORIGINAL not in text and len(text) < len(ORIGINAL) for text in texts
                ), mode
            if mode in {"error", "multi", "structured"}:
                assert any(ORIGINAL in text for text in texts), mode
            if mode == "error":
                assert results[0]["is_error"] is True
            if mode == "result-mutation":
                assert texts == ["CHECKED-ORIGINAL"], texts
                assert results[0].get("is_error", False) is False
                assert "CHECKED-ORIGINAL" in json.dumps(after[0]["tool"]["result"])
                assert "UNVERIFIED-NEW-RESULT" not in json.dumps(after[0]["tool"]["result"])
                assert (directory / "result-mutated").exists()
        if mode == "parallel":
            assert finished[0].endswith("second"), finished
            assert {event["tool"]["call_id"] for event in before} == {
                event["tool"]["call_id"] for event in after
            }
        assert native.count("PreToolUse") == count, native
        REPORT.append({"mode": f"mutation-{mutation}" if mutation else mode, "passed": True})
    finally:
        for pending in background:
            if not pending.done():
                pending.cancel()
        if background:
            await asyncio.gather(*background, return_exceptions=True)
        try:
            if other_client is not None:
                await other_client.disconnect()
                assert other_pid is None or not Path(f"/proc/{other_pid}").exists()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=3)
            assert not thread.is_alive()


async def main():
    modes = ("compress-error",) if os.environ["AW_TEST_SCENARIO"] == "mcp-failure" else (
        "allow", "deny", "before-error", "after-error", "error", "multi", "structured", "exception", "parallel",
        "result-mutation", "result-control")
    for index, mode in enumerate(modes):
        await task(index, mode)
    if os.environ["AW_TEST_SCENARIO"] == "mcp":
        mutations = ("outer", "instance", "clear", "cross-client", "tool-name", "tool-handler", "server-key")
        for index, mutation in enumerate(mutations, len(modes)):
            await task(index, "allow" if mutation == "tool-handler" else "deny", mutation)
    (ROOT / "result.json").write_text(json.dumps({"tasks": len(REPORT), "cases": REPORT, "passed": True}))


asyncio.run(main())
