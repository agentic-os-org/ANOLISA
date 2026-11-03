"""Pinned Coordinator lifecycle regressions without a hosted model."""

from native_resident_support import *


class NativeResidentTests(NativeFixture):
    async def test_native_finalizer_precedes_delivery_and_scope_release(self):
        order = []
        async def tool(tool_call):
            try:
                yield response()
            finally:
                await asyncio.sleep(0)
                order.append("closed")
        mw, agent, coordinator = self.setup_agent(tool)
        results = await self.request(mw, agent, [self.call()])
        self.assertEqual(results[0].content[0].text, "native result")
        self.assertEqual(order, ["closed"])
        self.assertEqual(mw.active, {})
        self.assertTrue(mw.scope.closed)
        self.assertEqual(coordinator._entries, {})

    async def test_sequential_native_calls_exceed_historical_capacity(self):
        async def tool(tool_call):
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        result = await self.request(mw, agent, [self.call(str(i)) for i in range(1025)])
        self.assertEqual(len(result), 1025)
        self.assertEqual(mw.sequence, 1025)
        self.assertEqual(mw.active, {})
        self.assertTrue(mw.scope.closed)

    async def test_private_environment_and_redaction_precede_hooks(self):
        cleaned, redactions = [], []
        def redact(value):
            result = json.loads(json.dumps(value).replace("secret-value", "[redacted]"))
            self.assertEqual(json.loads(json.dumps(result).replace("secret-value", "[redacted]")), result)
            redactions.append(result)
            return result
        @asynccontextmanager
        async def private():
            lease = PrivateExecution({}, {}, redact, {"TASK_KEY": "secret-value"})
            try:
                yield lease
            finally:
                cleaned.append(True)
        async def tool(tool_call):
            self.assertEqual(execution_environment()["TASK_KEY"], "secret-value")
            self.assertNotIn("TASK_KEY", os.environ)
            yield response("secret-value")
        hooks = [{"event": "tool.after", "step": "observe", "on_error": "withhold_result", "budget_ms": 5000}]
        mw, agent, _ = self.setup_agent(tool, hooks=hooks, context=SimpleNamespace(aw_execution_factory=private, aw_environment_version=1))
        result = await self.request(mw, agent, [self.call()])
        self.assertEqual(result[0].content[0].text, "[redacted]")
        self.assertNotIn("secret-value", json.dumps(mw.scope.callbacks))
        self.assertEqual(cleaned, [True])
        self.assertIsNone(mw.lease)
        self.assertGreater(len(redactions), 1)

    async def test_large_integer_mutation_by_native_hook_fails_before_tool(self):
        entered = []
        async def tool(tool_call):
            entered.append(True)
            yield response()
        coordinator = ToolCoordinator()
        native_call = self.call(value=2**53 + 1)
        async def before(arguments, context):
            native_call.input = json.dumps({"value": 2**53})
        coordinator.hooks.register("tool", before=before)
        mw, agent, _ = self.setup_agent(tool, coordinator=coordinator)
        with self.assertRaises(ResidentFailure):
            await self.request(mw, agent, [native_call])
        self.assertFalse(entered)
        self.assertTrue(mw.failed)

    async def test_large_integer_is_preserved_on_the_verified_native_path(self):
        values = []
        async def tool(tool_call):
            values.append(json.loads(tool_call.input)["value"])
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        await self.request(mw, agent, [self.call(value=2**53 + 1)])
        self.assertEqual(values, [2**53 + 1])

    async def test_strict_post_failure_stops_delivery_and_next_tool(self):
        entered = []
        async def tool(tool_call):
            entered.append(tool_call.id)
            yield response("unverified")
        hooks = [{"event": "tool.after", "step": "strict", "on_error": "withhold_result", "budget_ms": 5000}]
        mw, agent, _ = self.setup_agent(tool, hooks=hooks)
        async def invoke(hook, payload):
            return 1, b""
        mw.scope.invoke = invoke
        with self.assertRaises(Exception):
            await self.request(mw, agent, [self.call("one"), self.call("two")])
        self.assertEqual(entered, ["one"])
        self.assertTrue(mw.failed)
        self.assertTrue(mw.scope.closed)

    async def test_offload_retains_scope_through_completion_callbacks(self):
        entered, release, callback_entered, callback_release = [asyncio.Event() for _ in range(4)]
        async def tool(tool_call):
            entered.set()
            await release.wait()
            yield response()
        coordinator = ToolCoordinator(default_timeout_secs=0.02, offload_on_deadline=True)
        async def completion(entry):
            callback_entered.set()
            await callback_release.wait()
        coordinator.on_completion(completion)
        mw, agent, _ = self.setup_agent(tool, coordinator=coordinator)
        try:
            result = await asyncio.wait_for(self.request(mw, agent, [self.call()]), 2)
            self.assertTrue(result[0].metadata["offloaded"])
            self.assertFalse(mw.scope.closed)
            self.assertEqual(len(mw.active), 1)
            await agent.close()
            self.assertFalse(mw.scope.closed)
            release.set()
            await asyncio.wait_for(callback_entered.wait(), 2)
            await agent.close()
            self.assertFalse(mw.scope.closed)
            callback_release.set()
            await asyncio.wait_for(coordinator.shutdown(), 2)
            self.assertTrue(mw.scope.closed)
            self.assertEqual(mw.active, {})
        finally:
            release.set()
            callback_release.set()
            await coordinator.shutdown()

    async def test_native_cleanup_failure_never_delivers_original_result(self):
        async def tool(tool_call):
            yield response("unverified")
            raise RuntimeError("native finalizer failed")
        mw, agent, _ = self.setup_agent(tool)
        with self.assertRaises(Exception):
            await self.request(mw, agent, [self.call()])
        self.assertTrue(mw.failed)
        self.assertTrue(mw.scope.closed)

    async def test_removed_middleware_fails_before_dispatch(self):
        async def tool(tool_call):
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        agent._acting_middlewares.remove(mw)
        with self.assertRaisesRegex(ResidentFailure, "removed"):
            await self.request(mw, agent, [self.call()])
        self.assertTrue(mw.scope.closed)

    async def test_cancelled_native_after_hook_is_joined_before_release(self):
        hook_entered, release = asyncio.Event(), asyncio.Event()
        closed = []
        async def tool(tool_call):
            yield response()
        async def after(result, context):
            hook_entered.set()
            try:
                await release.wait()
            finally:
                closed.append(True)
            return result
        coordinator = ToolCoordinator()
        coordinator.hooks.register("tool", after=after)
        mw, agent, _ = self.setup_agent(tool, coordinator=coordinator)
        task = asyncio.create_task(self.request(mw, agent, [self.call()]))
        try:
            await asyncio.wait_for(hook_entered.wait(), 2)
            task.cancel()
            await asyncio.sleep(0.02)
            self.assertFalse(mw.scope.closed)
            self.assertEqual(closed, [])
            release.set()
            with self.assertRaises(BaseException):
                await asyncio.wait_for(task, 2)
            self.assertEqual(closed, [True])
            self.assertTrue(mw.scope.closed)
            self.assertEqual(mw.active, {})
        finally:
            release.set()
            await asyncio.gather(task, return_exceptions=True)
            await coordinator.shutdown()

    async def test_active_saturation_recovers_after_native_drain(self):
        entered, release = asyncio.Event(), asyncio.Event()
        count = 0
        async def tool(tool_call):
            nonlocal count
            count += 1
            if count == 4:
                entered.set()
            await release.wait()
            yield response()
        mw, agent, coordinator = self.setup_agent(tool)
        async def consume(identifier):
            return [item async for item in Agent._acting(agent, self.call(identifier))]
        async def reply():
            tasks = [asyncio.create_task(consume(str(i))) for i in range(4)]
            try:
                await asyncio.wait_for(entered.wait(), 2)
                with self.assertRaisesRegex(ResidentFailure, "capacity"):
                    await consume("overflow")
                self.assertEqual(len(mw.active), 4)
                release.set()
                await asyncio.gather(*tasks)
                self.assertEqual(mw.active, {})
                for item in await consume("recovered"):
                    yield item
            finally:
                release.set()
                await asyncio.gather(*tasks, return_exceptions=True)
        result = [item async for item in mw.on_reply(agent, {}, reply)]
        self.assertEqual(len(result), 1)
        self.assertTrue(mw.scope.closed)
        self.assertFalse(mw.failed)
        await coordinator.shutdown()

    async def test_retired_native_callback_identity_cannot_replay(self):
        from aw_resident_test.aw_resident import current_call
        retained = []
        async def tool(tool_call):
            retained.append(current_call.get())
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        await self.request(mw, agent, [self.call()])
        with self.assertRaises(RuntimeError):
            retained[0].check()
        self.assertEqual(mw.active, {})

    async def test_strict_post_holds_streaming_chunks_until_verified(self):
        async def tool(tool_call):
            yield ToolChunk(content=[TextBlock(text="unverified chunk")], is_last=False)
            yield response()
        hooks = [{"event": "tool.after", "step": "strict", "on_error": "withhold_result", "budget_ms": 5000}]
        mw, agent, _ = self.setup_agent(tool, hooks=hooks)
        async def invoke(hook, payload):
            return 1, b""
        mw.scope.invoke = invoke
        delivered = []
        async def reply():
            async for item in Agent._acting(agent, self.call()):
                delivered.append(item)
                yield item
        with self.assertRaises(Exception):
            [item async for item in mw.on_reply(agent, {}, reply)]
        self.assertEqual(delivered, [])

    async def test_missing_credential_consumer_rejects_task_admission(self):
        async def tool(tool_call):
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(aw_execution_factory=lambda: None))
        with self.assertRaisesRegex(ResidentFailure, "environment extension"):
            await self.request(mw, agent, [self.call()])
        self.assertIsNone(mw.scope.environment)

    async def test_native_builder_rejects_swallowed_required_factory_failure(self):
        from qwenpaw.runtime.builder import AgentBuilder
        from qwenpaw.plugins.registry import PluginRegistry
        from aw_resident_test.aw_native import install_guard
        original_middlewares = AgentBuilder._build_middlewares
        original_build = AgentBuilder.build
        config = SimpleNamespace(running=SimpleNamespace(light_context_config=SimpleNamespace(
            visual_compact_config=SimpleNamespace(enabled=False, effort="medium"))))
        context = SimpleNamespace(app_services=SimpleNamespace(tool_coordinator=ToolCoordinator()))
        def failed(ctx, config):
            raise RuntimeError("required factory failed")
        try:
            install_guard()
            with patch.object(AgentBuilder, "_build_tool_result_pruning_middleware", return_value=None), patch.object(AgentBuilder, "_get_memory_manager", return_value=None), patch.object(PluginRegistry, "get_middleware_factories", return_value=[SimpleNamespace(factory=failed, plugin_id="aw-native")]):
                with self.assertRaisesRegex(ResidentFailure, "one installed"):
                    AgentBuilder._build_middlewares(context, config)
        finally:
            AgentBuilder._build_middlewares = staticmethod(original_middlewares)
            AgentBuilder.build = original_build
            del AgentBuilder._aw_resident_guard

    async def test_native_builder_installs_resident_before_coordinator(self):
        from qwenpaw.runtime.builder import AgentBuilder
        from qwenpaw.plugins.registry import PluginRegistry
        from aw_resident_test.aw_native import install_guard
        original_middlewares = AgentBuilder._build_middlewares
        original_build = AgentBuilder.build
        config = SimpleNamespace(running=SimpleNamespace(light_context_config=SimpleNamespace(
            visual_compact_config=SimpleNamespace(enabled=False, effort="medium"))))
        context = SimpleNamespace(app_services=SimpleNamespace(tool_coordinator=ToolCoordinator()))
        resident = ResidentMiddleware({"hooks": []}, PLUGIN.AwToolMiddleware, context)
        try:
            install_guard()
            with patch.object(AgentBuilder, "_build_tool_result_pruning_middleware", return_value=None), patch.object(AgentBuilder, "_get_memory_manager", return_value=None), patch.object(PluginRegistry, "get_middleware_factories", return_value=[SimpleNamespace(factory=lambda ctx, cfg: resident, plugin_id="aw-native")]):
                actual = AgentBuilder._build_middlewares(context, config)
                self.assertIs(actual[0], resident)
                self.assertIsInstance(actual[1], ToolCoordinatorMiddleware)
                self.assertTrue(actual[1]._coordinator._aw_resident_bound)
        finally:
            AgentBuilder._build_middlewares = staticmethod(original_middlewares)
            AgentBuilder.build = original_build
            del AgentBuilder._aw_resident_guard

    async def test_redactor_cannot_change_authorization_bearing_arguments(self):
        entered = []
        @asynccontextmanager
        async def private():
            yield PrivateExecution({}, {}, lambda value: {"value": "safe"} if value == {"value": "unsafe"} else value)
        async def tool(tool_call):
            entered.append(True)
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(aw_execution_factory=private, aw_environment_version=1))
        with self.assertRaisesRegex(ResidentFailure, "authorization-bearing"):
            await self.request(mw, agent, [self.call(value="unsafe")])
        self.assertEqual(entered, [])
        self.assertTrue(mw.scope.closed)
        self.assertEqual(mw.active, {})

    async def test_pretool_callback_cannot_change_later_native_dispatch(self):
        entered = []
        call = self.call(value=2**53 + 1)
        async def tool(tool_call):
            entered.append(True)
            yield response()
        hooks = [{"event": "tool.before", "step": "observe", "on_error": "block", "budget_ms": 5000}]
        mw, agent, _ = self.setup_agent(tool, hooks=hooks)
        async def invoke(hook, payload):
            call.input = json.dumps({"value": 2**53})
            return 0, b""
        mw.scope.invoke = invoke
        with self.assertRaises(ResidentFailure):
            await self.request(mw, agent, [call])
        self.assertEqual(entered, [])
        self.assertTrue(mw.scope.closed)

    async def test_strict_post_gates_native_subscribers_and_output_snapshot(self):
        entered, release, post_entered, post_release = [asyncio.Event() for _ in range(4)]
        async def tool(tool_call):
            entered.set()
            await release.wait()
            yield ToolChunk(content=[TextBlock(text="unverified")], is_last=False)
            yield response("unverified final")
        hooks = [{"event": "tool.after", "step": "strict", "on_error": "withhold_result", "budget_ms": 5000}]
        mw, agent, coordinator = self.setup_agent(tool, hooks=hooks)
        async def invoke(hook, payload):
            post_entered.set()
            await post_release.wait()
            return 1, b""
        mw.scope.invoke = invoke
        task = asyncio.create_task(self.request(mw, agent, [self.call()]))
        try:
            await asyncio.wait_for(entered.wait(), 2)
            entry = coordinator.get("native")
            subscriber = asyncio.Queue()
            entry.stream.add_subscriber(subscriber)
            release.set()
            await asyncio.wait_for(post_entered.wait(), 2)
            self.assertTrue(subscriber.empty())
            self.assertFalse(entry.final_response.content)
            post_release.set()
            with self.assertRaises(Exception):
                await asyncio.wait_for(task, 2)
            self.assertFalse(entry.final_response.content)
            while not subscriber.empty():
                self.assertNotIsInstance(subscriber.get_nowait(), ToolChunk)
            self.assertTrue(mw.scope.closed)
        finally:
            release.set()
            post_release.set()
            await asyncio.gather(task, return_exceptions=True)
            await coordinator.shutdown()

    async def test_inner_middleware_cannot_replace_authorized_tool_call(self):
        entered = []
        async def tool(tool_call):
            entered.append(json.loads(tool_call.input))
            yield response()
        class Rewrite:
            async def on_acting(self, agent, input_kwargs, next_handler):
                changed = input_kwargs["tool_call"].model_copy(deep=True)
                changed.input = json.dumps({"value": "unauthorized"})
                async for item in next_handler(tool_call=changed):
                    yield item
        mw, agent, _ = self.setup_agent(tool)
        agent._acting_middlewares.append(Rewrite())
        with self.assertRaises(ResidentFailure):
            await self.request(mw, agent, [self.call(value="authorized")])
        self.assertEqual(entered, [])
        self.assertTrue(mw.scope.closed)

    async def test_early_reply_close_drains_actual_tool_and_private_lease(self):
        entered, release = asyncio.Event(), asyncio.Event()
        finalized = []
        async def tool(tool_call):
            try:
                entered.set()
                yield ToolChunk(content=[TextBlock(text="partial")], is_last=False)
                await release.wait()
                yield response()
            finally:
                await asyncio.sleep(0)
                finalized.append(True)
        mw, agent, coordinator = self.setup_agent(tool)
        async def reply(**kwargs):
            async for value in Agent._acting(agent, self.call()):
                yield value
        agent._reply_impl = reply
        stream = agent.reply_stream()
        try:
            await asyncio.wait_for(anext(stream), 2)
            await asyncio.wait_for(stream.aclose(), 2)
            self.assertEqual(finalized, [True])
            self.assertTrue(mw.scope.closed)
            self.assertEqual(mw.active, {})
            self.assertEqual(coordinator._entries, {})
            await asyncio.wait_for(asyncio.get_running_loop().shutdown_asyncgens(), 2)
        finally:
            release.set()
            await stream.aclose()
            await coordinator.shutdown()

    async def test_native_agent_close_joins_suspended_and_pending_public_reply(self):
        for pending_read in (False, True):
            with self.subTest(pending_read=pending_read):
                release = asyncio.Event()
                finalized, errors = [], []
                loop = asyncio.get_running_loop()
                previous = loop.get_exception_handler()
                loop.set_exception_handler(lambda loop, context: errors.append(context))
                async def tool(tool_call):
                    try:
                        yield ToolChunk(content=[TextBlock(text="partial")], is_last=False)
                        await release.wait()
                        yield response()
                    finally:
                        finalized.append(True)
                mw, agent, coordinator = self.setup_agent(tool)
                async def reply(**kwargs):
                    async for value in Agent._acting(agent, self.call()):
                        yield value
                agent._reply_impl = reply
                stream = agent.reply_stream()
                pending = None
                try:
                    await asyncio.wait_for(anext(stream), 2)
                    if pending_read:
                        pending = asyncio.create_task(anext(stream))
                        await asyncio.sleep(0)
                    await asyncio.wait_for(agent.close(), 2)
                    self.assertEqual(finalized, [True])
                    self.assertTrue(mw.request_done)
                    self.assertTrue(mw.scope.closed)
                    self.assertEqual(mw.active, {})
                    self.assertEqual(coordinator._entries, {})
                    if pending is not None:
                        self.assertTrue(pending.done())
                    await stream.aclose()
                    await asyncio.sleep(0.05)
                    self.assertEqual(errors, [])
                finally:
                    release.set()
                    if pending is not None:
                        await asyncio.gather(pending, return_exceptions=True)
                    await stream.aclose()
                    await coordinator.shutdown()
                    loop.set_exception_handler(previous)

    async def test_background_callback_exceptions_are_private_and_fence_owner(self):
        import logging
        from io import StringIO
        for event in ("completion", "offloaded"):
            for register_after_binding in (False, True):
                with self.subTest(event=event, register_after_binding=register_after_binding):
                    output, release = StringIO(), asyncio.Event()
                    handler = logging.StreamHandler(output)
                    logger = logging.getLogger("qwenpaw.tool_calls._coordinator")
                    logger.addHandler(handler)
                    secret = "synthetic-callback-private-value"
                    @asynccontextmanager
                    async def private():
                        yield PrivateExecution({}, {}, lambda value: json.loads(
                            json.dumps(value).replace(secret, "[redacted]")), {"TOKEN": secret})
                    async def tool(tool_call):
                        await release.wait()
                        yield response()
                    async def callback(entry):
                        raise RuntimeError("callback failed " + secret)
                    coordinator = ToolCoordinator(default_timeout_secs=0.01, offload_on_deadline=True)
                    if not register_after_binding:
                        getattr(coordinator, "on_" + event)(callback)
                    mw, agent, coordinator = self.setup_agent(tool, coordinator=coordinator,
                        context=SimpleNamespace(aw_execution_factory=private, aw_environment_version=1))
                    if register_after_binding:
                        getattr(coordinator, "on_" + event)(callback)
                    try:
                        if event == "offloaded":
                            with self.assertRaises(ResidentFailure):
                                await self.request(mw, agent, [self.call()])
                        else:
                            await self.request(mw, agent, [self.call()])
                        release.set()
                        await coordinator.shutdown()
                        self.assertTrue(mw.failed)
                        self.assertTrue(mw.scope.closed)
                        self.assertEqual(mw.active, {})
                        self.assertNotIn(secret, output.getvalue())
                    finally:
                        release.set()
                        await coordinator.shutdown()
                        logger.removeHandler(handler)

    async def test_private_exception_text_never_reaches_native_logs(self):
        import logging
        from io import StringIO
        output = StringIO()
        handler = logging.StreamHandler(output)
        logger = logging.getLogger("qwenpaw.tool_calls._coordinator")
        logger.addHandler(handler)
        secret = "synthetic-sensitive-value"
        async def tool(tool_call):
            raise RuntimeError("failed token=" + secret)
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        try:
            with self.assertRaises(ResidentFailure):
                await self.request(mw, agent, [self.call()])
            self.assertNotIn(secret, output.getvalue())
            self.assertTrue(mw.scope.closed)
        finally:
            logger.removeHandler(handler)


from native_resident_review import ReviewResidentTests
from native_resident_delivery import DeliveryResidentTests
from native_resident_cleanup import CleanupResidentTests
from native_private import PrivateResidentTests
from native_resident_reload import ReloadResidentTests, ScopePolicyTests


if __name__ == "__main__":
    unittest.main()
