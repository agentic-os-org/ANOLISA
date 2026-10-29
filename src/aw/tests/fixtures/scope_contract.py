"""Shared Python scope races with a deterministic CLI transport double."""

import asyncio
from contextlib import nullcontext
import json
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "crates/aw-adapter/native/claude-sdk"))
# These tests replace the entire CLI exchange; AnyIO is only used by that
# transport and the acknowledgement failure shield, outside the tested paths.
sys.modules["anyio"] = SimpleNamespace(CancelScope=lambda **kwargs: nullcontext())
import aw_scope


class Contracts(unittest.IsolatedAsyncioTestCase):
    def scope(self):
        scope = aw_scope.Scope({})
        scope.handle = {}
        return scope

    async def test_transport_failure_latches_but_cleanup_and_siblings_remain_usable(self):
        scope, other = self.scope(), self.scope()
        scope.policy_revision = "a" * 64
        async def failed(*args):
            raise RuntimeError("service restarted")
        with patch.object(aw_scope, "command", failed):
            with self.assertRaisesRegex(RuntimeError, "restarted"):
                await scope.validate()
        self.assertTrue(scope.failed)
        other.check()
        calls = []
        async def recovered(config, action, value, timeout):
            calls.append((action, value))
            return 0, b'{}'
        with patch.object(aw_scope, "command", recovered):
            with self.assertRaises(RuntimeError):
                await scope.validate()
            self.assertFalse(calls[0][1]["valid"])
            await scope.finish(1, "existing")
            await scope.close()
        self.assertEqual([action for action, _ in calls], ["check", "finish", "close"])

    async def test_idle_probe_reports_failure_and_joins_monitor(self):
        scope = self.scope()
        installed = True
        report = asyncio.Future()
        def probe():
            if not installed:
                raise RuntimeError("seam removed")
        scope.probe = probe
        async def command(config, action, value, timeout):
            if action == "watch":
                return 0, json.dumps({"policy_revision": "b" * 64, "interval_ms": 2000}).encode()
            if action == "check":
                report.set_result(value)
                return 1, b'{}'
            return 0, b'{}'
        real_sleep = asyncio.sleep
        async def tick(delay):
            self.assertEqual(delay, 2)
            await real_sleep(0)
        with patch.object(aw_scope, "command", command), patch.object(aw_scope.asyncio, "sleep", tick):
            await scope.watch()
            installed = False
            value = await asyncio.wait_for(report, 1)
            self.assertFalse(value["valid"])
            await scope.close()
        self.assertTrue(scope.failed)
        self.assertTrue(scope.monitor.done())

    async def test_concurrent_close_joins_failure_exactly_once(self):
        scope = self.scope()
        entered, gate = asyncio.Event(), asyncio.Event()
        calls = []
        async def command(config, action, value, timeout):
            calls.append(action)
            entered.set()
            await gate.wait()
            return 1, b'{}'
        with patch.object(aw_scope, "command", command):
            first = asyncio.create_task(scope.close())
            await entered.wait()
            second = asyncio.create_task(scope.close())
            gate.set()
            results = await asyncio.gather(first, second, return_exceptions=True)
            self.assertTrue(all(isinstance(result, RuntimeError) for result in results))
            with self.assertRaises(RuntimeError):
                await scope.close()
        self.assertEqual(calls, ["close"])

    async def test_close_waits_for_late_open_without_acknowledgement(self):
        scope = aw_scope.Scope({})
        entered, gate = asyncio.Event(), asyncio.Event()
        calls = []
        async def command(config, action, value, timeout):
            calls.append(action)
            if action == "open":
                entered.set()
                await gate.wait()
            return 0, b'{}'
        with patch.object(aw_scope, "command", command):
            opening = asyncio.create_task(scope.open("/tmp", {}))
            await entered.wait()
            closing = asyncio.create_task(scope.close())
            await asyncio.sleep(0)
            await asyncio.sleep(0)
            gate.set()
            with self.assertRaises(RuntimeError):
                await opening
            await closing
            with self.assertRaises(RuntimeError):
                await scope.open("/tmp", {})
        self.assertEqual(calls, ["open", "close"])

    async def test_cancelled_close_waiter_keeps_cleanup_owned(self):
        scope = self.scope()
        entered, gate = asyncio.Event(), asyncio.Event()
        async def command(*args):
            entered.set()
            await gate.wait()
            return 0, b'{}'
        with patch.object(aw_scope, "command", command):
            first = asyncio.create_task(scope.close())
            await entered.wait()
            first.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await first
            self.assertFalse(scope.close_task.done())
            gate.set()
            await scope.close()
        self.assertTrue(scope.close_task.done())


if __name__ == "__main__":
    unittest.main()
