"""Pinned native input types and resident admission before model dispatch."""

from native_resident_support import *
from agentscope.message import Msg
from agentscope.event import UserConfirmResultEvent, ExternalExecutionResultEvent, UserInterruptEvent
from aw_resident_test.aw_submission import submission_text, SubmissionDenied


def message(text):
    return Msg(name="user", role="user", content=[TextBlock(text=text)])


class NativeSubmissionTests(NativeFixture):
    def setup_submission(self, on_error="block"):
        async def tool(call):
            yield response()
        return self.setup_agent(tool, hooks=[{
            "event":"input.submit", "step":"scan", "on_error":on_error, "budget_ms":1000,
        }])

    async def submit(self, mw, agent, inputs, model):
        async def reply(**kwargs):
            model.append(kwargs["inputs"])
            yield "model response"
        return [value async for value in mw.on_reply(agent, {"inputs":inputs}, reply)]

    async def test_fresh_text_scans_before_model_and_turns_remain_distinct(self):
        mw, agent, _ = self.setup_submission()
        calls, model = [], []
        async def invoke(scope, hook, payload):
            calls.append(payload)
            self.assertEqual(len(model), len(calls)-1)
            return 0, b""
        with patch.object(Ledger, "invoke", invoke):
            await self.submit(mw, agent, message("one\ntwo"), model)
            await self.submit(mw, agent, message("second"), model)
        self.assertEqual([p["input_text"] for p in calls], ["one\ntwo", "second"])
        self.assertEqual(len({p["aw_turn_id"] for p in calls}), 2)
        self.assertTrue(all(p["session_id"] == "session" and "tool_call" not in p for p in calls))
        self.assertTrue(mw.scope.closed)

    async def test_risk_blocks_model_and_allows_next_fresh_submission(self):
        mw, agent, _ = self.setup_submission()
        model = []
        async def invoke(scope, hook, payload):
            return (2 if payload["input_text"] == "risk" else 0), b""
        with patch.object(Ledger, "invoke", invoke):
            with self.assertRaises(SubmissionDenied):
                await self.submit(mw, agent, message("risk"), model)
            self.assertEqual(model, [])
            self.assertTrue(mw.scope.closed)
            await self.submit(mw, agent, message("safe"), model)
        self.assertEqual(len(model), 1)

    async def test_backend_fault_respects_each_failure_policy(self):
        async def invoke(*args):
            return 1, b""
        with patch.object(Ledger, "invoke", invoke):
            for policy in ["block", "report"]:
                mw, agent, _ = self.setup_submission(policy)
                model = []
                if policy == "block":
                    with self.assertRaises(SubmissionDenied):
                        await self.submit(mw, agent, message("fresh"), model)
                else:
                    await self.submit(mw, agent, message("fresh"), model)
                self.assertEqual(len(model), int(policy == "report"))
                self.assertTrue(mw.scope.closed)

    async def test_confirmation_interrupt_and_tool_returns_do_not_rescan(self):
        mw, agent, _ = self.setup_submission()
        for event in [None, UserConfirmResultEvent.model_construct(),
                      UserInterruptEvent.model_construct(), ExternalExecutionResultEvent.model_construct()]:
            await self.submit(mw, agent, event, [])
            self.assertEqual(mw.scope.callbacks, [])
        self.assertIsNone(submission_text(Msg(name="assistant", role="assistant", content=[TextBlock(text="tool return")])))

    async def test_unknown_input_is_a_mapping_error_before_model(self):
        mw, agent, _ = self.setup_submission()
        model = []
        with self.assertRaises(SubmissionDenied):
            await self.submit(mw, agent, {"unexpected":"input"}, model)
        self.assertEqual(model, [])
        self.assertEqual(mw.scope.callbacks, [])
        async def reply(**kwargs):
            model.append("should not run")
            yield "model"
        with self.assertRaises(SubmissionDenied):
            _ = [item async for item in mw.on_reply(agent, {}, reply)]
        self.assertEqual(model, [])

    async def test_cancellation_drains_submission_without_model(self):
        mw, agent, _ = self.setup_submission("report")
        started, drained = asyncio.Event(), asyncio.Event()
        async def invoke(*args):
            try:
                started.set()
                await asyncio.Future()
            finally:
                drained.set()
        model = []
        with patch.object(Ledger, "invoke", invoke):
            pending = asyncio.create_task(self.submit(mw, agent, message("fresh"), model))
            await asyncio.wait_for(started.wait(), 2)
            pending.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await pending
        self.assertTrue(drained.is_set())
        self.assertTrue(mw.scope.closed)
        self.assertEqual(model, [])


if __name__ == "__main__":
    unittest.main()
