"""Unit tests for the framework-free Python language-level Source runtime."""

from __future__ import annotations

import asyncio
import unittest
from unittest.mock import AsyncMock, patch

from aw_sdk import AwError, Instance
from aw_sdk.sources import (
    LanguageSource,
    SourceDenied,
    confirm_source_routes,
    source_capabilities,
)


from source_fixture import fake_source


class SourceCapabilitiesTests(unittest.TestCase):
    def test_capabilities_and_route_confirmation(self) -> None:
        caps = source_capabilities(["tool.before", "tool.after", "tool.before"])
        self.assertEqual(caps["adapter"], "sdk")
        self.assertEqual(caps["version"], "aw-hook-source/v1")
        self.assertEqual(list(caps["events"]), ["tool.before", "tool.after"])
        self.assertTrue(
            confirm_source_routes(caps, {"hooks": [{"event": "tool.before"}, {"event": "tool.after"}]})
        )
        self.assertFalse(
            confirm_source_routes(caps, {"hooks": [{"event": "input.submit"}]})
        )
        self.assertFalse(
            confirm_source_routes(caps, {"hooks": [{"event": "tool.before", "replace_input": True}]})
        )
        self.assertFalse(
            confirm_source_routes(caps, {"hooks": [{"event": "tool.after", "replace_result": True}]})
        )
        self.assertFalse(
            confirm_source_routes(caps, {"hooks": [{"event": "tool.before", "skill_check": True}]})
        )
        with self.assertRaisesRegex(AwError, "semantic_event_unavailable"):
            source_capabilities([])
        with self.assertRaisesRegex(AwError, "semantic_event_unavailable"):
            source_capabilities(["agent.end"])


class SourceRuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def test_instrument_observes_blocks_and_restores_cleanly(self) -> None:
        async def handler(event: str, payload: dict) -> dict[str, str]:
            if event == "input.submit" and payload["text"] == "block-input":
                return {"decision": "block", "coverage": "checked"}
            if event == "tool.before" and payload["input"].get("cmd") == "rm":
                return {"decision": "block", "coverage": "checked"}
            return {"decision": "continue", "coverage": "checked"}

        source, tasks = fake_source(handler)
        runs: list[str] = []

        class Agent:
            async def ask(self, prompt: str) -> str:
                runs.append(f"ask:{prompt}")
                return await self.run_tool({"cmd": prompt})

            def run_tool(self, payload: dict) -> str:
                runs.append(f"tool:{payload['cmd']}")
                # Mutate caller dict to verify tool.after retains the original snapshot.
                payload["cmd"] = "mutated"
                return "ok"

        agent = Agent()
        async with source.session(session_id="sess-1") as session:
            handle = session.instrument(
                agent,
                input_methods=["ask"],
                tool_methods={"run_tool": "shell"},
            )
            with self.assertRaises(SourceDenied) as ctx:
                await agent.ask("block-input")
            self.assertEqual(ctx.exception.event, "input.submit")
            self.assertEqual(runs, [])

            with self.assertRaises(SourceDenied) as ctx:
                await agent.ask("rm")
            self.assertEqual(ctx.exception.event, "tool.before")
            self.assertEqual(ctx.exception.tool_name, "shell")
            self.assertEqual(runs, ["ask:rm"])

            self.assertEqual(await agent.ask("ls"), "ok")
            self.assertEqual(runs, ["ask:rm", "ask:ls", "tool:ls"])
            self.assertFalse(handle.restored)

        self.assertTrue(handle.restored)
        self.assertEqual(len(tasks), 1)
        self.assertTrue(tasks[0].closed)
        self.assertEqual(
            [event for event, _ in tasks[0].events],
            ["input.submit", "input.submit", "tool.before", "input.submit", "tool.before", "tool.after"],
        )
        # Verify tool.after retained the pre-mutation input snapshot.
        self.assertEqual(tasks[0].events[-1][1]["input"], {"cmd": "ls"})
        self.assertEqual(tasks[0].events[-1][1]["result"], "ok")
        # Restored method is back to the original sync method on the class.
        self.assertEqual(agent.run_tool({"cmd": "post"}), "ok")

    async def test_tool_exception_and_async_generator_early_close_boundaries(self) -> None:
        async def handler(event: str, payload: dict) -> dict[str, str]:
            if event == "tool.before" and payload["input"].get("deny"):
                return {"decision": "block", "coverage": "checked"}
            return {"decision": "continue", "coverage": "checked"}

        source, tasks = fake_source(handler)
        tool_calls = 0
        closed_streams = 0

        async def failing_tool(payload: dict) -> str:
            nonlocal tool_calls
            tool_calls += 1
            raise ValueError("boom")

        async def streaming_tool(payload: dict):
            nonlocal tool_calls, closed_streams
            tool_calls += 1
            try:
                yield "chunk-1"
                yield "chunk-2"
            finally:
                closed_streams += 1

        async with source.session(session_id="sess-2") as session:
            wrapped_fail = session.wrap_tool(failing_tool, name="fail")
            with self.assertRaisesRegex(ValueError, "boom"):
                await wrapped_fail({"k": "v"})
            self.assertEqual(tool_calls, 1)

            wrapped_stream = session.wrap_tool(streaming_tool, name="stream")
            # Denied stream never enters the underlying generator.
            with self.assertRaises(SourceDenied):
                async for _ in wrapped_stream({"deny": True}):
                    pass
            self.assertEqual(tool_calls, 1)
            self.assertEqual(closed_streams, 0)

            # Early break closes the inner generator and skips tool.after.
            seen: list[str] = []
            gen = wrapped_stream({"deny": False})
            async for item in gen:
                seen.append(item)
                break
            await gen.aclose()
            self.assertEqual(seen, ["chunk-1"])
            self.assertEqual(closed_streams, 1)

            # Complete consumption emits tool.after once with collected items.
            full = [item async for item in wrapped_stream({"full": True})]
            self.assertEqual(full, ["chunk-1", "chunk-2"])
            self.assertEqual(closed_streams, 2)

        self.assertEqual(
            [(event, payload["name"]) for event, payload in tasks[0].events],
            [
                ("tool.before", "fail"),
                ("tool.before", "stream"),
                ("tool.before", "stream"),
                ("tool.before", "stream"),
                ("tool.after", "stream"),
            ],
        )
        self.assertEqual(tasks[0].events[-1][1]["result"], ["chunk-1", "chunk-2"])

    async def test_instrument_rejects_conflicts_double_patch_and_rolls_back(self) -> None:
        source, _ = fake_source(AsyncMock(return_value={"decision": "continue", "coverage": "checked"}))

        class Sample:
            async def a(self, text: str) -> str:
                return text

            async def b(self, payload: dict) -> dict:
                return payload

        obj = Sample()
        async with source.session(session_id="sess-3") as session:
            with self.assertRaisesRegex(AwError, "source_method_conflict"):
                session.instrument(obj, input_methods=["a"], tool_methods=["a"])
            with self.assertRaisesRegex(AwError, "source_attribute_missing"):
                session.instrument(obj, input_methods=["a"], tool_methods=["missing"])
            # Rollback must have restored `a` after `missing` failed.
            self.assertNotIn("a", obj.__dict__)
            session.instrument(obj, input_methods=["a"])
            with self.assertRaisesRegex(AwError, "source_already_instrumented"):
                session.instrument(obj, input_methods=["a"])

    async def test_validation_and_closed_session_guards(self) -> None:
        source, _ = fake_source(
            AsyncMock(return_value={"decision": "continue", "coverage": "checked"}),
            events=["tool.before"],
        )
        session = source.session(session_id="sess-4")
        with self.assertRaisesRegex(AwError, "semantic_event_unavailable"):
            await session.submit("hello")
        with self.assertRaisesRegex(AwError, "source_invalid_payload"):
            await session.before_tool("t", {"bad": float("nan")})
        cyclic: dict = {}
        cyclic["self"] = cyclic
        with self.assertRaisesRegex(AwError, "source_invalid_payload"):
            await session.before_tool("t", cyclic)
        await session.close()
        with self.assertRaisesRegex(AwError, "source_session_closed"):
            await session.prepare()

    async def test_native_fallback_allows_native_tool_when_aw_absent(self) -> None:
        source = LanguageSource.create(
            endpoint="/tmp/definitely-missing-aw-source.sock",
            target="app",
            availability="native_fallback",
            events=["tool.before", "tool.after"],
        )
        try:
            self.assertFalse(await source.connect())
            async with source.session(session_id="fallback-1") as session:
                calls = 0

                async def tool(args: dict) -> str:
                    nonlocal calls
                    calls += 1
                    return args["v"]

                wrapped = session.wrap_tool(tool, name="echo")
                self.assertEqual(await wrapped({"v": "native"}), "native")
                self.assertTrue(session.fallback)
                self.assertEqual(calls, 1)
        finally:
            await source.close()

    async def test_narrowed_session_cannot_skip_admitted_tool_before_route(self) -> None:
        source, tasks = fake_source(
            AsyncMock(return_value={"decision": "continue", "coverage": "checked"})
        )
        original_open = source.instance.task

        async def open_with_routes(**kwargs: object) -> FakeTask:
            task = await original_open(**kwargs)
            task.routes = {"events": {}, "hooks": [{"event": "tool.before", "step": "policy"}]}
            return task

        source.instance.task = open_with_routes  # type: ignore[method-assign]
        ran = 0

        async def tool(args: dict) -> str:
            nonlocal ran
            ran += 1
            return "ok"

        async with source.session(session_id="narrow", events=["tool.after"]) as session:
            wrapped = session.wrap_tool(tool, name="sh")
            with self.assertRaisesRegex(AwError, "source_route_coverage_incomplete"):
                await wrapped({"cmd": "id"})
        self.assertEqual(ran, 0)
        self.assertEqual(tasks[0].events, [])

    async def test_concurrent_input_mutation_during_before_tool_aborts(self) -> None:
        shared = {"cmd": "safe"}
        checked = None
        ran = 0

        async def handler(event: str, payload: dict) -> dict[str, str]:
            if event == "tool.before":
                checked["cmd"] = "evil"
            return {"decision": "continue", "coverage": "checked"}

        source, _ = fake_source(handler)

        async def tool(args: dict) -> str:
            nonlocal ran
            ran += 1
            return args["cmd"]

        def extract_input(args: dict) -> dict:
            nonlocal checked
            checked = args
            return {"cmd": args["cmd"]}

        async with source.session(session_id="toctou") as session:
            wrapped = session.wrap_tool(tool, name="sh", extract_input=extract_input)
            with self.assertRaisesRegex(AwError, "source_input_mutated"):
                await wrapped(shared)
        self.assertEqual(ran, 0)
        self.assertEqual(shared["cmd"], "safe")

    async def test_session_close_closes_active_streams_and_isolates_stream_chunks(self) -> None:
        closed_streams = 0
        chunk_seen = []

        async def handler(event: str, payload: dict) -> dict[str, str]:
            return {"decision": "continue", "coverage": "checked"}

        source, tasks = fake_source(handler)

        async def gen_tool(args: dict):
            nonlocal closed_streams
            try:
                yield {"value": "first"}
                yield {"value": "second"}
            finally:
                closed_streams += 1

        session = source.session(session_id="stream-close")
        wrapped = session.wrap_tool(gen_tool, name="st")
        stream = wrapped({"k": 1})
        first = await stream.__anext__()
        chunk_seen.append(first)
        # Mutating yielded chunk must not corrupt snapshot; closing session closes inner stream.
        await session.close()
        self.assertEqual(closed_streams, 1)
        self.assertTrue(tasks[0].closed)
        with self.assertRaisesRegex(AwError, "source_session_closed"):
            await stream.__anext__()

        # Normal stream consumption snapshots each chunk before consumer mutation.
        async with source.session(session_id="stream-snapshot") as session2:
            wrapped2 = session2.wrap_tool(gen_tool, name="st", max_stream_items=2)
            out = []
            async for item in wrapped2({"k": 2}):
                out.append(item["value"])
                item["value"] = "mutated-by-consumer"
            self.assertEqual(out, ["first", "second"])
        self.assertEqual(
            tasks[1].events[-1][1]["result"],
            [{"value": "first"}, {"value": "second"}],
        )

        # Exceeding max_stream_items fails explicitly and closes the inner generator.
        async with source.session(session_id="stream-limit") as session3:
            wrapped3 = session3.wrap_tool(gen_tool, name="st", max_stream_items=1)
            with self.assertRaisesRegex(AwError, "source_stream_result_too_large") as raised:
                async for _ in wrapped3({"k": 3}):
                    pass
            self.assertTrue(raised.exception.tool_executed)

    async def test_input_snapshot_for_calls_and_streams(self) -> None:
        request = {"text": "draft"}

        async def handler(_, payload):
            self.assertEqual(payload["text"], "draft")
            request["text"] = "revised"
            return {"decision": "continue"}

        def text(arg):
            return arg["text"]

        async def chunks(arg):
            yield arg["text"]

        source, _ = fake_source(handler)
        async with source.session() as session:
            self.assertEqual(await session.wrap_input(text, extract_text=text)(request), "draft")
            request["text"] = "draft"
            stream = session.wrap_input(chunks, extract_text=text)(request)
            self.assertEqual([item async for item in stream], ["draft"])

    async def test_close_cancels_pending_read_before_releasing_task(self) -> None:
        for wrapper in ("wrap_input", "wrap_tool"):
            source, tasks = fake_source(AsyncMock(return_value={"decision": "continue"}))
            session = source.session()
            entered = asyncio.Event()
            exited = asyncio.Event()

            async def chunks(prompt):
                try:
                    entered.set()
                    await asyncio.Future()
                    yield prompt
                finally:
                    self.assertFalse(tasks[0].closed)
                    exited.set()

            stream = getattr(session, wrapper)(chunks)("prompt")
            async with asyncio.timeout(2):
                read = asyncio.create_task(anext(stream))
                await entered.wait()
                await session.close()
                with self.assertRaises(asyncio.CancelledError):
                    await read
            self.assertTrue(exited.is_set())
            self.assertTrue(tasks[0].closed)
            self.assertNotIn("tool.after", [e for e, _ in tasks[0].events])

    async def test_tool_stream_reads_checked_positional_and_keyword_arguments(self) -> None:
        class Request:
            def __init__(self, command: str) -> None:
                self.command = command

        def command_of(request):
            return request["command"] if isinstance(request, dict) else request.command

        class Tool:
            async def read(self, request, *, options):
                yield command_of(request), options["mode"]

        source, tasks = fake_source(AsyncMock(return_value={"decision": "continue"}))
        async with source.session() as session:
            for request in ({"command": "draft"}, Request("draft")):
                tool = Tool()
                options = {"mode": "draft"}
                extracts = 0

                def revise():
                    if isinstance(request, dict):
                        request["command"] = "revised"
                    else:
                        request.command = "revised"
                    options["mode"] = "revised"

                def extract_input(value, *, options):
                    nonlocal extracts
                    extracts += 1
                    if extracts == 2:
                        asyncio.get_running_loop().call_soon(revise)
                    return {"command": command_of(value), "mode": options["mode"]}

                snapshot = None
                if isinstance(request, Request):
                    for snapshot_arg in (None, lambda value: value):
                        wrapped = session.wrap_tool(
                            tool.read, extract_input=extract_input, snapshot_arg=snapshot_arg
                        )
                        with self.assertRaisesRegex(AwError, "source_input_snapshot_required"):
                            await anext(wrapped(request, options=options))
                    snapshot = lambda value: Request(value.command)
                handle = session.instrument(
                    tool,
                    tool_methods={"read": {"extract_input": extract_input}},
                    snapshot_arg=snapshot,
                )
                items = [item async for item in tool.read(request, options=options)]
                handle.restore()
                self.assertEqual(items, [("draft", "draft")])
                self.assertEqual(command_of(request), "revised")
                self.assertEqual(options["mode"], "revised")
        before = [payload["input"] for event, payload in tasks[0].events if event == "tool.before"]
        self.assertEqual(before, [{"command": "draft", "mode": "draft"}] * 2)

    async def test_close_reports_cleanup_error_from_cancelled_native_read(self) -> None:
        for wrapper in ("wrap_input", "wrap_tool"):
            source, tasks = fake_source(AsyncMock(return_value={"decision": "continue"}))
            session = source.session()
            entered = asyncio.Event()
            cleanup = RuntimeError("native cleanup failed")

            async def chunks(prompt):
                try:
                    entered.set()
                    await asyncio.Future()
                    yield prompt
                finally:
                    self.assertFalse(tasks[0].closed)
                    raise cleanup

            async with asyncio.timeout(2):
                read = asyncio.create_task(anext(getattr(session, wrapper)(chunks)("prompt")))
                await entered.wait()
                with self.assertRaises(RuntimeError) as closed:
                    await session.close()
                self.assertIs(closed.exception, cleanup)
                with self.assertRaises(RuntimeError) as failed_read:
                    await read
                self.assertIs(failed_read.exception, cleanup)
            self.assertTrue(tasks[0].closed)

    async def test_close_does_not_report_already_delivered_native_read_error(self) -> None:
        source, tasks = fake_source(AsyncMock(return_value={"decision": "continue"}))
        session = source.session()

        async def chunks():
            yield "first"
            raise RuntimeError("native read failed")

        stream = session.wrap_tool(chunks)()
        self.assertEqual(await anext(stream), "first")
        with self.assertRaisesRegex(RuntimeError, "native read failed"):
            await anext(stream)
        await session.close()
        self.assertTrue(tasks[0].closed)

    async def test_close_timeout_retains_task_while_native_cleanup_is_pending(self) -> None:
        source, tasks = fake_source(AsyncMock(return_value={"decision": "continue"}))
        session = source.session(timeout_ms=100)
        entered = asyncio.Event()
        release = asyncio.Event()

        async def chunks(prompt):
            try:
                entered.set()
                await asyncio.Future()
                yield prompt
            finally:
                await release.wait()
                self.assertFalse(tasks[0].closed)

        async with asyncio.timeout(2):
            read = asyncio.create_task(anext(session.wrap_input(chunks)("prompt")))
            await entered.wait()
            try:
                with self.assertRaisesRegex(AwError, "source_close_pending"):
                    await source.close()
                self.assertFalse(tasks[0].closed)
                self.assertFalse(source.instance.closed)
            finally:
                release.set()
            with self.assertRaises(asyncio.CancelledError):
                await read
            await source.close()
            self.assertTrue(tasks[0].closed)
            self.assertTrue(source.instance.closed)

    async def test_result_processing_uses_actual_routes_and_skips_native_fallback(self) -> None:
        cyclic = {}
        cyclic["self"] = cyclic

        async def chunks():
            yield cyclic
            yield cyclic

        for fallback in (False, True):
            source, tasks = fake_source(
                AsyncMock(return_value={"decision": "continue"}),
                events=["tool.before", "tool.after"], availability="native_fallback", fallback=fallback,
            )
            async with source.session() as session:
                await session.prepare()
                if not fallback:
                    tasks[0].routes["hooks"] = [{"event": "tool.before"}]
                opts = {"name": "tool", "extract_result": self.fail, "max_stream_items": 1}
                self.assertIs(await session.wrap_tool(lambda: cyclic, **opts)(), cyclic)
                items = [item async for item in session.wrap_tool(chunks, **opts)()]
                self.assertEqual(len(items), 2)
                self.assertTrue(all(item is cyclic for item in items))
                self.assertNotIn("tool.after", [e for e, _ in tasks[0].events])

    async def test_result_extractor_failure_marks_calls_and_streams(self) -> None:
        source, _ = fake_source(AsyncMock(return_value={"decision": "continue"}))
        calls = []

        def tool():
            calls.append("call")
            return 1

        async def chunks():
            calls.append("stream")
            yield 1

        def extract(_):
            raise ValueError("extract failed")

        async with source.session() as session:
            with self.assertRaisesRegex(ValueError, "extract failed") as raised:
                await session.wrap_tool(tool, extract_result=extract)()
            self.assertTrue(raised.exception.tool_executed)
            with self.assertRaisesRegex(ValueError, "extract failed") as raised:
                async for _ in session.wrap_tool(chunks, extract_result=extract)():
                    pass
            self.assertTrue(raised.exception.tool_executed)
        self.assertEqual(calls, ["call", "stream"])

    async def test_after_tool_failure_marks_tool_executed(self) -> None:
        async def handler(event: str, payload: dict) -> dict[str, str]:
            if event == "tool.after":
                raise AwError("sdk_step_failed")
            return {"decision": "continue", "coverage": "checked"}

        source, _ = fake_source(handler)
        runs = 0

        async def tool(args: dict) -> str:
            nonlocal runs
            runs += 1
            return "done"

        async with source.session(session_id="after-fail") as session:
            wrapped = session.wrap_tool(tool, name="sh")
            with self.assertRaises(AwError) as ctx:
                await wrapped({"k": "v"})
            self.assertTrue(getattr(ctx.exception, "tool_executed", False))
        self.assertEqual(runs, 1)

    async def test_slots_and_property_instrumentation_and_close_resilience(self) -> None:
        source, tasks = fake_source(
            AsyncMock(return_value={"decision": "continue", "coverage": "checked"})
        )

        class PureSlots:
            __slots__ = ("run",)

            def __init__(self) -> None:
                self.run = self._impl

            def _impl(self, args: dict) -> str:
                return args["v"]

        class SlotsWithDict:
            __slots__ = ("run", "__dict__")

            def __init__(self) -> None:
                self.run = lambda args: args["v"]

        class ReadOnlyProp:
            @property
            def run(self):
                return lambda args: args["v"]

        pure = PureSlots()
        orig_pure = pure.run
        mixed = SlotsWithDict()
        orig_mixed = mixed.run
        async with source.session(session_id="slots") as session:
            session.instrument(pure, tool_methods=["run"])
            session.instrument(mixed, tool_methods=["run"])
            self.assertEqual(await pure.run({"v": "a"}), "a")
            self.assertEqual(await mixed.run({"v": "b"}), "b")
            with self.assertRaisesRegex(AwError, "source_not_patchable"):
                session.instrument(ReadOnlyProp(), tool_methods=["run"])
        self.assertIs(pure.run, orig_pure)
        self.assertIs(mixed.run, orig_mixed)

        # Even if restore fails, session.close() still closes Admission/Task.
        session2 = source.session(session_id="restore-fail")
        await session2.prepare()
        handle = session2.instrument(pure, tool_methods=["run"])
        handle.restore = lambda: (_ for _ in ()).throw(RuntimeError("restore_boom"))  # type: ignore[method-assign]
        with self.assertRaisesRegex(RuntimeError, "restore_boom"):
            await session2.close()
        self.assertTrue(tasks[-1].closed)

    async def test_rejects_non_sdk_instance(self) -> None:
        instance = Instance(
            endpoint="/tmp/aw.sock",
            target="app",
            availability="required",
            capabilities={
                "adapter": "qwenpaw",
                "version": "public-middleware/v1",
                "entrypoint": "app",
                "events": {"tool.before": ["observe"]},
            },
            install=lambda _: True,
        )
        with self.assertRaisesRegex(AwError, "semantic_profile_required"):
            LanguageSource(instance)


if __name__ == "__main__":
    unittest.main()
