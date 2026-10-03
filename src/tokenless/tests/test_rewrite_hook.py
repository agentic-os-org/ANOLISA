#!/usr/bin/env python3
"""Protocol v2 and cross-operation tests for the Common PreTool hook."""

import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
REPO_ROOT = TESTS_DIR.parent
CONTRACT_DIR = TESTS_DIR / "contract"
sys.path.insert(0, str(CONTRACT_DIR))

import contract_runner
import corpus

RESPONSE_HOOK = REPO_ROOT / "adapters/tokenless/common/hooks/compress_response_hook.py"
MOCK_TOKENLESS = CONTRACT_DIR / "mock_tokenless.py"
QWEN_MANIFEST = REPO_ROOT / "adapters/tokenless/qwencode/qwen-extension.json.in"


def pre_tool_payload(call_id: str = "call-1") -> dict:
    payload = {
        "session_id": "session-1",
        "tool_name": "Bash",
        "tool_input": {"command": "grep error log", "timeout": 30},
    }
    if call_id:
        payload["tool_use_id"] = call_id
    return payload


def post_tool_payload(call_id: str) -> dict:
    return {
        "session_id": "session-1",
        "tool_use_id": call_id,
        "tool_name": "Bash",
        "tool_response": {"output": "x" * 80},
    }


class PreToolContractTest(unittest.TestCase):
    def run_case(self, behavior: str | None, call_id: str = "call-1"):
        return contract_runner.run_case(
            corpus.PRE_TOOL_HOOK,
            json.dumps(pre_tool_payload(call_id)),
            {"TOKENLESS_AGENT_ID": "qoder-cli"},
            behavior,
        )

    def test_applied_rewrite_uses_protocol_v2(self) -> None:
        result = self.run_case("applied")
        self.assertEqual(result.spawns, ["compress"])
        self.assertEqual(
            result.envelope,
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "tool_input": {"command": "/mock/rtk grep error log"},
                    "updatedInput": {
                        "command": "/mock/rtk grep error log",
                        "timeout": 30,
                    },
                }
            },
        )
        self.assertEqual(len(result.requests), 1)
        request = result.requests[0]
        self.assertEqual(request["protocol_version"], 2)
        self.assertEqual(request["operation"], "pre_tool")
        self.assertEqual(
            request["attribution"],
            {
                "agent_id": "qoder-cli",
                "session_id": "session-1",
                "tool_use_id": "call-1",
            },
        )
        self.assertEqual(
            request["input"],
            {
                "tool_name": "Bash",
                "arguments": {"command": "grep error log", "timeout": 30},
                "command_field": "command",
                "capabilities": {
                    "replace_arguments": True,
                    "block_and_suggest": False,
                },
            },
        )

    def test_passthrough_and_failure_classes_fail_open(self) -> None:
        for behavior in [
            "no_savings",
            "passthrough",
            "error_disposition",
            "nonzero_exit",
            "malformed_stdout",
        ]:
            with self.subTest(behavior=behavior):
                result = self.run_case(behavior)
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, ["compress"])

    def test_missing_binary_and_missing_call_id_do_not_spawn(self) -> None:
        self.assertEqual(self.run_case(None).envelope, {})
        missing_id = self.run_case("applied", call_id="")
        self.assertEqual(missing_id.envelope, {})
        self.assertEqual(missing_id.spawns, [])



class WorkBuddyContractTest(unittest.TestCase):
    """WorkBuddy PreToolUse contract: modifiedInput needs a decision.

    WorkBuddy/CodeBuddy hosts apply modifiedInput only together with
    permissionDecision "allow" (official PreToolUse contract and its
    troubleshooting Q5). Protocol v2 never reports rtk's permission
    verdict to the hook, so the hook cannot attest the rewrite; the safe
    default passes the original command through and
    TOKENLESS_WORKBUDDY_AUTO_ALLOW=1 opts into the bypass.
    """

    def run_case(self, behavior: str | None, extra_env: dict | None = None):
        agent_env = {"TOKENLESS_AGENT_ID": "workbuddy"}
        if extra_env:
            agent_env.update(extra_env)
        return contract_runner.run_case(
            corpus.PRE_TOOL_HOOK,
            json.dumps(pre_tool_payload()),
            agent_env,
            behavior,
        )

    def test_rewrite_passes_through_without_opt_in(self) -> None:
        """Unattested-by-default: no bypass without the explicit opt-in."""
        result = self.run_case("applied")
        self.assertEqual(result.envelope, {})
        # The gate fires before Core: no compression subprocess, no state.
        self.assertEqual(result.spawns, [])
        self.assertEqual(result.requests, [])

    def test_auto_allow_opt_in_emits_modified_input(self) -> None:
        """TOKENLESS_WORKBUDDY_AUTO_ALLOW=1 opts into the documented bypass."""
        result = self.run_case(
            "applied", {"TOKENLESS_WORKBUDDY_AUTO_ALLOW": "1"}
        )
        self.assertEqual(result.spawns, ["compress"])
        self.assertEqual(
            result.envelope,
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "tool_input": {"command": "/mock/rtk grep error log"},
                    "updatedInput": {
                        "command": "/mock/rtk grep error log",
                        "timeout": 30,
                    },
                    # Partial override payload: only the rewritten field,
                    # not the full input.
                    "modifiedInput": {"command": "/mock/rtk grep error log"},
                    "permissionDecision": "allow",
                    "permissionDecisionReason": (
                        "Tokenless: rtk rewrite auto-allowed via "
                        "TOKENLESS_WORKBUDDY_AUTO_ALLOW "
                        "(host confirmation bypassed)"
                    ),
                }
            },
        )
        request = result.requests[0]
        self.assertEqual(request["attribution"]["agent_id"], "workbuddy")

    def test_auto_allow_works_with_documented_idless_contract(self) -> None:
        """The documented workbuddy HookInput carries no tool-call ID.

        The official CLI, IDE and Enterprise HookInput shapes contain
        only session_id, transcript_path, cwd, permission_mode,
        hook_event_name and the event fields — no tool_use_id /
        toolCallId / call_id — so the opt-in rewrite must not depend on
        one; replaying exactly that shape must reach Core and apply.
        """
        result = contract_runner.run_case(
            corpus.PRE_TOOL_HOOK,
            json.dumps(pre_tool_payload(call_id="")),
            {
                "TOKENLESS_AGENT_ID": "workbuddy",
                "TOKENLESS_WORKBUDDY_AUTO_ALLOW": "1",
            },
            "applied",
        )
        self.assertEqual(result.spawns, ["compress"])
        hook_out = result.envelope.get("hookSpecificOutput", {})
        self.assertEqual(
            hook_out.get("modifiedInput"),
            {"command": "/mock/rtk grep error log"},
        )
        self.assertEqual(hook_out.get("permissionDecision"), "allow")
        # No call ID: the attribution omits the field entirely, and no
        # PreTool->PostTool optimization mark is written (an ID-less mark
        # could never be consumed).
        request = result.requests[0]
        self.assertNotIn("tool_use_id", request["attribution"])

    def test_auto_allow_opt_in_keeps_fail_open_classes(self) -> None:
        for behavior in [
            "no_savings",
            "passthrough",
            "error_disposition",
            "nonzero_exit",
            "malformed_stdout",
        ]:
            with self.subTest(behavior=behavior):
                result = self.run_case(
                    behavior, {"TOKENLESS_WORKBUDDY_AUTO_ALLOW": "1"}
                )
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, ["compress"])

    def test_non_workbuddy_keeps_rewrite_without_decision(self) -> None:
        """Other agents keep the v2 rewrite; no WorkBuddy dialect fields.

        Their output carries no permissionDecision, so the host permission
        flow still applies to the rewritten command.
        """
        result = contract_runner.run_case(
            corpus.PRE_TOOL_HOOK,
            json.dumps(pre_tool_payload()),
            {"TOKENLESS_AGENT_ID": "claude-code"},
            "applied",
        )
        hook_out = result.envelope.get("hookSpecificOutput", {})
        self.assertEqual(
            hook_out.get("tool_input", {}).get("command"),
            "/mock/rtk grep error log",
        )
        self.assertNotIn("modifiedInput", hook_out)
        self.assertNotIn("permissionDecision", hook_out)


class HookLifecycleStateTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.home = root / "home"
        self.bin_dir = root / "bin"
        self.home.mkdir()
        self.bin_dir.mkdir()
        tokenless = self.bin_dir / "tokenless"
        shutil.copy(MOCK_TOKENLESS, tokenless)
        tokenless.chmod(tokenless.stat().st_mode | stat.S_IXUSR)
        self.request_log = root / "requests.jsonl"
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "PATH": f"{self.bin_dir}:/usr/bin:/bin",
            "LC_ALL": "C.UTF-8",
            "TOKENLESS_AGENT_ID": "qoder-cli",
            "TOKENLESS_STATS_ENABLED": "0",
            "TOKENLESS_SLS_ENABLED": "0",
            "TOKENLESS_MOCK_BEHAVIOR": "applied",
            "TOKENLESS_MOCK_REQUEST_LOG": str(self.request_log),
        }

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_hook(self, hook: Path, payload: dict, *hook_args: str, env: dict | None = None) -> dict:
        proc = subprocess.run(
            [sys.executable, str(hook), *hook_args],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            env=self.env if env is None else env,
            timeout=15,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout or "{}")

    def requests(self) -> list[dict]:
        with self.request_log.open() as request_log:
            return [json.loads(line) for line in request_log if line.strip()]

    def test_rtk_state_reaches_post_tool_once(self) -> None:
        rewritten = self.run_hook(corpus.PRE_TOOL_HOOK, pre_tool_payload())
        self.assertIn("hookSpecificOutput", rewritten)

        first = self.run_hook(RESPONSE_HOOK, post_tool_payload("call-1"))
        self.assertEqual(first, {})
        self.assertEqual(self.requests()[-1]["input"]["output_optimization"], "rtk")
        self.assertEqual(self.requests()[-1]["input"]["capabilities"]["recovery"]["kind"], "none")

        second = self.run_hook(RESPONSE_HOOK, post_tool_payload("call-1"))
        self.assertIn("hookSpecificOutput", second)
        self.assertEqual(self.requests()[-1]["input"]["output_optimization"], "none")

    def test_state_is_isolated_by_tool_call(self) -> None:
        self.run_hook(corpus.PRE_TOOL_HOOK, pre_tool_payload("call-a"))

        other = self.run_hook(RESPONSE_HOOK, post_tool_payload("call-b"))
        self.assertIn("hookSpecificOutput", other)
        self.assertEqual(self.requests()[-1]["input"]["output_optimization"], "none")

        matching = self.run_hook(RESPONSE_HOOK, post_tool_payload("call-a"))
        self.assertEqual(matching, {})
        self.assertEqual(self.requests()[-1]["input"]["output_optimization"], "rtk")

    def test_rewrite_is_not_applied_when_state_cannot_be_written(self) -> None:
        tokenless_dir = self.home / ".tokenless"
        tokenless_dir.mkdir()
        (tokenless_dir / "hook-state").write_text("not a directory")
        output = self.run_hook(corpus.PRE_TOOL_HOOK, pre_tool_payload())
        self.assertEqual(output, {})

    def test_command_agent_id_keeps_lifecycle_state_stable(self) -> None:
        env = {key: value for key, value in self.env.items() if key != "TOKENLESS_AGENT_ID"}
        self.run_hook(
            corpus.PRE_TOOL_HOOK,
            pre_tool_payload(),
            "--agent-id",
            "copilot-shell",
            env=env,
        )
        self.run_hook(
            RESPONSE_HOOK,
            post_tool_payload("call-1"),
            "--agent-id",
            "copilot-shell",
            env=env,
        )
        requests = self.requests()
        self.assertEqual(requests[-2]["attribution"]["agent_id"], "copilot-shell")
        self.assertEqual(requests[-1]["attribution"]["agent_id"], "copilot-shell")
        self.assertEqual(requests[-1]["input"]["output_optimization"], "rtk")


    def test_workbuddy_default_skip_writes_no_state(self) -> None:
        """The WorkBuddy default pass-through leaves the ledger untouched.

        Without TOKENLESS_WORKBUDDY_AUTO_ALLOW the PreTool gate fires
        before Core, so no rtk state is persisted and the PostTool hook
        still sees an unoptimized result.
        """
        env = {**self.env, "TOKENLESS_AGENT_ID": "workbuddy"}
        output = self.run_hook(
            corpus.PRE_TOOL_HOOK, pre_tool_payload(), env=env
        )
        self.assertEqual(output, {})

        self.run_hook(RESPONSE_HOOK, post_tool_payload("call-1"), env=env)
        self.assertEqual(len(self.requests()), 1)
        self.assertEqual(self.requests()[-1]["operation"], "post_tool")
        self.assertEqual(
            self.requests()[-1]["input"]["output_optimization"], "none"
        )

    def test_abandoned_state_is_bounded_on_next_rewrite(self) -> None:
        state_dir = self.home / ".tokenless" / "hook-state"
        state_dir.mkdir(parents=True)
        stale = state_dir / "stale"
        stale.write_text("rtk\n")
        stale_time = stale.stat().st_mtime - 25 * 60 * 60
        os.utime(stale, (stale_time, stale_time))
        claimed = state_dir / "claimed.consuming.1234"
        claimed.write_text("rtk\n")
        crashed = state_dir / "crashed.consuming.5678"
        crashed.write_text("rtk\n")
        os.utime(crashed, (stale_time, stale_time))
        for index in range(1025):
            (state_dir / f"abandoned-{index:04}").write_text("rtk\n")

        self.run_hook(corpus.PRE_TOOL_HOOK, pre_tool_payload())

        self.assertFalse(stale.exists())
        self.assertTrue(claimed.exists())
        self.assertFalse(crashed.exists())
        unclaimed = [path for path in state_dir.iterdir() if ".consuming." not in path.name]
        self.assertLessEqual(len(unclaimed), 1024)

    def test_qwen_lifecycle_commands_pin_agent_id(self) -> None:
        manifest = json.loads(QWEN_MANIFEST.read_text().replace("@VERSION@", "test"))
        rewrite = manifest["hooks"]["PreToolUse"][1]["hooks"][0]["command"]
        response = manifest["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
        self.assertIn("rewrite_hook.py --agent-id qwencode", rewrite)
        self.assertIn("compress_response_hook.py --agent-id qwencode", response)


@unittest.skipUnless(
    os.path.exists(corpus.DEBUG_TOKENLESS_BIN),
    "tokenless debug binary not built",
)
class RealCorePreToolTest(unittest.TestCase):
    def test_core_owns_rtk_execution_and_path_anchoring(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            bin_dir = root / "bin"
            home.mkdir()
            bin_dir.mkdir()
            os.symlink(corpus.DEBUG_TOKENLESS_BIN, bin_dir / "tokenless")
            rtk = bin_dir / "rtk"
            rtk.write_text(
                "#!/bin/sh\n"
                'if [ "$1" = "--version" ]; then echo \'rtk 0.43.0\'; exit 0; fi\n'
                'if [ "$1" = "rewrite" ]; then echo \'rtk grep --count error log\'; exit 0; fi\n'
                "exit 1\n"
            )
            rtk.chmod(rtk.stat().st_mode | stat.S_IXUSR)
            env = {
                **os.environ,
                "HOME": str(home),
                "PATH": f"{bin_dir}:/usr/bin:/bin",
                "TOKENLESS_AGENT_ID": "qoder-cli",
                "TOKENLESS_STATS_ENABLED": "0",
                "TOKENLESS_SLS_ENABLED": "0",
            }
            proc = subprocess.run(
                [sys.executable, corpus.PRE_TOOL_HOOK],
                input=json.dumps(pre_tool_payload()),
                capture_output=True,
                text=True,
                env=env,
                timeout=15,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            output = json.loads(proc.stdout)["hookSpecificOutput"]
            command = output["tool_input"]["command"]
            self.assertIn(f" {rtk} grep --count error log", command)
            self.assertIn("TOKENLESS_AGENT_ID=qoder-cli", command)
            self.assertEqual(output["updatedInput"]["timeout"], 30)


if __name__ == "__main__":
    unittest.main()
