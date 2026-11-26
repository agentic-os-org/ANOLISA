"""Verify request Scope ownership and the native input checked for that Scope."""

import asyncio
import threading
from contextvars import ContextVar, copy_context
from unittest.mock import AsyncMock

from aw_sdk import AwError
from aw_sdk.profile_source import ProfileSource
from test_profile_eval import capability, document, field, target
from test_profile_source import CONTINUE, MODULE, ProfileFixture, tool_target


class ProfileRequestTests(ProfileFixture):
    async def test_dynamic_session_and_turn_identity_are_isolated(self) -> None:
        entered = asyncio.Event()
        unblock = asyncio.Event()
        context = ContextVar("profile_session", default="context-default")

        async def run(prompt: str, session: str | None = None, *, turn: str) -> str:
            if prompt == "first":
                entered.set()
                await unblock.wait()
            return prompt

        async def handler(event: str, payload: dict) -> dict:
            return CONTINUE

        self.module.run = run
        target_spec = target(
            MODULE,
            call="async",
            hooks={
                event: capability()
                for event in (
                    "session.start",
                    "input.submit",
                    "turn.stop",
                    "session.end",
                )
            },
            fields={
                "prompt": field("arg.0"),
                "session_id": field("arg.1"),
                "turn_id": field("kwarg.turn"),
            },
        )
        profile = await self.activate(
            [target_spec], handler, session_id="static", session_id_context=context
        )

        async def request(prompt: str, session: str | None, turn: str) -> str:
            async with profile.request():
                return await self.module.run(prompt, session, turn=turn)

        self.assertEqual(self.tasks, [])
        first = asyncio.create_task(request("first", "dynamic", "turn-a"))
        await asyncio.wait_for(entered.wait(), 1)
        context.set("context-b")
        self.assertEqual(await request("second", None, "turn-b"), "second")
        self.assertFalse(self.tasks[0].closed)
        self.assertTrue(self.tasks[1].closed)
        unblock.set()
        self.assertEqual(await first, "first")
        context.set(None)
        await request("third", None, "turn-c")
        self.assertEqual(len(self.tasks), 3)
        self.assertTrue(all(task.closed for task in self.tasks))
        for task in self.tasks:
            self.assertEqual(
                len({payload["session_id"] for _, payload in task.events}), 1
            )
        events = [entry for task in self.tasks for entry in task.events]
        by_session = {
            session_id: [
                (event, payload.get("turn_id"))
                for event, payload in events
                if payload["session_id"] == session_id
            ]
            for session_id in ("dynamic", "context-b", "static")
        }
        for session_id, turn_id in zip(by_session, ("turn-a", "turn-b", "turn-c")):
            self.assertEqual(
                by_session[session_id],
                [
                    ("session.start", None),
                    ("input.submit", turn_id),
                    ("turn.stop", turn_id),
                    ("session.end", None),
                ],
            )

    async def test_borrowed_scope_and_expired_context(self) -> None:
        async def run(payload):
            return payload

        self.module.run = run
        profile = ProfileSource.probe(document(tool_target()))
        self.addAsyncCleanup(profile.teardown)
        profile.install()
        profile.verify()
        source = self.fake_language(profile, AsyncMock(return_value=CONTINUE), None)
        await profile.activate(source)
        with self.assertRaisesRegex(AwError, "source_request_session_required"):
            await self.module.run({})
        release = asyncio.Event()

        async def later():
            await release.wait()
            return await self.module.run({})

        async with source.session("existing") as existing:
            await existing.prepare()
            async with profile.request(existing) as bound:
                self.assertIs(bound.task, existing.task)
                self.assertEqual(await self.module.run({"n": 1}), {"n": 1})
                self.assertEqual(await self.module.run({"n": 2}), {"n": 2})
                escaped = asyncio.create_task(later())
            release.set()
            with self.assertRaisesRegex(AwError, "source_request_session_required"):
                await escaped
            self.assertEqual(len(self.tasks), 1)
            self.assertFalse(existing.task.closed)
            events = existing.task.events
            self.assertEqual(
                [event for event, _ in events], ["tool.before", "tool.after"] * 2
            )
            self.assertEqual(events[0][1]["call_id"], events[1][1]["call_id"])
            self.assertEqual(events[2][1]["call_id"], events[3][1]["call_id"])
        self.assertTrue(self.tasks[0].closed)

    async def test_failure_actions_checked_at_root_and_request(self) -> None:
        async def run(prompt):
            return prompt

        self.module.run = run
        spec = target(
            MODULE,
            call="async",
            hooks={
                "input.submit": capability("block", actions=("report",), mode="control")
            },
            fields={"prompt": field("arg.0")},
        )
        for at_root in (True, False):
            with self.subTest(at_root=at_root):
                profile = ProfileSource.probe(document(spec))
                profile.install()
                profile.verify()
                route = {"event": "input.submit", "step": "policy", "on_error": "block"}
                source = self.fake_language(profile, AsyncMock(), [route])
                try:
                    if at_root:
                        with self.assertRaisesRegex(
                            AwError, "source_route_effect_unsupported"
                        ):
                            await profile.activate(source)
                    else:
                        route["on_error"] = "report"
                        await profile.activate(source)
                        route["on_error"] = "block"
                        with self.assertRaisesRegex(AwError, "ready_not_confirmed"):
                            async with profile.request():
                                self.fail("unconfirmed request admitted")
                finally:
                    await profile.teardown()

    async def test_checked_argument_isolated_through_native_execution(self) -> None:
        entered, resume = asyncio.Event(), asyncio.Event()
        marker = object()

        class Agent:
            async def run(self, envelope, other):
                entered.set()
                await resume.wait()
                return (self, envelope["payload"]["items"][:], other, envelope["other"])

        self.module.Agent = Agent
        spec = tool_target("Agent.run")
        del spec["hooks"]["tool.after"], spec["fields"]["result"]
        for receiver_input in ("receiver.payload", "kwarg.self.payload"):
            with self.subTest(receiver_input=receiver_input):
                spec["fields"]["input"] = field(receiver_input, "json")
                with self.assertRaisesRegex(
                    AwError, "source_probe_receiver_input_unsupported"
                ):
                    ProfileSource.probe(document(spec))
        spec["fields"]["input"] = field("arg.0.payload", "json")
        profile = await self.activate([spec], AsyncMock(return_value=CONTINUE))
        agent = Agent()
        envelope = {"payload": {"items": ["checked"]}, "other": marker}
        async with profile.request():
            pending = asyncio.create_task(agent.run(envelope, other=marker))
            await asyncio.wait_for(entered.wait(), 1)
            envelope["payload"]["items"][0] = "later"
            resume.set()
            self.assertEqual(await pending, (agent, ["checked"], marker, marker))
        self.assertEqual(self.tasks[0].events[0][1]["input"], {"items": ["checked"]})

    async def test_request_exit_waits_for_native_call_and_after_hook(self) -> None:
        entered, release = asyncio.Event(), asyncio.Event()

        async def run(payload):
            entered.set()
            await release.wait()
            return "done"

        self.module.run = run
        profile = await self.activate([tool_target()], AsyncMock(return_value=CONTINUE))

        async def request():
            async with profile.request():
                pending = asyncio.create_task(self.module.run({}))
                await entered.wait()
            return await pending

        owner = asyncio.create_task(request())
        await asyncio.wait_for(entered.wait(), 1)
        try:
            self.assertFalse(self.tasks[0].closed)
        finally:
            release.set()
        self.assertEqual(await asyncio.wait_for(owner, 1), "done")
        self.assertTrue(self.tasks[0].closed)
        self.assertEqual(
            [name for name, _ in self.tasks[0].events], ["tool.before", "tool.after"]
        )

    async def test_borrowed_session_drains_native_work_and_after_on_owner_exit(
        self,
    ) -> None:
        entered, finish = threading.Event(), threading.Event()
        after_entered, after_finish = asyncio.Event(), asyncio.Event()
        order = []

        def run(payload):
            entered.set()
            self.assertTrue(finish.wait(5))
            order.append("native")
            return "ok"

        async def handler(event, payload):
            if event == "tool.after":
                after_entered.set()
                await after_finish.wait()
                self.assertFalse(self.tasks[-1].closed)
                order.append("after")
            return CONTINUE

        self.module.run = run
        profile = ProfileSource.probe(document(tool_target(call="sync")))
        profile.install()
        profile.verify()
        self.addAsyncCleanup(profile.teardown)
        language = self.fake_language(profile, handler, None)
        await profile.activate(language)
        for cancel in (False, True):
            with self.subTest(cancel=cancel):
                entered.clear()
                finish.clear()
                after_entered.clear()
                after_finish.clear()
                order.clear()
                existing = language.session(timeout_ms=100)
                started = asyncio.get_running_loop().create_future()

                async def serve(session, startup, wait_for_cancel):
                    async with session, profile.request(session):
                        worker = asyncio.create_task(
                            asyncio.to_thread(self.module.run, {})
                        )
                        startup.set_result(worker)
                        await asyncio.to_thread(entered.wait, 2)
                        if wait_for_cancel:
                            await asyncio.Event().wait()

                owner = asyncio.create_task(serve(existing, started, cancel))
                worker = await started
                try:
                    self.assertTrue(await asyncio.to_thread(entered.wait, 2))
                    if cancel:
                        owner.cancel()
                    with self.assertRaisesRegex(AwError, "source_close_pending"):
                        await asyncio.wait_for(owner, 2)
                    self.assertTrue(existing.closed)
                    self.assertFalse(existing.task.closed)
                    finish.set()
                    await asyncio.wait_for(after_entered.wait(), 2)
                    self.assertFalse(existing.task.closed)
                finally:
                    finish.set()
                    after_finish.set()
                    await asyncio.wait_for(worker, 3)
                    await existing.close()
                self.assertEqual(order, ["native", "after"])
                self.assertTrue(existing.task.closed)

    async def test_stream_resumption_checks_owner_and_allows_foreign_close(
        self,
    ) -> None:
        actions = []
        identity = ContextVar("business_session")
        local = ContextVar("native_stream_state", default="caller")

        def cleanup(payload):
            actions.append((identity.get(), local.get()))
            return "ok"

        def sync_stream(payload):
            token = local.set("sync")
            try:
                yield "first"
                actions.append("resumed")
                yield "second"
            finally:
                self.module.cleanup({})
                local.reset(token)

        async def async_stream(payload):
            token = local.set("async")
            try:
                yield "first"
                actions.append("resumed")
                yield "second"
            finally:
                await asyncio.to_thread(self.module.cleanup, {})
                local.reset(token)

        self.module.sync_stream, self.module.async_stream = sync_stream, async_stream
        self.module.cleanup = cleanup
        specs = [
            tool_target("sync_stream", call="sync_iter"),
            tool_target("async_stream", call="async_iter"),
        ]
        for spec in specs:
            spec["hooks"]["tool.after"] = capability()
        specs.append(tool_target("cleanup", call="sync"))
        profile = await self.activate(
            specs, AsyncMock(return_value=CONTINUE), session_id_context=identity
        )
        async with profile.request():
            identity.set("construction")
            sync = self.module.sync_stream({})
            astream = self.module.async_stream({})
            identity.set("A")
            self.assertEqual(await asyncio.to_thread(next, sync), "first")
            self.assertEqual(await anext(astream), "first")
            async with profile.request():
                identity.set("B")
                try:
                    for method, args in (
                        (next, (sync,)),
                        (sync.send, ("value",)),
                        (sync.throw, (ValueError(),)),
                    ):
                        with self.assertRaisesRegex(
                            AwError, "source_request_session_mismatch"
                        ):
                            await asyncio.to_thread(method, *args)
                    for step in (
                        lambda: anext(astream),
                        lambda: astream.asend("value"),
                        lambda: astream.athrow(ValueError()),
                    ):
                        with self.assertRaisesRegex(
                            AwError, "source_request_session_mismatch"
                        ):
                            await step()
                    self.assertEqual(actions, [])
                finally:
                    await asyncio.to_thread(sync.close)
                    await astream.aclose()
                self.assertEqual((identity.get(), local.get()), ("B", "caller"))
            self.assertEqual(actions, [("A", "sync"), ("A", "async")])
            self.assertEqual(
                {event["session_id"] for _, event in self.tasks[0].events}, {"A"}
            )
            self.assertEqual(self.tasks[1].events, [])
            self.assertEqual(
                [event for event, _ in self.tasks[0].events].count("tool.after"), 2
            )
        self.assertTrue(all(task.closed for task in self.tasks))

    async def test_finalizer_can_finish_while_request_drains(self) -> None:
        entered, release = asyncio.Event(), asyncio.Event()
        completed = []
        inherited = []

        async def cleanup(payload):
            inherited.append(copy_context())
            entered.set()
            await release.wait()
            completed.append("cleanup")
            return "done"

        async def stream(payload):
            try:
                yield "first"
            finally:
                await self.module.cleanup({})

        self.module.cleanup, self.module.stream = cleanup, stream
        spec = tool_target("stream", call="async_iter")
        spec["hooks"]["tool.after"] = capability()
        profile = await self.activate(
            [spec, tool_target("cleanup")], AsyncMock(return_value=CONTINUE)
        )
        ready = asyncio.get_running_loop().create_future()

        async def serve():
            async with profile.request() as session:
                chunks = self.module.stream({})
                self.assertEqual(await anext(chunks), "first")
                ready.set_result((chunks, session, copy_context()))

        owner = asyncio.create_task(serve())
        chunks, session, ordinary = await asyncio.wait_for(ready, 2)
        closing = asyncio.create_task(chunks.aclose())
        try:
            await asyncio.wait_for(entered.wait(), 2)
            closing.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await closing
            self.assertFalse(owner.done())
            self.assertFalse(session.task.closed)
            with self.assertRaisesRegex(AwError, "source_request_session_required"):
                ordinary.run(self.module.cleanup, {})
        finally:
            release.set()
            await chunks.aclose()
            await owner
        await chunks.aclose()
        self.assertEqual(completed, ["cleanup"])
        self.assertTrue(session.task.closed)
        self.assertEqual(
            [event for event, _ in session.task.events],
            ["tool.before", "tool.before", "tool.after"],
        )
        with self.assertRaisesRegex(AwError, "source_request_session_required"):
            inherited[0].run(self.module.cleanup, {})

    async def test_async_stream_preserves_native_deadline_across_yields(self) -> None:
        tasks = []

        async def stream(payload):
            async with asyncio.timeout(None) as deadline:
                tasks.append(asyncio.current_task())
                try:
                    yield "first"
                except ValueError:
                    tasks.append(asyncio.current_task())
                    yield "caught"
                tasks.append(asyncio.current_task())
                loop = asyncio.get_running_loop()
                wakeup = asyncio.Event()
                deadline.reschedule(loop.time())
                loop.call_soon(wakeup.set)
                await wakeup.wait()
                yield "late"

        self.module.stream = stream
        spec = tool_target("stream", call="async_iter")
        spec["hooks"]["tool.after"] = capability()
        profile = await self.activate([spec], AsyncMock(return_value=CONTINUE))
        async with profile.request():
            for factory in (stream, self.module.stream):
                with self.subTest(factory=factory):
                    tasks.clear()
                    chunks = factory({})
                    try:
                        self.assertEqual(await anext(chunks), "first")
                        self.assertEqual(await chunks.athrow(ValueError()), "caught")
                        with self.assertRaises(TimeoutError):
                            await chunks.asend("next")
                        self.assertEqual(len(tasks), 3)
                        self.assertTrue(all(task is tasks[0] for task in tasks))
                    finally:
                        await chunks.aclose()

    async def test_stream_task_survives_reader_handoff_and_idle_deadline(self) -> None:
        tasks = []
        scopes = []
        finalized = asyncio.Event()

        async def stream(payload):
            async with asyncio.timeout(None) as scope:
                scopes.append(scope)
                try:
                    tasks.append(asyncio.current_task())
                    yield "first"
                    tasks.append(asyncio.current_task())
                    yield "second"
                finally:
                    tasks.append(asyncio.current_task())
                    finalized.set()

        self.module.stream = stream
        spec = tool_target("stream", call="async_iter")
        spec["hooks"]["tool.after"] = capability()
        profile = await self.activate([spec], AsyncMock(return_value=CONTINUE))
        async with profile.request():
            chunks = self.module.stream({})
            try:
                self.assertEqual(await asyncio.create_task(anext(chunks)), "first")
                self.assertEqual(await asyncio.create_task(anext(chunks)), "second")
                scopes[0].reschedule(asyncio.get_running_loop().time())
                await asyncio.wait_for(finalized.wait(), 2)
                with self.assertRaises(TimeoutError):
                    await anext(chunks)
                with self.assertRaises(StopAsyncIteration):
                    await anext(chunks)
            finally:
                await chunks.aclose()
        self.assertEqual(len(tasks), 3)
        self.assertTrue(all(task is tasks[0] for task in tasks))

    async def test_abandoned_async_stream_releases_its_reader_task(self) -> None:
        finalized = asyncio.Event()
        tasks = []

        async def stream(payload):
            tasks.append(asyncio.current_task())
            try:
                yield "first"
            finally:
                finalized.set()

        self.module.stream = stream
        spec = tool_target("stream", call="async_iter")
        spec["hooks"]["tool.after"] = capability()
        profile = await self.activate([spec], AsyncMock(return_value=CONTINUE))
        async with profile.request():
            self.assertEqual(await anext(self.module.stream({})), "first")
            await asyncio.wait_for(finalized.wait(), 2)
        self.assertTrue(tasks[0].done())
        self.assertTrue(self.tasks[0].closed)

    async def test_cancelled_read_drains_instrumented_cleanup(self) -> None:
        await self._check_cancelled_read(deadline=False)

    async def test_native_deadline_drains_instrumented_cleanup(self) -> None:
        await self._check_cancelled_read(deadline=True)

    async def _check_cancelled_read(self, *, deadline: bool) -> None:
        scopes = []
        reading, cleaning, finish = asyncio.Event(), asyncio.Event(), asyncio.Event()
        completed = []

        async def cleanup(payload):
            cleaning.set()
            await finish.wait()
            completed.append("cleanup")
            return "done"

        async def stream(payload):
            async with asyncio.timeout(None) as scope:
                scopes.append(scope)
                try:
                    yield "first"
                    reading.set()
                    await asyncio.Event().wait()
                finally:
                    await self.module.cleanup({})

        self.module.cleanup, self.module.stream = cleanup, stream
        spec = tool_target("stream", call="async_iter")
        spec["hooks"]["tool.after"] = capability()
        profile = await self.activate(
            [spec, tool_target("cleanup")], AsyncMock(return_value=CONTINUE)
        )
        ready = asyncio.get_running_loop().create_future()

        async def serve():
            async with profile.request() as session:
                chunks = self.module.stream({})
                self.assertEqual(await anext(chunks), "first")
                reader = asyncio.create_task(anext(chunks))
                await reading.wait()
                ready.set_result((chunks, session, reader, copy_context()))

        owner = asyncio.create_task(serve())
        chunks, session, reader, expired = await asyncio.wait_for(ready, 2)
        try:
            with self.assertRaisesRegex(AwError, "source_request_session_required"):
                expired.run(self.module.cleanup, {})
            if deadline:
                scopes[0].reschedule(asyncio.get_running_loop().time())
            else:
                reader.cancel()
            await asyncio.wait_for(cleaning.wait(), 2)
            self.assertFalse(reader.done())
            self.assertFalse(owner.done())
            self.assertFalse(session.task.closed)
            with self.assertRaisesRegex(AwError, "source_request_session_required"):
                expired.run(self.module.cleanup, {})
        finally:
            finish.set()
            await asyncio.gather(reader, return_exceptions=True)
            await chunks.aclose()
            await asyncio.wait_for(owner, 2)
        with self.assertRaises(TimeoutError if deadline else asyncio.CancelledError):
            await reader
        self.assertEqual(completed, ["cleanup"])
        self.assertTrue(session.task.closed)
        self.assertEqual(
            [event for event, _ in session.task.events],
            ["tool.before", "tool.before", "tool.after"],
        )

    async def test_lazy_calls_cannot_move_between_normal_and_fallback_requests(
        self,
    ) -> None:
        executed = []

        async def run(payload):
            executed.append("coroutine")
            return "done"

        def stream(payload):
            executed.append("stream")
            yield "item"

        self.module.run, self.module.stream = run, stream
        spec = tool_target("stream", call="sync_iter")
        spec["hooks"]["tool.after"] = capability()
        profile = await self.activate(
            [tool_target(), spec],
            AsyncMock(return_value=CONTINUE),
            availability="native_fallback",
        )
        for fallback in (False, True):
            async with profile.request() as first:
                first.task.fallback = fallback
                coro, iterator = self.module.run({}), self.module.stream({})
                async with profile.request() as second:
                    second.task.fallback = not fallback
                    try:
                        with self.assertRaisesRegex(
                            AwError, "source_request_session_mismatch"
                        ):
                            await coro
                        with self.assertRaisesRegex(
                            AwError, "source_request_session_mismatch"
                        ):
                            await asyncio.to_thread(next, iterator)
                    finally:
                        coro.close()
                        iterator.close()
        self.assertEqual(executed, [])
