"""Out-of-tree Python agent proving observation and blocking with only core anolisa-aw-sdk."""

import asyncio
import json
import subprocess
import sys
from unittest import TestCase

from aw_sdk import AwError
from aw_sdk.sources import LanguageSource, SourceDenied, SourceSession


class BrandNewPythonAgent:
    """Arbitrary Python agent with zero dedicated Rust Adapter."""

    def __init__(self) -> None:
        self.prompts: list[str] = []
        self.tool_runs: list[str] = []
        self.stream_closed = 0

    async def ask(self, prompt: str, *, command: str) -> str:
        self.prompts.append(prompt)
        return await self.shell({"command": command})

    def shell(self, args: dict) -> str:
        cmd = args["command"]
        self.tool_runs.append(cmd)
        args["command"] = "mutated-in-tool"
        if cmd == "boom":
            raise RuntimeError("tool_exploded")
        return f"ran:{cmd}"

    async def stream(self, args: dict):
        try:
            yield "part-1"
            yield "part-2"
        finally:
            self.stream_closed += 1


async def check_tool_snapshots(session: SourceSession) -> None:
    class Request:
        def __init__(self, command: str) -> None:
            self.command = command

    def command_of(request):
        return request["command"] if isinstance(request, dict) else request.command

    def extract_input(*, request):
        return {"command": command_of(request)}

    for request in ({"command": "draft"}, Request("draft")):
        entered, release = asyncio.Event(), asyncio.Event()

        async def read_tool(*, request):
            entered.set()
            await release.wait()
            yield command_of(request)

        snapshot = None
        if isinstance(request, Request):
            try:
                wrapped = session.wrap_tool(read_tool, extract_input=extract_input)
                await anext(wrapped(request=request))
            except AwError as error:
                assert error.code == "source_input_snapshot_required"
            else:
                raise AssertionError("shared request entered the tool stream")
            snapshot = lambda value: Request(value.command)
        wrapped = session.wrap_tool(
            read_tool, name="readTool", extract_input=extract_input, snapshot_arg=snapshot
        )
        stream = wrapped(request=request)
        first = asyncio.create_task(anext(stream))
        await entered.wait()
        if isinstance(request, dict):
            request["command"] = "revised"
        else:
            request.command = "revised"
        release.set()
        assert await first == "draft"
        assert [item async for item in stream] == []


async def check_pending_cleanup(binary: str, endpoint: str) -> None:
    source = LanguageSource.create(endpoint=endpoint, target="app")
    assert await source.connect()
    session = source.session()
    entered = asyncio.Event()
    cleanup = RuntimeError("native cleanup failed")
    cleaning, release = asyncio.Event(), asyncio.Event()

    async def pending_tool():
        try:
            entered.set()
            await asyncio.Future()
            yield "late"
        finally:
            cleaning.set()
            await release.wait()
            raise cleanup

    wrapped = session.wrap_tool(pending_tool, name="pendingTool", extract_input=dict)
    read = asyncio.create_task(anext(wrapped()))
    await entered.wait()
    closing = asyncio.create_task(source.close(timeout_ms=100))
    try:
        await cleaning.wait()
        with TestCase().assertRaisesRegex(AwError, "^source_closed$"):
            source.session()
        with TestCase().assertRaisesRegex(AwError, "^source_close_pending$"):
            await closing
        assert service_bindings(binary, endpoint) == 2
    finally:
        release.set()
    with TestCase().assertRaises(RuntimeError) as failed_read:
        await read
    assert failed_read.exception is cleanup
    for close, bindings in ((session.close, 1), (source.close, 0)):
        with TestCase().assertRaises(RuntimeError) as failed_close:
            await close()
        assert failed_close.exception is cleanup
        assert service_bindings(binary, endpoint) == bindings


async def check_mixed_cleanup(binary: str, endpoint: str) -> None:
    source = LanguageSource.create(endpoint=endpoint, target="app")
    assert await source.connect()
    failed, release = asyncio.Event(), asyncio.Event()
    cleanup = RuntimeError("first cleanup failed")

    async def chunks():
        try:
            yield "ready"
        finally:
            if not failed.is_set():
                failed.set()
                raise cleanup
            await release.wait()

    streams = [source.session().wrap_tool(chunks, extract_input=dict)() for _ in range(2)]
    for stream in streams:
        assert await anext(stream) == "ready"
    assert service_bindings(binary, endpoint) == 3
    try:
        with TestCase().assertRaisesRegex(AwError, "^source_close_pending$"):
            await source.close(timeout_ms=100)
        assert service_bindings(binary, endpoint) == 2
    finally:
        release.set()
    with TestCase().assertRaises(RuntimeError) as raised:
        await source.close()
    assert raised.exception is cleanup
    assert service_bindings(binary, endpoint) == 0


def service_bindings(binary: str, endpoint: str) -> int:
    status = subprocess.check_output(
        [binary, "service", "status", "--socket", endpoint], timeout=5
    )
    return json.loads(status)["result"]["bindings"]


async def check_call_drain(source: LanguageSource, binary: str, endpoint: str) -> None:
    for time_out in (False, True):
        session = source.session(timeout_ms=100 if time_out else 5000)
        entered, release = asyncio.Event(), asyncio.Event()

        async def paused_tool(tool_input):
            entered.set()
            await release.wait()
            return tool_input["command"]

        wrapped = session.wrap_tool(paused_tool, name="pausedTool")
        call = asyncio.create_task(wrapped({"command": "held"}))
        await entered.wait()
        closing = asyncio.create_task(session.close())
        await asyncio.sleep(0)
        try:
            assert service_bindings(binary, endpoint) == 2
            if time_out:
                try:
                    await closing
                except AwError as error:
                    assert error.code == "source_close_pending"
                else:
                    raise AssertionError("close released the Task with an active call")
                assert service_bindings(binary, endpoint) == 2
        finally:
            release.set()
        result, _ = await asyncio.gather(
            call, session.close(timeout_ms=5000) if time_out else closing
        )
        assert result == "held"
        assert service_bindings(binary, endpoint) == 1


async def check_instrumented_drain(source: LanguageSource, binary: str, endpoint: str) -> None:
    entered, release = asyncio.Event(), asyncio.Event()

    class DrainAgent:
        def __init__(self) -> None:
            self.tool_runs: list[str] = []

        async def ask(self, prompt: str, *, command: str) -> str:
            entered.set()
            await release.wait()
            return await self.shell({"command": command})

        async def shell(self, args: dict) -> str:
            self.tool_runs.append(args["command"])
            return f"ran:{args['command']}"

    target = DrainAgent()
    session = source.session()
    handle = session.instrument(target, input_methods=["ask"], tool_methods={"shell": "shell"})
    try:
        await target.shell({"command": "deny"})
    except SourceDenied as denied:
        assert denied.event == "tool.before" and denied.tool_name == "shell"
    else:
        raise AssertionError("control tool call was not blocked")
    call = asyncio.create_task(target.ask("drain-input", command="deny"))
    await entered.wait()
    try:
        try:
            await session.close(timeout_ms=100)
        except AwError as error:
            assert error.code == "source_close_pending"
        else:
            raise AssertionError("close completed while admitted input was paused")
        assert service_bindings(binary, endpoint) == 2
        assert not handle.restored
    finally:
        release.set()
    try:
        await call
    except AwError as error:
        assert error.code == "source_session_closed", error.code
    else:
        raise AssertionError("draining input executed an unchecked tool")
    assert target.tool_runs == []
    await session.close()
    assert handle.restored
    assert service_bindings(binary, endpoint) == 1


async def main() -> None:
    endpoint, missing_endpoint, binary = sys.argv[1:]
    await check_pending_cleanup(binary, endpoint)
    await check_mixed_cleanup(binary, endpoint)
    agent = BrandNewPythonAgent()
    async with LanguageSource.create(
        endpoint=endpoint, target="app", availability="required"
    ) as source:
        async with source.session(session_id="nth-python-agent") as session:
            handle = session.instrument(
                agent,
                input_methods=["ask"],
                tool_methods={"shell": "shell", "stream": "stream"},
            )
            class Prompt:
                def __init__(self, text: str) -> None:
                    self.text = text

            request = Prompt("draft")
            entered, release = asyncio.Event(), asyncio.Event()

            async def read(prompt):
                entered.set()
                await release.wait()
                yield prompt.text

            def text_of(prompt):
                return prompt.text

            try:
                await anext(session.wrap_input(read, extract_text=text_of)(request))
            except AwError as error:
                assert error.code == "source_input_snapshot_required"
            else:
                raise AssertionError("shared prompt entered the input stream")
            stream = session.wrap_input(
                read, extract_text=text_of, snapshot_arg=lambda p: Prompt(p.text)
            )(request)
            first = asyncio.create_task(anext(stream))
            await entered.wait()
            request.text = "revised"
            release.set()
            assert await first == "draft"
            await stream.aclose()
            await check_tool_snapshots(session)

            # 1. Blocked input never runs the agent or tools.
            try:
                await agent.ask("deny", command="echo")
            except SourceDenied as denied:
                assert denied.event == "input.submit"
            else:
                raise AssertionError("denied input executed")
            assert agent.prompts == [] and agent.tool_runs == []

            # 2. Allowed input with blocked tool never executes the tool.
            try:
                await agent.ask("allow-then-deny-tool", command="deny")
            except SourceDenied as denied:
                assert denied.event == "tool.before" and denied.tool_name == "shell"
            else:
                raise AssertionError("denied tool executed")
            assert agent.prompts == ["allow-then-deny-tool"]
            assert agent.tool_runs == []

            # 3. Allowed input + tool executes once and emits tool.after with original snapshot.
            assert await agent.ask("allow-all", command="echo") == "ran:echo"
            assert agent.tool_runs == ["echo"]

            # 4. Failing tool executes once, propagates error, skips tool.after.
            try:
                await agent.shell({"command": "boom"})
            except RuntimeError as error:
                assert str(error) == "tool_exploded"
            else:
                raise AssertionError("expected tool failure")
            assert agent.tool_runs == ["echo", "boom"]

            # 5. Early-closed async generator closes inner stream and skips tool.after.
            gen = agent.stream({"mode": "early"})
            seen = []
            async for item in gen:
                seen.append(item)
                break
            await gen.aclose()
            assert seen == ["part-1"] and agent.stream_closed == 1

            # 6. Fully-consumed async generator emits tool.after once.
            full = [item async for item in agent.stream({"mode": "full"})]
            assert full == ["part-1", "part-2"] and agent.stream_closed == 2
        assert handle.restored
        await check_call_drain(source, binary, endpoint)
        await check_instrumented_drain(source, binary, endpoint)

        # 6b. Narrowed session cannot bypass admitted required tool.before route.
        async with source.session(
            session_id="nth-python-narrow", events=["tool.after"]
        ) as narrow:
            wrapped_narrow = narrow.wrap_tool(agent.shell, name="shell")
            try:
                await wrapped_narrow({"command": "bypass"})
            except AwError as error:
                assert error.code == "source_route_coverage_incomplete", error.code
            else:
                raise AssertionError("narrowed session bypassed tool.before")
        assert agent.tool_runs == ["echo", "boom"]

    # 7. Restored method runs natively; native_fallback runs natively without AW.
    assert agent.shell({"command": "plain"}) == "ran:plain"
    async with LanguageSource.create(
        endpoint=missing_endpoint,
        target="app",
        availability="native_fallback",
        events=["tool.before", "tool.after"],
    ) as fallback_source:
        async with fallback_source.session(session_id="nth-python-fallback") as session:
            session.instrument(agent, tool_methods={"shell": "shell"})
            assert await agent.shell({"command": "offline"}) == "ran:offline"
            assert session.fallback


asyncio.run(main())
