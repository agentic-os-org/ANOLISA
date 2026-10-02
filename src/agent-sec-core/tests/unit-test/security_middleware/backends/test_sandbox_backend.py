"""Unit tests for security_middleware.backends.sandbox — SandboxBackend."""

import unittest

from agent_sec_cli.security_middleware.backends.sandbox import SandboxBackend
from agent_sec_cli.security_middleware.context import RequestContext


class TestSandboxBackend(unittest.TestCase):
    def setUp(self):
        self.backend = SandboxBackend()
        self.ctx = RequestContext(action="sandbox_prehook")

    def test_execute_returns_decision_data(self):
        result = self.backend.execute(
            self.ctx,
            decision="allow",
            command="ls -la",
            reasons="trusted command",
            network_policy="none",
            cwd="/home/user",
        )
        self.assertTrue(result.success)
        self.assertEqual(result.data["decision"], "allow")
        self.assertEqual(result.data["command"], "ls -la")
        self.assertEqual(result.data["reasons"], "trusted command")
        self.assertEqual(result.data["network_policy"], "none")
        self.assertEqual(result.data["cwd"], "/home/user")

    def test_always_succeeds(self):
        result = self.backend.execute(self.ctx)
        self.assertTrue(result.success)

    def test_defaults_are_empty_strings(self):
        result = self.backend.execute(self.ctx)
        for key in ("decision", "command", "reasons", "network_policy", "cwd"):
            self.assertEqual(result.data[key], "", f"{key} should default to empty")

    def test_extra_kwargs_ignored(self):
        result = self.backend.execute(self.ctx, decision="deny", extra_field="ignored")
        self.assertTrue(result.success)
        self.assertNotIn("extra_field", result.data)


if __name__ == "__main__":
    unittest.main()
