"""Shared admission against the actual Service, independent of native frameworks."""

import asyncio
import json
import unittest

from aw_sdk import Admission, AwError
import test_client


class AdmissionTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.fixture = test_client.Lifecycle()
        await self.fixture.asyncSetUp()
        config = json.loads(self.fixture.config.read_text())
        config["spec"]["agents"]["app"]["adapter"] = "sdk"
        config["spec"]["events"]["input.submit"] = {
            "enabled": True,
            "required": False,
            "steps": [],
        }
        self.fixture.config.write_text(json.dumps(config))
        self.client = self.fixture.instance(
            capabilities={
                "adapter": "sdk",
                "version": "aw-hook-source/v1",
                "entrypoint": "application",
                "events": {
                    event: ["observe"] for event in ["input.submit", "tool.before", "tool.after"]
                },
            }
        )
        self.gates = []

    async def asyncTearDown(self) -> None:
        try:
            for gate in self.gates:
                await gate.close()
        finally:
            await self.fixture.asyncTearDown()

    def gate(self, events: list[str], **options: object) -> Admission:
        gate = Admission(self.client, events=events, **options)
        self.gates.append(gate)
        return gate

    async def test_input_waits_for_ready_and_freezes_one_shared_task(self) -> None:
        gate = self.gate(["input.submit", "tool.before", "tool.after"])
        first, second = asyncio.create_task(gate.prepare()), asyncio.create_task(gate.prepare())
        try:
            await asyncio.sleep(0.03)
            self.assertFalse(first.done())
            self.assertFalse(second.done())
            self.assertFalse(self.client.tasks)
            await self.fixture.start()
            await self.client.connect()
            await asyncio.gather(first, second)
            self.assertEqual(len(self.client.tasks), 1)
            self.assertFalse(gate.fallback)
            self.assertEqual(
                await gate.emit(
                    "input.submit",
                    {
                        "session_id": "session",
                        "turn_id": "turn",
                        "text": "hello",
                    },
                ),
                {"decision": "continue", "coverage": "not_configured"},
            )
        finally:
            await gate.close()
            await asyncio.gather(first, second, return_exceptions=True)

    async def test_optional_tool_fallback_and_input_deadline_remain_distinct(self) -> None:
        tool = self.gate(["tool.before", "tool.after"])
        await tool.prepare()
        self.assertTrue(tool.fallback)
        input_gate = self.gate(["input.submit"], timeout_ms=100)
        started = asyncio.get_running_loop().time()
        await input_gate.prepare()
        self.assertGreaterEqual(asyncio.get_running_loop().time() - started, 0.08)
        self.assertTrue(input_gate.fallback)

    async def test_required_timeout_and_explicit_errors_never_fall_back(self) -> None:
        self.client = self.fixture.instance(
            availability="required", capabilities=self.client.capabilities
        )
        gate = self.gate(["tool.after"], timeout_ms=100)
        with self.assertRaises(TimeoutError):
            await gate.prepare()
        self.assertIsNone(gate.task)
        self.client.last_error = AwError("policy_invalid")
        rejected = self.gate(["input.submit"])
        with self.assertRaisesRegex(AwError, "policy_invalid"):
            await rejected.prepare()
        self.assertIsNone(rejected.task)

    async def test_close_cancels_pending_admission_without_a_task(self) -> None:
        gate = self.gate(["input.submit"])
        waiting = asyncio.create_task(gate.prepare())
        await asyncio.sleep(0.02)
        await asyncio.gather(gate.close(), gate.close())
        with self.assertRaises(asyncio.CancelledError):
            await waiting
        self.assertIsNone(gate.task)
        self.assertFalse(self.client.tasks)
        with self.assertRaisesRegex(AwError, "admission_closed"):
            await gate.prepare()
