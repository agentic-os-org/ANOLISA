"""Reject uncovered factory paths before launching a bundled runtime."""
from dataclasses import dataclass, field, replace
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, sys.argv.pop())
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

    def test_duplicate_identity_and_missing_runtime_receipt_fail_closed(self):
        self.compose()
        native = {"tool_use_id": "call", "tool_name": "Bash", "tool_input": {},
                  "session_id": "session", "cwd": "/work"}
        self.gate.record(native)
        self.gate.record(native)
        records = json.loads((self.root / "claude-final-records.json").read_text())
        self.assertTrue(records["call"]["ambiguous"])
        with self.assertRaises(FileNotFoundError):
            self.gate.ready()
        (self.root / "claude-final-ready.json").write_text('{"token":"wrong"}')
        with self.assertRaises(ValueError):
            self.gate.ready()


if __name__ == "__main__":
    unittest.main()
