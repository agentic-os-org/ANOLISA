"""Exercise Source session shutdown across native calls and result observation."""

import asyncio
import unittest

from aw_sdk import AwError

from source_fixture import fake_source


class SourceShutdownTests(unittest.IsolatedAsyncioTestCase):
    async def test_close_drains_native_calls_and_their_observations(self) -> None:
        for method in ("wrap_input", "wrap_tool"):
            for time_out in (False, True):
                with self.subTest(method=method, time_out=time_out):
                    await self.check_call_shutdown(method, time_out)

    async def check_call_shutdown(self, method: str, time_out: bool) -> None:
        entered, release = asyncio.Event(), asyncio.Event()
        observed, finish = asyncio.Event(), asyncio.Event()

        async def handler(event, _):
            self.assertFalse(tasks[0].closed)
            if event == "tool.after":
                observed.set()
                await finish.wait()
            return {"decision": "continue"}

        async def native(text):
            entered.set()
            await release.wait()
            return text

        source, tasks = fake_source(handler)
        session = source.session(timeout_ms=100 if time_out else 5000)
        try:
            async with asyncio.timeout(3):
                call = asyncio.create_task(getattr(session, method)(native)("done"))
                await entered.wait()
                if time_out:
                    with self.assertRaisesRegex(AwError, "source_close_pending"):
                        await source.close()
                    self.assertFalse(source.instance.closed)
                else:
                    closing = asyncio.create_task(session.close())
                    done, _ = await asyncio.wait({closing}, timeout=0.01)
                    self.assertFalse(done)
                self.assertFalse(tasks[0].closed)
                with self.assertRaisesRegex(AwError, "source_session_closed") as raised:
                    await session.after_tool("external", {}, None, call_id="external")
                self.assertTrue(raised.exception.tool_executed)
                release.set()
                if method == "wrap_tool":
                    await observed.wait()
                    self.assertFalse(tasks[0].closed)
                finish.set()
                if time_out:
                    closing = asyncio.create_task(session.close(timeout_ms=5000))
                result, _ = await asyncio.gather(call, closing)
                self.assertEqual(result, "done")
                self.assertTrue(tasks[0].closed)
        finally:
            release.set()
            finish.set()
            await source.close(timeout_ms=5000)
        self.assertTrue(source.instance.closed)
        with self.assertRaisesRegex(AwError, "^source_closed$"):
            source.session()
        self.assertEqual(
            [event for event, _ in tasks[0].events],
            ["tool.before", "tool.after"] if method == "wrap_tool" else ["input.submit"],
        )

    async def test_close_keeps_instrumented_methods_until_admitted_work_drains(self) -> None:
        for async_tool in (False, True):
            with self.subTest(async_tool=async_tool):
                entered, release = asyncio.Event(), asyncio.Event()
                tool_runs: list[str] = []

                class Agent:
                    async def ask(self, prompt: str, command: str) -> str:
                        entered.set()
                        await release.wait()
                        return await self.shell({"command": command})

                    if async_tool:
                        async def shell(self, args: dict) -> str:
                            tool_runs.append(args["command"])
                            return f"ran:{args['command']}"
                    else:
                        def shell(self, args: dict) -> str:
                            tool_runs.append(args["command"])
                            return f"ran:{args['command']}"

                async def handler(event, payload):
                    if event == "tool.before" and payload["input"].get("command") == "deny":
                        return {"decision": "block", "coverage": "checked"}
                    return {"decision": "continue", "coverage": "checked"}

                source, tasks = fake_source(handler)
                agent = Agent()
                session = source.session(timeout_ms=100)
                with self.assertRaisesRegex(AwError, "invalid_attach_timeout"):
                    await session.close(timeout_ms=50)
                self.assertFalse(session.closed)
                handle = session.instrument(
                    agent, input_methods=["ask"], tool_methods={"shell": "shell"}
                )
                try:
                    async with asyncio.timeout(3):
                        with self.assertRaises(AwError) as denied:
                            await agent.shell({"command": "deny"})
                        self.assertEqual(denied.exception.code, "source_denied")
                        call = asyncio.create_task(agent.ask("admitted", "deny"))
                        await entered.wait()
                        with self.assertRaisesRegex(AwError, "source_close_pending"):
                            await source.close()
                        self.assertFalse(handle.restored)
                        self.assertFalse(tasks[0].closed)
                        self.assertFalse(source.instance.closed)
                        release.set()
                        with self.assertRaisesRegex(AwError, "source_session_closed"):
                            await call
                        self.assertEqual(tool_runs, [])
                        await source.close(timeout_ms=5000)
                        self.assertTrue(handle.restored)
                        self.assertTrue(tasks[0].closed)
                        self.assertTrue(source.instance.closed)
                        self.assertEqual(
                            [event for event, _ in tasks[0].events],
                            ["tool.before", "input.submit"],
                        )
                finally:
                    release.set()
                    await source.close(timeout_ms=5000)

    async def test_close_waits_for_completed_stream_observation(self) -> None:
        observed, finish = asyncio.Event(), asyncio.Event()

        async def handler(event, _):
            if event == "tool.after":
                observed.set()
                await finish.wait()
                self.assertFalse(tasks[0].closed)
            return {"decision": "continue"}

        async def native():
            yield "done"

        source, tasks = fake_source(handler)
        session = source.session()
        stream = session.wrap_tool(native)()
        try:
            async with asyncio.timeout(3):
                self.assertEqual(await anext(stream), "done")
                read = asyncio.create_task(anext(stream))
                await observed.wait()
                closing = asyncio.create_task(session.close())
                done, _ = await asyncio.wait({closing}, timeout=0.01)
                self.assertFalse(done)
                finish.set()
                with self.assertRaises(StopAsyncIteration):
                    await read
                await closing
                self.assertTrue(tasks[0].closed)
        finally:
            finish.set()
            await source.close()

    async def test_source_close_closes_all_sessions_concurrently_without_escape(self) -> None:
        source, tasks = fake_source(lambda *_: asyncio.sleep(0, result={"decision": "continue"}))
        s1 = source.session(session_id="s1")
        s2 = source.session(session_id="s2")
        e1, e2, release = asyncio.Event(), asyncio.Event(), asyncio.Event()

        async def slow(event_flag, args):
            event_flag.set()
            await release.wait()
            return args["id"]

        t1 = asyncio.create_task(s1.wrap_tool(lambda args: slow(e1, args), name="a")({"id": 1}))
        t2 = asyncio.create_task(s2.wrap_tool(lambda args: slow(e2, args), name="b")({"id": 2}))
        await asyncio.gather(e1.wait(), e2.wait())
        loop = asyncio.get_running_loop()
        started = loop.time()
        try:
            closing = asyncio.create_task(source.close(timeout_ms=120))
            await asyncio.sleep(0.02)
            # Both existing sessions must already reject new work during shutdown.
            self.assertTrue(s1.closed and s2.closed)
            with self.assertRaisesRegex(AwError, "^source_session_closed$"):
                await s2.before_tool("fast", {})
            with self.assertRaisesRegex(AwError, "^source_close_pending$"):
                await closing
            elapsed = loop.time() - started
            # Two blocked sessions must share the single 120ms close budget, not 240ms.
            self.assertLess(elapsed, 0.21)
            self.assertFalse(any(task.closed for task in tasks))
        finally:
            release.set()
        self.assertEqual(await asyncio.gather(t1, t2), [1, 2])
        await source.close(timeout_ms=5000)
        self.assertTrue(all(task.closed for task in tasks))
        self.assertTrue(source.instance.closed)

    async def test_direct_submit_and_before_tool_track_occupancy_and_reject_after_close(self) -> None:
        for kind in ("submit", "before_tool", "session_start", "turn_stop", "session_end"):
            with self.subTest(kind=kind):
                entered, release = asyncio.Event(), asyncio.Event()

                async def handler(event, _):
                    self.assertFalse(tasks[0].closed)
                    entered.set()
                    await release.wait()
                    self.assertFalse(tasks[0].closed)
                    return {"decision": "continue", "coverage": "checked"}

                source, tasks = fake_source(
                    handler,
                    events=(
                        "input.submit",
                        "tool.before",
                        "tool.after",
                        "session.start",
                        "turn.stop",
                        "session.end",
                    ),
                )
                session = source.session(timeout_ms=100)
                try:
                    if kind == "submit":
                        call = asyncio.create_task(session.submit("hello"))
                    elif kind == "before_tool":
                        call = asyncio.create_task(session.before_tool("shell", {"cmd": "ls"}))
                    elif kind == "session_start":
                        call = asyncio.create_task(session.session_start())
                    elif kind == "turn_stop":
                        call = asyncio.create_task(session.turn_stop("turn1", "stop"))
                    else:
                        call = asyncio.create_task(session.session_end("done"))
                    await entered.wait()
                    with self.assertRaisesRegex(AwError, "^source_close_pending$"):
                        await session.close(timeout_ms=100)
                    self.assertFalse(tasks[0].closed)
                    release.set()
                    with self.assertRaisesRegex(AwError, "^source_session_closed$"):
                        await call
                    await session.close(timeout_ms=5000)
                    self.assertTrue(tasks[0].closed)
                finally:
                    release.set()
                    await source.close(timeout_ms=5000)

    async def test_turn_stop_preserves_block_decision_during_close(self) -> None:
        entered, release = asyncio.Event(), asyncio.Event()

        async def handler(event, _):
            self.assertEqual(event, "turn.stop")
            self.assertFalse(tasks[0].closed)
            entered.set()
            await release.wait()
            return {"decision": "block", "coverage": "checked"}

        source, tasks = fake_source(
            handler,
            events=("input.submit", "tool.before", "tool.after", "turn.stop"),
        )
        session = source.session(timeout_ms=100)
        try:
            call = asyncio.create_task(session.turn_stop("turn1", "stop"))
            await entered.wait()
            with self.assertRaisesRegex(AwError, "^source_close_pending$"):
                await session.close(timeout_ms=100)
            self.assertFalse(tasks[0].closed)
            release.set()
            self.assertEqual(await call, {"decision": "block", "coverage": "checked"})
            await session.close(timeout_ms=5000)
            self.assertTrue(tasks[0].closed)
        finally:
            release.set()
            await source.close(timeout_ms=5000)

    async def test_consumer_stream_cleanup_error_not_repeated_and_stop_race_rejects(self) -> None:
        source, tasks = fake_source(lambda *_: asyncio.sleep(0, result={"decision": "continue"}))
        # 1. Consumer-handled cleanup failure is reported once, not re-raised by session.close().
        session = source.session(session_id="cleanup-once")
        cleanup = RuntimeError("native stream cleanup boom")

        async def failing_cleanup(_args):
            try:
                yield "first"
                yield "second"
            finally:
                raise cleanup

        gen = session.wrap_tool(failing_cleanup, name="fc")({"k": 1})
        self.assertEqual(await anext(gen), "first")
        with self.assertRaises(RuntimeError) as consumer_err:
            await gen.aclose()
        self.assertIs(consumer_err.exception, cleanup)
        self.assertTrue(getattr(consumer_err.exception, "tool_executed", False))
        await session.close()
        self.assertTrue(tasks[0].closed)

        # 2. StopAsyncIteration racing with session.close() raises source_session_closed
        #    with tool_executed instead of silently dropping tool.after.
        session2 = source.session(session_id="stop-race")
        gate, entered = asyncio.Event(), asyncio.Event()

        async def finishing(_args):
            yield "part-1"
            entered.set()
            await gate.wait()

        gen2 = session2.wrap_tool(finishing, name="fin")({"k": 2})
        self.assertEqual(await anext(gen2), "part-1")
        read_task = asyncio.create_task(anext(gen2))
        await entered.wait()

        async def close_on_gate():
            await gate.wait()
            await session2.close()

        closer = asyncio.create_task(close_on_gate())
        await asyncio.sleep(0)
        gate.set()
        with self.assertRaisesRegex(AwError, "^source_session_closed$") as closed_err:
            await read_task
        self.assertTrue(getattr(closed_err.exception, "tool_executed", False))
        await closer
        await source.close()

    async def test_stream_extract_result_receives_native_chunks_and_instrument_uses_attr_name(
        self,
    ) -> None:
        import functools

        source, tasks = fake_source(lambda *_: asyncio.sleep(0, result={"decision": "continue"}))

        class Chunk:
            def __init__(self, value: int) -> None:
                self.value = value

        async def stream_chunks(_args: dict):
            yield Chunk(1)
            yield Chunk(2)

        async def stream_dicts(_args: dict):
            yield {"v": "a"}
            yield {"v": "b"}

        def helper(args: dict) -> dict:
            return args

        class Target:
            pass

        async with source.session(session_id="native-chunks-and-attr") as session:
            wrapped = session.wrap_tool(
                stream_chunks,
                name="chunks",
                extract_result=lambda items: sum(item.value for item in items),
            )
            seen = [chunk.value async for chunk in wrapped({"k": 1})]
            self.assertEqual(seen, [1, 2])
            self.assertEqual(tasks[0].events[-1][0], "tool.after")
            self.assertEqual(tasks[0].events[-1][1]["result"], 3)

            # Plain JSON chunks remain isolated even when extract_result is supplied.
            wrapped_dicts = session.wrap_tool(
                stream_dicts,
                name="dicts",
                extract_result=lambda items: [item["v"] for item in items],
            )
            async for item in wrapped_dicts({"k": 2}):
                item["v"] = "mutated"
            self.assertEqual(tasks[0].events[-1][1]["result"], ["a", "b"])

            # instrument() defaults tool name to the attribute name (supporting partial & aliases).
            target = Target()
            target.run = functools.partial(helper)
            target.search = helper
            session.instrument(target, tool_methods=["run", "search"])
            self.assertEqual(await target.run({"x": 1}), {"x": 1})
            self.assertEqual(await target.search({"q": "hi"}), {"q": "hi"})
        tool_names = [
            payload["name"] for event, payload in tasks[0].events if event == "tool.before"
        ]
        self.assertEqual(tool_names, ["chunks", "dicts", "run", "search"])

    async def test_source_close_omits_stale_pending_when_all_sessions_finish_before_return(
        self,
    ) -> None:
        for fail_short in (False, True):
            with self.subTest(fail_short=fail_short):
                source, tasks = fake_source(
                    lambda *_: asyncio.sleep(0, result={"decision": "continue"})
                )
                short = source.session(session_id="short", timeout_ms=100)
                longer = source.session(session_id="long", timeout_ms=5000)
                short_release, long_release = asyncio.Event(), asyncio.Event()
                cleanup_error = RuntimeError("short cleanup failed")

                async def short_stream(_args):
                    try:
                        yield "ready"
                    finally:
                        await short_release.wait()
                        if fail_short:
                            raise cleanup_error

                async def long_stream(_args):
                    try:
                        yield "ready"
                    finally:
                        await long_release.wait()

                s_gen = short.wrap_tool(short_stream, name="s")({})
                l_gen = longer.wrap_tool(long_stream, name="l")({})
                self.assertEqual(await anext(s_gen), "ready")
                self.assertEqual(await anext(l_gen), "ready")

                async def release_in_order():
                    await asyncio.sleep(0.14)
                    short_release.set()
                    await asyncio.sleep(0.04)
                    long_release.set()

                releaser = asyncio.create_task(release_in_order())
                try:
                    if fail_short:
                        with self.assertRaises(RuntimeError) as raised:
                            await source.close()
                        self.assertIs(raised.exception, cleanup_error)
                    else:
                        await source.close()
                    await releaser
                finally:
                    short_release.set()
                    long_release.set()
                self.assertTrue(all(task.closed for task in tasks))
                self.assertTrue(source.instance.closed)
