#!/usr/bin/env python3
"""Regression tests for validate_mcp.py against degenerate existing configs.

The existing config file may itself carry a non-object ``mcpServers``
(JSON null, or a list). merge() called .update() on it and crashed with
AttributeError; check() called .items() on it and crashed the same way.
Both paths must fail (or merge safely) with a clean message and exit 1.
"""

import os
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "..", "src", "os-skills", "ai", "setup-mcp", "scripts",
    "validate_mcp.py",
)

NEW = '{"mcpServers": {"fetch": {"command": "uvx", "args": ["mcp-server-fetch"]}}}'


class TestExistingMcpServersShape(unittest.TestCase):
    def test_merge_survives_null_mcp_servers(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg = os.path.join(tmp, "config.json")
            with open(cfg, "w") as f:
                f.write('{"mcpServers": null}')
            r = subprocess.run([sys.executable, SCRIPT, NEW, "--merge", cfg],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            with open(cfg) as f:
                merged = __import__("json").load(f)
            self.assertIn("fetch", merged["mcpServers"])

    def test_merge_survives_list_mcp_servers(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg = os.path.join(tmp, "config.json")
            with open(cfg, "w") as f:
                f.write('{"mcpServers": ["broken"]}')
            r = subprocess.run([sys.executable, SCRIPT, NEW, "--merge", cfg],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            with open(cfg) as f:
                merged = __import__("json").load(f)
            self.assertIn("fetch", merged["mcpServers"])

    def test_check_rejects_list_mcp_servers_cleanly(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg = os.path.join(tmp, "config.json")
            with open(cfg, "w") as f:
                f.write('{"mcpServers": [{"command": "x"}]}')
            r = subprocess.run([sys.executable, SCRIPT, "--check", cfg],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 1)
            self.assertNotIn("Traceback", r.stderr)
            self.assertIn("mcpServers", r.stderr)

    def test_check_still_accepts_normal_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg = os.path.join(tmp, "config.json")
            with open(cfg, "w") as f:
                f.write('{"mcpServers": {"fetch": {"command": "uvx"}}}')
            r = subprocess.run([sys.executable, SCRIPT, "--check", cfg],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("OK", r.stdout)


if __name__ == "__main__":
    unittest.main()
