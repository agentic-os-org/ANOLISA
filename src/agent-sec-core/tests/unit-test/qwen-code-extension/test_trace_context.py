"""Contract tests for the shared Qwen Code trace-context helper."""

import ast
import json
import sys
from pathlib import Path
from types import ModuleType

from standalone_hook_test_loader import (
    load_module_from_path,
    load_standalone_hook,
)

_ROOT = Path(__file__).resolve().parents[3]
_HOOKS_DIR = _ROOT / "qwen-code-extension" / "hooks"
_HELPER_PATH = _HOOKS_DIR / "trace_context.py"
_CONSUMERS = (
    "code_scanner_hook.py",
    "observability_hook.py",
    "pii_checker_hook.py",
    "prompt_scanner_hook.py",
    "skill_ledger_hook.py",
)


trace_context_helper = load_module_from_path("qwen_trace_context_helper", _HELPER_PATH)


def test_trace_context_normalizes_identifiers_and_uses_canonical_precedence(
    monkeypatch,
):
    monkeypatch.setenv("QWEN_CODE_SESSION_ID", "environment-session")
    context = trace_context_helper.trace_context(
        {
            "trace_id": " trace-1 ",
            "session_id": " session-1 ",
            "run_id": "run-1",
            "turn_id": "fallback-run",
            "call_id": "call-1",
            "tool_call_id": "preferred-tool",
            "tool_use_id": "fallback-tool",
        }
    )

    assert context == {
        "agent_name": "qwen-code",
        "trace_id": "trace-1",
        "session_id": "session-1",
        "run_id": "run-1",
        "call_id": "call-1",
        "tool_call_id": "preferred-tool",
    }


def test_trace_context_uses_legacy_and_environment_fallbacks(monkeypatch):
    monkeypatch.setenv("QWEN_CODE_SESSION_ID", "s" * 300)

    context = trace_context_helper.trace_context(
        {
            "turn_id": "turn-1",
            "tool_use_id": "tool-use-1",
        }
    )

    assert context["agent_name"] == "qwen-code"
    assert context["run_id"] == "turn-1"
    assert context["tool_call_id"] == "tool-use-1"
    assert context["session_id"] == "s" * 256


def test_with_trace_context_inserts_one_compact_top_level_argument():
    command = trace_context_helper.with_trace_context(
        ["agent-sec-cli", "scan-pii", "--stdin"],
        {"session_id": "session-1"},
    )

    assert command[0:2] == ["agent-sec-cli", "--trace-context"]
    assert json.loads(command[2]) == {
        "agent_name": "qwen-code",
        "session_id": "session-1",
    }
    assert command[3:] == ["scan-pii", "--stdin"]


def test_hook_loader_isolates_and_restores_foreign_sibling_modules(monkeypatch):
    foreign_trace_context = ModuleType("trace_context")
    foreign_pii_text = ModuleType("pii_text")
    monkeypatch.setitem(sys.modules, "trace_context", foreign_trace_context)
    monkeypatch.setitem(sys.modules, "pii_text", foreign_pii_text)

    hook = load_standalone_hook(
        "qwen_observability_isolation_probe",
        _HOOKS_DIR / "observability_hook.py",
    )

    assert hook.trace_context({})["agent_name"] == "qwen-code"
    assert sys.modules["trace_context"] is foreign_trace_context
    assert sys.modules["pii_text"] is foreign_pii_text


def test_all_qwen_hooks_import_the_shared_trace_context_helper():
    assert not (_HOOKS_DIR / "qwen_trace_context.py").exists()

    for filename in _CONSUMERS:
        tree = ast.parse((_HOOKS_DIR / filename).read_text(encoding="utf-8"))
        imports = {
            node.module for node in ast.walk(tree) if isinstance(node, ast.ImportFrom)
        }
        assert "trace_context" in imports, filename
        assert "qwen_trace_context" not in imports, filename
        assert not any(
            isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name == "_trace_context"
            for node in ast.walk(tree)
        ), filename
