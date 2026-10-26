"""Real SDK cancellation acceptance; no model service or network is used."""

from __future__ import annotations

import argparse
import asyncio
import errno
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterator
from unittest.mock import patch


def hook(root: Path, label: str) -> None:
    payload = json.load(sys.stdin)
    mode = json.loads(payload["tool_call"]["input"])["mode"]
    with (root / "callbacks.jsonl").open("a") as output:
        output.write(json.dumps({"label": label, "payload": payload}) + "\n")
    if mode.startswith("cancel-") and label == mode.split("-")[1] + "-first":
        (root / mode).write_text(str(os.getpid()))
        time.sleep(30)
    if mode.startswith("approval/"):
        _, encoding, field, value, action = mode.split("/")
        if label == ("before-first" if action == "report" else "before-last"):
            sys.stdout.buffer.write(json.dumps({field: value}).encode(encoding))
            return
    if mode == "deep/" + label:
        print("[" * 10000 + "0" + "]" * 10000)
        return
    print("{}")


async def worker() -> None:
    from agentscope.agent import InjectionConfig
    from agentscope.message import TextBlock, ToolCallBlock, UserMsg
    from agentscope.permission import PermissionBehavior, PermissionDecision
    from agentscope.state import AgentState
    from agentscope.tool import ToolBase, ToolChunk, Toolkit
    from aw_agentscope import AwBridge
    from fixtures.agentscope.worker import OfflineModel

    root = Path(os.environ["AW_TEST_ROOT"])
    aw = os.environ["AW_TEST_BINARY"]
    socket = root / "state/aw.sock"
    executed = []
    entry_events: dict[str, int] = {}

    class LocalTool(ToolBase):
        name = "local_tool"
        description = "Record local execution"
        input_schema = {
            "type": "object",
            "properties": {"mode": {"type": "string"}},
            "required": ["mode"],
        }
        is_read_only = True
        is_concurrency_safe = False

        async def check_permissions(
            self, tool_input: dict[str, Any], context: Any
        ) -> PermissionDecision:
            return PermissionDecision(behavior=PermissionBehavior.ALLOW)

        async def call(self, mode: str) -> Any:
            if mode.startswith("rpc-before-"):
                entry_events[mode] = (await asyncio.to_thread(status))["events"]
            executed.append(mode)
            yield ToolChunk(content=[TextBlock(text="complete")], is_last=True)

    agent = AwBridge().create_agent(
        name="cancellation-agent",
        system_prompt="Use the fixture tool",
        model=OfflineModel([]),
        toolkit=Toolkit(tools=[LocalTool()]),
        state=AgentState(session_id="same-session"),
        injection_config=InjectionConfig(inject_runtime_state=False),
    )

    def status() -> dict[str, Any]:
        return json.loads(
            subprocess.check_output([aw, "status", "--socket", str(socket)], timeout=3)
        )["result"]

    def turn(mode: str) -> Any:
        agent.model.calls = [
            ToolCallBlock(
                id="reused-native-id", name="local_tool", input=json.dumps({"mode": mode})
            )
        ]
        agent.model.round = 0
        return agent.reply(UserMsg("user", "run"))

    cancelled = prespawn_cancelled = spawn_failures = deep_directives = 0
    for event in ["before", "after"]:
        for number in range(34):
            mode = f"cancel-{event}-{number}"
            task = asyncio.create_task(turn(mode))
            try:
                deadline = time.monotonic() + 5
                while not (root / mode).exists():
                    assert not task.done(), f"turn finished before {mode}: {task.result()}"
                    assert time.monotonic() < deadline, f"callback not entered: {mode}"
                    await asyncio.sleep(0.005)
                task.cancel()
                try:
                    await asyncio.wait_for(task, 5)
                except asyncio.CancelledError:
                    pass
                checked = await asyncio.to_thread(status)
                assert checked["events"] == 0, (mode, checked)
                pid = int((root / mode).read_text())
                assert not Path(f"/proc/{pid}").exists(), f"native hook child remains: {pid}"
                cancelled += 1
            finally:
                if not task.done():
                    task.cancel()
                    try:
                        await asyncio.wait_for(task, 5)
                    except asyncio.CancelledError:
                        pass
        await asyncio.wait_for(turn("normal-" + event), 5)
        # Completed automatic events acknowledge the step before registry removal.
        deadline = time.monotonic() + 2
        while (await asyncio.to_thread(status))["events"] != 0:
            assert time.monotonic() < deadline, "completed event was not released"
            await asyncio.sleep(0.005)
    assert len(executed) == 36, executed
    assert not any(mode.startswith("cancel-before-") for mode in executed)
    create = asyncio.create_subprocess_exec
    labels = [
        event + "-" + position
        for event in ["before", "after"]
        for position in ["first", "middle", "last"]
    ]
    for number in range(36):
        failed = labels[number % len(labels)]
        mode = f"spawn-{failed}-{number}"
        injected = []

        async def spawn(*args: Any, **kwargs: Any) -> Any:
            if (
                "--step" in args
                and args[args.index("--step") + 1] == failed
                and "--close-event" not in args
            ):
                assert not injected, "callback was retried"
                injected.append(failed)
                raise OSError(errno.EMFILE, "fixture spawn failure")
            return await create(*args, **kwargs)

        with patch.object(asyncio, "create_subprocess_exec", side_effect=spawn):
            await asyncio.wait_for(turn(mode), 5)
        assert injected == [failed], (mode, injected)
        checked = await asyncio.to_thread(status)
        assert checked["events"] == 0, (mode, checked)
        observed = [
            item["label"]
            for line in (root / "callbacks.jsonl").read_text().splitlines()
            if (item := json.loads(line))["payload"]["tool_call"]["input"]
            == json.dumps({"mode": mode})
        ]
        assert observed == [label for label in labels if label != failed], (mode, observed)
        assert (mode in executed) == (failed != "before-last"), (mode, executed)
        spawn_failures += 1
    await asyncio.wait_for(turn("normal-after-spawns"), 5)
    binding = json.loads(Path(os.environ["AW_NATIVE_CONFIG"]).read_text())["binding"]
    proxy_socket = root / "fault.sock"
    proxy_binding = root / "fault-binding.json"
    document = json.loads(Path(binding).read_text())
    document["socket"] = str(proxy_socket)
    proxy_binding.write_text(json.dumps(document))
    proxy_binding.chmod(0o600)
    handlers: set[asyncio.Task] = set()
    dropped: list[str] = []
    proxy_errors: list[str] = []

    async def frame(reader: asyncio.StreamReader) -> bytes:
        prefix = await reader.readexactly(4)
        size = int.from_bytes(prefix, "big")
        assert size <= 8 * 1024 * 1024, size
        return prefix + await reader.readexactly(size)

    async def relay(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        task = asyncio.current_task()
        handlers.add(task)
        upstream = None
        try:
            async with asyncio.timeout(3):
                request = await frame(reader)
                operation = json.loads(request[4:])["operation"]
                if operation["method"] == "invoke_step":
                    # Open succeeds, but this request never reaches the service.
                    # Never retain the callback environment in failure evidence.
                    dropped.append(operation["step_id"])
                    return
                response, upstream = await asyncio.open_unix_connection(str(socket))
                upstream.write(request)
                await upstream.drain()
                writer.write(await frame(response))
                await writer.drain()
        except Exception as error:
            proxy_errors.append(repr(error))
        finally:
            streams = [writer] + ([upstream] if upstream else [])
            for stream in streams:
                stream.close()
            try:
                for stream in streams:
                    await asyncio.wait_for(stream.wait_closed(), 1)
            finally:
                handlers.discard(task)

    server = await asyncio.start_unix_server(relay, path=str(proxy_socket))
    try:
        for number in range(36):
            failed = labels[number % len(labels)]
            mode = f"rpc-{failed}-{number}"

            async def failed_rpc(*args: Any, **kwargs: Any) -> Any:
                if (
                    "--step" in args
                    and args[args.index("--step") + 1] == failed
                    and "--close-event" not in args
                ):
                    args = list(args)
                    args[args.index("--binding") + 1] = str(proxy_binding)
                return await create(*args, **kwargs)

            with patch.object(asyncio, "create_subprocess_exec", side_effect=failed_rpc):
                await asyncio.wait_for(turn(mode), 5)
            assert len(dropped) == number + 1, (mode, dropped)
            checked = await asyncio.to_thread(status)
            assert checked["events"] == 0, (mode, checked)
            observed = [
                item["label"]
                for line in (root / "callbacks.jsonl").read_text().splitlines()
                if (item := json.loads(line))["payload"]["tool_call"]["input"]
                == json.dumps({"mode": mode})
            ]
            assert observed == [label for label in labels if label != failed], (mode, observed)
            assert (mode in executed) == (failed != "before-last"), (mode, executed)
            if mode in entry_events:
                assert entry_events[mode] == 0, (
                    mode,
                    "event retained at tool entry",
                    entry_events[mode],
                )
        assert not proxy_errors, proxy_errors
    finally:
        server.close()
        await server.wait_closed()
        active = list(handlers)
        for task in active:
            task.cancel()
        if active:
            await asyncio.wait_for(asyncio.gather(*active, return_exceptions=True), 3)
        proxy_socket.unlink(missing_ok=True)
        proxy_binding.unlink(missing_ok=True)
    for event in ["before", "after"]:
        for number in range(34):
            mode = f"prespawn-{event}-{number}"
            entered = asyncio.Event()

            async def pending_spawn(*args: Any, **kwargs: Any) -> Any:
                if (
                    "--step" in args
                    and args[args.index("--step") + 1] == event + "-middle"
                    and "--close-event" not in args
                ):
                    entered.set()
                    await asyncio.wait_for(asyncio.Event().wait(), 5)
                    raise AssertionError("cancelled spawn unexpectedly resumed")
                return await create(*args, **kwargs)

            with patch.object(asyncio, "create_subprocess_exec", side_effect=pending_spawn):
                task = asyncio.create_task(turn(mode))
                try:
                    await asyncio.wait_for(entered.wait(), 5)
                    checked = await asyncio.to_thread(status)
                    assert checked["events"] == 1, (mode, checked)
                    task.cancel()
                    try:
                        await asyncio.wait_for(task, 5)
                    except asyncio.CancelledError:
                        pass
                    checked = await asyncio.to_thread(status)
                    assert checked["events"] == 0, (mode, checked)
                    assert (mode in executed) == (event == "after"), (mode, executed)
                    cancelled += 1
                    prespawn_cancelled += 1
                finally:
                    if not task.done():
                        task.cancel()
                        try:
                            await asyncio.wait_for(task, 5)
                        except asyncio.CancelledError:
                            pass
        await asyncio.wait_for(turn("normal-after-prespawn-" + event), 5)
    finalizer_closes = []
    for number in range(34):
        mode = f"finalizer-{number}"
        entered, release = asyncio.Event(), asyncio.Event()
        injected = []
        close_child = None

        async def finalizing_spawn(*args: Any, **kwargs: Any) -> Any:
            nonlocal close_child
            if "--close-event" not in args and args[args.index("--step") + 1] == "after-middle":
                injected.append("after-middle")
                raise OSError(errno.EMFILE, "fixture finalization gap")
            child = await create(*args, **kwargs)
            if "--close-event" in args and args[args.index("--event") + 1] == "tool.after":
                close_child = child
                communicate = child.communicate

                async def pending_close(data: bytes) -> Any:
                    entered.set()
                    await asyncio.wait_for(release.wait(), 3)
                    return await communicate(data)

                child.communicate = pending_close
            return child

        with patch.object(asyncio, "create_subprocess_exec", side_effect=finalizing_spawn):
            task = asyncio.create_task(turn(mode))
            try:
                await asyncio.wait_for(entered.wait(), 5)
                assert (await asyncio.to_thread(status))["events"] == 1, mode
                for _ in range(2):
                    task.cancel("finalization interrupted")
                    await asyncio.sleep(0)
                assert not task.done(), (mode, "cleanup abandoned on cancellation")
                release.set()
                try:
                    await asyncio.wait_for(task, 5)
                except asyncio.CancelledError:
                    pass
                assert injected == ["after-middle"], (mode, injected)
                assert (await asyncio.to_thread(status))["events"] == 0, mode
                assert close_child is not None and close_child.returncode == 0, mode
                assert not Path(f"/proc/{close_child.pid}").exists(), mode
                finalizer_closes.append(close_child.pid)
            finally:
                release.set()
                if not task.done():
                    task.cancel()
                await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), 5)
    await asyncio.wait_for(turn("normal-after-finalizers"), 5)
    approvals = 0
    for encoding in [
        "utf-8",
        "utf-8-sig",
        "utf-16",
        "utf-16-le",
        "utf-16-be",
        "utf-32",
        "utf-32-le",
        "utf-32-be",
    ]:
        for field, value in [("action", "ask"), ("action", "approve"), ("decision", "ask")]:
            for action in ["report", "block"]:
                mode = f"approval/{encoding}/{field}/{value}/{action}"
                await asyncio.wait_for(turn(mode), 5)
                assert (mode in executed) == (action == "report"), mode
                results = [
                    block
                    for msg in agent.state.model_dump(mode="json")["context"]
                    for block in msg["content"]
                    if block["type"] == "tool_result"
                ]
                assert results[-1]["state"] == ("success" if action == "report" else "denied"), mode
                assert status()["events"] == 0, mode
                approvals += 1
    for failed in ["before-first", "before-last", "after-first"]:
        mode = "deep/" + failed
        await asyncio.wait_for(turn(mode), 5)
        blocked = failed == "before-last"
        assert (mode in executed) != blocked, mode
        results = [
            block
            for msg in agent.state.model_dump(mode="json")["context"]
            for block in msg["content"]
            if block["type"] == "tool_result"
        ]
        assert results[-1]["state"] == ("denied" if blocked else "success"), mode
        if not blocked:
            assert results[-1]["output"][0]["text"] == "complete", mode
        observed = [
            item["label"]
            for line in (root / "callbacks.jsonl").read_text().splitlines()
            if json.loads((item := json.loads(line))["payload"]["tool_call"]["input"])["mode"]
            == mode
        ]
        assert observed == labels, (mode, observed)
        assert status()["events"] == 0, mode
        deep_directives += 1
    (root / "worker-result.json").write_text(
        json.dumps(
            {
                "cancelled": cancelled,
                "prespawn_cancelled": prespawn_cancelled,
                "finalizer_cancelled": len(finalizer_closes),
                "finalizer_close_pids": finalizer_closes,
                "spawn_failures": spawn_failures,
                "rpc_failures": len(dropped),
                "approval_failures": approvals,
                "deep_directives": deep_directives,
                "executed": executed,
                "status": status(),
            }
        )
    )


@contextmanager
def owned(
    command: list[str], root: Path, output: Path, name: str, env: dict[str, str]
) -> Iterator[subprocess.Popen]:
    record = {
        "command": command,
        "cwd": str(root),
        "ports": [],
        "lifetime_seconds": 740,
        "logs": [str(output / (name + ".stdout")), str(output / (name + ".stderr"))],
    }
    path = output / (name + "-process.json")
    path.write_text(json.dumps(record))
    with Path(record["logs"][0]).open("wb") as stdout, Path(record["logs"][1]).open("wb") as stderr:
        child = subprocess.Popen(
            command, cwd=root, env=env, stdout=stdout, stderr=stderr, start_new_session=True
        )
        record.update(pid=child.pid, stop=f"kill -TERM -- -{child.pid}")
        path.write_text(json.dumps(record))
        try:
            yield child
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=3)
            record.update(exited=True, returncode=child.returncode)
            path.write_text(json.dumps(record))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    aw = str(args.aw.resolve())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    script = str(Path(__file__).resolve())
    with tempfile.TemporaryDirectory(prefix="s-", dir=output) as temporary:
        root = Path(temporary)
        state = root / "state"
        assert len(os.fsencode(state / "aw.sock")) <= 107
        config = {
            "apiVersion": "aw/v1alpha1",
            "kind": "AWConfiguration",
            "metadata": {"name": "sdk-cancellation"},
            "spec": {
                "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 55000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {
                    "sdk": {"adapter": "agentscope", "argv": [sys.executable, script, "--worker"]}
                },
                "providers": {},
                "events": {},
            },
        }
        for event in ["before", "after"]:
            steps = []
            # After wrappers unwind inside out, so register the last one first.
            for position in (
                ["first", "middle", "last"] if event == "before" else ["last", "middle", "first"]
            ):
                label = event + "-" + position
                config["spec"]["providers"][label] = {
                    "protocol": "native-hook/v1alpha1",
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [sys.executable, script, "--hook", str(root), label],
                    },
                    "timeout_ms": 35000,
                    "max_output_bytes": 32768,
                    "config": {},
                }
                steps.append(
                    {
                        "id": label,
                        "provider": label,
                        "native": {},
                        "on_error": "block" if label == "before-last" else "report",
                    }
                )
            config["spec"]["events"]["tool." + event] = {
                "enabled": True,
                "required": True,
                "steps": steps,
            }
        path = root / "aw.json"
        path.write_text(json.dumps(config))
        env = {**os.environ, "AW_TEST_ROOT": str(root), "AW_TEST_BINARY": aw}
        with owned(
            [aw, "serve", "--config", str(path), "--state-dir", str(state)],
            root,
            output,
            "serve",
            env,
        ) as service:
            try:
                deadline = time.monotonic() + 5
                while not (state / "aw.sock").exists():
                    assert (
                        service.poll() is None and time.monotonic() < deadline
                    ), "service startup failed"
                    time.sleep(0.01)
                with owned(
                    [aw, "run", "--config", str(path), "--agent", "sdk"],
                    root,
                    output,
                    "worker",
                    env,
                ) as child:
                    # This sequential stress suite runs nearly 300 cancellation/error cases.
                    # Keep the per-event budgets fixed while allowing hosted-runner overhead.
                    child.wait(timeout=720)
                    assert child.returncode == 0, (output / "worker.stderr").read_text()
                status = json.loads(
                    subprocess.check_output(
                        [aw, "status", "--socket", str(state / "aw.sock")], timeout=3
                    )
                )["result"]
                assert status["bindings"] == 0 and status["events"] == 0, status
                errors = (output / "worker.stderr").read_text()
                assert "event_limit" not in errors and "event_cleanup_failed" not in errors, errors
                assert errors.count("unsupported_approval") == 48, errors
                assert errors.count("native_output_too_deep") == 3, errors
                callbacks = [
                    json.loads(line) for line in (root / "callbacks.jsonl").read_text().splitlines()
                ]
                for event in ["before", "after"]:
                    normal = [
                        item["label"]
                        for item in callbacks
                        if json.loads(item["payload"]["tool_call"]["input"])["mode"]
                        == "normal-" + event
                    ]
                    assert normal == [
                        "before-first",
                        "before-middle",
                        "before-last",
                        "after-first",
                        "after-middle",
                        "after-last",
                    ], normal
                (output / "callbacks.json").write_text(json.dumps(callbacks, indent=2))
                (output / "result.json").write_text((root / "worker-result.json").read_text())
            finally:
                (output / "journal.json").write_text(
                    json.dumps(
                        {str(p.relative_to(state)): p.read_text() for p in state.rglob("*.jsonl")}
                    )
                )
                if (root / "callbacks.jsonl").exists():
                    (output / "callbacks.jsonl").write_text((root / "callbacks.jsonl").read_text())
                subprocess.run(
                    [aw, "stop", "--socket", str(state / "aw.sock")], timeout=5, capture_output=True
                )
                service.wait(timeout=5)
    print(
        "AgentScope 2.0.6: 136 cancellations, 36 spawn failures, 36 RPC failures, "
        "34 finalizer cancellations, 48 approval encodings and 3 deep directives passed"
    )


if __name__ == "__main__":
    if sys.argv[1:2] == ["--hook"]:
        hook(Path(sys.argv[2]), sys.argv[3])
    elif sys.argv[1:2] == ["--worker"]:
        asyncio.run(worker())
    else:
        main()
