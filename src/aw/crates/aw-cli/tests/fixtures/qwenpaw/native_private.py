"""Native request acquisition owns late credentials through cancellation drain."""

from native_resident_support import *
from aw_resident_test import aw_resident


class PrivateResidentTests(NativeFixture):
    def setUp(self):
        super().setUp()
        self.fence = patch.object(aw_resident, "cleanup_failed", False)
        self.fence.start()
        self.addCleanup(self.fence.stop)

    async def test_factory_failure_never_enters_native_tools(self):
        entered = []
        @asynccontextmanager
        async def private():
            raise RuntimeError("credential acquisition unavailable")
            yield
        async def tool(tool_call):
            entered.append(True)
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(
            aw_execution_factory=private, aw_environment_version=1))
        with self.assertRaisesRegex(RuntimeError, "acquisition"):
            await self.request(mw, agent, [self.call()])
        self.assertEqual(entered, [])
        self.assertTrue(mw.failed)
        self.assertIsNone(mw.lease)

    async def test_invalid_hook_view_releases_before_native_dispatch(self):
        executed, released = [], []
        @asynccontextmanager
        async def private():
            try:
                yield PrivateExecution({}, {"SAFE": "fixture-secret"}, lambda value: value,
                                       {"TASK_KEY": "fixture-secret"})
            finally:
                released.append(True)
        async def tool(tool_call):
            executed.append(True)
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(
            aw_execution_factory=private, aw_environment_version=1))
        with self.assertRaises(Exception):
            await self.request(mw, agent, [self.call()])
        self.assertEqual(executed, [])
        self.assertEqual(released, [True])
        self.assertIsNone(mw.lease)

    async def test_cancelled_acquisition_releases_after_original_factory_finishes(self):
        entered, finish = asyncio.Event(), asyncio.Event()
        executed, released = [], []
        @asynccontextmanager
        async def private():
            entered.set()
            await finish.wait()
            try:
                yield PrivateExecution({}, {}, lambda value: value, {"TASK_KEY": "fixture-secret"})
            finally:
                released.append(True)
        async def tool(tool_call):
            executed.append(True)
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(
            aw_execution_factory=private, aw_environment_version=1))
        task = asyncio.create_task(self.request(mw, agent, [self.call()]))
        try:
            await asyncio.wait_for(entered.wait(), 1)
            task.cancel()
            await asyncio.sleep(0)
            self.assertFalse(task.done())
            self.assertEqual(released, [])
            finish.set()
            with self.assertRaises(asyncio.CancelledError):
                await asyncio.wait_for(task, 1)
            self.assertEqual(executed, [])
            self.assertEqual(released, [True])
            self.assertIsNone(mw.lease)
        finally:
            finish.set()
            await asyncio.gather(task, return_exceptions=True)

    async def test_acquisition_timeout_retains_and_releases_late_lease(self):
        entered, finish = asyncio.Event(), asyncio.Event()
        executed, released = [], []
        @asynccontextmanager
        async def private():
            entered.set()
            while not finish.is_set():
                try:
                    await finish.wait()
                except asyncio.CancelledError:
                    pass
            try:
                yield PrivateExecution({}, {}, lambda value: value, {"TASK_KEY": "fixture-secret"})
            finally:
                released.append(True)
        async def tool(tool_call):
            executed.append(True)
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(
            aw_execution_factory=private, aw_environment_version=1))
        task = asyncio.create_task(self.request(mw, agent, [self.call()]))
        try:
            with patch.object(aw_resident, "CLEANUP_TIMEOUT", 0.05):
                await asyncio.wait_for(entered.wait(), 1)
                with self.assertRaisesRegex(ResidentFailure, "deadline"):
                    await asyncio.wait_for(task, 1)
            self.assertEqual(released, [])
            self.assertTrue(aw_resident.cleanup_failed)
            retained = list(aw_resident.pending_cleanup)
            self.assertTrue(retained)
            finish.set()
            await asyncio.wait_for(asyncio.gather(*retained, return_exceptions=True), 1)
            self.assertEqual(released, [True])
            self.assertEqual(executed, [])
            self.assertIsNone(mw.lease)
        finally:
            finish.set()
            await asyncio.gather(task, *list(aw_resident.pending_cleanup), return_exceptions=True)
