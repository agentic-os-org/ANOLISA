"""The shared resident Scope carrier against the actual Service."""

import os
import unittest

from aw_sdk import AwError
from aw_sdk.task_scope import TaskScope
from aw_sdk.transport import rpc
import test_client


class TaskScopeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.fixture = test_client.Lifecycle()
        await self.fixture.asyncSetUp()
        self.endpoint = self.fixture.endpoint

    async def asyncTearDown(self) -> None:
        await self.fixture.asyncTearDown()

    async def start(self) -> None:
        await self.fixture.start()

    def instance(self, **options: object):
        return self.fixture.instance(**options)

    async def test_task_carries_the_producer_contract_through_native_drain(self) -> None:
        await self.start()
        installed = []
        client = self.instance(availability="required")
        await client.connect()
        scope = TaskScope(client, 2000, lambda routes: installed.append(routes) or True)
        await scope.open(os.getcwd(), {}, acknowledge=False)
        self.assertEqual(len(installed), 1)
        hook = next(route for route in installed[0]["hooks"] if route["event"] == "tool.before")
        native = {**test_client.FIXTURE["native_before"], "cwd": os.getcwd()}
        # Routes install before the Task acknowledges; invocation admission waits for the bridge.
        self.assertEqual(scope.invocation_limit, 0)
        self.assertIsNone(scope.watch)
        await scope.acknowledge()
        self.assertGreater(scope.invocation_limit, 0)
        await scope.register(1, native["tool_use_id"], native["tool_name"], native["tool_input"])
        code, _, _ = await scope.invoke_native(hook, native)
        self.assertEqual(code, 0)
        await scope.finish(1, native["tool_use_id"])
        await scope.close()
        with self.assertRaisesRegex(RuntimeError, "closed"):
            scope.check()
        status = await rpc(self.endpoint, None, {"method": "status"}, 500)
        self.assertEqual(status["result"]["bindings"], 1)
        await client.close()
        status = await rpc(self.endpoint, None, {"method": "status"}, 500)
        self.assertEqual(status["result"]["bindings"], 0)

    async def test_failed_route_install_releases_the_unacknowledged_scope(self) -> None:
        await self.start()
        client = self.instance(availability="required")
        await client.connect()
        scope = TaskScope(client, 2000, lambda _: False)
        with self.assertRaisesRegex(AwError, "ready_not_confirmed"):
            await scope.open(os.getcwd(), {})
        self.assertFalse(client.tasks)

    async def test_invalidated_scope_fences_execution_and_allows_drain(self) -> None:
        await self.start()
        client = self.instance(availability="required")
        await client.connect()
        scope = TaskScope(client, 2000, lambda _: True)
        await scope.open(os.getcwd(), {})
        native = test_client.FIXTURE["native_before"]
        await scope.register(1, native["tool_use_id"], native["tool_name"], native["tool_input"])
        scope.invalidate(RuntimeError("bridge lease expired"))
        with self.assertRaisesRegex(RuntimeError, "AW scope bridge validity failed"):
            await scope.register(2, "second-call", native["tool_name"], native["tool_input"])
        # Cleanup bypasses the execution validity fence so active calls can drain.
        await scope.finish(1, native["tool_use_id"])
        await scope.close()
        await client.close()
        status = await rpc(self.endpoint, None, {"method": "status"}, 500)
        self.assertEqual(status["result"]["bindings"], 0)


if __name__ == "__main__":
    unittest.main()
