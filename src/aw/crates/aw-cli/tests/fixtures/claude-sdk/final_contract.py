"""Exercise the final gate and registered callbacks without a bundled runtime."""
import asyncio
from dataclasses import dataclass, field, replace
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import ModuleType
import unittest
from unittest.mock import patch

native = Path(sys.argv.pop()).resolve()
sys.path.insert(0, str(native))
from aw_final import FinalGate


@dataclass
class Options:
    model: str = "claude-sonnet-4-6"
    setting_sources: list = field(default_factory=list)
    settings: object = None
    resume: object = None
    continue_conversation: bool = False
    agents: object = None
    plugins: list = field(default_factory=list)
    tools: list = field(default_factory=lambda: ["Bash"])
    mcp_servers: dict = field(default_factory=lambda: {"local": {"type": "sdk"}})
    env: dict = field(default_factory=dict)
    cwd: str = "/work"
    hooks: dict = field(default_factory=dict)
    extra_args: dict = field(default_factory=dict)
    can_use_tool: object = None
    permission_prompt_tool_name: object = None
    cli_path: object = None


@dataclass
class Matcher:
    hooks: list
    timeout: float = 0


sdk = ModuleType("claude_agent_sdk")
sdk.ClaudeAgentOptions = Options
sdk.HookMatcher = Matcher
sdk.ClaudeSDKClient = sdk.PermissionResultDeny = object
sys.modules["claude_agent_sdk"] = sdk

from aw_claude import Bridge


class Factory(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.env = patch.dict(os.environ, {}, clear=True)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.gate = FinalGate({"ready_path": str(self.root / "ready"),
                              "ready_token": "token", "final_profile": {"sha256": {}}})
        self.addCleanup(self.gate.close)

    def compose(self, options=Options()):
        return self.gate.options(options, self.root / "_bundled/claude")

    def test_uncovered_configuration_is_rejected(self):
        for change in [{"setting_sources": None}, {"tools": None}, {"tools": ["Agent"]},
                       {"model": "bedrock/claude-sonnet-4-6"},
                       {"resume": "old"}, {"plugins": ["plugin"]},
                       {"mcp_servers": {"http": {"type": "http"}}},
                       {"env": {"BUN_OPTIONS": "--preload=other"}},
                       {"env": {"CLAUDE_CODE_USE_BEDROCK": "1"}}]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.compose(replace(Options(), **change))
        os.environ["AW_CLAUDE_RESIDENT"] = "1"
        with self.assertRaises(ValueError):
            self.compose()

    def test_preload_is_client_scoped_and_background_calls_are_denied(self):
        options = self.compose()
        self.assertIn("--preload=", options.env["BUN_OPTIONS"])
        self.assertNotIn("BUN_OPTIONS", os.environ)
        self.assertEqual(options.env["CLAUDE_CODE_DISABLE_BACKGROUND_TASKS"], "1")
        for name, background, supported in [("Bash", False, True), ("Bash", True, False),
                                            ("Agent", False, False), ("mcp__local__echo", False, True),
                                            ("mcp__other__echo", False, False)]:
            self.assertEqual(self.gate.supports_tool({"tool_name": name,
                "tool_input": {"run_in_background": background}}), supported)

    def test_compatible_model_ids_retain_explicit_selection(self):
        self.compose(replace(Options(), model="qwen3.8-flash",
                             env={"ANTHROPIC_BASE_URL": "https://model.test/apps/anthropic/"}))
        runtime = json.loads((self.root / "claude-final-runtime.json").read_text())
        self.assertEqual(runtime["origin"], "https://model.test")
        self.assertEqual(runtime["messages_path"], "/apps/anthropic/v1/messages")
        for model in ["auto", "default", "sonnet", "opus", "haiku", "model with spaces", "a/b"]:
            with self.subTest(model=model), self.assertRaises(ValueError):
                self.compose(replace(Options(), model=model))
        for base in ["https://user:secret@model.test", "https://model.test/?q=x", "https://model.test/%2e"]:
            with self.subTest(base=base), self.assertRaises(ValueError):
                self.compose(replace(Options(), env={"ANTHROPIC_BASE_URL": base}))

    def test_duplicate_identity_and_missing_runtime_receipt_fail_closed(self):
        self.compose()
        native = {"tool_use_id": "call", "tool_name": "Bash", "tool_input": {},
                  "session_id": "session", "cwd": "/work"}
        self.gate.record(native)
        self.gate.record(native)
        records = json.loads((self.root / "claude-final-records.json").read_text())["records"]
        self.assertTrue(records["call"]["ambiguous"])
        with self.assertRaises(FileNotFoundError):
            self.gate.ready()
        (self.root / "claude-final-ready.json").write_text('{"token":"wrong"}')
        with self.assertRaises(ValueError):
            self.gate.ready()


class BridgeCallbacks(unittest.IsolatedAsyncioTestCase):
    async def test_final_mode_preserves_native_callbacks_and_ordinary_routes(self):
        for mode in ("off", "shadow", "enforce"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                await self.check_mode(Path(directory), mode)

    async def check_mode(self, root, mode):
        sdk.__file__ = str(root / "__init__.py")
        bundled = root / "_bundled/claude"
        bundled.parent.mkdir()
        bundled.touch()
        enforcing = mode == "enforce"
        hooks = [
            {"event": event, "step": event, "on_error": "report",
             "budget_ms": 100, "replace_result": False}
            for event in ("tool.before", "tool.after")
        ]
        if mode != "off":
            hooks.append({**hooks[0], "event": "security.violation", "step": "final",
                          "on_error": "block" if enforcing else "report"})
        bridge = Bridge({"executable": "fixture", "binding": "fixture", "ready_token": "fixture",
                         "ready_path": str(root / "ready"), "hooks": hooks,
                         "final_profile": None if mode == "off" else {"sha256": {}},
                         "final_enforcing": enforcing})
        if bridge.final:
            self.addCleanup(bridge.final.close)
        seen = []

        async def transport(hook, payload):
            seen.append((hook["event"], payload["tool_use_id"]))
            return 0, b"{}"

        bridge._transport = transport
        native_output = {"systemMessage": "native callback"}

        async def business_callback(payload, call, context):
            return native_output

        options = bridge.options(Options(cwd=str(root), hooks={
            "PreToolUse": [Matcher([business_callback])],
        }))
        self.assertEqual(options.env.get("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS"), "1" if enforcing else None)

        async def invoke(event, call, background=False, matcher=-1):
            payload = {"hook_event_name": event, "tool_use_id": call, "tool_name": "Bash",
                       "tool_input": {"command": "true", "run_in_background": background},
                       "session_id": "session", "cwd": str(root)}
            return await options.hooks[event][matcher].hooks[0](payload, call, {})

        self.assertEqual(await invoke("PreToolUse", "foreground"), {})
        background = await invoke("PreToolUse", "background", background=True)
        token = ("tool.before", "background")
        if enforcing:
            self.assertEqual(background["hookSpecificOutput"]["permissionDecision"], "deny")
            self.assertNotIn(token, seen)
            with self.assertRaisesRegex(ValueError, "context injection"):
                await invoke("PreToolUse", "foreground", matcher=0)
        else:
            self.assertEqual(background, {})
            self.assertIn(token, seen)
            self.assertEqual(await invoke("PreToolUse", "background", background=True, matcher=0), native_output)
            self.assertEqual(await invoke("PostToolUse", "background", background=True), {})

        temporary = root / "claude-final-records.tmp"
        output = root / "claude-final-records.json"

        def read():
            data = json.loads(output.read_text())
            return data["records"] if enforcing else data

        def check_final(call, allowed):
            # The preload lives in a different process from this Python Bridge.
            helper = (native.parent / "shared/final-bridge-test.mjs").as_uri()
            script = f"""
                import {{checkFinalResult}} from {json.dumps(helper)};
                const [root, id, allowed] = process.argv.slice(1);
                await checkFinalResult(root, 'claude', id,
                    {{command:'true', run_in_background:false}}, allowed === 'true');
            """
            result = subprocess.run(["node", "--input-type=module", "-e", script, str(root), call,
                                     str(allowed).lower()], capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)

        start = 0
        if enforcing:
            self.assertEqual(await invoke("PostToolUse", "call-0"), {})
            check_final("call-0", True)
            temporary.mkdir()
            with self.assertRaises(IsADirectoryError):
                await invoke("PostToolUse", "call-0")
            self.assertNotIn("ambiguous", read()["call-0"])
            check_final("call-0", False)
            temporary.rmdir()
            self.assertEqual(await invoke("PostToolUse", "call-1"), {})
            self.assertTrue(read()["call-0"]["ambiguous"])
            check_final("call-0", False)
            check_final("call-1", True)
            self.assertEqual(seen.count(("tool.after", "call-0")), 1)
            start = 2
        else:
            temporary.mkdir()
            self.assertEqual(await invoke("PostToolUse", "write-failure"), {})
            temporary.rmdir()
        self.assertEqual(("tool.after", "write-failure") in seen, not enforcing)

        for number in range(start, 1024):
            self.assertEqual(await invoke("PostToolUse", f"call-{number}"), {})
        if enforcing:
            with self.assertRaisesRegex(ValueError, "native identity limit"):
                await invoke("PostToolUse", "overflow")
            self.assertEqual(await invoke("PostToolUse", "call-1023"), {})
            self.assertTrue(read()["call-1023"]["ambiguous"])
        else:
            self.assertEqual(await invoke("PostToolUse", "overflow"), {})
        self.assertEqual(("tool.after", "overflow") in seen, not enforcing)
        if mode == "off":
            self.assertFalse(output.exists())
        else:
            self.assertEqual(set(read()), {f"call-{n}" for n in range(1024)})


if __name__ == "__main__":
    unittest.main()
