"""Unit tests for security_middleware.lifecycle — pre/post/error hooks."""

import unittest
from typing import Any
from unittest.mock import patch

from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.backends.pii_scan import PiiScanBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.lifecycle import (
    _category_for,
    on_error,
    post_action,
    pre_action,
)
from agent_sec_cli.security_middleware.result import ActionResult


class DummyBackend(BaseBackend):
    def execute(self, ctx: RequestContext, **kwargs: Any) -> ActionResult:
        return ActionResult(success=True, data={})


class TestCategoryMapping(unittest.TestCase):
    def test_harden_maps_to_hardening(self):
        self.assertEqual(_category_for("harden"), "hardening")

    def test_sandbox_prehook_maps_to_sandbox(self):
        self.assertEqual(_category_for("sandbox_prehook"), "sandbox")

    def test_verify_maps_to_asset_verify(self):
        self.assertEqual(_category_for("verify"), "asset_verify")

    def test_summary_maps_to_summary(self):
        self.assertEqual(_category_for("summary"), "summary")

    def test_pii_scan_maps_to_pii_scan(self):
        self.assertEqual(_category_for("pii_scan"), "pii_scan")

    def test_unknown_action_falls_back_to_action_name(self):
        self.assertEqual(_category_for("custom_thing"), "custom_thing")


class TestPreAction(unittest.TestCase):
    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_pre_action_does_not_log(self, mock_log):
        ctx = RequestContext(action="harden")
        pre_action(ctx, {"mode": "scan"})
        mock_log.assert_not_called()


class TestPostAction(unittest.TestCase):
    def setUp(self):
        self.telemetry_patch = patch(
            "agent_sec_cli.security_middleware.lifecycle.record_security_event_telemetry"
        )
        self.mock_telemetry = self.telemetry_patch.start()
        self.addCleanup(self.telemetry_patch.stop)

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_post_action_logs_event(self, mock_log):
        ctx = RequestContext(action="harden", trace_id="t-123")
        result = ActionResult(success=True, data={"passed": 5})
        post_action(ctx, result, {"mode": "scan"}, DummyBackend())

        mock_log.assert_called_once()
        event = mock_log.call_args[0][0]
        self.assertEqual(event.event_type, "harden")
        self.assertEqual(event.category, "hardening")
        self.assertEqual(event.result, "succeeded")
        self.assertEqual(event.trace_id, "t-123")
        self.assertIn("request", event.details)
        self.assertIn("result", event.details)

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_post_action_marks_unsuccessful_action_result_as_failed(self, mock_log):
        ctx = RequestContext(action="code_scan", trace_id="t-failed-result")
        result = ActionResult(
            success=False,
            data={"ok": False, "verdict": "error"},
            error="scan error",
            exit_code=1,
            error_type="ErrUnsupportedLang",
        )

        post_action(ctx, result, {"code": "bad"}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.assertEqual(event.event_type, "code_scan")
        self.assertEqual(event.category, "code_scan")
        self.assertEqual(event.result, "failed")
        self.assertEqual(event.details["result"], {"ok": False, "verdict": "error"})
        self.assertEqual(event.details["error"], "scan error")
        self.assertEqual(event.details["error_type"], "ErrUnsupportedLang")
        self.assertEqual(event.details["exit_code"], 1)

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_post_action_maps_failed_result_to_event_result(self, mock_log):
        ctx = RequestContext(action="harden", trace_id="t-123")
        result = ActionResult(
            success=False,
            data={"passed": 20, "failed": 3, "total": 23},
            exit_code=1,
        )
        post_action(ctx, result, {"args": ["--scan"]}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.assertEqual(event.result, "failed")
        self.assertEqual(event.details["result"]["passed"], 20)

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_post_action_preserves_no_candidates_outcome(self, mock_log):
        ctx = RequestContext(action="verify", trace_id="t-no-candidates")
        result = ActionResult(
            success=True,
            data={
                "outcome": "no_candidates",
                "checked": 0,
                "passed": 0,
                "failed": 0,
            },
            exit_code=0,
        )

        post_action(ctx, result, {"skill": None}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.assertEqual(event.result, "succeeded")
        self.assertEqual(event.details["result"]["outcome"], "no_candidates")
        self.assertEqual(event.details["result"]["checked"], 0)

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_post_action_copies_request_tracing_to_security_event(self, mock_log):
        ctx = RequestContext(
            action="code_scan",
            trace_id="trace-1",
            session_id="session-1",
            run_id="run-1",
            call_id="call-1",
            tool_call_id="tool-1",
        )
        result = ActionResult(success=True, data={"passed": 5})

        post_action(ctx, result, {"mode": "scan"}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.assertEqual(event.trace_id, "trace-1")
        self.assertEqual(event.session_id, "session-1")
        self.assertEqual(event.run_id, "run-1")
        self.assertEqual(event.call_id, "call-1")
        self.assertEqual(event.tool_call_id, "tool-1")

    def test_post_action_writes_telemetry_for_security_event(self):
        ctx = RequestContext(action="code_scan", trace_id="trace-telemetry")
        result = ActionResult(success=True, data={"verdict": "pass"})

        with patch("agent_sec_cli.security_middleware.lifecycle.log_event") as mock_log:
            post_action(ctx, result, {"code": "echo ok"}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.mock_telemetry.assert_called_once_with(event, ctx)

    def test_post_action_passes_agent_name_to_telemetry(self):
        ctx = RequestContext(
            action="code_scan",
            trace_id="trace-telemetry",
            agent_name="hermes",
        )
        result = ActionResult(success=True, data={"verdict": "pass"})

        with patch("agent_sec_cli.security_middleware.lifecycle.log_event") as mock_log:
            post_action(ctx, result, {"code": "echo ok"}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.mock_telemetry.assert_called_once_with(event, ctx)

    def test_post_action_swallows_telemetry_failure(self):
        ctx = RequestContext(action="code_scan", trace_id="trace-telemetry-fail")
        result = ActionResult(success=True, data={"verdict": "pass"})

        self.mock_telemetry.side_effect = RuntimeError("telemetry failed")
        with patch("agent_sec_cli.security_middleware.lifecycle.log_event") as mock_log:
            post_action(ctx, result, {"code": "echo ok"}, DummyBackend())

        mock_log.assert_called_once()

    def test_post_action_log_event_failure_does_not_block_telemetry(self):
        ctx = RequestContext(action="code_scan", trace_id="trace-log-fail")
        result = ActionResult(success=True, data={"verdict": "pass"})

        with patch(
            "agent_sec_cli.security_middleware.lifecycle.log_event",
            side_effect=RuntimeError("log failed"),
        ):
            post_action(ctx, result, {"code": "echo ok"}, DummyBackend())

        self.mock_telemetry.assert_called_once()

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_pii_scan_event_redacts_request_and_result(self, mock_log):
        ctx = RequestContext(action="pii_scan", trace_id="t-pii")
        result = ActionResult(
            success=True,
            data={
                "ok": True,
                "verdict": "deny",
                "summary": {"total": 1},
                "findings": [
                    {
                        "type": "api_key",
                        "category": "credential",
                        "severity": "deny",
                        "confidence": 0.99,
                        "evidence_redacted": "sk-a...[REDACTED]...7890",
                        "span": {"start": 8, "end": 40},
                        "metadata": {"field": "api_key"},
                        "raw_evidence": "sk-abcdefghijklmnopqrstuvwxyz7890",
                    }
                ],
                "redacted_text": "api_key=sk-a...[REDACTED]...7890",
                "elapsed_ms": 1,
            },
        )
        post_action(
            ctx,
            result,
            {
                "text": "api_key=sk-abcdefghijklmnopqrstuvwxyz7890",
                "source": "manual",
                "raw_evidence": True,
            },
            PiiScanBackend(),
        )

        event = mock_log.call_args[0][0]
        details = event.details
        self.assertNotIn("text", details["request"])
        self.assertEqual(details["request"]["source"], "manual")
        self.assertIn("text_sha256", details["request"])
        self.assertNotIn("redacted_text", details["result"])
        self.assertNotIn("raw_evidence", details["result"]["findings"][0])
        self.assertNotIn(
            "sk-abcdefghijklmnopqrstuvwxyz7890",
            str(details),
        )


class TestOnError(unittest.TestCase):
    def setUp(self):
        self.telemetry_patch = patch(
            "agent_sec_cli.security_middleware.lifecycle.record_security_event_telemetry"
        )
        self.mock_telemetry = self.telemetry_patch.start()
        self.addCleanup(self.telemetry_patch.stop)

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_on_error_logs_event(self, mock_log):
        ctx = RequestContext(action="verify", trace_id="t-456")
        exc = RuntimeError("test error")
        on_error(ctx, exc, {"skill": "/path"}, DummyBackend())

        mock_log.assert_called_once()
        event = mock_log.call_args[0][0]
        self.assertEqual(event.event_type, "verify")
        self.assertEqual(event.category, "asset_verify")
        self.assertEqual(event.result, "failed")
        self.assertIn("error", event.details)
        self.assertEqual(event.details["error"], "test error")
        self.assertEqual(event.details["error_type"], "RuntimeError")

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_on_error_copies_request_tracing_to_security_event(self, mock_log):
        ctx = RequestContext(
            action="verify",
            trace_id="trace-1",
            session_id="session-1",
            run_id="run-1",
            call_id="call-1",
            tool_call_id="tool-1",
        )
        exc = RuntimeError("test error")

        on_error(ctx, exc, {"skill": "/path"}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.assertEqual(event.trace_id, "trace-1")
        self.assertEqual(event.session_id, "session-1")
        self.assertEqual(event.run_id, "run-1")
        self.assertEqual(event.call_id, "call-1")
        self.assertEqual(event.tool_call_id, "tool-1")

    def test_on_error_writes_telemetry_for_security_event(self):
        ctx = RequestContext(action="verify", trace_id="trace-error-telemetry")
        exc = RuntimeError("boom")

        with patch("agent_sec_cli.security_middleware.lifecycle.log_event") as mock_log:
            on_error(ctx, exc, {"skill": "/path"}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.mock_telemetry.assert_called_once_with(event, ctx)

    def test_on_error_passes_agent_name_to_telemetry(self):
        ctx = RequestContext(
            action="verify",
            trace_id="trace-error-telemetry",
            agent_name="cosh",
        )
        exc = RuntimeError("boom")

        with patch("agent_sec_cli.security_middleware.lifecycle.log_event") as mock_log:
            on_error(ctx, exc, {"skill": "/path"}, DummyBackend())

        event = mock_log.call_args[0][0]
        self.mock_telemetry.assert_called_once_with(event, ctx)

    def test_on_error_swallows_telemetry_failure(self):
        ctx = RequestContext(action="verify", trace_id="trace-error-telemetry-fail")
        exc = RuntimeError("boom")

        self.mock_telemetry.side_effect = RuntimeError("telemetry failed")
        with patch("agent_sec_cli.security_middleware.lifecycle.log_event") as mock_log:
            on_error(ctx, exc, {"skill": "/path"}, DummyBackend())

        mock_log.assert_called_once()

    def test_on_error_log_event_failure_does_not_block_telemetry(self):
        ctx = RequestContext(action="verify", trace_id="trace-error-log-fail")
        exc = RuntimeError("boom")

        with patch(
            "agent_sec_cli.security_middleware.lifecycle.log_event",
            side_effect=RuntimeError("log failed"),
        ):
            on_error(ctx, exc, {"skill": "/path"}, DummyBackend())

        self.mock_telemetry.assert_called_once()

    @patch("agent_sec_cli.security_middleware.lifecycle.log_event")
    def test_pii_scan_error_redacts_request(self, mock_log):
        ctx = RequestContext(action="pii_scan", trace_id="t-pii-error")
        exc = RuntimeError("boom")
        on_error(
            ctx,
            exc,
            {"text": "alice@example.com", "source": "user_input"},
            PiiScanBackend(),
        )

        event = mock_log.call_args[0][0]
        self.assertNotIn("text", event.details["request"])
        self.assertNotIn("alice@example.com", str(event.details))


if __name__ == "__main__":
    unittest.main()
