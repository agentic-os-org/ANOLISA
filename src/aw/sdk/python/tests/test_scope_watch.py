"""Refresh native Scope validity against the actual Service across long waits."""

import asyncio
import json
import unittest
from unittest.mock import patch

import test_client


class ScopeWatchTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.fixture = test_client.Lifecycle()
        await self.fixture.asyncSetUp()
        config = json.loads(self.fixture.config.read_text())
        config["spec"]["agents"]["app"]["adapter"] = "qwenpaw"
        self.fixture.config.write_text(json.dumps(config))
        self.client = self.fixture.instance(
            availability="required",
            capabilities={
                "adapter": "qwenpaw",
                "version": "installed-native-hooks/v1",
                "entrypoint": "app",
                "events": {
                    "tool.before": ["observe", "block"],
                    "tool.after": ["observe", "replace_result"],
                },
                "failure_actions": {"tool.after": ["withhold_result"]},
            },
        )
        await self.fixture.start()
        await self.client.connect()
        self.task = await self.client.task()
        await self.task.call(
            {"method": "enable_invocations", "instance_id": self.task.binding["instance_id"]}
        )

    async def asyncTearDown(self) -> None:
        await self.fixture.asyncTearDown()

    async def test_watch_preserves_revision_and_close_joins_monitor(self) -> None:
        checks = []
        renewed = asyncio.Event()
        original = self.task.call

        async def observed(operation: dict, *args: object) -> dict:
            result = await original(operation, *args)
            if operation["method"] == "check_scope":
                checks.append(operation)
                if len(checks) >= 2:
                    renewed.set()
            return result

        with patch.object(self.task, "call", side_effect=observed):
            watch = await self.task.watch_scope()
            revision = watch.policy_revision
            async with asyncio.timeout(7):
                await renewed.wait()
            self.assertGreaterEqual(len(checks), 2)
            self.assertTrue(all(check["policy_revision"] == revision for check in checks))
            self.assertFalse(watch.monitor.done())
            self.assertEqual(watch.policy_revision, revision)
            self.assertIsNone(watch.error)
            await self.task.close()
            self.assertTrue(watch.monitor.done())
            self.assertTrue(watch.closed)

    async def test_probe_failure_is_latched_and_notifies_native_owner_once(self) -> None:
        failure = RuntimeError("native seam removed")
        failures = []

        def probe() -> None:
            raise failure

        watch = await self.task.watch_scope(probe=probe, on_failure=failures.append)
        with self.assertRaisesRegex(RuntimeError, "scope_bridge_invalid"):
            await watch.validate()
        self.assertEqual(watch.error.code, "scope_bridge_invalid")
        self.assertEqual(failures, [watch.error])
        with self.assertRaisesRegex(RuntimeError, "scope_bridge_invalid"):
            watch.check()
        await self.task.close()
        self.assertTrue(watch.monitor.done())
