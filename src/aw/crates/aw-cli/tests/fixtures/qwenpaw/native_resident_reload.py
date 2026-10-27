"""Resident task snapshots stay reserved through native and private cleanup."""

from copy import deepcopy
from unittest.mock import AsyncMock

from native_resident_support import *
from aw_resident_test import aw_resident
import native_mcp


class SnapshotLedger(Ledger):
    hooks = []
    instances = []

    def __init__(self, config):
        super().__init__(config)
        self.reserved = False
        self.acknowledged = False
        self.snapshot = {"hooks": deepcopy(self.hooks), "policy_revision": "a" * 64}
        self.instances.append(self)

    async def reserve(self):
        self.reserved = True

    async def open(self, cwd, environment, *, acknowledge=True):
        assert self.reserved
        assert not acknowledge
        await super().open(cwd, environment)

    async def policy(self):
        return self.snapshot

    async def acknowledge(self):
        self.acknowledged = True

    async def release(self):
        assert self.closed
        self.reserved = False

    async def invoke(self, hook, payload):
        assert self.acknowledged
        self.callbacks.append((hook["event"], payload))
        return (1, b"") if hook.get("reject") else (0, b"")


def hook(event="tool.before", failure="block", reject=False):
    return {"event": event, "step": "policy", "on_error": failure,
            "budget_ms": 1000, "reject": reject}


class ReloadResidentTests(NativeFixture):
    def setUp(self):
        super().setUp()
        for field, value in (("hooks", []), ("instances", [])):
            patcher = patch.object(SnapshotLedger, field, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        for target, value in (("Scope", SnapshotLedger), ("cleanup_failed", False)):
            patcher = patch.object(aw_resident, target, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def setup_agent(self, *args, **kwargs):
        middleware, agent, coordinator = super().setup_agent(*args, **kwargs)
        middleware.root_config["resident_policy_reload"] = True
        return middleware, agent, coordinator

    fixture = native_mcp.NativeMcpTests.fixture

    async def test_malformed_snapshots_reject_before_ack_or_dispatch(self):
        entered = []
        async def tool(tool_call):
            entered.append(tool_call.id)
            yield response()
        bad = [None, {}, {"hooks": [], "policy_revision": "bad"}]
        for hooks in [None, [None], [dict(hook(), event="unknown")],
                      [dict(hook(), step="")], [dict(hook(), on_error="ignore")],
                      [dict(hook(), budget_ms=True)], [dict(hook(), budget_ms=0)],
                      [dict(hook(), replace_result="true")]]:
            bad.append({"hooks": hooks, "policy_revision": "a" * 64})
        for snapshot in bad:
            with self.subTest(snapshot=snapshot):
                mw, agent, _ = self.setup_agent(tool)
                original = mw.config
                with patch.object(SnapshotLedger, "policy", AsyncMock(return_value=snapshot)):
                    with self.assertRaises((ValueError, ResidentFailure)):
                        await self.request(mw, agent, [self.call()])
                self.assertFalse(mw.scope.acknowledged)
                self.assertFalse(mw.scope.reserved)
                self.assertIs(mw.config, original)
        self.assertEqual(entered, [])

    async def test_snapshot_cannot_replace_launch_identity(self):
        async def tool(tool_call):
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        original = dict(mw.root_config)
        snapshot = {"hooks": [], "policy_revision": "b" * 64,
                    "binding": "injected", "executable": "injected", "resident": False}
        with patch.object(SnapshotLedger, "policy", AsyncMock(return_value=snapshot)):
            await self.request(mw, agent, [self.call()])
        for key in ("binding", "executable", "resident"):
            self.assertEqual(mw.config.get(key), original.get(key))
        self.assertEqual(mw.config["policy_revision"], "b" * 64)

    async def test_mcp_policy_update_keeps_identity_and_original_envelope(self):
        async with self.fixture() as f:
            await self.request(f.mw, f.agent, [f.call])
            self.assertEqual(f.mw.scope.callbacks, [])
            SnapshotLedger.hooks = [hook(), hook("tool.after", "report")]
            results = await self.request(f.mw, f.agent, [f.call])
            self.assertEqual(len(f.calls), 2)
            self.assertEqual([name for name, _ in f.mw.scope.callbacks],
                             ["tool.before", "tool.after"])
            before, after = [payload for _, payload in f.mw.scope.callbacks]
            self.assertEqual(before["aw_mcp_context"], after["aw_mcp_context"])
            self.assertEqual(before["aw_mcp_context"]["parent_tool_call_id"], "model-parent")
            result = next(item for item in results if isinstance(item, ToolResponse))
            self.assertEqual(result.metadata["aw_mcp_result"],
                             f.envelope.model_dump(mode="json", by_alias=True))
            SnapshotLedger.hooks = []
            await self.request(f.mw, f.agent, [f.call])
            self.assertEqual(len(f.calls), 3)
            self.assertEqual(f.mw.scope.callbacks, [])

    async def test_same_agent_adds_and_removes_policy_between_replies(self):
        entered = []
        async def tool(tool_call):
            entered.append(tool_call.id)
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        await self.request(mw, agent, [self.call("first")])
        self.assertEqual(mw.scope.callbacks, [])
        SnapshotLedger.hooks = [hook()]
        await self.request(mw, agent, [self.call("second")])
        self.assertEqual([name for name, _ in mw.scope.callbacks], ["tool.before"])
        SnapshotLedger.hooks = []
        await self.request(mw, agent, [self.call("third")])
        self.assertEqual(mw.scope.callbacks, [])
        self.assertEqual(entered, ["first", "second", "third"])
        self.assertTrue(all(not scope.reserved for scope in SnapshotLedger.instances))

    async def test_new_strict_post_policy_withholds_chunks_and_result(self):
        async def tool(tool_call):
            yield ToolChunk(content=[TextBlock(text="private chunk")])
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        await self.request(mw, agent, [self.call("first")])
        SnapshotLedger.hooks = [hook("tool.after", "withhold_result", True)]
        delivered = []
        async def reply():
            async for value in Agent._acting(agent, self.call("second")):
                yield value
        with self.assertRaises(ResidentFailure):
            async for item in mw.on_reply(agent, {}, reply):
                delivered.append(item)
        self.assertEqual(delivered, [])
        self.assertTrue(mw.failed)

    async def test_reservation_precedes_private_acquisition(self):
        observed = []
        @asynccontextmanager
        async def private():
            observed.append(SnapshotLedger.instances[-1].reserved)
            yield PrivateExecution({}, {}, lambda value: value)
        async def tool(tool_call):
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(
            aw_execution_factory=private, aw_environment_version=1))
        await self.request(mw, agent, [self.call()])
        self.assertEqual(observed, [True])
        self.assertFalse(mw.scope.reserved)

    async def test_lease_exit_keeps_reservation_after_child_scope_close(self):
        entered, finish = asyncio.Event(), asyncio.Event()
        @asynccontextmanager
        async def private():
            try:
                yield PrivateExecution({}, {}, lambda value: value)
            finally:
                entered.set()
                await finish.wait()
        async def tool(tool_call):
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(
            aw_execution_factory=private, aw_environment_version=1))
        task = asyncio.create_task(self.request(mw, agent, [self.call()]))
        try:
            await asyncio.wait_for(entered.wait(), 1)
            self.assertTrue(mw.scope.closed)
            self.assertTrue(mw.scope.reserved)
            self.assertFalse(mw.request_released)
            finish.set()
            await asyncio.wait_for(task, 1)
            self.assertFalse(mw.scope.reserved)
        finally:
            finish.set()
            await asyncio.gather(task, return_exceptions=True)

    async def test_failed_lease_cleanup_retains_reservation_and_fences_worker(self):
        @asynccontextmanager
        async def private():
            yield PrivateExecution({}, {}, lambda value: value)
            raise RuntimeError("cleanup uncertain")
        async def tool(tool_call):
            yield response()
        mw, agent, _ = self.setup_agent(tool, context=SimpleNamespace(
            aw_execution_factory=private, aw_environment_version=1))
        with self.assertRaisesRegex(ResidentFailure, "private cleanup"):
            await self.request(mw, agent, [self.call()])
        self.assertTrue(mw.scope.reserved)
        self.assertFalse(mw.request_released)
        self.assertTrue(aw_resident.cleanup_failed)

    async def test_concurrent_reply_does_not_replace_original_owner(self):
        entered, finish = asyncio.Event(), asyncio.Event()
        async def tool(tool_call):
            entered.set()
            await finish.wait()
            yield response()
        mw, agent, _ = self.setup_agent(tool)
        task = asyncio.create_task(self.request(mw, agent, [self.call()]))
        try:
            await asyncio.wait_for(entered.wait(), 1)
            original = mw.reply_stream
            with self.assertRaisesRegex(ResidentFailure, "concurrent"):
                await self.request(mw, agent, [self.call("second")])
            self.assertIs(mw.reply_stream, original)
            self.assertTrue(mw.scope.reserved)
            finish.set()
            await asyncio.wait_for(task, 1)
            self.assertFalse(mw.scope.reserved)
        finally:
            finish.set()
            await asyncio.gather(task, return_exceptions=True)


class ScopePolicyTests(unittest.IsolatedAsyncioTestCase):
    async def test_app_startup_confirms_readiness_before_any_scope_or_reply(self):
        calls = []
        async def command(config, action, value, timeout):
            self.assertEqual(config["adapter"], "qwenpaw")
            calls.append((action, value))
            return 0, b'{}'
        config = {"resident_policy_reload": True}
        with patch.object(SCOPE, "command", command), patch.object(PLUGIN.AwPlugin, "_ready") as receipt:
            await PLUGIN.AwPlugin._startup(config)
            receipt.assert_called_once_with(config)
        self.assertEqual(calls, [("ready", {"pid": os.getpid()})])

    async def test_failed_app_startup_publishes_no_receipt(self):
        async def command(config, action, value, timeout):
            return 1, b'{}'
        with patch.object(SCOPE, "command", command), patch.object(PLUGIN.AwPlugin, "_ready") as receipt:
            with self.assertRaisesRegex(RuntimeError, "readiness failed"):
                await PLUGIN.AwPlugin._startup({"resident_policy_reload": True, "adapter": "qwenpaw"})
            receipt.assert_not_called()

    async def test_scope_fetches_routes_before_ack_and_retains_root_until_release(self):
        calls = []
        snapshot = {"policy_revision": "revision-2", "events": {}, "hooks": []}
        async def command(config, action, value, timeout):
            calls.append(action)
            if action == "open":
                return 0, b'{"instance_id":"child"}'
            if action == "policy":
                return 0, json.dumps(snapshot).encode()
            if action == "enable":
                return 0, b'{"invocations_per_binding":8}'
            return 0, b'{}'
        scope = SCOPE.Scope({"adapter": "qwenpaw"})
        with patch.object(SCOPE, "command", command):
            await scope.reserve()
            await scope.open("/tmp", {}, acknowledge=False)
            self.assertEqual(await scope.policy(), snapshot)
            self.assertEqual(calls, ["reserve", "open", "policy"])
            await scope.acknowledge()
            self.assertEqual(scope.invocation_limit, 8)
            await scope.close()
            self.assertIsNotNone(scope.reservation)
            await scope.release()
        self.assertEqual(calls, ["reserve", "open", "policy", "acknowledge", "enable", "close", "release"])
        self.assertIsNone(scope.reservation)

    async def test_unconfirmed_child_close_prevents_root_release(self):
        calls = []
        async def command(config, action, value, timeout):
            calls.append(action)
            if action == "open":
                return 0, b'{"instance_id":"child"}'
            return (1 if action == "close" else 0), b'{}'
        scope = SCOPE.Scope({"adapter": "qwenpaw"})
        with patch.object(SCOPE, "command", command):
            await scope.reserve()
            await scope.open("/tmp", {}, acknowledge=False)
            with self.assertRaisesRegex(RuntimeError, "restart"):
                await scope.close()
            with self.assertRaisesRegex(RuntimeError, "unconfirmed"):
                await scope.release()
        self.assertNotIn("release", calls)
        self.assertIsNotNone(scope.reservation)


if __name__ == "__main__":
    unittest.main()
