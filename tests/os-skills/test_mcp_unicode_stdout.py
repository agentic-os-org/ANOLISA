"""Manual regression for MCP merge/check reports with real temporary configs.

Run: python3 tests/os-skills/test_mcp_unicode_stdout.py -v
Baseline on unchanged main: 2 failures / 4 passing controls.
This file is not currently discovered by repository CI.
"""

import contextlib
import io
import json
import os
import runpy
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any, Dict, List, Optional, TextIO
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/ai/setup-mcp/scripts/validate_mcp.py"
)
EMOJI = "\U0001f642"


class MCPUnicodeStdoutTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.unicode_directory = self.root / f"workspace-{EMOJI}"
        self.unicode_directory.mkdir()
        self.existing = {
            "mcpServers": {"existing": {"url": "http://localhost:8080/sse"}},
            "mcp": {"timeout": 5},
        }
        self.ascii_payload = {
            "mcpServers": {"new": {"httpUrl": "https://mcp.example.com/v1"}}
        }
        self.unicode_payload = {
            "mcpServers": {
                "new": {
                    "command": "npx",
                    "args": [
                        "-y",
                        "@modelcontextprotocol/server-filesystem",
                        str(self.unicode_directory),
                    ],
                    "cwd": str(self.unicode_directory),
                }
            }
        }

    @staticmethod
    def write_config(path: Path, data: Dict[str, Any]) -> None:
        path.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")

    def expected(self, payload: Dict[str, Any]) -> Dict[str, Any]:
        return {
            "mcpServers": {
                "existing": {"url": "http://localhost:8080/sse"},
                "new": payload["mcpServers"]["new"],
            },
            "mcp": {"timeout": 5},
        }

    def invoke(self, arguments: List[str], codec: str) -> subprocess.CompletedProcess:
        environment = os.environ.copy()
        environment["PYTHONUTF8"] = "0"
        environment["PYTHONIOENCODING"] = f"{codec}:strict"
        return subprocess.run(
            [sys.executable, "-X", "utf8=0", str(SCRIPT), *arguments],
            cwd=self.root,
            env=environment,
            capture_output=True,
            timeout=30,
            check=False,
        )

    def merge_cli(
        self,
        payload: Dict[str, Any],
        codec: str,
        filename: str = "settings.json",
    ) -> Path:
        target = self.root / filename
        self.write_config(target, self.existing)
        result = self.invoke(
            [json.dumps(payload, ensure_ascii=True), "--merge", filename], codec
        )
        expected = self.expected(payload)
        # Merge writes real UTF-8 config before the baseline's stdout failure.
        self.assertEqual(json.loads(target.read_text(encoding="utf-8")), expected)
        self.assertEqual(
            result.returncode, 0, result.stderr.decode(codec, errors="replace")
        )
        self.assertEqual(json.loads(result.stdout.decode("utf-8")), expected)
        self.assertNotIn(b"Traceback", result.stderr)
        return target

    def check_cli(self, target: Path, codec: str, server_count: int) -> None:
        original = target.read_bytes()
        result = self.invoke(["--check", target.name], codec)
        self.assertEqual(target.read_bytes(), original)
        self.assertEqual(
            result.returncode, 0, result.stderr.decode(codec, errors="replace")
        )
        self.assertEqual(
            result.stdout.decode("utf-8").rstrip("\r\n"),
            f"OK: {server_count} server(s) in {target.name}",
        )
        self.assertNotIn(b"Traceback", result.stderr)

    def run_with_stdout(self, stream: Optional[TextIO]) -> str:
        target = self.root / "embedded.json"
        self.write_config(target, self.existing)
        arguments = [
            str(SCRIPT),
            json.dumps(self.unicode_payload, ensure_ascii=True),
            "--merge",
            str(target),
        ]
        diagnostics = io.StringIO()
        with mock.patch.object(sys, "argv", arguments):
            with contextlib.redirect_stdout(stream), contextlib.redirect_stderr(
                diagnostics
            ):
                namespace = runpy.run_path(str(SCRIPT), run_name="__main__")
                self.assertIs(sys.stdout, stream)
        self.assertEqual(namespace["__name__"], "__main__")
        self.assertEqual(
            json.loads(target.read_text(encoding="utf-8")),
            self.expected(self.unicode_payload),
        )
        return diagnostics.getvalue()

    def test_merge_unicode_directory_report_under_gbk_stdout(self) -> None:
        self.merge_cli(self.unicode_payload, "gbk")

    def test_check_unicode_filename_report_under_gbk_stdout(self) -> None:
        target = self.root / f"settings-{EMOJI}.json"
        self.write_config(target, self.ascii_payload)
        self.check_cli(target, "gbk", 1)

    def test_ascii_merge_and_check_under_gbk_control(self) -> None:
        target = self.merge_cli(self.ascii_payload, "gbk")
        self.check_cli(target, "gbk", 2)

    def test_unicode_merge_and_check_under_utf8_control(self) -> None:
        target = self.merge_cli(self.unicode_payload, "utf-8", f"settings-{EMOJI}.json")
        self.check_cli(target, "utf-8", 2)

    def test_stringio_stdout_remains_supported(self) -> None:
        with io.StringIO() as stream:
            self.assertFalse(hasattr(stream, "reconfigure"))
            diagnostics = self.run_with_stdout(stream)
            self.assertEqual(
                json.loads(stream.getvalue()), self.expected(self.unicode_payload)
            )
        self.assertIn("Written to:", diagnostics)

    def test_unavailable_stdout_still_performs_real_merge(self) -> None:
        diagnostics = self.run_with_stdout(None)
        self.assertIn("Written to:", diagnostics)


if __name__ == "__main__":
    unittest.main()
