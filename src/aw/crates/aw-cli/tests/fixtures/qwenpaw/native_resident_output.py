"""Checked foreground output reaches native Agent events exactly once."""

from native_resident_support import *
from agentscope.permission import PermissionBehavior, PermissionDecision


class OutputResidentTests(NativeFixture):
    async def event_request(self, middleware, agent, call, events, contexts):
        async def available(name, groups):
            return SimpleNamespace(input_schema={}, is_external_tool=False)
        async def allowed(*args):
            return PermissionDecision(behavior=PermissionBehavior.ALLOW, message="allowed")
        async def compress(result):
            return result, None
        agent.state.reply_id = "reply"
        agent.toolkit.check_tool_available = available
        agent._check_permission = allowed
        agent._update_tool_call_state = lambda *args: None
        agent._acting = MethodType(Agent._acting, agent)
        agent._convert_tool_chunk_to_event = MethodType(Agent._convert_tool_chunk_to_event, agent)
        agent._split_tool_result_for_compression = compress
        agent._save_to_context = contexts.extend
        async def reply():
            async for event in Agent._execute_tool_call(agent, call):
                yield event
        async for event in middleware.on_reply(agent, {}, reply):
            events.append(event.model_dump(mode="json"))

    def strict(self, **values):
        return {"event": "tool.after", "step": "check", "on_error": "withhold_result",
                "budget_ms": 1000, **values}

    def deltas(self, events):
        return [event["delta"] for event in events if "delta" in event]

    async def test_checked_output_reaches_native_events_and_context_once(self):
        for sensitive in (False, True):
            with self.subTest(sensitive=sensitive):
                events, contexts, finalized = [], [], []
                checking, release = asyncio.Event(), asyncio.Event()
                raw = "synthetic-card-number" if sensitive else "ordinary tool output"
                safe = "Sensitive data detected by AW; content withheld." if sensitive else raw
                async def tool(tool_call):
                    try:
                        yield ToolChunk(content=[TextBlock(text=raw[:5])], is_last=False)
                        yield ToolChunk(content=[TextBlock(text=raw[5:])], is_last=False)
                        result = response(raw)
                        result.id = "response-id"
                        result.metadata = {"exit_code": 0}
                        yield result
                    finally:
                        finalized.append(True)
                mw, agent, coordinator = self.setup_agent(tool, hooks=[self.strict(replace_result=True)])
                async def invoke(hook, payload):
                    self.assertEqual(finalized, [True])
                    self.assertEqual(self.deltas(events), [])
                    checking.set()
                    await release.wait()
                    if sensitive:
                        checked = payload["tool_response"]
                        checked["content"][0]["text"] = safe
                        return 0, json.dumps({"aw_replacement": checked}).encode()
                    return 0, b""
                mw.scope.invoke = invoke
                task = asyncio.create_task(self.event_request(mw, agent, self.call(), events, contexts))
                try:
                    await asyncio.wait_for(checking.wait(), 2)
                    self.assertEqual(self.deltas(events), [])
                    self.assertEqual(contexts, [])
                    release.set()
                    await asyncio.wait_for(task, 2)
                    self.assertEqual(self.deltas(events), [safe])
                    self.assertEqual(contexts[0].output[0].text, safe)
                    self.assertEqual(contexts[0].id, "native")
                    self.assertEqual(contexts[0].metadata, {"exit_code": 0})
                    self.assertEqual(events[-1]["state"], "success")
                    self.assertEqual(events[-1]["tool_call_id"], "native")
                    self.assertTrue(mw.scope.closed)
                    if sensitive:
                        self.assertNotIn(raw, json.dumps(events))
                finally:
                    release.set()
                    await asyncio.gather(task, return_exceptions=True)
                    await coordinator.shutdown()

    async def test_final_response_without_chunks_emits_checked_output(self):
        async def tool(tool_call):
            yield response("complete result")
        mw, agent, coordinator = self.setup_agent(tool, hooks=[self.strict()])
        events, contexts = [], []
        try:
            await self.event_request(mw, agent, self.call(), events, contexts)
            self.assertEqual(self.deltas(events), ["complete result"])
            self.assertEqual(len(contexts), 1)
        finally:
            await coordinator.shutdown()

    async def test_checked_chunk_preserves_final_state_identity_and_metadata(self):
        for state in ("error", "denied", "interrupted"):
            with self.subTest(state=state):
                async def tool(tool_call):
                    result = response("checked " + state)
                    result.state = state
                    result.id = "terminal-id"
                    result.metadata = {"exit_code": 7}
                    yield result
                mw, agent, coordinator = self.setup_agent(tool, hooks=[self.strict()])
                try:
                    values = await self.request(mw, agent, [self.call()])
                    self.assertEqual(len(values), 2)
                    chunk, result = values
                    self.assertIsInstance(chunk, ToolChunk)
                    self.assertIsInstance(result, ToolResponse)
                    self.assertTrue(chunk.is_last)
                    self.assertEqual(chunk.id, result.id)
                    self.assertEqual(chunk.state, result.state)
                    self.assertEqual(chunk.metadata, result.metadata)
                    chunk.content[0].text = "consumer mutation"
                    self.assertEqual(result.content[0].text, "checked " + state)
                finally:
                    await coordinator.shutdown()

    async def test_failed_check_never_publishes_output_or_context(self):
        async def tool(tool_call):
            yield ToolChunk(content=[TextBlock(text="private raw")], is_last=False)
            yield response("private raw")
        mw, agent, coordinator = self.setup_agent(tool, hooks=[self.strict()])
        async def invoke(*args):
            return 1, b""
        mw.scope.invoke = invoke
        events, contexts = [], []
        try:
            with self.assertRaises(ResidentFailure):
                await self.event_request(mw, agent, self.call(), events, contexts)
            self.assertEqual(self.deltas(events), [])
            self.assertEqual(contexts, [])
            self.assertNotIn("private raw", json.dumps(events))
        finally:
            await coordinator.shutdown()

    async def test_cancelled_check_never_publishes_output_or_context(self):
        checking, release = asyncio.Event(), asyncio.Event()
        async def tool(tool_call):
            yield response("private raw")
        mw, agent, coordinator = self.setup_agent(
            tool, hooks=[self.strict()], coordinator=ToolCoordinator(cancel_grace_period_secs=0.01))
        async def invoke(*args):
            checking.set()
            await release.wait()
            return 0, b""
        mw.scope.invoke = invoke
        events, contexts = [], []
        task = asyncio.create_task(self.event_request(mw, agent, self.call(), events, contexts))
        try:
            await asyncio.wait_for(checking.wait(), 2)
            task.cancel()
            with self.assertRaises((asyncio.CancelledError, ResidentFailure)):
                await asyncio.wait_for(task, 2)
            self.assertEqual(self.deltas(events), [])
            self.assertEqual(contexts, [])
        finally:
            release.set()
            await asyncio.gather(task, return_exceptions=True)
            await coordinator.shutdown()

    async def test_failed_tool_cleanup_never_publishes_output(self):
        async def tool(tool_call):
            try:
                yield response("private raw")
            finally:
                raise RuntimeError("tool cleanup failed")
        mw, agent, coordinator = self.setup_agent(tool, hooks=[self.strict()])
        events, contexts = [], []
        try:
            with self.assertRaises(ResidentFailure):
                await self.event_request(mw, agent, self.call(), events, contexts)
            self.assertEqual(self.deltas(events), [])
            self.assertEqual(contexts, [])
        finally:
            await coordinator.shutdown()

    async def test_unselected_strict_policy_does_not_duplicate_native_chunks(self):
        async def tool(tool_call):
            yield ToolChunk(content=[TextBlock(text="native chunk")], is_last=False)
            yield response("native final")
        mw, agent, coordinator = self.setup_agent(tool, hooks=[self.strict(step_tools=["other"])])
        events, contexts = [], []
        try:
            await self.event_request(mw, agent, self.call(), events, contexts)
            self.assertEqual(self.deltas(events), ["native chunk"])
            self.assertEqual(contexts[0].output[0].text, "native final")
        finally:
            await coordinator.shutdown()


if __name__ == "__main__":
    unittest.main()
