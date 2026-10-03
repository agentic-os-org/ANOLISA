#!/usr/bin/env python3
"""Adapter contract suite for the common hooks (roadmap §5.6).

Every migrated adapter must behave correctly for the five behavior
classes — passthrough, replacement, no-savings, timeout, and malformed
input (both malformed hook stdin and malformed core stdout) — and must
start at most one Tokenless subprocess per invocation. The core is the
mock protocol binary in tests/contract/mock_tokenless.py, so this suite
tests the adapters' envelope translation, not compression itself.

Later adapter migrations extend the agent matrices below rather than
adding new suites.
"""

import json
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "contract"))

import contract_runner
import corpus

# Behavior classes whose envelope must be a plain skip on every replacement
# host: the hook fails open and emits the original unchanged.
FAIL_OPEN_BEHAVIORS = [
    "no_savings",
    "passthrough",
    "error_disposition",
    "nonzero_exit",
    "malformed_stdout",
]

PRE_TOOL_AGENTS = {
    "claude-code": {"TOKENLESS_AGENT_ID": "claude-code"},
    "qoder-cli": {"TOKENLESS_AGENT_ID": "qoder-cli"},
    "opencode": {"TOKENLESS_AGENT_ID": "opencode"},
    "qwencode": {"TOKENLESS_AGENT_ID": "qwencode"},
    "cosh-ng": {"COSH_NG_VERSION": "0.5.0"},
}


def load_fixture(kind: str, name: str) -> str:
    with open(corpus.fixture_path(kind, name)) as f:
        return f.read()


def mock_applied_output(content: str) -> str:
    """The deterministic transform mock_tokenless.py applies."""
    data = json.loads(content)
    if isinstance(data, str):
        data = json.loads(data)

    def truncate(value):
        if isinstance(value, str):
            return value[:20] if len(value) > 20 else value
        if isinstance(value, list):
            return [truncate(item) for item in value]
        if isinstance(value, dict):
            return {key: truncate(item) for key, item in value.items()}
        return value

    return json.dumps(truncate(data), separators=(",", ":"), ensure_ascii=False)


class PreToolHookContract(unittest.TestCase):
    def run_case(self, agent: str, behavior: str | None):
        payload = json.dumps(
            {
                "session_id": "session-1",
                "tool_use_id": "call-1",
                "tool_call_id": "call-1",
                "tool_name": "Bash",
                "tool_input": {"command": "grep error log"},
            }
        )
        return contract_runner.run_case(
            corpus.PRE_TOOL_HOOK,
            payload,
            PRE_TOOL_AGENTS[agent],
            behavior,
        )

    def test_replacement(self):
        expected = {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "tool_input": {"command": "/mock/rtk grep error log"},
                "updatedInput": {"command": "/mock/rtk grep error log"},
            }
        }
        for agent in PRE_TOOL_AGENTS:
            with self.subTest(agent=agent):
                result = self.run_case(agent, "applied")
                self.assertEqual(result.envelope, expected)
                self.assertEqual(result.spawns, ["compress"])
                self.assertEqual(result.requests[0]["operation"], "pre_tool")

    def test_fail_open_classes_pass_through(self):
        for agent in PRE_TOOL_AGENTS:
            for behavior in FAIL_OPEN_BEHAVIORS:
                with self.subTest(agent=agent, behavior=behavior):
                    result = self.run_case(agent, behavior)
                    self.assertEqual(result.envelope, {})
                    self.assertEqual(result.spawns, ["compress"])

    def test_missing_binary_passes_through_without_spawning(self):
        for agent in PRE_TOOL_AGENTS:
            with self.subTest(agent=agent):
                result = self.run_case(agent, None)
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, [])


class ResponseHookContract(unittest.TestCase):
    maxDiff = None

    # Replacement hosts and how an applied output lands in their envelope.
    # qwencode is additionalContext-only: Core sees that it cannot replace
    # output and returns passthrough, but the hook still makes its one v2 call.
    REPLACEMENT_AGENTS = ["claude-code", "qoder-cli", "opencode", "cosh-ng"]

    def setUp(self):
        self.fixture = load_fixture("post_tool", "api_records")
        payload = json.loads(self.fixture)
        self.content = json.dumps(
            payload["tool_response"], separators=(",", ":"), ensure_ascii=False
        )

    def run_case(self, agent: str, behavior):
        return contract_runner.run_case(
            corpus.RESPONSE_HOOK,
            self.fixture,
            corpus.RESPONSE_AGENTS[agent],
            behavior,
        )

    def expected_replacement(self, agent: str) -> dict:
        output_text = mock_applied_output(self.content)
        if agent == "cosh-ng":
            value_key, value = "updatedToolResponse", output_text
        elif agent == "qoder-cli":
            value_key, value = "updatedToolOutput", output_text
        else:
            value_key, value = "updatedToolOutput", json.loads(output_text)
        return {
            "suppressOutput": True,
            "hookSpecificOutput": {"hookEventName": "PostToolUse", value_key: value},
        }

    def test_replacement(self):
        for agent in self.REPLACEMENT_AGENTS:
            with self.subTest(agent=agent):
                result = self.run_case(agent, "applied")
                self.assertEqual(result.envelope, self.expected_replacement(agent))
                self.assertEqual(result.spawns, ["compress"])
                self.assertEqual(
                    result.requests[0]["input"]["capabilities"]["recovery"]["kind"], "shell"
                )

    def test_retrieve_command_is_bypassed(self):
        marker = "<<tokenless:0123456789abcdef01234567>>"
        payload = json.loads(self.fixture)
        payload.update(
            {
                "tool_name": "Bash",
                "tool_input": {"command": f"tokenless retrieve '{marker}'"},
            }
        )
        for agent in self.REPLACEMENT_AGENTS:
            with self.subTest(agent=agent):
                result = contract_runner.run_case(
                    corpus.RESPONSE_HOOK,
                    json.dumps(payload),
                    corpus.RESPONSE_AGENTS[agent],
                    "applied",
                )
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, ["compress"])
                request = result.requests[0]["input"]
                self.assertEqual(request["result_kind"], "retrieve")
                self.assertEqual(request["capabilities"]["recovery"]["kind"], "none")

    def test_fallback_binary_does_not_advertise_marker_recovery(self):
        result = contract_runner.run_case(
            corpus.RESPONSE_HOOK,
            self.fixture,
            corpus.RESPONSE_AGENTS["claude-code"],
            "applied",
            tokenless_on_path=False,
        )

        self.assertEqual(result.spawns, ["compress"])
        self.assertEqual(result.requests[0]["input"]["capabilities"]["recovery"]["kind"], "none")

    def test_failed_retrieve_command_remains_a_tool_error(self):
        marker = "<<tokenless:0123456789abcdef01234567>>"
        payload = json.loads(self.fixture)
        payload.update(
            {
                "tool_name": "Bash",
                "tool_input": {"command": f"tokenless retrieve '{marker}'"},
                "is_error": True,
            }
        )
        result = contract_runner.run_case(
            corpus.RESPONSE_HOOK,
            json.dumps(payload),
            corpus.RESPONSE_AGENTS["claude-code"],
            "passthrough",
        )
        request = result.requests[0]["input"]
        self.assertEqual(request["status"], "error")
        self.assertEqual(request["result_kind"], "tool")
        self.assertEqual(request["capabilities"]["recovery"]["kind"], "none")

    def test_fail_open_classes_pass_through(self):
        for agent in self.REPLACEMENT_AGENTS:
            for behavior in FAIL_OPEN_BEHAVIORS:
                with self.subTest(agent=agent, behavior=behavior):
                    result = self.run_case(agent, behavior)
                    self.assertEqual(result.envelope, {})
                    self.assertEqual(result.spawns, ["compress"])

    def test_additive_host_declares_passthrough_capability(self):
        for behavior in ["applied"] + FAIL_OPEN_BEHAVIORS:
            with self.subTest(behavior=behavior):
                result = self.run_case("qwencode", behavior)
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, ["compress"])
                if result.requests:
                    self.assertEqual(
                        result.requests[0]["input"]["capabilities"]["recovery"], {"kind": "none"}
                    )

    def test_missing_binary_passes_through(self):
        for agent in self.REPLACEMENT_AGENTS:
            with self.subTest(agent=agent):
                result = self.run_case(agent, None)
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, [])

    def test_malformed_hook_stdin_passes_through(self):
        for agent in self.REPLACEMENT_AGENTS + ["qwencode"]:
            with self.subTest(agent=agent):
                result = contract_runner.run_case(
                    corpus.RESPONSE_HOOK,
                    "this is not JSON {{",
                    corpus.RESPONSE_AGENTS[agent],
                    "applied",
                )
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, [])

    def test_timeout_kills_the_subprocess_and_passes_through(self):
        # One representative agent: the timeout class costs a real 8-second
        # wait per case (the hook's subprocess timeout must fire).
        result = self.run_case("claude-code", "timeout")
        self.assertEqual(result.envelope, {})
        self.assertEqual(result.spawns, ["compress"])


class SchemaHookContract(unittest.TestCase):
    maxDiff = None

    AGENTS = ["qwencode", "cosh-ng"]

    def setUp(self):
        self.fixture = load_fixture("before_model", "tools_canonical")
        payload = json.loads(self.fixture)
        self.tools = payload["llm_request"]["config"]["tools"]
        self.content = json.dumps(self.tools, separators=(",", ":"))

    def run_case(self, agent: str, behavior):
        return contract_runner.run_case(
            corpus.SCHEMA_HOOK,
            self.fixture,
            corpus.SCHEMA_AGENTS[agent],
            behavior,
        )

    def envelope_with(self, tools) -> dict:
        return {
            "hookSpecificOutput": {
                "hookEventName": "BeforeModel",
                "llm_request": {"config": {"tools": tools}},
            }
        }

    def test_replacement(self):
        expected = self.envelope_with(json.loads(mock_applied_output(self.content)))
        for agent in self.AGENTS:
            with self.subTest(agent=agent):
                result = self.run_case(agent, "applied")
                self.assertEqual(result.envelope, expected)
                self.assertEqual(result.spawns, ["compress"])
                self.assertEqual(
                    result.requests[0]["input"]["capabilities"]["recovery"]["kind"], "none"
                )

    def test_no_savings_wraps_the_original(self):
        # The historical schema-hook behavior: a well-formed response whose
        # output is the original array is wrapped exactly like a win.
        expected = self.envelope_with(self.tools)
        for agent in self.AGENTS:
            for behavior in ["no_savings", "passthrough"]:
                with self.subTest(agent=agent, behavior=behavior):
                    result = self.run_case(agent, behavior)
                    self.assertEqual(result.envelope, expected)
                    self.assertEqual(result.spawns, ["compress"])

    def test_failure_classes_pass_through(self):
        for agent in self.AGENTS:
            for behavior in ["error_disposition", "nonzero_exit", "malformed_stdout"]:
                with self.subTest(agent=agent, behavior=behavior):
                    result = self.run_case(agent, behavior)
                    self.assertEqual(result.envelope, {})
                    self.assertEqual(result.spawns, ["compress"])

    def test_missing_binary_and_malformed_stdin_pass_through(self):
        for agent in self.AGENTS:
            with self.subTest(agent=agent, case="missing"):
                result = self.run_case(agent, None)
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, [])
            with self.subTest(agent=agent, case="malformed-stdin"):
                result = contract_runner.run_case(
                    corpus.SCHEMA_HOOK,
                    "this is not JSON {{",
                    corpus.SCHEMA_AGENTS[agent],
                    "applied",
                )
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, [])

    def test_timeout_kills_the_subprocess_and_passes_through(self):
        result = self.run_case("qwencode", "timeout")
        self.assertEqual(result.envelope, {})
        self.assertEqual(result.spawns, ["compress"])


if __name__ == "__main__":
    unittest.main()
