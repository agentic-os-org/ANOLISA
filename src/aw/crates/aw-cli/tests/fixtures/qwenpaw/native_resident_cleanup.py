"""Deterministic ownership, retirement acknowledgement and cleanup deadlines."""

from native_resident_support import *
from aw_resident_test import aw_resident


class CleanupResidentTests(NativeFixture):
    def setUp(self):
        super().setUp()
        self.fence = patch.object(aw_resident, "cleanup_failed", False)
        self.fence.start()
        self.addCleanup(self.fence.stop)

    async def prepared(self):
        closed = []
        @asynccontextmanager
        async def private():
            try:
                yield PrivateExecution({}, {}, lambda value: value, {"TASK_KEY": "fixture-secret"})
            finally:
                closed.append(True)
        async def tool(tool_call):
            yield response()
        mw, agent, coordinator = self.setup_agent(tool, context=SimpleNamespace(
            aw_execution_factory=private, aw_environment_version=1))
        mw.agent = agent
        mw.lease_context = private()
        mw.lease = await mw.lease_context.__aenter__()
        await mw.scope.open("/tmp", mw.lease.hook_environment)
        call = aw_resident.Invocation(mw, self.call(), 1, '{"value": 1}')
        mw.active[call.identity] = call
        await mw.scope.register(1, call.identity, "tool", {"value": 1})
        return mw, call, closed

    async def test_finish_failure_releases_drained_credentials_and_fences_worker(self):
        async def tool(tool_call):
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        attempts = []
        async def failed_finish(*args):
            attempts.append(args)
            raise RuntimeError("finish unavailable")
        mw.scope.finish = failed_finish
        with self.assertRaises(Exception):
            await self.request(mw, agent, [self.call()])
        self.assertEqual(len(attempts), 1)
        self.assertTrue(mw.failed)
        self.assertEqual(len(mw.active), 1)
        self.assertIsNone(mw.lease)
        with self.assertRaisesRegex(ResidentFailure, "restart"):
            aw_resident.check_cleanup()
        fresh, next_agent, _ = self.setup_agent(tool)
        with self.assertRaisesRegex(ResidentFailure, "restart"):
            await self.request(fresh, next_agent, [self.call()])

    async def test_failed_finish_does_not_skip_other_owned_native_tasks(self):
        for offloaded, inserted_first in ((False, False), (True, False), (True, True)):
            with self.subTest(offloaded=offloaded, inserted_first=inserted_first):
                with patch.object(aw_resident, "cleanup_failed", False):
                    await self.failed_finish_drains_other_call(offloaded, inserted_first)

    async def failed_finish_drains_other_call(self, offloaded, inserted_first):
        mw, first, closed = await self.prepared()
        first.native_done = True
        second = aw_resident.Invocation(mw, self.call("second"), 2, '{"value": 1}')
        second.offloaded = offloaded
        if inserted_first:
            mw.active = {second.identity: second, first.identity: first}
        else:
            mw.active[second.identity] = second
        await mw.scope.register(2, second.identity, "tool", {"value": 1})
        entered, drained = asyncio.Event(), asyncio.Event()
        async def original():
            entered.set()
            try:
                await asyncio.Event().wait()
            finally:
                self.assertEqual(closed, [])
                self.assertEqual(mw.lease.secrets["TASK_KEY"], "fixture-secret")
                second.native_done = True
                drained.set()
        task = asyncio.create_task(original())
        second.entry = SimpleNamespace(background_task=task)
        async def supervisor():
            await asyncio.gather(task, return_exceptions=True)
            second.background_done = True
        if offloaded:
            second.supervisor = asyncio.create_task(supervisor())
        await asyncio.wait_for(entered.wait(), 1)
        finish = mw.scope.finish
        async def fail_first(sequence, identity):
            if identity == first.identity:
                raise RuntimeError("first finish failed")
            await finish(sequence, identity)
        mw.scope.finish = fail_first
        mw.request_done = True
        try:
            with self.assertRaisesRegex(RuntimeError, "first finish failed"):
                await aw_resident.settle(mw.drain_foreground(), timeout=1)
            self.assertTrue(drained.is_set())
            self.assertTrue(task.done())
            self.assertEqual(closed, [True])
            self.assertIsNone(mw.lease)
            self.assertEqual(list(mw.active), [first.identity])
            self.assertTrue(aw_resident.cleanup_failed)
            if offloaded:
                self.assertTrue(second.supervisor.done())
                self.assertTrue(second.background_done)
        finally:
            if not task.done():
                task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            if second.supervisor is not None:
                await second.supervisor

    async def test_scope_close_failure_clears_lease_and_fences_worker(self):
        mw, call, closed = await self.prepared()
        call.native_done = True
        await mw.retire(call)
        async def failed_close():
            raise RuntimeError("close unavailable")
        mw.scope.close = failed_close
        mw.request_done = True
        with self.assertRaisesRegex(RuntimeError, "close unavailable"):
            await mw.close_if_drained()
        self.assertEqual(closed, [True])
        self.assertIsNone(mw.lease)
        self.assertTrue(aw_resident.cleanup_failed)

    async def test_three_retirement_paths_share_one_finish_acknowledgement(self):
        mw, call, closed = await self.prepared()
        entered, release = asyncio.Event(), asyncio.Event()
        finish = mw.scope.finish
        attempts = []
        async def delayed_finish(*args):
            attempts.append(args)
            entered.set()
            await release.wait()
            await finish(*args)
        mw.scope.finish = delayed_finish
        call.native_done = True
        mw.request_done = True
        tasks = [asyncio.create_task(mw.retire(call)) for _ in range(3)]
        try:
            await asyncio.wait_for(entered.wait(), 1)
            await asyncio.sleep(0)
            self.assertEqual(len(attempts), 1)
            self.assertEqual(closed, [])
            tasks[0].cancel()
            release.set()
            results = await asyncio.wait_for(asyncio.gather(*tasks, return_exceptions=True), 1)
            self.assertIsInstance(results[0], asyncio.CancelledError)
            self.assertEqual(results[1:], [None, None])
            await mw.retire(call)
            self.assertEqual(len(attempts), 1)
            self.assertEqual(closed, [True])
            self.assertEqual(mw.active, {})
            self.assertTrue(call.retired)
        finally:
            release.set()
            await asyncio.gather(*tasks, return_exceptions=True)

    async def test_raising_closed_coordinator_allows_safe_credential_cleanup(self):
        mw, call, closed = await self.prepared()
        async def stream():
            try:
                yield response()
            finally:
                raise RuntimeError("coordinator close failed")
        call.coordinator_stream = stream()
        await anext(call.coordinator_stream)
        call.native_done = True
        mw.request_done = True
        with self.assertRaisesRegex(RuntimeError, "coordinator close failed"):
            await call.close_coordinator()
        self.assertTrue(call.coordinator_closed)
        self.assertTrue(aw_resident.cleanup_failed)
        await mw.retire(call)
        self.assertEqual(closed, [True])
        self.assertIsNone(mw.lease)

    async def test_unknown_coordinator_close_failure_retains_lease_until_confirmed(self):
        mw, call, closed = await self.prepared()
        class UnknownStream:
            async def aclose(self):
                raise RuntimeError("stream still owns work")
        call.coordinator_stream = UnknownStream()
        call.native_done = True
        mw.request_done = True
        with self.assertRaisesRegex(RuntimeError, "still owns"):
            await call.close_coordinator()
        await mw.close_if_drained()
        self.assertEqual(closed, [])
        self.assertIsNotNone(mw.lease)
        self.assertTrue(aw_resident.cleanup_failed)
        call.coordinator_stream = None
        await mw.retire(call)
        self.assertEqual(closed, [True])

    async def test_private_lease_finalizer_failure_fences_worker(self):
        mw, call, _ = await self.prepared()
        call.native_done = True
        await mw.retire(call)
        original = mw.lease_context
        class FailingContext:
            async def __aexit__(self, *args):
                await original.__aexit__(*args)
                raise RuntimeError("private cleanup failed")
        mw.lease_context = FailingContext()
        mw.request_done = True
        with self.assertRaisesRegex(RuntimeError, "private cleanup"):
            await mw.close_if_drained()
        self.assertIsNone(mw.lease)
        self.assertTrue(aw_resident.cleanup_failed)

    async def test_timeout_retains_credentials_until_original_task_exits(self):
        mw, call, closed = await self.prepared()
        entered, release = asyncio.Event(), asyncio.Event()
        async def original():
            entered.set()
            while not release.is_set():
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    pass
            self.assertEqual(mw.lease.secrets["TASK_KEY"], "fixture-secret")
        task = asyncio.create_task(original())
        await asyncio.wait_for(entered.wait(), 1)
        call.entry = SimpleNamespace(background_task=task)
        call.native_done = True
        mw.request_done = True
        mw.failed = True
        try:
            with self.assertRaisesRegex(ResidentFailure, "deadline"):
                await aw_resident.settle(mw.drain_foreground(), timeout=0.05)
            await mw.close_if_drained()
            self.assertFalse(task.done())
            self.assertEqual(closed, [])
            self.assertIsNotNone(mw.lease)
            self.assertTrue(aw_resident.cleanup_failed)
        finally:
            release.set()
            await asyncio.wait_for(task, 1)
            await mw.drain_foreground()
            await mw.close_if_drained()
        self.assertEqual(closed, [True])
        self.assertIsNone(mw.lease)

    async def test_nested_settle_uses_one_deadline_including_cancellation_join(self):
        release = asyncio.Event()
        async def resistant():
            while not release.is_set():
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    pass
        task = asyncio.create_task(resistant())
        async def nested():
            await aw_resident.settle(task)
        started = asyncio.get_running_loop().time()
        try:
            with self.assertRaisesRegex(ResidentFailure, "deadline"):
                await aw_resident.settle(nested(), timeout=0.05)
            self.assertLess(asyncio.get_running_loop().time() - started, 0.2)
            self.assertFalse(task.done())
            self.assertIn(task, aw_resident.pending_cleanup)
        finally:
            release.set()
            await asyncio.wait_for(task, 1)
            pending = list(aw_resident.pending_cleanup)
            if pending:
                await asyncio.wait_for(asyncio.gather(*pending, return_exceptions=True), 1)

    async def test_settle_completes_within_budget_without_process_fence(self):
        async def complete():
            await asyncio.sleep(0.01)
            return "finished"
        self.assertEqual(await aw_resident.settle(complete(), timeout=0.1), "finished")
        self.assertFalse(aw_resident.cleanup_failed)


if __name__ == "__main__":
    unittest.main()
