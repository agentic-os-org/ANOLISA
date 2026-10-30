"""Pinned native Coordinator through real CLI scope transport and the AW service."""

import argparse
import asyncio
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from types import MethodType, SimpleNamespace
import unittest

import native_resident as native
from native_resident_policy import ResidentPolicyTests


class NativeResidentServiceTests(ResidentPolicyTests, unittest.IsolatedAsyncioTestCase):
    async def command(self, *args, operation=None):
        process = await asyncio.create_subprocess_exec(
            str(self.binary), *map(str, args), cwd=self.root,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        try:
            return await asyncio.wait_for(process.communicate(
                json.dumps(operation).encode() if operation is not None else None
            ), 8), process.returncode
        finally:
            if process.returncode is None:
                process.kill()
            await process.wait()

    async def rpc(self, method, **fields):
        (stdout, stderr), code = await self.command(
            "request", "--socket", self.socket, operation={"method": method, **fields}
        )
        self.assertEqual(code, 0, stderr.decode())
        return json.loads(stdout)["result"]

    async def asyncSetUp(self):
        directory = tempfile.TemporaryDirectory(prefix="", dir=native.OUTPUT)
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.socket = self.root / "s/aw.sock"
        document = {
            "apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
            "metadata": {"name": "native-resident"}, "spec": {
                "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(self.root / "s")},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {"native": {"adapter": "qwenpaw", "argv": [sys.executable]}},
                "providers": {"policy": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {"type": "stdio", "location": "agent", "argv": [sys.executable, str(Path(__file__).with_name("action.py")), str(self.root), "provider", "policy"]},
                    "timeout_ms": 3000, "max_output_bytes": 65536, "config": {},
                }},
                "events": {
                    event: {"enabled": True, "steps": [{"id": step, "provider": "policy", "operation": step,
                        "effects": ["observe", "block"] if event == "tool.before" else ["observe"],
                        "on_error": "block" if event == "tool.before" else "report"}]}
                    for event, step in [("tool.before", "check"), ("tool.after", "record")]
                },
            },
        }
        if self._testMethodName == "test_unmatched_steps_preserve_before_and_after_host_order":
            (self.root / "skipped").mkdir()
            document["spec"]["providers"]["skipped"] = json.loads(json.dumps(document["spec"]["providers"]["policy"]))
            document["spec"]["providers"]["skipped"]["transport"]["argv"][2] = str(self.root / "skipped")
            for event in ("tool.before", "tool.after"):
                steps = document["spec"]["events"][event]["steps"]
                skipped = {**steps[0], "id": "skipped", "provider": "skipped", "match": {"tools": ["native:qwenpaw:another"]}}
                steps.insert(0, skipped)
        config = self.root / "aw.json"
        config.write_text(json.dumps(document))
        log = native.OUTPUT / f"{self._testMethodName}.service.log"
        command = [str(self.binary), "serve", "--config", str(config), "--state-dir", str(self.root / "s")]
        with log.open("wb") as stream:
            self.service = await asyncio.create_subprocess_exec(*command, cwd=self.root, stdout=stream, stderr=stream)
        self.addAsyncCleanup(self.stop)
        (native.OUTPUT / f"{self._testMethodName}.process.json").write_text(json.dumps({
            "command": command, "cwd": str(self.root), "pid": self.service.pid,
            "ports": [], "log": str(log), "expected_lifetime_seconds": 30,
            "stop": f"kill -TERM {self.service.pid}",
        }))
        deadline = time.monotonic() + 5
        while not self.socket.exists():
            self.assertIsNone(self.service.returncode, log.read_text())
            self.assertLess(time.monotonic(), deadline, "service startup timeout")
            await asyncio.sleep(0.02)
        binding = await self.rpc("bind", target="native", cwd=str(self.root), environment={}, capabilities={
            "adapter": "qwenpaw", "version": "2.2.2.post1", "entrypoint": "app",
            "events": {"tool.before": ["observe", "block"], "tool.after": ["observe", "replace_result"]},
            "failure_actions": {"tool.after": ["withhold_result"]},
        })
        self.parent = binding["instance_id"]
        await self.rpc("own_instance", instance_id=self.parent, pid=os.getpid())
        binding_path = self.root / "binding.json"
        hooks = []
        for event, rule in document["spec"]["events"].items():
            steps = rule["steps"] if event == "tool.before" else list(reversed(rule["steps"]))
            hooks.extend({"event": event, "step": step["id"], "on_error": step["on_error"],
                          "budget_ms": 5000, "event_tools": ["*"], "step_tools": step.get("match", {}).get("tools", ["*"])}
                         for step in steps)
        binding_path.write_text(json.dumps({
            "adapter": "qwenpaw", "binding": binding, "socket": str(self.socket), "cwd": str(self.root),
            "events": {h["event"]: {"budget_ms": 5000, "steps": {step["id"]: step["on_error"] for step in document["spec"]["events"][h["event"]]["steps"]}, "replacement_step": None} for h in hooks},
        }))
        binding_path.chmod(0o600)
        self.middleware = native.ResidentMiddleware({
            "executable": str(self.binary), "binding": str(binding_path), "hooks": hooks,
        }, native.PLUGIN.AwToolMiddleware, SimpleNamespace(workspace_dir=self.root))

    async def stop(self):
        if self.service.returncode is None:
            self.service.terminate()
        try:
            await asyncio.wait_for(self.service.wait(), 5)
        finally:
            if self.service.returncode is None:
                self.service.kill()
                await self.service.wait()
        self.assertFalse(self.socket.exists(), "service left its socket behind")

    async def run_native(self, deny, reason="security policy denied the tool"):
        executed, finalized = [], []
        async def tool(tool_call):
            try:
                executed.append(True)
                yield native.response()
            finally:
                finalized.append(True)
        coordinator = native.ToolCoordinator()
        native.bind_coordinator(coordinator)
        mw = self.middleware
        async def available(name, groups):
            return SimpleNamespace(input_schema={})
        async def call_tool(tool_call, state):
            async for item in tool(tool_call):
                yield item
        agent = SimpleNamespace(
            toolkit=SimpleNamespace(check_tool_available=available, call_tool=call_tool),
            _reply_middlewares=[mw], _acting_middlewares=[mw, native.ToolCoordinatorMiddleware(coordinator)],
            _acting_impl=tool, state=SimpleNamespace(session_id="session", context=[], tool_context=SimpleNamespace(activated_groups=[])),
            _request_context={"session_id": "session", "agent_id": "agent"},
        )
        agent.reply_stream = MethodType(native.Agent.reply_stream, agent)
        agent.reply = MethodType(native.Agent.reply, agent)
        agent.close = MethodType(native.QwenPawAgent.close, agent)
        agent._reply = MethodType(native.Agent._reply, agent)
        native.bind_agent(agent)
        async def reply():
            call = native.ToolCallBlock(id="original-native-id", name="tool", input=json.dumps({"deny": deny, "reason": reason}))
            async for item in native.Agent._acting(agent, call):
                yield item
        result = [item async for item in mw.on_reply(agent, {}, reply)]
        self.assertEqual(len(result), 1)
        if deny:
            self.assertEqual(result[0].content[0].text, reason)
        self.assertEqual(bool(executed), not deny)
        self.assertEqual(bool(finalized), not deny)
        self.assertTrue(mw.scope.closed)
        self.assertEqual(mw.active, {})
        self.assertEqual(coordinator._entries, {})
        event = json.loads((self.root / "tool.after-provider.json").read_text())
        self.assertEqual(event["tool"]["result"]["state"], "denied" if deny else "success")
        self.assertNotEqual(event["tool"]["call_id"], "original-native-id")
        status = await self.rpc("status")
        self.assertEqual(len(status["instances"]), 1)
        await self.rpc("release_instance", instance_id=self.parent)
        self.assertEqual((await self.rpc("status"))["instances"], [])
        await coordinator.shutdown()

    async def test_unmatched_steps_preserve_before_and_after_host_order(self):
        await self.run_native(False)
        self.assertEqual(list((self.root / "skipped").iterdir()), [])
        recent = await self.rpc("recent_executions")
        completed = [item for item in reversed(recent["items"]) if item["method"] == "invoke"]
        self.assertEqual([item["step_id"] for item in completed], ["skipped", "check", "skipped", "record"])
        self.assertEqual([item["kind"] for item in completed], ["skipped", "provider", "skipped", "provider"])
        self.assertTrue(all(item["status"] == "ok" and item["failure"] is None for item in completed))
        sequences = []
        for path in (self.root / "s/journal").iterdir():
            records = [json.loads(line)["record"] for line in path.read_text().splitlines()]
            steps = [record["call"]["step_id"] for record in records
                     if record.get("phase") == "completed" and record.get("call", {}).get("method") == "invoke"]
            if steps:
                sequences.append(steps)
        self.assertCountEqual(sequences, [["skipped", "check"], ["skipped", "record"]])

    async def test_provider_denial_prevents_native_handler_and_releases_scope(self):
        await self.run_native(True)

    async def test_tokenless_retry_text_reaches_native_denial(self):
        await self.run_native(True, '[tokenless:rewrite] Retry this tool with arguments: {"command":"rtk git status"}')

    async def test_native_completion_drains_real_scope_and_provider_callbacks(self):
        await self.run_native(False)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    args, remaining = parser.parse_known_args()
    NativeResidentServiceTests.binary = args.bin.resolve() / "aw"
    unittest.main(argv=[sys.argv[0], *remaining])
