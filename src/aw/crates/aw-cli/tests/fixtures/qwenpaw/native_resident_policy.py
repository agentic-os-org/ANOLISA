"""Real service update transactions meet pinned native reply and lease boundaries."""

import asyncio
from contextlib import asynccontextmanager
from copy import deepcopy
import json
import os
from types import SimpleNamespace

import native_mcp
import native_resident_support as native


class ResidentPolicyTests:
    async def enable_policy_reload(self):
        await self.rpc("enable_reload", instance_id=self.parent)
        await self.rpc("associate_process", instance_id=self.parent, pid=os.getpid())
        binding = self.root / "binding.json"
        value = json.loads(binding.read_text())
        value["resident_policy_reload"] = True
        binding.write_text(json.dumps(value))
        self.middleware.root_config["resident_policy_reload"] = True
        self.document = json.loads((self.root / "aw.json").read_text())

    async def update_policy(self, document, *, busy=False):
        candidate = self.root / "candidate.json"
        candidate.write_text(json.dumps(document))
        (stdout, stderr), code = await self.command(
            "update", "--config", self.root / "aw.json", "--agent", "native", "--policy", candidate,
        )
        if busy:
            self.assertNotEqual(code, 0)
            self.assertIn("busy", stderr.decode())
        else:
            self.assertEqual(code, 0, stderr.decode())
        return stdout

    def setup_agent(self, *args, **kwargs):
        mw, agent, coordinator = native.NativeFixture.setup_agent(self, *args, **kwargs)
        mw.root_config = dict(self.middleware.root_config)
        mw.config = dict(self.middleware.config)
        mw.scope = native.SCOPE.Scope(mw.root_config)
        return mw, agent, coordinator

    request = native.NativeFixture.request
    call = native.NativeFixture.call
    fixture = native_mcp.NativeMcpTests.fixture

    async def finish_root(self, coordinator):
        await coordinator.shutdown()
        status = (await self.rpc("status"))["instances"]
        self.assertEqual(len(status), 1)
        self.assertEqual(status[0]["requests"], 0)
        await self.rpc("release_instance", instance_id=self.parent)
        self.assertEqual((await self.rpc("status"))["instances"], [])

    async def test_same_agent_refreshes_real_policy_routes_between_replies(self):
        await self.enable_policy_reload()
        executed = []
        async def tool(tool_call):
            executed.append(tool_call.id)
            yield native.response()
        mw, agent, coordinator = self.setup_agent(tool)
        call = self.call()
        # Exercise the App's startup seam before the first task has reserved a scope.
        config = {**mw.root_config, "ready_path": str(self.root / "prewarm-ready.json"),
                  "ready_token": "prewarm"}
        await native.PLUGIN.AwPlugin._startup(config)
        self.assertTrue((self.root / "prewarm-ready.json").is_file())
        status = (await self.rpc("status"))["instances"]
        self.assertEqual(len(status), 1)
        self.assertEqual(status[0]["requests"], 0)
        empty = deepcopy(self.document)
        empty["spec"]["events"] = {}
        await self.update_policy(empty)
        await self.update_policy(self.document)
        call.input = '{"deny":false}'
        await self.request(mw, agent, [call])
        first_revision = mw.config["policy_revision"]
        self.assertEqual(len(mw.config["hooks"]), 2)
        empty = deepcopy(self.document)
        empty["spec"]["events"] = {}
        await self.update_policy(empty)
        await self.request(mw, agent, [call])
        self.assertEqual(mw.config["hooks"], [])
        self.assertNotEqual(mw.config["policy_revision"], first_revision)
        await self.update_policy(self.document)
        call.input = '{"deny":true}'
        result = await self.request(mw, agent, [call])
        self.assertEqual(result[0].state.value, "denied")
        self.assertEqual(len(executed), 2)
        self.assertEqual(mw.config["policy_revision"], first_revision)
        await self.finish_root(coordinator)

    async def test_new_strict_post_stops_native_delivery_through_real_update(self):
        await self.enable_policy_reload()
        async def tool(tool_call):
            yield native.ToolChunk(content=[native.TextBlock(text="protected chunk")])
            yield native.response()
        mw, agent, coordinator = self.setup_agent(tool)
        call = self.call()
        call.input = '{"deny":false}'
        await self.request(mw, agent, [call])
        candidate = deepcopy(self.document)
        provider = candidate["spec"]["providers"]["policy"]
        provider["transport"]["argv"] = [
            provider["transport"]["argv"][0], "-c",
            "import json,sys\nr=json.load(sys.stdin)\n"
            "if r['method']=='invoke': sys.exit(1)\n"
            "out={'api_version':'aw-provider/v1alpha1','request_id':r['request_id'],'status':'ok'}\n"
            "if r['method']=='describe': out['operations']=[{'name':'record','events':['tool.after'],'effects':['observe']}]\n"
            "print(json.dumps(out))",
        ]
        candidate["spec"]["events"].pop("tool.before")
        candidate["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = "withhold_result"
        await self.update_policy(candidate)
        delivered = []
        async def reply():
            async for item in native.Agent._acting(agent, call):
                yield item
        with self.assertRaises(native.ResidentFailure):
            async for item in mw.on_reply(agent, {}, reply):
                delivered.append(item)
        self.assertEqual(delivered, [])
        self.assertTrue(mw.failed)
        await self.finish_root(coordinator)

    async def test_real_update_remains_busy_until_private_finalizer_exits(self):
        await self.enable_policy_reload()
        entered, finish = asyncio.Event(), asyncio.Event()
        @asynccontextmanager
        async def private():
            try:
                yield native.PrivateExecution({}, {}, lambda value: value)
            finally:
                entered.set()
                await finish.wait()
        async def tool(tool_call):
            yield native.response()
        mw, agent, coordinator = self.setup_agent(tool, context=SimpleNamespace(
            workspace_dir=self.root, aw_execution_factory=private, aw_environment_version=1))
        call = self.call()
        call.input = '{"deny":false}'
        task = asyncio.create_task(self.request(mw, agent, [call]))
        try:
            await asyncio.wait_for(entered.wait(), 5)
            self.assertTrue(mw.scope.closed)
            self.assertIsNotNone(mw.scope.reservation)
            before = (await self.rpc("status"))["instances"]
            self.assertEqual(len(before), 1)
            self.assertEqual(before[0]["requests"], 1)
            candidate = deepcopy(self.document)
            candidate["spec"]["events"] = {}
            await self.update_policy(candidate, busy=True)
            self.assertEqual((await self.rpc("status"))["instances"][0]["policy_revision"],
                             before[0]["policy_revision"])
            finish.set()
            await asyncio.wait_for(task, 5)
            await self.update_policy(candidate)
        finally:
            finish.set()
            await asyncio.gather(task, return_exceptions=True)
        await self.finish_root(coordinator)

    async def test_real_mcp_routes_refresh_without_changing_result_envelope(self):
        await self.enable_policy_reload()
        async with self.fixture() as f:
            f.call.input = '{"value":3,"deny":false}'
            await self.request(f.mw, f.agent, [f.call])
            candidate = deepcopy(self.document)
            candidate["spec"]["events"] = {}
            await self.update_policy(candidate)
            await self.request(f.mw, f.agent, [f.call])
            self.assertEqual(f.mw.config["hooks"], [])
            await self.update_policy(self.document)
            results = await self.request(f.mw, f.agent, [f.call])
            self.assertEqual(len(f.calls), 3)
            result = next(item for item in results if isinstance(item, native.ToolResponse))
            self.assertEqual(result.metadata["aw_mcp_result"],
                             f.envelope.model_dump(mode="json", by_alias=True))
            before = json.loads((self.root / "tool.before-provider.json").read_text())
            after = json.loads((self.root / "tool.after-provider.json").read_text())
            self.assertEqual(before["native"]["aw_mcp_context"], after["native"]["aw_mcp_context"])
            await self.finish_root(f.coordinator)
