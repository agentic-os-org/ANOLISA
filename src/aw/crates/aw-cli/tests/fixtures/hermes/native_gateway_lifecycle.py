"""Exercise Gateway admission settlement and session retirement boundaries."""

import asyncio
from types import SimpleNamespace
from unittest.mock import patch

from native_gateway import GatewayTests
from native_support import NativeFixture


class GatewayLifecycleTests(NativeFixture):
    bind_gateway = GatewayTests.bind_gateway

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
