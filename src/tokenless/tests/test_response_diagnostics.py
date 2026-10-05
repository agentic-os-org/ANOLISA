#!/usr/bin/env python3
"""Integration tests for the codex response-diagnostics input contract.

The hook's docstring promises: "Fail-open: every error path exits
successfully with empty stdout so a diagnostic failure never blocks the
agent session." These tests pin that contract for payloads that are valid
JSON but not an object — the same input class the sibling
compress_schema_hook.py handles explicitly.
"""

import json
import os
import subprocess
import sys
import unittest


def _hook_path() -> str:
    """Absolute path of the codex response-diagnostics script."""
    scripts_dir = os.path.normpath(os.path.join(
        os.path.dirname(__file__),
        os.pardir, "adapters", "tokenless", "codex", "scripts",
    ))
    return os.path.join(scripts_dir, "response-diagnostics")


def _run_hook_raw(stdin_text: str) -> subprocess.CompletedProcess:
    """Run the hook as a subprocess and return the CompletedProcess."""
    return subprocess.run(
        [sys.executable, _hook_path()],
        input=stdin_text,
        capture_output=True,
        text=True,
        timeout=10,
    )


class TestNonObjectPayload(unittest.TestCase):
    def test_array_payload_passes_through_silently(self):
        proc = _run_hook_raw("[1, 2, 3]")

        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, "")

    def test_scalar_payload_passes_through_silently(self):
        proc = _run_hook_raw('"just a string"')

        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, "")

    def test_object_payload_with_skip_tool_passes_through(self):
        """Sanity: an object payload for a skipped tool still exits 0."""
        proc = _run_hook_raw(
            json.dumps({"tool_name": "Task", "tool_response": "anything"})
        )

        self.assertEqual(proc.returncode, 0)


if __name__ == "__main__":
    unittest.main()