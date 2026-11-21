"""Check explicit lifecycle delivery through one language Source Session."""

import unittest

from aw_sdk.sources import source_capabilities
from source_fixture import fake_source


class LifecycleHooks(unittest.IsolatedAsyncioTestCase):
    async def test_retains_reasons_and_returns_stop_block(self) -> None:
        events = ["session.start", "turn.stop", "session.end"]
        caps = source_capabilities(events)
        self.assertEqual(caps["events"]["turn.stop"], ["observe", "block"])

        async def decide(event: str, payload: dict) -> dict:
            decision = "block" if event == "turn.stop" and payload["reason"] == "timeout" else "continue"
            return {"decision": decision, "coverage": "checked"}

        source, tasks = fake_source(decide, events=events)
        async with source.session("session") as session:
            self.assertEqual((await session.session_start())["decision"], "continue")
            blocked = await session.turn_stop("turn", "timeout", attempt_id="retry")
            self.assertEqual(blocked["decision"], "block")
            await session.session_end("cancelled")
        self.assertEqual(tasks[0].events, [
            ("session.start", {"session_id": "session"}),
            ("turn.stop", {"session_id": "session", "turn_id": "turn", "reason": "timeout", "attempt_id": "retry"}),
            ("session.end", {"session_id": "session", "reason": "cancelled"}),
        ])
