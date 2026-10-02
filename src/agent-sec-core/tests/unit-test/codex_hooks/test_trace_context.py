"""Unit tests for codex-plugin/hooks/trace_context.py."""

import json
from pathlib import Path

from standalone_hook_test_loader import load_module_from_path

_HOOKS_DIR = (
    Path(__file__).resolve().parents[2]
    / ".."
    / "codex-plugin"
    / "hooks-plugin"
    / "hooks"
)
trace_context = load_module_from_path(
    "codex_trace_context",
    _HOOKS_DIR / "trace_context.py",
)


class TestTraceContext:
    """Tests for trace_context() function."""

    def test_returns_agent_name_for_empty_input(self):
        """Even with no trace fields, agent_name is always injected."""
        ctx = trace_context.trace_context({})
        assert ctx == {"agent_name": "codex"}

    def test_returns_agent_name_for_no_matching_fields(self):
        ctx = trace_context.trace_context({"foo": "bar", "baz": 123})
        assert ctx == {"agent_name": "codex"}

    def test_extracts_all_fields(self):
        data = {
            "trace_id": "t1",
            "session_id": "s1",
            "turn_id": "r1",
            "tool_use_id": "c1",
            "call_id": "c2",
        }
        ctx = trace_context.trace_context(data)
        assert ctx == {
            "agent_name": "codex",
            "trace_id": "t1",
            "session_id": "s1",
            "run_id": "r1",
            "call_id": "c2",
            "tool_call_id": "c1",
        }

    def test_skips_empty_string_fields(self):
        data = {
            "trace_id": "t1",
            "session_id": "",
            "run_id": "   ",
        }
        ctx = trace_context.trace_context(data)
        assert ctx == {"agent_name": "codex", "trace_id": "t1"}

    def test_skips_non_string_fields(self):
        data = {
            "trace_id": 123,
            "session_id": None,
            "run_id": "r1",
        }
        ctx = trace_context.trace_context(data)
        assert ctx == {"agent_name": "codex", "run_id": "r1"}

    def test_strips_whitespace(self):
        data = {"trace_id": "  t1  "}
        ctx = trace_context.trace_context(data)
        assert ctx == {"agent_name": "codex", "trace_id": "t1"}

    def test_partial_fields(self):
        data = {"trace_id": "t1", "session_id": "s1"}
        ctx = trace_context.trace_context(data)
        assert ctx == {"agent_name": "codex", "trace_id": "t1", "session_id": "s1"}

    def test_turn_id_is_preferred_over_legacy_run_id(self):
        data = {"turn_id": "turn-1", "run_id": "legacy-run"}
        ctx = trace_context.trace_context(data)
        assert ctx == {"agent_name": "codex", "run_id": "turn-1"}

    def test_legacy_run_id_remains_supported(self):
        ctx = trace_context.trace_context({"run_id": "legacy-run"})
        assert ctx == {"agent_name": "codex", "run_id": "legacy-run"}


class TestWithTraceContext:
    """Tests for with_trace_context() function."""

    def test_always_injects_agent_name(self):
        """Even with no trace fields, agent_name causes injection."""
        args = ["agent-sec-cli", "scan-code", "--code", "echo hi"]
        result = trace_context.with_trace_context(args, {})
        # agent_name is always present, so --trace-context is always injected
        assert "--trace-context" in result
        ctx_json = result[result.index("--trace-context") + 1]
        ctx = json.loads(ctx_json)
        assert ctx == {"agent_name": "codex"}

    def test_injects_trace_context_after_first_arg(self):
        args = ["agent-sec-cli", "scan-code", "--code", "echo hi"]
        data = {"trace_id": "t1", "session_id": "s1"}
        result = trace_context.with_trace_context(args, data)

        expected_ctx = json.dumps(
            {"agent_name": "codex", "trace_id": "t1", "session_id": "s1"},
            ensure_ascii=False,
            separators=(",", ":"),
        )
        assert result == [
            "agent-sec-cli",
            "--trace-context",
            expected_ctx,
            "scan-code",
            "--code",
            "echo hi",
        ]

    def test_preserves_original_args_list(self):
        """Ensure original list is not mutated."""
        args = ["agent-sec-cli", "scan-code"]
        original = args.copy()
        trace_context.with_trace_context(args, {"trace_id": "t1"})
        assert args == original

    def test_full_field_mapping(self):
        """Verify tool_use_id maps to tool_call_id in output."""
        data = {"tool_use_id": "tu1"}
        result = trace_context.with_trace_context(["cli", "cmd"], data)
        ctx_json = result[2]
        ctx = json.loads(ctx_json)
        assert "tool_call_id" in ctx
        assert ctx["tool_call_id"] == "tu1"
