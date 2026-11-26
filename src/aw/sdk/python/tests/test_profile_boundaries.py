"""Verify guarded startup, result delivery and teardown at application entry points."""

import asyncio
import gc
import threading
import weakref
from contextvars import ContextVar
from unittest.mock import AsyncMock, patch

from aw_sdk import AwError
from aw_sdk.profile_source import ProfileSource
from test_profile_eval import capability, document
from test_profile_source import CONTINUE, ProfileFixture, tool_target


class ProfileBoundaryTests(ProfileFixture):
    async def test_bad_activation_options_keep_required_guards(self) -> None:
        def run(payload):
            return payload

        self.module.run = run
        profile = ProfileSource.probe(document(tool_target(call="sync")))
        profile.install()
        profile.verify()
        try:
            with self.assertRaisesRegex(AwError, "source_endpoint_and_target_required"):
                await profile.activate()
            with self.assertRaisesRegex(AwError, "source_unavailable"):
                self.module.run({})
        finally:
            await profile.teardown()
        self.assertIs(self.module.run, run)

    async def test_nonstream_results_reject_deferred_execution(self) -> None:
        async def deferred():
            self.fail("unconsumed coroutine ran")

        def iterator():
            self.fail("unconsumed generator ran")
            yield None

        async def run(payload):
            return iterator()

        self.module.run = run
        self.module.sync = lambda payload: deferred()
        profile = await self.activate(
            [tool_target(), tool_target("sync", call="sync")],
            AsyncMock(return_value=CONTINUE),
        )
        async with profile.request():
            with self.assertRaisesRegex(AwError, "source_target_call_shape_mismatch"):
                await self.module.run({})
            with self.assertRaisesRegex(AwError, "source_target_call_shape_mismatch"):
                await asyncio.to_thread(self.module.sync, {})
        self.assertTrue(self.tasks[0].closed)

    async def test_wrong_loop_first_read_leaves_stream_available_to_owner(self) -> None:
        async def stream(payload):
            yield "ok"

        self.module.stream = stream
        spec = tool_target("stream", call="async_iter")
        spec["hooks"]["tool.after"] = capability()
        profile = await self.activate(
            [spec], AsyncMock(return_value=CONTINUE), availability="native_fallback"
        )
        for fallback in (False, True):
            async with profile.request() as session:
                session.task.fallback = fallback
                chunks = self.module.stream({})
                with self.assertRaisesRegex(AwError, "source_activation_loop_required"):
                    await asyncio.to_thread(asyncio.run, anext(chunks))
                self.assertEqual(await anext(chunks), "ok")
                await chunks.aclose()

    async def test_sync_gc_finalizer_keeps_request_hooks_during_teardown(self) -> None:
        identity = ContextVar("identity", default="outside")
        calls = []

        def cleanup(payload):
            calls.append(identity.get())
            return "clean"

        def stream(payload):
            token = identity.set("stream")
            try:
                yield "item"
            finally:
                self.module.cleanup({})
                identity.reset(token)

        self.module.stream, self.module.cleanup = stream, cleanup
        spec = tool_target("stream", call="sync_iter")
        spec["hooks"]["tool.after"] = capability()
        profile = await self.activate(
            [spec, tool_target("cleanup", call="sync")],
            AsyncMock(return_value=CONTINUE),
            session_id_context=identity,
        )
        async with profile.request() as session:
            chunks = self.module.stream({})
            self.assertEqual(await asyncio.to_thread(next, chunks), "item")
            stopping = asyncio.create_task(profile.teardown())
            await asyncio.sleep(0)
            try:
                with self.assertRaisesRegex(AwError, "source_unavailable"):
                    self.module.cleanup({})
                del chunks
            finally:
                self.assertFalse(stopping.done())
        await asyncio.wait_for(stopping, 2)
        self.assertEqual(calls, ["stream"])
        self.assertEqual(identity.get(), "outside")
        self.assertEqual(
            [event for event, _ in session.task.events],
            ["tool.before", "tool.before", "tool.after"],
        )
        self.assertEqual(session.task.events[-1][1]["session_id"], "stream")
        self.assertTrue(session.task.closed)
        self.assertIs(self.module.cleanup, cleanup)

    async def test_activate_without_verification_gates_installed_targets(self) -> None:
        def run(payload):
            return payload

        self.module.run = run
        profile = ProfileSource.probe(document(tool_target(call="sync")))
        profile.install()
        cached = self.module.run
        try:
            with self.assertRaisesRegex(AwError, "source_profile_state_invalid"):
                await profile.activate()
            for fn in (cached, self.module.run):
                with self.assertRaisesRegex(AwError, "source_unavailable"):
                    fn({})
        finally:
            await profile.teardown()
        self.assertIs(self.module.run, run)

    async def test_normal_stream_completion_keeps_hooks_during_teardown(self) -> None:
        completed = []

        async def cleanup(payload):
            completed.append(payload["name"])
            return "ok"

        async def async_stream(payload):
            try:
                yield "first"
            finally:
                await self.module.cleanup({"name": "async"})

        def sync_cleanup(payload):
            completed.append(payload["name"])
            return "ok"

        def sync_stream(payload):
            try:
                yield "first"
            finally:
                self.module.sync_cleanup({"name": "sync"})

        self.module.cleanup, self.module.sync_cleanup = cleanup, sync_cleanup
        self.module.stream, self.module.astream = sync_stream, async_stream
        streams = [
            tool_target("stream", call="sync_iter"),
            tool_target("astream", call="async_iter"),
        ]
        for spec in streams:
            spec["hooks"]["tool.after"] = capability()
        profile = await self.activate(
            [
                *streams,
                tool_target("cleanup"),
                tool_target("sync_cleanup", call="sync"),
            ],
            AsyncMock(return_value=CONTINUE),
        )
        async with profile.request() as session:
            chunks, achunks = self.module.stream({}), self.module.astream({})
            self.assertEqual(await asyncio.to_thread(next, chunks), "first")
            self.assertEqual(await anext(achunks), "first")
            stopping = asyncio.create_task(profile.teardown())
            await asyncio.sleep(0)
            with self.assertRaisesRegex(AwError, "source_unavailable"):
                self.module.cleanup({})
            self.assertIsNone(await asyncio.to_thread(next, chunks, None))
            self.assertIsNone(await anext(achunks, None))
            self.assertEqual(completed, ["sync", "async"])
            self.assertEqual(
                [e for e, _ in session.task.events].count("tool.before"), 4
            )
        await stopping
        self.assertTrue(session.task.closed)

    async def test_request_closes_sync_stream_with_application_back_reference(
        self,
    ) -> None:
        closed = []
        identity = ContextVar("stream", default="outside")

        def stream():
            token = identity.set("native")
            try:
                yield "item"
            finally:
                closed.append(identity.get())
                identity.reset(token)

        self.module.stream = stream
        spec = tool_target("stream", call="sync_iter")
        spec["hooks"] = {"session.start": capability()}
        spec["fields"] = {}
        profile = await self.activate([spec], AsyncMock(return_value=CONTINUE))
        holder = ContextVar("holder")
        async with profile.request() as session:
            refs = []
            token = holder.set(refs)
            chunks = self.module.stream()
            ref = weakref.ref(chunks)
            refs.append(chunks)
            self.assertEqual(await asyncio.to_thread(next, chunks), "item")
            holder.reset(token)
            del chunks, refs
            gc.collect()
        gc.collect()
        self.assertIsNone(ref())
        self.assertEqual(closed, ["native"])
        self.assertTrue(session.task.closed)

    async def test_abandoned_sync_cleanup_failure_reaches_request_and_teardown(
        self,
    ) -> None:
        def stream():
            try:
                yield "item"
            finally:
                raise ValueError("cleanup failed")

        self.module.stream = stream
        spec = tool_target("stream", call="sync_iter")
        spec.update(hooks={"session.start": capability()}, fields={})
        profile = ProfileSource.probe(document(spec))
        profile.install()
        profile.verify()
        await profile.activate(
            self.fake_language(profile, AsyncMock(return_value=CONTINUE), None)
        )

        async def serve(mode):
            async with asyncio.timeout(None) as deadline:
                async with profile.request():
                    chunks = self.module.stream()
                    self.assertEqual(await asyncio.to_thread(next, chunks), "item")
                    if mode == "cancel":
                        asyncio.current_task().cancel("request cancelled")
                    elif mode == "timeout":
                        deadline.reschedule(asyncio.get_running_loop().time())
                    elif mode == "inner_timeout":
                        async with asyncio.timeout(0):
                            await asyncio.sleep(0)
                    del chunks
                    await asyncio.sleep(0)

        try:
            for mode, error in (
                ("normal", ValueError),
                ("cancel", asyncio.CancelledError),
                ("timeout", TimeoutError),
                ("inner_timeout", TimeoutError),
            ):
                with self.subTest(mode=mode), self.assertRaises(error) as caught:
                    await asyncio.create_task(serve(mode))
                self.assertTrue(self.tasks[-1].closed)
                if mode == "normal":
                    self.assertEqual(str(caught.exception), "cleanup failed")
                if mode == "cancel":
                    self.assertEqual(str(caught.exception), "request cancelled")
        finally:
            with self.assertRaisesRegex(AwError, "source_cleanup_unconfirmed"):
                await profile.teardown()

    async def test_sync_stream_drains_after_default_executor_shutdown(self) -> None:
        closed = []

        def cleanup(payload):
            closed.append(self.tasks[-1].closed)
            return "done"

        def stream():
            try:
                yield "item"
            finally:
                self.module.cleanup({})

        self.module.stream, self.module.cleanup = stream, cleanup
        spec = tool_target("stream", call="sync_iter")
        spec.update(hooks={"session.start": capability()}, fields={})
        profile = await self.activate(
            [spec, tool_target("cleanup", call="sync")],
            AsyncMock(return_value=CONTINUE),
        )
        async with profile.request() as session:
            chunks = self.module.stream()
            self.assertEqual(await asyncio.to_thread(next, chunks), "item")
            await asyncio.get_running_loop().shutdown_default_executor()
        chunks.close()
        self.assertEqual(closed, [False])
        self.assertTrue(session.task.closed)

    async def test_cancel_drains_all_streams_when_new_threads_cannot_start(
        self,
    ) -> None:
        closed = []

        def cleanup(payload):
            closed.append((payload["index"], self.tasks[-1].closed))
            return "ok"

        def stream(index):
            try:
                yield index
            finally:
                self.module.cleanup({"index": index})

        self.module.stream, self.module.cleanup = stream, cleanup
        spec = tool_target("stream", call="sync_iter")
        spec.update(hooks={"session.start": capability()}, fields={})
        profile = await self.activate(
            [spec, tool_target("cleanup", call="sync")],
            AsyncMock(return_value=CONTINUE),
        )

        async def serve():
            async with profile.request():
                streams = [self.module.stream(i) for i in range(2)]
                for i, chunks in enumerate(streams):
                    self.assertEqual(await asyncio.to_thread(next, chunks), i)
                await asyncio.get_running_loop().shutdown_default_executor()
                disabled.start()
                asyncio.current_task().cancel("business request")
                await asyncio.sleep(0)

        disabled = patch.object(
            threading.Thread, "start", side_effect=RuntimeError("no threads")
        )
        try:
            with self.assertRaisesRegex(asyncio.CancelledError, "business request"):
                await asyncio.create_task(serve())
            self.assertCountEqual(closed, [(0, False), (1, False)])
            self.assertTrue(self.tasks[0].closed)
        finally:
            disabled.stop()

    async def test_rejected_task_is_cancelled_and_joined_before_scope_release(
        self,
    ) -> None:
        started, cleaning, finish = asyncio.Event(), asyncio.Event(), asyncio.Event()
        workers = []

        async def work():
            try:
                started.set()
                await asyncio.Event().wait()
            finally:
                cleaning.set()
                await finish.wait()

        async def run(payload):
            workers.append(asyncio.create_task(work()))
            await started.wait()
            return workers[-1]

        self.module.run = run
        profile = await self.activate([tool_target()], AsyncMock(return_value=CONTINUE))

        async def serve():
            async with profile.request():
                with self.assertRaisesRegex(
                    AwError, "source_target_call_shape_mismatch"
                ):
                    await self.module.run({})

        owner = asyncio.create_task(serve())
        try:
            await asyncio.wait_for(cleaning.wait(), 2)
            self.assertFalse(owner.done())
            self.assertFalse(self.tasks[0].closed)
        finally:
            finish.set()
            await asyncio.wait_for(owner, 2)
        self.assertTrue(workers[0].cancelled())
        self.assertTrue(self.tasks[0].closed)

    async def test_sync_caller_loop_drains_rejected_task(self) -> None:
        cleaning, finish = threading.Event(), threading.Event()
        workers = []

        async def work():
            try:
                await asyncio.Event().wait()
            finally:
                cleaning.set()
                await asyncio.to_thread(finish.wait, 2)

        def run(payload):
            return workers[-1]

        self.module.run = run
        profile = await self.activate(
            [tool_target(call="sync")], AsyncMock(return_value=CONTINUE)
        )

        async def caller():
            workers.append(asyncio.create_task(work()))
            await asyncio.sleep(0)
            with self.assertRaisesRegex(AwError, "source_target_call_shape_mismatch"):
                self.module.run({})
            self.assertTrue(workers[-1].cancelling())
            with self.assertRaises(asyncio.CancelledError):
                await workers[-1]

        draining = asyncio.Event()

        async def serve():
            async with profile.request():
                caller_task = asyncio.create_task(
                    asyncio.to_thread(asyncio.run, caller())
                )
                self.assertTrue(await asyncio.to_thread(cleaning.wait, 2))
                draining.set()
            await caller_task

        owner = asyncio.create_task(serve())
        try:
            await asyncio.wait_for(draining.wait(), 2)
            self.assertFalse(owner.done())
            self.assertFalse(self.tasks[-1].closed)
        finally:
            finish.set()
        await asyncio.wait_for(owner, 2)
        self.assertTrue(self.tasks[-1].closed)

    async def test_rejected_task_retains_scope_across_foreign_loop_stop(self) -> None:
        foreign = asyncio.new_event_loop()
        self.addCleanup(foreign.close)
        closed_at_cleanup = []

        async def work():
            try:
                await asyncio.Event().wait()
            finally:
                closed_at_cleanup.append(self.tasks[-1].closed)

        async def run(payload):
            return worker

        self.module.run, self.module.sync = run, lambda payload: worker
        profile = ProfileSource.probe(
            document(tool_target(), tool_target("sync", call="sync"))
        )
        self.addAsyncCleanup(profile.teardown)
        profile.install()
        profile.verify()
        await profile.activate(
            self.fake_language(profile, AsyncMock(return_value=CONTINUE), None),
            timeout_ms=100,
        )

        async def serve(sync, rejected):
            async with profile.request():
                with self.assertRaisesRegex(
                    AwError, "source_target_call_shape_mismatch"
                ):
                    if sync:
                        await asyncio.to_thread(self.module.sync, {})
                    else:
                        await self.module.run({})
                rejected.set()

        def stop_loop(stopped, release):
            foreign.stop()
            stopped.set()
            release.wait(3)

        async def join(worker):
            try:
                await asyncio.wait_for(asyncio.shield(worker), 2)
            except asyncio.CancelledError:
                pass
            finally:
                if not worker.done():
                    worker.cancel()
                    await asyncio.gather(worker, return_exceptions=True)

        for sync in (False, True):
            with self.subTest(sync=sync):
                worker = foreign.create_task(work())
                await asyncio.to_thread(foreign.run_until_complete, asyncio.sleep(0))
                stopped = threading.Event()
                release = threading.Event()

                if sync:
                    # Stop after the rejection is queued but before it is delivered.
                    foreign.call_soon(stop_loop, stopped, release)
                    running = asyncio.create_task(
                        asyncio.to_thread(foreign.run_forever)
                    )
                    self.assertTrue(await asyncio.to_thread(stopped.wait, 2))
                rejected = asyncio.Event()
                owner = asyncio.create_task(serve(sync, rejected))
                try:
                    await asyncio.wait_for(rejected.wait(), 2)
                    if sync:
                        owner.cancel()
                        with self.assertRaises(asyncio.CancelledError):
                            await owner
                    else:
                        with self.assertRaisesRegex(AwError, "source_close_pending"):
                            await asyncio.wait_for(owner, 2)
                    self.assertFalse(self.tasks[-1].closed)
                finally:
                    release.set()
                    if sync:
                        await running
                    # Resume the owner loop to deliver the queued cancellation.
                    await asyncio.to_thread(foreign.run_until_complete, join(worker))
                self.assertTrue(worker.cancelled())
                await asyncio.wait_for(asyncio.gather(*profile._requests), 2)
                self.assertTrue(self.tasks[-1].closed)
        self.assertEqual(closed_at_cleanup, [False, False])

    async def test_rejected_foreign_task_can_run_its_cleanup_hooks(self) -> None:
        foreign = asyncio.new_event_loop()
        self.addCleanup(foreign.close)
        finalizers = []
        workers = []

        def cleanup(payload):
            finalizers.append((payload["fail"], self.tasks[-1].closed))
            if payload["fail"]:
                raise ValueError("native cleanup failed")
            return "done"

        async def work(fail):
            try:
                await asyncio.Event().wait()
            finally:
                # A sibling inherits the request context, but has no admission.
                async def sibling():
                    with self.assertRaisesRegex(
                        AwError, "source_request_session_required"
                    ):
                        self.module.cleanup({"fail": False})

                await asyncio.create_task(sibling())
                self.module.cleanup({"fail": fail})

        async def run(payload):
            worker = foreign.create_task(work(payload["fail"]))
            workers.append(worker)
            await asyncio.to_thread(foreign.run_until_complete, asyncio.sleep(0))
            return worker

        self.module.run, self.module.cleanup = run, cleanup
        profile = ProfileSource.probe(
            document(tool_target(), tool_target("cleanup", call="sync"))
        )
        profile.install()
        profile.verify()
        await profile.activate(
            self.fake_language(profile, AsyncMock(return_value=CONTINUE), None)
        )

        async def serve(fail, draining):
            async with profile.request():
                with self.assertRaisesRegex(
                    AwError, "source_target_call_shape_mismatch"
                ):
                    await self.module.run({"fail": fail})
                draining.set()

        try:
            for fail in (False, True):
                draining = asyncio.Event()
                owner = asyncio.create_task(serve(fail, draining))
                await asyncio.wait_for(draining.wait(), 2)
                self.assertFalse(self.tasks[-1].closed)
                outcomes = await asyncio.to_thread(
                    foreign.run_until_complete,
                    asyncio.gather(workers[-1], return_exceptions=True),
                )
                if fail:
                    with self.assertRaisesRegex(ValueError, "native cleanup failed"):
                        await asyncio.wait_for(owner, 2)
                    self.assertIsInstance(outcomes[0], ValueError)
                else:
                    await asyncio.wait_for(owner, 2)
                    self.assertIsInstance(outcomes[0], asyncio.CancelledError)
                self.assertTrue(self.tasks[-1].closed)
                self.assertEqual(
                    [event for event, _ in self.tasks[-1].events],
                    ["tool.before", "tool.before"] + ([] if fail else ["tool.after"]),
                )
            self.assertEqual(finalizers, [(False, False), (True, False)])
        finally:
            with self.assertRaisesRegex(AwError, "source_cleanup_unconfirmed"):
                await profile.teardown()

    async def test_rejected_result_cleanup_preserves_cancel_and_timeout(self) -> None:
        closing, closed = asyncio.Event(), asyncio.Event()
        release = asyncio.Event()

        class Result:
            def __aiter__(self):
                raise AssertionError("unexpected iteration")

            async def aclose(self):
                closing.set()
                await release.wait()
                closed.set()

        async def run(payload):
            return Result()

        self.module.run = run
        profile = await self.activate([tool_target()], AsyncMock(return_value=CONTINUE))

        async def serve(deadlines):
            async with profile.request():
                async with asyncio.timeout(None) as clock:
                    deadlines.append(clock)
                    await self.module.run({})

        for timeout in (False, True):
            with self.subTest(timeout=timeout):
                closing.clear()
                closed.clear()
                release.clear()
                deadlines = []

                owner = asyncio.create_task(serve(deadlines))
                try:
                    await asyncio.wait_for(closing.wait(), 2)
                    if timeout:
                        deadlines[0].reschedule(asyncio.get_running_loop().time())
                    else:
                        owner.cancel()
                    await asyncio.sleep(0)
                    self.assertFalse(self.tasks[-1].closed)
                finally:
                    release.set()
                with self.assertRaises(
                    TimeoutError if timeout else asyncio.CancelledError
                ):
                    await asyncio.wait_for(owner, 2)
                self.assertTrue(closed.is_set())
                self.assertTrue(self.tasks[-1].closed)

    async def test_nonstream_target_rejects_next_only_result(self) -> None:
        closed = []

        class Result:
            def __next__(self):
                raise AssertionError("unexpected iteration")

            def close(self):
                closed.append(True)

        self.module.run = lambda payload: Result()
        spec = tool_target(call="sync")
        del spec["hooks"]["tool.after"], spec["fields"]["result"]
        profile = await self.activate([spec], AsyncMock(return_value=CONTINUE))
        async with profile.request():
            with self.assertRaisesRegex(AwError, "source_target_call_shape_mismatch"):
                await asyncio.to_thread(self.module.run, {})
        self.assertEqual(closed, [True])
