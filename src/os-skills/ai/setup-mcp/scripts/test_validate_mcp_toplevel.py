# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Top-level JSON shape must fail with a clean error, not a traceback.

The SKILL.md instructs the agent "if the script errors, read the error
message and fix the JSON yourself, then retry" — an AttributeError
traceback from a valid-JSON non-object input breaks that retry contract.
"""

import json
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "validate_mcp.py"


def _run(*args):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True, text=True, encoding="utf-8",
    )


class TestTopLevelShape(unittest.TestCase):
    def test_merge_rejects_non_object_input_with_clean_error(self):
        for bad in ('"just a string"', "[1, 2, 3]", "123"):
            with self.subTest(bad=bad):
                r = _run(bad, "--merge", "out.json")
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertIn("ERROR", r.stderr)
                self.assertNotIn("Traceback", r.stderr)

    def test_merge_still_accepts_valid_object(self):
        r = _run(
            json.dumps({"mcpServers": {"s": {"command": "npx"}}}),
            "--merge", "out.json",
        )
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_check_rejects_non_object_config_with_clean_error(self):
        import tempfile

        with tempfile.NamedTemporaryFile(
            "w", suffix=".json", delete=False, encoding="utf-8"
        ) as f:
            f.write("[1, 2, 3]")
            cfg = f.name
        r = _run("--check", cfg)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("ERROR", r.stderr)
        self.assertNotIn("Traceback", r.stderr)

    def test_check_still_accepts_valid_object(self):
        import tempfile

        with tempfile.NamedTemporaryFile(
            "w", suffix=".json", delete=False, encoding="utf-8"
        ) as f:
            json.dump({"mcpServers": {"s": {"command": "npx"}}}, f)
            cfg = f.name
        r = _run("--check", cfg)
        # A valid object must not crash on shape; other validation results
        # are this test's boundary only.
        self.assertNotIn("Traceback", r.stderr)


if __name__ == "__main__":
    unittest.main()
