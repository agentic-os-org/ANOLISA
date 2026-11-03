"""Real Claude SDK callback budgets, payloads and preserved native behavior."""

from __future__ import annotations
import json
import os
from pathlib import Path
import sys
import tempfile
from typing import Any
from unittest.mock import patch


async def check_callback_timeouts(output: Path) -> None:
    """Exercise real callback deadlines, failure actions and child cleanup on Python 3.10+."""
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "crates/aw-adapter/native/claude-sdk"))
    from aw_claude import Bridge

    case = Path(tempfile.mkdtemp(prefix="callback-timeouts-", dir=output))
    trace = case / "children.jsonl"
    executable = case / "hook.py"
    executable.write_text(
        f"#!{sys.executable}\n"
        "import json, os, sys, time\n"
        "step = sys.argv[sys.argv.index('--step') + 1]\n"
        f"with open({str(trace)!r}, 'a') as log:\n"
        "    log.write(json.dumps({'pid': os.getpid(), 'command': sys.argv, "
        "'cwd': os.getcwd(), 'ports': [], 'timeout_seconds': 3.001, "
        "'stop_command': f'kill -KILL -- -{os.getpid()}'}) + '\\n')\n"
        "if step == 'slow': time.sleep(60)\n"
        "print('{}')\n"
    )
    executable.chmod(0o700)
    for native, on_error in (
        ("PreToolUse", "block"),
        ("PreToolUse", "report"),
        ("PostToolUse", "block"),
    ):
        event = "tool.before" if native == "PreToolUse" else "tool.after"
        config = case / "config.json"
        config.write_text(
            json.dumps(
                {
                    "executable": str(executable),
                    "binding": "test",
                    "ready_path": str(case / "ready"),
                    "ready_token": "test",
                    "hooks": [
                        dict(
                            event=event,
                            step=step,
                            on_error=on_error,
                            budget_ms=1,
                            replace_result=False,
                        )
                        for step in ("slow", "later")
                    ],
                }
            )
        )
        trace.write_text("")
        with patch.dict(os.environ, AW_CLAUDE_CONFIG=str(config)):
            result = await Bridge().callback({"hook_event_name": native}, None, {})
        blocked = native == "PreToolUse" and on_error == "block"
        if blocked:
            assert result["hookSpecificOutput"]["permissionDecision"] == "deny"
        else:
            assert result == {}
        children = [json.loads(line) for line in trace.read_text().splitlines()]
        assert len(children) == (1 if blocked else 2), children
        for child in children:
            try:
                os.kill(child["pid"], 0)
            except ProcessLookupError:
                continue
            raise AssertionError(f"Callback child remains: {child}")
        (case / f"{native}-{on_error}.json").write_text(json.dumps(children, indent=2))
    print("3 real callback timeout/failure-action/cleanup scenarios passed")


async def check_callback_payloads(output: Path) -> None:
    """Keep serializable nested inputs intact and apply failure actions at the JSON limit."""
    from aw_claude import Bridge

    case = Path(tempfile.mkdtemp(prefix="callback-payloads-", dir=output))
    executable = case / "hook.py"
    executable.write_text(
        f"#!{sys.executable}\n"
        "import json, sys\n"
        "value = json.load(sys.stdin)['tool_input']\n"
        "depth = 0\n"
        "while 'nested' in value:\n"
        "    depth += 1\n"
        "    value = value['nested']\n"
        "assert depth == 500\n"
        "print('{}')\n"
    )
    executable.chmod(0o700)
    for depth in (500, 10000):
        nested = {}
        for _ in range(depth):
            nested = {"nested": nested}
        for native, on_error in (
            ("PreToolUse", "block"),
            ("PreToolUse", "report"),
            ("PostToolUse", "report"),
        ):
            bridge = object.__new__(Bridge)
            bridge.config = {
                "executable": str(executable),
                "binding": "test",
                "hooks": [
                    dict(
                        event="tool.before" if native == "PreToolUse" else "tool.after",
                        step=step,
                        on_error=on_error,
                        budget_ms=1000,
                        replace_result=False,
                    )
                    for step in ("first", "later")
                ],
            }
            payload = {"hook_event_name": native, "tool_input": nested}
            with patch("aw_claude.logging.Logger.error") as error:
                result = await bridge.callback(payload, None, {})
            blocked = depth == 10000 and on_error == "block"
            if blocked:
                assert result["hookSpecificOutput"]["permissionDecision"] == "deny"
            else:
                assert result == {}
            assert error.call_count == (0 if depth == 500 else 1 if blocked else 2)
            assert all(call.args[3] == "RecursionError" for call in error.call_args_list)
            assert payload["tool_input"] is nested
    print("6 callback nested-payload/failure-action scenarios passed")


async def check_preserved_callbacks() -> None:
    """Preserve native callbacks and isolate mutations for SDK-decodable nested JSON."""
    from aw_claude import Bridge
    from claude_agent_sdk import (
        ClaudeAgentOptions,
        HookMatcher,
        PermissionResultAllow,
        PermissionResultDeny,
    )

    original = json.loads('{"nested":' * 500 + "{}" + "}" * 500)
    before_deny = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "original hook veto",
        }
    }
    for mode in ("before", "after", "permission-deny", "permission-allow", "permission-rewrite"):
        calls = []

        def observe(value: dict[str, Any]) -> None:
            calls.append(mode)
            for _ in range(500):
                value = value["nested"]
            assert value == {}
            if mode != "permission-allow":
                value["changed"] = True

        async def hook(payload: Any, call_id: Any, context: Any) -> Any:
            observe(payload["tool_input"])
            return before_deny if mode == "before" else {}

        async def permission(name: Any, tool_input: Any, context: Any) -> Any:
            observe(tool_input)
            if mode == "permission-deny":
                return PermissionResultDeny(message="original permission veto")
            return PermissionResultAllow(updated_input=tool_input)

        bridge = object.__new__(Bridge)
        bridge.config = {
            "hooks": [
                {"event": event, "budget_ms": 1000, "replace_result": event == "tool.after"}
                for event in ("tool.before", "tool.after")
            ]
        }
        options = bridge.options(
            ClaudeAgentOptions(
                setting_sources=[],
                hooks={
                    event: [HookMatcher(hooks=[hook])] for event in ("PreToolUse", "PostToolUse")
                },
                can_use_tool=permission,
            )
        )
        if mode in {"before", "after"}:
            event = "PreToolUse" if mode == "before" else "PostToolUse"
            result = await options.hooks[event][0].hooks[0]({"tool_input": original}, "id", {})
            assert result == (before_deny if mode == "before" else {})
        else:
            result = await options.can_use_tool("custom", original, None)
            if mode == "permission-allow":
                assert isinstance(result, PermissionResultAllow)
            else:
                assert isinstance(result, PermissionResultDeny)
                assert result.message == (
                    "original permission veto"
                    if mode == "permission-deny"
                    else "Tool argument rewriting is unsupported with AW"
                )
        assert calls == [mode]
        assert json.dumps(original) == '{"nested": ' * 500 + "{}" + "}" * 500
    print("5 preserved callback/nested-input/isolation scenarios passed")
