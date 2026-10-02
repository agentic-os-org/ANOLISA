"""Unit tests for security_middleware.context — RequestContext dataclass."""

import unittest
import uuid
from datetime import datetime

from agent_sec_cli.correlation_context import (
    TraceContext,
    clear_invocation_context_for_tests,
    clear_process_trace_context,
    init_invocation_context,
    init_process_trace_context,
)
from agent_sec_cli.security_middleware.context import RequestContext


class TestRequestContext(unittest.TestCase):
    def setUp(self):
        clear_process_trace_context()
        clear_invocation_context_for_tests()

    def tearDown(self):
        clear_process_trace_context()
        clear_invocation_context_for_tests()

    def test_auto_trace_id_is_valid_uuid(self):
        ctx = RequestContext(action="test")
        # Should not raise
        uuid.UUID(ctx.trace_id)

    def test_auto_timestamp_is_parseable(self):
        ctx = RequestContext(action="test")
        # ISO-8601 must be parseable
        dt = datetime.fromisoformat(ctx.timestamp)
        self.assertIsInstance(dt, datetime)

    def test_explicit_trace_id_preserved(self):
        ctx = RequestContext(action="test", trace_id="my-trace")
        self.assertEqual(ctx.trace_id, "my-trace")

    def test_explicit_timestamp_preserved(self):
        ctx = RequestContext(action="test", timestamp="2025-01-01T00:00:00+00:00")
        self.assertEqual(ctx.timestamp, "2025-01-01T00:00:00+00:00")

    def test_caller_defaults_to_empty(self):
        ctx = RequestContext(action="test")
        self.assertEqual(ctx.caller, "")

    def test_session_id_defaults_to_none(self):
        ctx = RequestContext(action="test")
        self.assertIsNone(ctx.session_id)

    def test_two_contexts_get_different_trace_ids(self):
        ctx1 = RequestContext(action="a")
        ctx2 = RequestContext(action="b")
        self.assertNotEqual(ctx1.trace_id, ctx2.trace_id)

    def test_uses_caller_trace_context_when_available(self):
        init_process_trace_context(
            TraceContext(
                trace_id="trace-1",
                session_id="session-1",
                run_id="run-1",
                call_id="call-1",
                tool_call_id="tool-1",
                agent_name="hermes",
            )
        )

        ctx = RequestContext(action="code_scan")

        self.assertEqual(ctx.trace_id, "trace-1")
        self.assertEqual(ctx.session_id, "session-1")
        self.assertEqual(ctx.run_id, "run-1")
        self.assertEqual(ctx.call_id, "call-1")
        self.assertEqual(ctx.tool_call_id, "tool-1")
        self.assertEqual(ctx.agent_name, "hermes")

    def test_generates_trace_id_when_caller_does_not_supply_one(self):
        init_process_trace_context(TraceContext(session_id="session-1"))

        ctx = RequestContext(action="code_scan")

        self.assertTrue(ctx.trace_id)
        self.assertEqual(ctx.session_id, "session-1")

    def test_explicit_tracing_fields_are_preserved(self):
        init_process_trace_context(
            TraceContext(session_id="process-session", agent_name="process-agent")
        )

        ctx = RequestContext(
            action="code_scan",
            trace_id="explicit-trace",
            session_id="explicit-session",
            run_id="explicit-run",
            call_id="explicit-call",
            tool_call_id="explicit-tool",
            agent_name="explicit-agent",
        )

        self.assertEqual(ctx.trace_id, "explicit-trace")
        self.assertEqual(ctx.session_id, "explicit-session")
        self.assertEqual(ctx.run_id, "explicit-run")
        self.assertEqual(ctx.call_id, "explicit-call")
        self.assertEqual(ctx.tool_call_id, "explicit-tool")
        self.assertEqual(ctx.agent_name, "explicit-agent")

    def test_invocation_id_comes_from_process_context(self):
        init_invocation_context()

        ctx = RequestContext(action="code_scan")

        self.assertTrue(ctx.invocation_id)

    def test_explicit_invocation_id_is_preserved(self):
        init_invocation_context()

        ctx = RequestContext(action="code_scan", invocation_id="explicit-invocation")

        self.assertEqual(ctx.invocation_id, "explicit-invocation")


if __name__ == "__main__":
    unittest.main()
