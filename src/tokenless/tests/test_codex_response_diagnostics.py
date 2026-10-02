#!/usr/bin/env python3
"""Regression tests for the Codex PostToolUse diagnostics contract."""

from __future__ import annotations

import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parent.parent
    / "adapters"
    / "tokenless"
    / "codex"
    / "scripts"
    / "response-diagnostics"
)


def _create_marker_tokenless(tmpdir: str) -> tuple[str, str]:
    """Create a tokenless stub that records any unexpected invocation."""
    marker = os.path.join(tmpdir, "tokenless-called")
    mock_script = os.path.join(tmpdir, "tokenless")
    script = textwrap.dedent(
        f"""\
        #!/usr/bin/env python3
        from pathlib import Path
        Path({marker!r}).touch()
        raise SystemExit(0)
        """
    )
    Path(mock_script).write_text(script, encoding="utf-8")
    os.chmod(
        mock_script,
        os.stat(mock_script).st_mode | stat.S_IXUSR | stat.S_IXGRP,
    )
    return mock_script, marker


def _run_hook(tool_response: object, tool_name: str = "Bash") -> subprocess.CompletedProcess[str]:
    input_data = {
        "tool_name": tool_name,
        "tool_response": tool_response,
        "session_id": "test-session",
        "tool_use_id": "toolu_test",
    }
    return subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=json.dumps(input_data),
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )


class CodexResponseDiagnosticsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmpdir = tempfile.mkdtemp(prefix="test_codex_diagnostics_")
        self.addCleanup(shutil.rmtree, self.tmpdir, ignore_errors=True)

    def test_large_success_is_not_compressed_or_injected(self) -> None:
        """Successful output must remain untouched instead of being duplicated."""
        mock_bin, marker = _create_marker_tokenless(self.tmpdir)
        original_path = os.environ.get("PATH", "")
        with mock.patch.dict(
            os.environ,
            {
                "PATH": f"{self.tmpdir}{os.pathsep}{original_path}",
                "TOKENLESS_BIN": mock_bin,
            },
        ):
            result = _run_hook(
                {
                    "stdout": "TOKENLESS_SENTINEL_" + "x" * 10_000,
                    "stderr": "",
                    "exit_code": 0,
                }
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertFalse(
            os.path.exists(marker),
            "Codex diagnostics must not invoke the compression pipeline",
        )

    def test_environment_failure_adds_diagnostic_only(self) -> None:
        result = _run_hook(
            {
                "stdout": "",
                "stderr": "bash: frobnicate: command not found",
                "exit_code": 127,
            }
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        output = json.loads(result.stdout)
        hook_output = output["hookSpecificOutput"]
        self.assertEqual(hook_output["hookEventName"], "PostToolUse")
        context = hook_output["additionalContext"]
        self.assertIn("[tokenless:env:ENV_DEPENDENCY_MISSING]", context)
        self.assertIn(" — fix the environment first.", context)
        self.assertNotIn("[tokenless:compressed]", context)
        self.assertNotIn("updatedMCPToolOutput", hook_output)
        self.assertNotIn("suppressOutput", output)

    def test_unclassified_failure_passes_through(self) -> None:
        result = _run_hook(
            {"stdout": "", "stderr": "domain-specific failure", "exit_code": 1}
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_content_tools_remain_silent(self) -> None:
        result = _run_hook("permission denied", tool_name="Read")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
