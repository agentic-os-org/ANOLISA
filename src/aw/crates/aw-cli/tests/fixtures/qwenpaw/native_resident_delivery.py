"""Pinned HTTP output and next-request hint publication after background drain."""

from native_resident_support import *
from fastapi import HTTPException
import qwenpaw

# Load the installed HTTP router itself without unrelated channel plugin imports.
ROUTER_SPEC = importlib.util.spec_from_file_location(
    "aw_native_output_router", Path(qwenpaw.__file__).parent / "app/routers/tool_calls.py")
ROUTER = importlib.util.module_from_spec(ROUTER_SPEC)
sys.modules[ROUTER_SPEC.name] = ROUTER
ROUTER_SPEC.loader.exec_module(ROUTER)
get_output = ROUTER.get_output


class DeliveryResidentTests(NativeFixture):
    async def visible_output(self, coordinator, identifier="native"):
        request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(
            app_services=SimpleNamespace(tool_coordinator=coordinator))))
        try:
            return await get_output("session", identifier, request)
        except HTTPException as error:
            self.assertEqual(error.status_code, 404)
            return None

    async def next_agent_hints(self, coordinator):
        agent = SimpleNamespace(state=SimpleNamespace(context=[]),
                                _request_context={"session_id": "session", "tool_coordinator": coordinator})
        agent._get_tool_coordinator = MethodType(QwenPawAgent._get_tool_coordinator, agent)
        await QwenPawAgent._inject_pending_hints(agent)
        return agent.state.context

    async def background_delivery(self, stage, fail, late=False):
        release, entered, finish = (asyncio.Event() for _ in range(3))
        async def tool(tool_call):
            await release.wait()
            yield response("unverified background output")
        async def processor(result):
            if stage == "processor":
                entered.set()
                await finish.wait()
                if fail:
                    raise RuntimeError("processor rejected output")
            return response("verified background output")
        async def completed(entry):
            public = coordinator.get(entry.ctx.tool_call_id)
            self.assertIsNot(entry, public)
            self.assertIs(entry.ctx, public.ctx)
            self.assertIs(entry.background_task, public.background_task)
            self.assertIs(entry.stream, public.stream)
            self.assertTrue(entry.background_task.done())
            self.assertEqual(public.final_response.content, [])
            if stage == "completion":
                entered.set()
                await finish.wait()
                if fail:
                    raise RuntimeError("completion rejected output")
        coordinator = ToolCoordinator(default_timeout_secs=0.02, offload_on_deadline=True)
        coordinator.on_completion(completed)
        if late:
            # A deadline can win after the native tool finishes, before the
            # foreground task consumes the closed-stream notification.
            release.set()
            original_next = coordinator._await_next_event
            async def completed_before_deadline(entry, *, chunk_queue):
                if chunk_queue is not None:
                    await entry.background_task
                    return SimpleNamespace(type="deadline_reached")
                return await original_next(entry, chunk_queue=chunk_queue)
            coordinator._await_next_event = completed_before_deadline
        mw, agent, _ = self.setup_agent(tool, coordinator=coordinator)
        agent._acting_middlewares[1]._background_result_processor = processor
        try:
            results = await asyncio.wait_for(self.request(mw, agent, [self.call()]), 2)
            self.assertTrue(results[0].metadata["offloaded"])
            release.set()
            await asyncio.wait_for(entered.wait(), 2)
            visible = await self.visible_output(coordinator)
            self.assertIsNotNone(visible)
            self.assertEqual(visible["content"], [])
            self.assertEqual(coordinator._completed_cache, {})
            self.assertEqual(await self.next_agent_hints(coordinator), [])
            self.assertFalse(mw.scope.closed)
            finish.set()
            await asyncio.wait_for(coordinator.shutdown(), 2)
            visible = await self.visible_output(coordinator)
            hints = await self.next_agent_hints(coordinator)
            if fail:
                self.assertIsNone(visible)
                self.assertEqual(hints, [])
                self.assertTrue(mw.failed)
            else:
                self.assertEqual(visible["content"][0]["text"], "verified background output")
                self.assertEqual(len(hints), 1)
                self.assertIn("verified background output", str(hints))
                self.assertNotIn("unverified background output", str(hints))
                self.assertFalse(mw.failed)
            self.assertEqual(mw.active, {})
            self.assertTrue(mw.scope.closed)
        finally:
            release.set()
            finish.set()
            await coordinator.shutdown()

    async def offloaded_callback_delivery(self, fail):
        entered, release, tool_release, completed = (asyncio.Event() for _ in range(4))
        async def tool(tool_call):
            await tool_release.wait()
            yield response("verified offloaded output")
        async def offloaded(entry):
            entered.set()
            await release.wait()
            if fail:
                raise RuntimeError("offloaded callback rejected output")
        async def completion(entry):
            completed.set()
        coordinator = ToolCoordinator(default_timeout_secs=0.02, offload_on_deadline=True)
        coordinator.on_offloaded(offloaded)
        coordinator.on_completion(completion)
        mw, agent, _ = self.setup_agent(tool, coordinator=coordinator)
        request = asyncio.create_task(self.request(mw, agent, [self.call()]))
        try:
            await asyncio.wait_for(entered.wait(), 2)
            tool_release.set()
            await asyncio.wait_for(completed.wait(), 2)
            self.assertFalse(request.done())
            visible = await self.visible_output(coordinator)
            self.assertIsNotNone(visible)
            self.assertEqual(visible["content"], [])
            self.assertEqual(coordinator._completed_cache, {})
            self.assertEqual(await self.next_agent_hints(coordinator), [])
            self.assertFalse(mw.scope.closed)
            release.set()
            if fail:
                with self.assertRaises(ResidentFailure):
                    await asyncio.wait_for(request, 2)
            else:
                results = await asyncio.wait_for(request, 2)
                self.assertTrue(results[0].metadata["offloaded"])
            await asyncio.wait_for(coordinator.shutdown(), 2)
            visible = await self.visible_output(coordinator)
            hints = await self.next_agent_hints(coordinator)
            if fail:
                self.assertIsNone(visible)
                self.assertEqual(hints, [])
                self.assertTrue(mw.failed)
            else:
                self.assertEqual(visible["content"][0]["text"], "verified offloaded output")
                self.assertEqual(len(hints), 1)
                self.assertIn("verified offloaded output", str(hints))
                self.assertFalse(mw.failed)
            self.assertEqual(mw.active, {})
            self.assertTrue(mw.scope.closed)
        finally:
            tool_release.set()
            release.set()
            await asyncio.gather(request, return_exceptions=True)
            await coordinator.shutdown()

    async def test_offloaded_callback_failure_never_publishes_http_or_hints(self):
        await self.offloaded_callback_delivery(True)

    async def test_offloaded_callback_commits_only_after_success(self):
        await self.offloaded_callback_delivery(False)

    async def test_rejected_foreground_finalization_reclaims_private_result(self):
        import inspect
        from aw_resident_test.aw_resident import current_call
        async def tool(tool_call):
            yield response("private result awaiting finalization")
        coordinator = ToolCoordinator()
        async def reject_after_native_drain(entry, *, chunk_queue):
            await entry.background_task
            pending = inspect.getclosurevars(coordinator.execute.__func__).nonlocals["pending_results"]
            call = current_call.get()
            self.assertTrue(call.native_done)
            self.assertIn(call.identity, pending)
            self.assertEqual(pending[call.identity][0].content[0].text,
                             "private result awaiting finalization")
            call.owner.failed = True
            return SimpleNamespace(type="stream_closed")
        coordinator._await_next_event = reject_after_native_drain
        mw, agent, _ = self.setup_agent(tool, coordinator=coordinator)
        with self.assertRaises(ResidentFailure):
            await self.request(mw, agent, [self.call()])
        pending = inspect.getclosurevars(coordinator.execute.__func__).nonlocals["pending_results"]
        self.assertEqual(pending, {})
        self.assertEqual(coordinator._completed_cache, {})
        self.assertEqual(await self.next_agent_hints(coordinator), [])
        self.assertEqual(mw.active, {})
        self.assertTrue(mw.scope.closed)
        await coordinator.shutdown()

    async def test_bound_shutdown_shares_deadline_with_original_native_shutdown(self):
        from aw_resident_test import aw_native, aw_resident
        release, entered = asyncio.Event(), asyncio.Event()
        async def resistant():
            entered.set()
            while not release.is_set():
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    pass
        coordinator = ToolCoordinator(cancel_grace_period_secs=5)
        bind_coordinator(coordinator)
        entry = coordinator._create_entry(self.call(), "session", "agent", "session", None, "agent")
        task = asyncio.create_task(resistant())
        entry.background_task = task
        coordinator._entries[entry.ctx.tool_call_id] = entry
        await asyncio.wait_for(entered.wait(), 1)
        budgets = []
        async def short_budget(operation, timeout=None):
            budgets.append(timeout)
            return await aw_resident.settle(operation, timeout=0.05)
        retained_before = set(aw_resident.pending_cleanup)
        started = asyncio.get_running_loop().time()
        try:
            with patch.object(aw_resident, "cleanup_failed", False):
                with patch.object(aw_native, "settle", short_budget):
                    with self.assertRaisesRegex(ResidentFailure, "deadline"):
                        await coordinator.shutdown()
                self.assertEqual(budgets, [10])
                self.assertLess(asyncio.get_running_loop().time() - started, 0.2)
                self.assertTrue(entry.ctx.cancel_event.is_set())
                self.assertFalse(task.done())
                self.assertTrue(aw_resident.cleanup_failed)
                self.assertTrue(set(aw_resident.pending_cleanup) - retained_before)
        finally:
            release.set()
            await asyncio.wait_for(task, 1)
            retained = list(set(aw_resident.pending_cleanup) - retained_before)
            if retained:
                await asyncio.wait_for(asyncio.gather(*retained, return_exceptions=True), 1)
            coordinator._entries.pop(entry.ctx.tool_call_id, None)

    async def test_background_processor_failure_never_publishes_http_or_hints(self):
        await self.background_delivery("processor", True)

    async def test_completion_failure_never_publishes_http_or_hints(self):
        await self.background_delivery("completion", True)

    async def test_background_processor_commits_only_after_success(self):
        await self.background_delivery("processor", False)

    async def test_completion_commits_only_after_success(self):
        await self.background_delivery("completion", False)

    async def test_already_completed_tool_is_private_before_late_offload(self):
        await self.background_delivery("processor", True, late=True)


if __name__ == "__main__":
    unittest.main()
