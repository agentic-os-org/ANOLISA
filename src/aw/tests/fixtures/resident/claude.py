"""Real resident SDK/CLI tasks, isolated local model endpoints and native Hooks."""

import asyncio
import gc
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import threading
from contextlib import aclosing
from http.server import ThreadingHTTPServer

from aw_claude import create_client
from claude_agent_sdk import ClaudeAgentOptions, HookMatcher, ResultMessage

ROOT = Path(os.environ["AW_TEST_ROOT"])
SOURCE = Path(os.environ["AW_TEST_CLAUDE_FIXTURE"])
RESULTS = []


async def lifecycle():
    loop = asyncio.get_running_loop()
    previous = loop.get_exception_handler()
    baseline_tasks = asyncio.all_tasks()
    unhandled = []
    resource_checks = []
    config = json.loads(Path(os.environ["AW_CLAUDE_CONFIG"]).read_text())

    def observe_exception(current_loop, context):
        error = context.get("exception")
        unhandled.append(
            {"message": context.get("message"), "exception": type(error).__name__}
        )
        if previous is None:
            current_loop.default_exception_handler(context)
        else:
            previous(current_loop, context)

    async def drain_finalizers():
        # Collection schedules async-generator closes; keep the observer installed
        # while those tasks and their completion callbacks run.
        for _ in range(6):
            gc.collect()
            await asyncio.sleep(0.05)

    async def assert_idle(index):
        await drain_finalizers()
        assert not unhandled, (index, "unretrieved cleanup exceptions", unhandled)
        remaining = [
            pending for pending in asyncio.all_tasks()
            if pending not in baseline_tasks and not pending.done()
        ]
        assert not remaining, (index, "SDK tasks survived client close", remaining)
        pids = [
            json.loads(path.read_text())["pid"]
            for path in ROOT.glob("*/sdk-process.json")
        ]
        assert all(not Path(f"/proc/{pid}").exists() for pid in pids), (index, pids)
        status = json.loads(
            subprocess.check_output(
                [config["executable"], "status", "--config", str(ROOT / "aw.json")],
                timeout=5,
            )
        )["result"]
        usage = status["resources"]["usage"]
        assert status["bindings"] == 1 and status["events"] == 0, status
        assert all(usage[name] == 0 for name in ("invocations", "correlations", "calls")), status
        resource_checks.append(
            {"task": index, "live_tasks": 0, "sdk_processes_absent": pids,
             "bindings": status["bindings"], "events": status["events"], "usage": usage}
        )

    loop.set_exception_handler(observe_exception)
    try:
        for index in range(18):
            await task(index, "allow")
        await asyncio.gather(*(task(18 + index, "allow") for index in range(3)))
        await task(21, "deny")
        for index, mode in ((22, "before-error"), (24, "after-error")):
            await task(index, mode)
            await assert_idle(index)
            # A fresh client in this same worker must remain usable after failure.
            await task(index + 1, "allow")
            await assert_idle(index + 1)
    finally:
        try:
            await drain_finalizers()
        finally:
            loop.set_exception_handler(previous)
    assert not unhandled, ("unretrieved cleanup exceptions", unhandled)
    assert len({item["instance"] for item in RESULTS}) == 26
    (ROOT / "result.json").write_text(
        json.dumps(
            {
                "tasks": 26,
                "cases": RESULTS,
                "fault_recovery_tasks": [23, 25],
                "unhandled_async_errors": unhandled,
                "resource_checks": resource_checks,
                "passed": True,
            }
        )
    )


async def task(index, mode, consumer="response"):
    directory = ROOT / str(index)
    directory.mkdir()
    spec = importlib.util.spec_from_file_location(f"model_{index}", SOURCE)
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    fixture.ROOT, fixture.MODE, fixture.BASH = directory, "bash-compress", True
    fixture.ORIGINAL = f"private-result-{index}"

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

    async def hook(payload, call_id, context):
        native.append(payload["hook_event_name"])
        return {}

    options = ClaudeAgentOptions(
        model="claude-sonnet-4-6",
        cwd=str(directory),
        tools=["Bash"],
        allowed_tools=["Bash"],
        setting_sources=[],
        max_turns=2,
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
    client = create_client(options)
    failed = False
    try:
        async with client:
            cli_pid = (
                client._transport.delegate._process.pid
            )  # Test-only process evidence; production uses public SDK methods.
            (directory / "sdk-process.json").write_text(
                json.dumps({"pid": cli_pid, "owner": os.getpid()})
            )
            turns = 1 if mode.endswith("error") else 2
            for turn in range(turns):
                await client.query("Run the fixture tool")
                try:
                    stream = client.receive_messages() if consumer == "messages" else client.receive_response()
                    async with aclosing(stream) as messages:
                        async for message in messages:
                            if isinstance(message, ResultMessage) and consumer == "messages":
                                break
                except Exception:
                    failed = True
                assert client._transport.delegate._process.pid == cli_pid
            if mode.endswith("error"):
                try:
                    await client.query("failed client must remain stopped")
                except RuntimeError:
                    pass
                else:
                    raise AssertionError("failed client accepted another query")
        await client.disconnect()
        try:
            await client.connect()
        except RuntimeError:
            pass
        else:
            raise AssertionError("closed task reconnected")
        requests = [value for value in fixture.REQUESTS if "messages" in value]
        (directory / "requests.json").write_text(json.dumps(requests))
        events = [
            json.loads(line) for line in (directory / "events.jsonl").read_text().splitlines()
        ]
        assert all(item["tenant"] == str(index) for item in events)
        assert len({item["event"]["agent"]["instance_id"] for item in events}) == 1
        assert (directory / "effect").exists() == (mode in {"allow", "after-error"}), mode
        assert failed == mode.endswith("error"), (mode, failed)
        if mode.endswith("error"):
            assert len(requests) == 1, (mode, "model continued after Hook failure", len(requests))
            assert fixture.ORIGINAL not in json.dumps(requests)
        else:
            assert len(requests) == 2 * turns, (mode, len(requests))
        assert native.count("PreToolUse") == turns, native
        assert native.count("PostToolUse") == turns * (mode in {"allow", "after-error"}), native
        RESULTS.append(
            {
                "mode": mode,
                "task": index,
                "passed": True,
                "instance": events[0]["event"]["agent"]["instance_id"],
            }
        )
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)
        assert not thread.is_alive()


async def main():
    (ROOT / "application-started").write_text("started")
    # Host readiness must precede the first task/client.
    config = json.loads(Path(os.environ["AW_CLAUDE_CONFIG"]).read_text())
    assert Path(config["ready_path"]).exists()
    scenario = os.environ.get("AW_TEST_SCENARIO", "lifecycle")
    if scenario in {"messages", "eof-stop-failure"}:
        from claude_messages import run, eof_stop_failure
        if scenario == "eof-stop-failure":
            await eof_stop_failure(ROOT, config)
        else:
            await run(ROOT, config, task)
        return
    if scenario == "denial":
        from claude_denial import run

        await run(ROOT, SOURCE)
        return
    if scenario == "store":
        from claude_store import run

        await run(ROOT, SOURCE)
        return
    if scenario == "boundaries":
        from claude_boundaries import run

        await run(ROOT, SOURCE)
        return
    if scenario in {"drain", "drain-timeout"}:
        from claude_drain import run

        await run(ROOT, SOURCE)
        return
    if scenario == "startup":
        from claude_startup import run

        await run(ROOT, config)
        return
    if scenario == "stop-failure":
        from claude_agent_sdk import ClaudeSDKClient

        async def broken_interrupt(self):
            raise RuntimeError("injected interrupt control failure")

        ClaudeSDKClient.interrupt = broken_interrupt
        await task(0, "before-error")
        raise AssertionError("unacknowledged interruption did not terminate the host")
    if scenario == "cancel":
        running = asyncio.create_task(task(0, "cancel"))
        async with asyncio.timeout(15):
            while not (ROOT / "0/events.jsonl").exists():
                await asyncio.sleep(0.02)
        running.cancel()
        try:
            await running
        except asyncio.CancelledError:
            pass
        else:
            raise AssertionError("task did not propagate cancellation")
        assert not (ROOT / "0/effect").exists()
        (ROOT / "result.json").write_text(json.dumps({"tasks": 1, "passed": True}))
        return
    await lifecycle()


asyncio.run(main())
