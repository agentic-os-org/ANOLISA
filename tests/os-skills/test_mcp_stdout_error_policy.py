"""Run the complete MCP entrypoint with genuine TextIOWrapper error policies.

Run: python tests/os-skills/test_mcp_stdout_error_policy.py -v
The raw-byte filename case requires POSIX; Windows does not validate that case.
These tests are not currently discovered by repository CI.
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
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/setup-mcp/scripts/validate_mcp.py"


class MCPStdoutErrorPolicyTests(unittest.TestCase):
    def check_policy(self, errors, populated):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "settings.json"
            data = (
                {"mcpServers": {"one": {"url": "https://example.invalid/mcp"}}} if populated else {}
            )
            original = json.dumps(data).encode("utf-8")
            config.write_bytes(original)
            buffer = io.BytesIO()
            output = io.TextIOWrapper(buffer, encoding="gbk", errors=errors, newline="\n")
            try:
                # argv and the host's real text stream are entrypoint boundaries.
                # The full module, JSON parser, file access and report remain real.
                with mock.patch.object(sys, "argv", [str(SCRIPT), "--check", str(config)]):
                    with contextlib.redirect_stdout(output):
                        try:
                            runpy.run_path(str(SCRIPT), run_name="__main__")
                        except SystemExit as exit_status:
                            self.assertEqual(exit_status.code, 0)
                self.assertEqual(output.encoding, "utf-8")
                self.assertEqual(output.errors, errors)
                output.flush()
                report = buffer.getvalue()
                expected = "OK: 1 server(s) in " if populated else "No mcpServers in "
                self.assertEqual(report, (expected + str(config) + "\n").encode("utf-8"))
                # Subsequent host writes must retain their original error policy.
                if errors == "strict":
                    with self.assertRaises(UnicodeEncodeError):
                        output.write("\udce9")
                else:
                    output.write("\udce9")
                    output.flush()
                    suffix = b"\xe9" if errors == "surrogateescape" else b"\\udce9"
                    self.assertEqual(buffer.getvalue(), report + suffix)
                self.assertEqual(config.read_bytes(), original)
            finally:
                output.close()

    def test_surrogateescape_populated(self):
        self.check_policy("surrogateescape", True)

    def test_surrogateescape_empty(self):
        self.check_policy("surrogateescape", False)

    def test_backslashreplace_populated(self):
        self.check_policy("backslashreplace", True)

    def test_backslashreplace_empty(self):
        self.check_policy("backslashreplace", False)

    def test_strict_populated_control(self):
        self.check_policy("strict", True)

    def test_strict_empty_control(self):
        self.check_policy("strict", False)

    @unittest.skipUnless(os.name == "posix", "raw-byte filenames require POSIX")
    def test_real_posix_filename_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            name = os.fsencode(directory) + b"/settings-\xe9.json"
            original = b'{"mcpServers":{"one":{"url":"https://example.invalid/mcp"}}}'
            with open(name, "wb") as config:
                config.write(original)
            environment = dict(os.environ, PYTHONIOENCODING="utf-8:surrogateescape", LC_ALL="C")
            result = subprocess.run(
                [os.fsencode(sys.executable), os.fsencode(SCRIPT), b"--check", name],
                env=environment,
                capture_output=True,
                timeout=30,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, b"OK: 1 server(s) in " + name + b"\n")
            with open(name, "rb") as config:
                self.assertEqual(config.read(), original)


if __name__ == "__main__":
    unittest.main()
