#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check the real MCP validation command's transport diagnostics and exit code."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate_mcp.py")


class TransportValidationTests(unittest.TestCase):
    def run_check(self, config):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "settings.json"
            path.write_text(json.dumps({"mcpServers": {"example": config}}), encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), "--check", str(path)],
                capture_output=True, text=True, encoding="utf-8",
                env={**os.environ, "PYTHONUTF8": "1"}, check=False)

    def test_invalid_transport_values_fail(self):
        for field in ("command", "httpUrl", "url"):
            for value in (None, "", " \t", 123, False, [], {}):
                with self.subTest(field=field, value=value):
                    result = self.run_check({field: value})
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(field, result.stderr)
                    self.assertNotIn("OK:", result.stdout)

    def test_documented_transport_forms_pass(self):
        for config in (
            {"command": "npx", "args": ["-y", "example-server"]},
            {"httpUrl": "https://mcp.example.com/v1"},
            {"url": "http://localhost:8080/sse"},
        ):
            with self.subTest(config=config):
                result = self.run_check(config)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("OK: 1 server(s)", result.stdout)

    def test_missing_transport_still_fails(self):
        result = self.run_check({"args": []})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing transport", result.stderr)


if __name__ == "__main__":
    unittest.main()
