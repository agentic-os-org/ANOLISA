"""MCP settings merges reject malformed targets without changing the file."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "ai/setup-mcp/scripts/validate_mcp.py"


class McpSettingsMergeTests(unittest.TestCase):
    def merge(self, existing, incoming):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "settings.json"
            before = json.dumps(existing).encode("utf-8")
            path.write_bytes(before)
            result = subprocess.run(
                [
                    sys.executable,
                    "-X",
                    "utf8",
                    str(SCRIPT),
                    json.dumps(incoming),
                    "--merge",
                    str(path),
                ],
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=10,
            )
            return result, before, path.read_bytes()

    def test_non_object_settings_target_is_clean_error_and_preserved(self):
        for value in (None, [], "private-setting", 3, False):
            with self.subTest(value=value):
                result, before, after = self.merge(
                    {"mcpServers": {"keep": {"command": "old"}}, "mcp": value},
                    {
                        "mcpServers": {"fresh": {"command": "new"}},
                        "mcp": {"timeout": 30},
                    },
                )
                self.assertEqual(result.returncode, 1)
                self.assertIn("ERROR:", result.stderr)
                self.assertIn("mcp", result.stderr)
                self.assertNotIn("Traceback", result.stderr)
                self.assertNotIn("private-setting", result.stderr)
                self.assertNotIn("Written to:", result.stderr)
                self.assertEqual(result.stdout, "")
                self.assertEqual(after, before)

    def test_object_settings_merge_preserves_unrelated_fields(self):
        result, _, after = self.merge(
            {
                "mcpServers": {"keep": {"command": "old"}},
                "mcp": {"timeout": 10, "keep": True},
                "other": [1],
            },
            {"mcpServers": {"fresh": {"command": "new"}}, "mcp": {"timeout": 30}},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(after),
            {
                "mcpServers": {"keep": {"command": "old"}, "fresh": {"command": "new"}},
                "mcp": {"timeout": 30, "keep": True},
                "other": [1],
            },
        )

    def test_absent_settings_mapping_is_created(self):
        result, _, after = self.merge(
            {},
            {"mcpServers": {"fresh": {"command": "new"}}, "mcp": {"timeout": 30}},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(after)["mcp"], {"timeout": 30})

    def test_server_only_merge_does_not_change_settings(self):
        result, _, after = self.merge(
            {"mcp": None},
            {"mcpServers": {"fresh": {"command": "new"}}},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(after),
            {
                "mcp": None,
                "mcpServers": {"fresh": {"command": "new"}},
            },
        )


if __name__ == "__main__":
    unittest.main()
