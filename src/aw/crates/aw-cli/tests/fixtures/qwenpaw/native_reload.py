"""Exercise real AgentScope closure chains through AW bridge and update RPCs."""

import argparse
import asyncio
import copy
import gc
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest

from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock, ToolResultState
from agentscope.tool import ToolChunk, ToolResponse
from qwenpaw.tool_calls._coordinator import ToolCoordinator
from qwenpaw.tool_calls._middleware import ToolCoordinatorMiddleware

from native_middleware import MODULE


class ReloadTests(unittest.IsolatedAsyncioTestCase):
    async def command(self, *arguments, operation=None):
        child = await asyncio.create_subprocess_exec(
            str(self.binary), *map(str, arguments), cwd=self.root,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        try:
            data = json.dumps(operation).encode() if operation else None
            stdout, stderr = await asyncio.wait_for(child.communicate(data), 8)
            return child.returncode, stdout, stderr
        finally:
            if child.returncode is None:
                child.kill()
            await child.wait()

    async def rpc(self, method, **fields):
        code, stdout, stderr = await self.command(
            "request", "--socket", self.socket,
            operation={"method": method, **fields},
        )
        self.assertEqual(code, 0, stderr.decode())
        return json.loads(stdout)["result"]

    async def asyncSetUp(self):
        # Linux Unix socket names cannot accommodate long build/output paths.
        directory = tempfile.TemporaryDirectory(prefix="aw-close-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.socket = self.root / "state/aw.sock"
        self.config = self.root / "current.json"
        document = {
            "apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
            "metadata": {"name": "native-close"}, "spec": {
                "daemon": {"startup": "external", "endpoint": "auto",
                           "state_dir": str(self.root / "state")},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 1000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {"native": {"adapter": "qwenpaw", "argv": [sys.executable]}},
                "providers": {}, "events": {},
            },
        }
        self.config.write_text(json.dumps(document))
        self.original = hashlib.sha256(self.config.read_bytes()).hexdigest()
        candidate = copy.deepcopy(document)
        candidate["spec"]["providers"]["observe"] = {
            "protocol": "native-hook/v1alpha1",
            "transport": {"type": "stdio", "location": "agent", "argv": ["/bin/true"]},
            "timeout_ms": 500, "max_output_bytes": 4096, "config": {},
        }
        candidate["spec"]["events"]["tool.before"] = {"enabled": True, "steps": [
            {"id": "observe", "provider": "observe", "native": {}, "on_error": "block"},
        ]}
        self.candidate = self.root / "next.json"
        self.candidate.write_text(json.dumps(candidate))
        output = Path(os.environ["AW_TEST_OUTPUT"])
        log = output / f"{self._testMethodName}.daemon.log"
        with log.open("wb") as stream:
            command = [str(self.binary), "serve", "--config", str(self.config),
                       "--state-dir", str(self.root / "state")]
            self.daemon = await asyncio.create_subprocess_exec(
                *command, cwd=self.root, stdout=stream, stderr=stream,
            )
        self.addAsyncCleanup(self.stop)
        receipt = {"command": command, "cwd": str(self.root), "pid": self.daemon.pid,
                   "ports": [], "log": str(log), "expected_lifetime_seconds": 60,
                   "stop": f"kill -TERM {self.daemon.pid}"}
        (output / f"{self._testMethodName}.process.json").write_text(json.dumps(receipt))
        deadline = time.monotonic() + 5
        while not self.socket.exists():
            self.assertIsNone(self.daemon.returncode, log.read_text())
            self.assertLess(time.monotonic(), deadline, "daemon readiness timeout")
            await asyncio.sleep(0.01)
        binding = await self.rpc(
            "bind", target="native", cwd=str(self.root), environment={},
            capabilities={"adapter": "qwenpaw", "version": "2.2.2.post1", "entrypoint": "app",
                          "events": {"tool.before": ["observe", "block"],
                                     "tool.after": ["observe", "replace_result"]}},
        )
        self.instance = binding["instance_id"]
        await self.rpc("enable_reload", instance_id=self.instance)
        await self.rpc("associate_process", instance_id=self.instance, pid=os.getpid())
        binding_path = self.root / "binding.json"
        binding_path.write_text(json.dumps({
            "adapter": "qwenpaw", "reloadable": True, "binding": binding,
            "socket": str(self.socket), "cwd": str(self.root), "events": {},
        }))
        binding_path.chmod(0o600)
        self.middleware = MODULE.AwReloadMiddleware({
            "executable": str(self.binary), "binding": str(binding_path),
        })
        await self.middleware.lifecycle("ready", "")

    async def stop(self):
        try:
            if self.daemon.returncode is None:
                self.daemon.terminate()
            await asyncio.wait_for(self.daemon.wait(), 5)
        finally:
            if self.daemon.returncode is None:
                self.daemon.kill()
                await self.daemon.wait()
        self.assertFalse(self.socket.exists(), "daemon left its socket behind")

    async def update(self):
        return await self.command("update", "--config", self.config, "--agent", "native",
                                  "--policy", self.candidate)

    async def busy(self, field):
        state = (await self.rpc("status"))["instances"][0]
        self.assertEqual(state[field], 1)
        self.assertEqual(state["policy_revision"], self.original)
        code, _, stderr = await self.update()
        self.assertNotEqual(code, 0, "activated policy before native cleanup completed")
        self.assertIn("busy", stderr.decode())
        self.assertEqual((await self.rpc("status"))["instances"][0]["policy_revision"], self.original)

    async def exercise(self, tool, early_close):
        entered, release, completed = asyncio.Event(), asyncio.Event(), asyncio.Event()

        async def native(*args, **kwargs):
            try:
                if tool:
                    yield ToolChunk(content=[TextBlock(text="partial")], is_last=False)
                yield ToolResponse(content=[TextBlock(text="final")]) if tool else TextBlock(text="partial")
            finally:
                entered.set()
                await release.wait()
                completed.set()

        agent = SimpleNamespace(
            _reply_middlewares=[self.middleware], _reply_impl=native,
            _acting_middlewares=[self.middleware], _acting_impl=native,
            state=SimpleNamespace(session_id="session"),
        )
        stream = (Agent._acting(agent, ToolCallBlock(id="call", name="tool", input="{}"))
                  if tool else Agent._reply(agent))
        field = "tools" if tool else "requests"
        pending = None
        try:
            if tool:
                # Let the real reply chain finish with a tool still suspended;
                # its request claim must not hide premature tool completion.
                async def reply(**kwargs):
                    yield await anext(stream)
                agent._reply_impl = reply
                self.assertEqual(len([item async for item in Agent._reply(agent)]), 1)
                self.assertEqual((await self.rpc("status"))["instances"][0]["requests"], 0)
            else:
                await anext(stream)
            await self.busy(field)
            if early_close:
                await stream.aclose()
                # AgentScope drops nested generators; drive their real finalizers.
                gc.collect()
            else:
                async def finish():
                    return [item async for item in stream]
                pending = asyncio.create_task(finish())
            await asyncio.wait_for(entered.wait(), 3)
            for _ in range(3):
                self.assertFalse(completed.is_set())
                await self.busy(field)
            release.set()
            await asyncio.wait_for(completed.wait(), 3)
            if early_close:
                # Wrapper closure supplies no evidence of the eventual native end.
                await asyncio.wait_for(asyncio.get_running_loop().shutdown_asyncgens(), 3)
                await self.busy(field)
            else:
                result = await asyncio.wait_for(pending, 3)
                self.assertEqual(len(result), 1 if tool else 0)
                if tool:
                    self.assertEqual(result[0].content[0].text, "final")
                state = (await self.rpc("status"))["instances"][0]
                self.assertEqual(state["requests"], 0)
                self.assertEqual(state["tools"], 0)
                code, stdout, stderr = await self.update()
                self.assertEqual(code, 0, stderr.decode())
                expected = hashlib.sha256(self.candidate.read_bytes()).hexdigest()
                self.assertNotEqual(expected, self.original)
                self.assertEqual(json.loads(stdout)["policy_revision"], expected)
                self.assertEqual((await self.rpc("status"))["instances"][0]["policy_revision"], expected)
        finally:
            release.set()
            if pending is not None:
                if not pending.done():
                    pending.cancel()
                await asyncio.gather(pending, return_exceptions=True)
            await stream.aclose()
            await asyncio.wait_for(asyncio.get_running_loop().shutdown_asyncgens(), 3)
            await self.rpc("release_instance", instance_id=self.instance)
            self.assertEqual((await self.rpc("status"))["bindings"], 0)

    async def test_reply_early_close_retains_busy(self):
        await self.exercise(tool=False, early_close=True)

    async def test_tool_partial_stream_early_close_retains_busy(self):
        await self.exercise(tool=True, early_close=True)

    async def test_reply_normal_cleanup_allows_update(self):
        await self.exercise(tool=False, early_close=False)

    async def test_tool_normal_cleanup_allows_update(self):
        await self.exercise(tool=True, early_close=False)

    async def test_coordinator_final_response_waits_for_cleanup_then_allows_update(self):
        await self.coordinator_completion(None)

    async def test_coordinator_error_chunk_waits_for_cleanup_then_allows_update(self):
        await self.coordinator_completion(ToolResultState.ERROR)

    async def test_coordinator_interrupted_chunk_waits_for_cleanup_then_allows_update(self):
        await self.coordinator_completion(ToolResultState.INTERRUPTED)

    async def coordinator_completion(self, state):
        entered, release = asyncio.Event(), asyncio.Event()
        coordinator = ToolCoordinator(default_timeout_secs=10, cancel_grace_period_secs=0.1)
        seen = []

        async def native(tool_call):
            try:
                if state is not None:
                    yield ToolChunk(content=[TextBlock(text="final")], state=state, is_last=True)
                yield ToolResponse(content=[TextBlock(text="final")],
                                   state=state or ToolResultState.SUCCESS,
                                   metadata={"kept": True})
            finally:
                entered.set()
                await release.wait()

        async def reply(**kwargs):
            async for item in Agent._acting(agent, ToolCallBlock(id="coordinated", name="tool", input="{}")):
                seen.append(item)
                yield item

        agent = SimpleNamespace(
            _reply_middlewares=[self.middleware], _reply_impl=reply,
            _acting_middlewares=[ToolCoordinatorMiddleware(coordinator), self.middleware],
            _acting_impl=native, state=SimpleNamespace(session_id="session"),
        )
        async def consume():
            return [item async for item in Agent._reply(agent)]
        task = asyncio.create_task(consume())
        try:
            await asyncio.wait_for(entered.wait(), 3)
            for _ in range(3):
                await self.busy("tools")
            self.assertEqual(seen, [], "Coordinator received final response before native cleanup")
            release.set()
            result = await asyncio.wait_for(task, 3)
            self.assertEqual(len(result), 1 if state is None else 2)
            self.assertIsInstance(result[-1], ToolResponse)
            self.assertEqual(result[-1].content[0].text, "final")
            self.assertEqual(result[-1].state, state or ToolResultState.SUCCESS)
            if state is not None:
                self.assertIsInstance(result[0], ToolChunk)
                self.assertEqual(result[0].state, state)
            if state is None:
                self.assertEqual(result[0].metadata, {"kept": True})
            state = (await self.rpc("status"))["instances"][0]
            self.assertEqual((state["requests"], state["tools"]), (0, 0))
            code, stdout, stderr = await self.update()
            self.assertEqual(code, 0, stderr.decode())
            self.assertEqual(json.loads(stdout)["policy_revision"], hashlib.sha256(self.candidate.read_bytes()).hexdigest())
        finally:
            release.set()
            if not task.done():
                task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            await coordinator.shutdown()
            await asyncio.wait_for(asyncio.get_running_loop().shutdown_asyncgens(), 3)
            await self.rpc("release_instance", instance_id=self.instance)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    args, remaining = parser.parse_known_args()
    ReloadTests.binary = args.bin.resolve() / "aw"
    unittest.main(argv=[sys.argv[0], *remaining])
