"""Native parsing, Hook policies, bounded cleanup and private streaming regressions."""

from native_resident_support import *
from agentscope.tool import FunctionTool, Toolkit, _toolkit
from agentscope._utils._common import _json_loads_with_repair
from aw_resident_test import aw_resident


class ReviewResidentTests(NativeFixture):
    async def test_incomplete_native_assembly_rejects_before_model_or_tool(self):
        for name in ("_aw_managed_acting_impl", "_aw_managed_parser",
                     "_aw_managed_toolkit", "_aw_managed_toolkit_call"):
            with self.subTest(missing=name):
                entered = []
                async def tool(tool_call):
                    entered.append("tool")
                    yield response()
                middleware, agent, _ = self.setup_agent(tool)
                delattr(agent, name)
                async def reply():
                    entered.append("model")
                    async for item in Agent._acting(agent, self.call()):
                        yield item
                with self.assertRaisesRegex(ResidentFailure, "assembly is incomplete"):
                    [item async for item in middleware.on_reply(agent, {}, reply)]
                self.assertEqual(entered, [])
                self.assertTrue(middleware.failed)
                self.assertTrue(middleware.scope.closed)
                self.assertEqual(middleware.active, {})

    async def toolkit_agent(self, schema, *, hooks=None, coordinator=None):
        seen = []
        async def function(**kwargs):
            seen.append(kwargs)
            return ToolChunk(content=[TextBlock(text="result")])
        function_tool = FunctionTool(function, name="tool", input_schema=schema)
        toolkit = Toolkit(tools=[function_tool])
        async def dispatch(tool_call):
            async for item in toolkit.call_tool(tool_call, agent.state):
                yield item
        mw, agent, coordinator = self.setup_agent(dispatch, hooks=hooks, coordinator=coordinator, toolkit=toolkit)
        return mw, agent, coordinator, seen, function_tool

    async def test_native_repair_registers_the_effective_tool_arguments(self):
        cases = [("", {}, {}), (" \n\t", {}, {}),
                 ('{"value": "42"}', {"value": {"type": "integer"}}, {"value": 42}),
                 ("{value: 42}", {"value": {"type": "integer"}}, {"value": 42})]
        for raw, properties, expected in cases:
            with self.subTest(raw=raw):
                mw, agent, _, seen, _ = await self.toolkit_agent({"type": "object", "properties": properties})
                registered = []
                original = mw.scope.register
                async def register(sequence, identity, name, arguments):
                    registered.append(arguments)
                    await original(sequence, identity, name, arguments)
                mw.scope.register = register
                await self.request(mw, agent, [ToolCallBlock(id="native", name="tool", input=raw)])
                self.assertEqual(registered, [expected])
                self.assertEqual(seen, [expected])
                self.assertTrue(mw.scope.closed)

    async def test_native_reparse_cannot_change_registered_argument_types(self):
        loop = asyncio.get_running_loop()
        errors = []
        previous = loop.get_exception_handler()
        loop.set_exception_handler(lambda _, context: errors.append(context))
        try:
            for mutation in ("schema", "toolkit", "call_tool", "parser"):
                with self.subTest(mutation=mutation):
                    hooks = [{"event": "tool.before", "step": "check", "on_error": "block", "budget_ms": 1000}]
                    schema = {"type": "object", "properties": {"value": {"type": "integer"}}}
                    mw, agent, _, seen, tool = await self.toolkit_agent(schema, hooks=hooks)
                    async def invoke(hook, payload):
                        tool.input_schema["properties"]["value"] = {"type": "string"}
                        if mutation == "toolkit":
                            agent.toolkit = Toolkit(tools=[tool])
                        elif mutation == "call_tool":
                            agent.toolkit.call_tool = MethodType(Toolkit.call_tool, agent.toolkit)
                        elif mutation == "parser":
                            _toolkit._json_loads_with_repair = _json_loads_with_repair
                        return 0, b""
                    mw.scope.invoke = invoke
                    parser = _toolkit._json_loads_with_repair
                    try:
                        with self.assertRaises(ResidentFailure):
                            await self.request(mw, agent, [self.call()])
                    finally:
                        _toolkit._json_loads_with_repair = parser
                    self.assertEqual(seen, [])
                    self.assertTrue(mw.failed)
                    self.assertTrue(mw.scope.closed)
        finally:
            try:
                await asyncio.wait_for(loop.shutdown_asyncgens(), 1)
                await asyncio.sleep(0)
                self.assertEqual(errors, [])
            finally:
                loop.set_exception_handler(previous)

    async def test_nested_native_toolkit_keeps_its_own_arguments(self):
        for shared in (False, True):
            with self.subTest(shared_toolkit_and_native_id=shared):
                inner_calls, outer_calls = [], []
                async def inner(value):
                    inner_calls.append(value)
                    return ToolChunk(content=[TextBlock(text="inner")])
                inner_tool = FunctionTool(inner, name="inner")
                inner_toolkit = Toolkit(tools=[inner_tool])
                async def outer(value):
                    outer_calls.append(value)
                    inner_call = ToolCallBlock(id="native" if shared else "inner", name="inner", input='{"value": 2}')
                    async for _ in inner_toolkit.call_tool(inner_call, agent.state):
                        pass
                    return ToolChunk(content=[TextBlock(text="outer")])
                toolkit = Toolkit(tools=[FunctionTool(outer, name="tool"), inner_tool])
                if shared:
                    inner_toolkit = toolkit
                async def dispatch(tool_call):
                    async for item in toolkit.call_tool(tool_call, agent.state):
                        yield item
                mw, agent, _ = self.setup_agent(dispatch, toolkit=toolkit)
                result = await self.request(mw, agent, [self.call(value=1)])
                self.assertEqual(outer_calls, [1])
                self.assertEqual(inner_calls, [2])
                self.assertEqual(result[0].content[0].text, "outer")
                self.assertEqual(mw.sequence, 1)
                self.assertFalse(mw.failed)
                self.assertTrue(mw.scope.closed)

    async def test_typed_scope_transport_failures_follow_hook_policy(self):
        for event, action in [("tool.before", "report"), ("tool.before", "block"),
                              ("tool.after", "report"), ("tool.after", "withhold_result")]:
            with self.subTest(event=event, action=action):
                hook = {"event": event, "step": "check", "on_error": action, "budget_ms": 1000}
                middleware = PLUGIN.AwToolMiddleware({}, hook)
                async def invoke(*_):
                    raise SCOPE.ScopeTransportFailure("fixed size failure")
                middleware.scope = SimpleNamespace(invoke_native=invoke)
                if action == "withhold_result":
                    with self.assertRaises(PLUGIN.CallbackFailure):
                        await middleware._run({})
                elif action == "block":
                    self.assertEqual((await middleware._run({})).state.value, "denied")
                else:
                    self.assertIsNone(await middleware._run({}))
                async def unavailable(*_):
                    raise RuntimeError("scope closed")
                middleware.scope.invoke_native = unavailable
                with self.assertRaisesRegex(RuntimeError, "scope closed"):
                    await middleware._run({})
        with self.assertRaises(SCOPE.ScopeTransportFailure):
            await SCOPE.command({}, "invoke", {"input": "x" * 1024 * 1024}, 1)

    async def test_real_scope_response_limit_uses_typed_transport_failure(self):
        with tempfile.TemporaryDirectory(prefix="scope-output-", dir=OUTPUT) as root:
            executable = Path(root) / "oversized"
            executable.write_text("#!" + sys.executable + "\nimport sys\nsys.stdout.write('x' * (1024 * 1024 + 1))\n")
            executable.chmod(0o700)
            with self.assertRaises(SCOPE.ScopeTransportFailure):
                await asyncio.wait_for(SCOPE.command({"executable": str(executable), "binding": "unused"}, "invoke", {}, 1), 2)

    async def test_native_user_hook_failure_poisoning_preserves_cleanup(self):
        for phase in ("before", "after"):
            with self.subTest(phase=phase):
                seen = []
                async def tool(tool_call):
                    seen.append(True)
                    yield response()
                async def broken(*args):
                    raise ValueError("private hook detail")
                coordinator = ToolCoordinator()
                coordinator.hooks.register("tool", **{phase: broken})
                mw, agent, _ = self.setup_agent(tool, coordinator=coordinator)
                with self.assertRaises(ResidentFailure):
                    await self.request(mw, agent, [self.call()])
                self.assertEqual(bool(seen), phase == "after")
                self.assertTrue(mw.failed)
                self.assertTrue(mw.scope.closed)

    async def test_cleanup_timeout_fences_worker_and_retains_original_task(self):
        entered, cancelled, release = [asyncio.Event() for _ in range(3)]
        async def resistant():
            entered.set()
            try:
                await release.wait()
            except asyncio.CancelledError:
                cancelled.set()
                await release.wait()
            raise ValueError("late private cleanup failure")
        task = asyncio.create_task(resistant())
        await entered.wait()
        async def tool(tool_call):
            yield response()
        existing, agent, _ = self.setup_agent(tool)
        existing.agent = agent
        existing.scope.environment = {}
        with patch.object(aw_resident, "cleanup_failed", False):
            try:
                with patch.object(aw_resident, "CLEANUP_TIMEOUT", 0.02), patch.object(aw_resident, "CANCEL_TIMEOUT", 0.02):
                    with self.assertRaisesRegex(ResidentFailure, "restart required"):
                        await asyncio.wait_for(aw_resident.settle(task), 0.5)
                self.assertTrue(cancelled.is_set())
                self.assertIn(task, aw_resident.pending_cleanup)
                self.assertFalse(task.done())
                with self.assertRaisesRegex(ResidentFailure, "restart required"):
                    existing.check()
                new, new_agent, _ = self.setup_agent(tool)
                with self.assertRaisesRegex(ResidentFailure, "restart required"):
                    await self.request(new, new_agent, [self.call()])
                self.assertFalse(new.opened)
                self.assertIsNone(new.lease_context)
                self.assertIsNone(new.scope.environment)
            finally:
                release.set()
                await asyncio.wait({task}, timeout=1)
                await asyncio.sleep(0)
                self.assertTrue(task.done())
                self.assertNotIn(task, aw_resident.pending_cleanup)
                self.assertFalse(task._log_traceback, "late exception was not retrieved")
                self.assertTrue(aw_resident.cleanup_failed)

    async def test_unselected_strict_post_keeps_public_native_streaming(self):
        for field in ("event_tools", "step_tools"):
            with self.subTest(selector=field):
                async def tool(tool_call):
                    yield ToolChunk(content=[TextBlock(text="chunk")], is_last=False)
                    yield response()
                hooks = [{"event": "tool.after", "step": "strict", "on_error": "withhold_result",
                          "budget_ms": 1000, field: ["native:qwenpaw:another"]}]
                mw, agent, _ = self.setup_agent(tool, hooks=hooks)
                output = await self.request(mw, agent, [self.call()])
                self.assertTrue(any(isinstance(item, ToolChunk) for item in output))
                self.assertEqual(len(mw.scope.callbacks), 1)

    async def test_private_report_mode_holds_split_secret_from_all_native_streams(self):
        entered, release = asyncio.Event(), asyncio.Event()
        after = []
        @asynccontextmanager
        async def private():
            yield PrivateExecution({}, {}, lambda value: json.loads(json.dumps(value).replace("split-secret", "[redacted]")), {"KEY": "split-secret"})
        async def tool(tool_call):
            entered.set()
            await release.wait()
            yield ToolChunk(content=[TextBlock(id="same", text="split-")], is_last=False)
            yield ToolChunk(content=[TextBlock(id="same", text="secret")], is_last=False)
            yield response("split-secret")
        async def native_after(result, context):
            after.append(result.model_dump(mode="json"))
        coordinator = ToolCoordinator()
        coordinator.hooks.register("tool", after=native_after)
        hooks = [{"event": "tool.after", "step": "observe", "on_error": "report", "budget_ms": 1000}]
        mw, agent, _ = self.setup_agent(tool, hooks=hooks, coordinator=coordinator,
                                        context=SimpleNamespace(aw_execution_factory=private, aw_environment_version=1))
        task = asyncio.create_task(self.request(mw, agent, [self.call()]))
        try:
            await asyncio.wait_for(entered.wait(), 1)
            entry = coordinator.get("native")
            subscriber = asyncio.Queue()
            entry.stream.add_subscriber(subscriber)
            release.set()
            results = await asyncio.wait_for(task, 2)
            self.assertEqual(len(results), 1)
            self.assertEqual(results[0].content[0].text, "[redacted]")
            self.assertNotIn("split-secret", json.dumps(after))
            self.assertNotIn("split-secret", json.dumps(mw.scope.callbacks))
            while not subscriber.empty():
                self.assertNotIsInstance(subscriber.get_nowait(), ToolChunk)
        finally:
            release.set()
            await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), 2)
            await coordinator.shutdown()
