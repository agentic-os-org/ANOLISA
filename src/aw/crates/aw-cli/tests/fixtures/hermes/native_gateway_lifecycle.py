"""Exercise Gateway admission settlement and session retirement boundaries."""

import asyncio
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from native_gateway import GatewayTests
from native_support import NativeFixture


class GatewayLifecycleTests(NativeFixture):
    bind_gateway = GatewayTests.bind_gateway

    def test_gateway_prime_rejects_zero_native_hooks(self):
        self.bind_gateway([("tool.before", "block"), ("tool.after", "observe")],
                          prime_only=True, missing_hook="*")

    def test_gateway_prime_rejects_missing_native_hook(self):
        self.bind_gateway([("tool.before", "block"), ("tool.after", "observe")],
                          prime_only=True, missing_hook="pre_tool_call")

    def test_empty_policy_still_registers_gateway_bridge(self):
        hooks, _ = self.bind_gateway([], prime_only=True)
        self.assertEqual(self.gateway_receipt["hooks"], 0)
        self.assertIn(hooks["pre_tool_call"], self.gateway_manager._hooks["pre_tool_call"])

    def setUp(self):
        super().setUp()
        from hermes_logging import _reset_queued_handlers
        self.addCleanup(_reset_queued_handlers)

    def test_settled_unknown_releases_before_next_queued_claim(self):
        observed = []

        async def run(_authority, _ref, _live, row):
            observed.append(row["request_id"])
            return (None, "settled_unknown") if len(observed) == 1 else ("done", "completed")

        self.bind_gateway([("tool.before", "observe")], turn_run=run, prime_only=True)
        from gateway.session_authority import SessionAuthority

        for request in ("first", "next"):
            asyncio.run(SessionAuthority._execute_claimed(
                SimpleNamespace(), SimpleNamespace(session_id="session"), None,
                {"request_id": request, "admission_id": request}))
        self.assertEqual(observed, ["first", "next"])

    def test_resolved_unknown_allows_next_claim(self):
        observed = []

        async def run(_authority, _ref, _live, row):
            observed.append(row["request_id"])
            if row["request_id"] == "unknown":
                raise RuntimeError("native outcome unknown")
            return "done", "completed"

        self.bind_gateway([("tool.before", "observe")], turn_run=run, prime_only=True)
        from gateway.session_authority import SessionAuthority
        authority = SimpleNamespace(db=object())
        ref = SimpleNamespace(session_id="session")
        with self.assertRaisesRegex(RuntimeError, "native outcome unknown"):
            asyncio.run(SessionAuthority._execute_claimed(
                authority, ref, None, {"request_id": "unknown", "admission_id": "old"}))
        with patch("hermes_state_runtime.get_session_admission", return_value={
                "target_session_id": "session", "status": "terminal"}):
            self.assertEqual(asyncio.run(SessionAuthority._execute_claimed(
                authority, ref, None, {"request_id": "next", "admission_id": "new"})),
                ("done", "completed"))
        self.assertEqual(observed, ["unknown", "next"])

    def test_resolved_unknown_allows_retire_and_unload(self):
        async def run(_authority, _ref, _live, _row):
            raise RuntimeError("native outcome unknown")

        self.bind_gateway([("tool.before", "observe")], turn_run=run, prime_only=True)
        from gateway.session_authority import SessionAuthority
        with self.assertRaisesRegex(RuntimeError, "native outcome unknown"):
            asyncio.run(SessionAuthority._execute_claimed(
                SimpleNamespace(), SimpleNamespace(session_id="session"), None,
                {"request_id": "unknown", "admission_id": "old"}))
        launch = __import__("os").environ["AW_HERMES_LAUNCH"]
        params = {"target_session_id": "session", "launch_path": launch,
                  "token": "fixture-token"}
        authority = SimpleNamespace(db=SimpleNamespace(
            get_session=lambda _session: {"ended_at": "closed"}))
        with (patch("gateway.run_api._ACTIVE_RUNNER", object()),
              patch("gateway.session_authorities.active_authority", return_value=authority),
              patch("hermes_state_runtime.get_session_admission", return_value={
                  "target_session_id": "session", "status": "terminal"}),
              patch("hermes_state_runtime.list_session_admissions", return_value=[])):
            self.assertEqual(self.gateway_rpc["retire"](params), {"retired": True})
        self.gateway_manager.unload()

    def test_changed_launch_snapshot_keeps_active_registration(self):
        async def run(_authority, _ref, _live, _row):
            raise RuntimeError("native outcome unknown")

        self.bind_gateway([("tool.before", "observe")], turn_run=run, prime_only=True)
        from gateway.session_authority import SessionAuthority

        with self.assertRaisesRegex(RuntimeError, "native outcome unknown"):
            asyncio.run(SessionAuthority._execute_claimed(
                SimpleNamespace(), SimpleNamespace(session_id="session"), None,
                {"request_id": "unknown", "admission_id": "unknown"}))
        launch = Path(__import__("os").environ["AW_HERMES_LAUNCH"])
        config = json.loads(launch.read_text())
        config["hooks"][0]["timeout"] += 1
        launch.write_text(json.dumps(config))
        with self.assertRaisesRegex(ValueError, "launch snapshot differs"):
            self.gateway_rpc["retire"]({
                "target_session_id": "session", "launch_path": str(launch),
                "token": "fixture-token"})
        with self.assertRaisesRegex(RuntimeError, "active turn"):
            self.gateway_manager.unload()

    def test_changed_launch_snapshot_keeps_pending_registration(self):
        self.bind_gateway([("tool.before", "observe")], prime_only=True)
        launch = Path(__import__("os").environ["AW_HERMES_LAUNCH"])
        config = json.loads(launch.read_text())
        config["hooks"][0]["timeout"] += 1
        launch.write_text(json.dumps(config))
        authority = SimpleNamespace(db=SimpleNamespace(
            get_session=lambda _session: {"ended_at": "closed"}))
        with (patch("gateway.run_api._ACTIVE_RUNNER", object()),
              patch("gateway.session_authorities.active_authority", return_value=authority),
              patch("hermes_state_runtime.list_session_admissions",
                    return_value=[{"status": "queued"}])):
            with self.assertRaisesRegex(ValueError, "launch snapshot differs"):
                self.gateway_rpc["retire"]({
                    "target_session_id": "session", "launch_path": str(launch),
                    "token": "fixture-token"})
        with self.assertRaisesRegex(RuntimeError, "registered session"):
            self.gateway_manager.unload()
