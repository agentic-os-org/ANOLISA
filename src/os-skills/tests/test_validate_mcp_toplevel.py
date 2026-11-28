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
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[1] / "ai" / "setup-mcp" / "scripts" / "validate_mcp.py"
)

# Every valid-JSON top-level shape that is not an object.
NON_OBJECT_SHAPES = {
    "array": "[1, 2, 3]",
    "string": '"just a string"',
    "number": "123",
    "null": "null",
    "boolean": "true",
}

VALID_OBJECT = json.dumps({"mcpServers": {"s": {"command": "npx"}}})


def _run(*args):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True, text=True, encoding="utf-8",
    )


class TestMergeShape(unittest.TestCase):
    def test_merge_rejects_every_non_object_shape_cleanly(self):
        for name, bad in NON_OBJECT_SHAPES.items():
            with self.subTest(shape=name):
                with tempfile.TemporaryDirectory() as tmp:
                    target = Path(tmp) / "out.json"
                    r = _run(bad, "--merge", str(target))
                    self.assertEqual(r.returncode, 1, r.stderr)
                    self.assertIn("ERROR", r.stderr)
                    self.assertNotIn("Traceback", r.stderr)
                    # a rejected input must not create the merge target
                    self.assertFalse(target.exists(), target)

    def test_merge_rejection_leaves_an_existing_target_untouched(self):
        original = {"mcpServers": {"keep": {"command": "keep"}}}
        for name, bad in NON_OBJECT_SHAPES.items():
            with self.subTest(shape=name):
                with tempfile.TemporaryDirectory() as tmp:
                    target = Path(tmp) / "out.json"
                    target.write_text(json.dumps(original), encoding="utf-8")
                    r = _run(bad, "--merge", str(target))
                    self.assertEqual(r.returncode, 1, r.stderr)
                    self.assertIn("ERROR", r.stderr)
                    # a rejected input must not modify the merge target
                    self.assertEqual(
                        json.loads(target.read_text(encoding="utf-8")), original
                    )

    def test_merge_still_accepts_valid_object(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "out.json"
            r = _run(VALID_OBJECT, "--merge", str(target))
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertTrue(target.exists(), "valid merge must write the target")


class TestCheckShape(unittest.TestCase):
    def test_check_rejects_every_non_object_shape_cleanly(self):
        for name, bad in NON_OBJECT_SHAPES.items():
            with self.subTest(shape=name):
                with tempfile.TemporaryDirectory() as tmp:
                    cfg = Path(tmp) / "config.json"
                    cfg.write_text(bad, encoding="utf-8")
                    r = _run("--check", str(cfg))
                    self.assertEqual(r.returncode, 1, r.stderr)
                    self.assertIn("ERROR", r.stderr)
                    self.assertNotIn("Traceback", r.stderr)

    def test_check_still_accepts_valid_object(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg = Path(tmp) / "config.json"
            cfg.write_text(VALID_OBJECT, encoding="utf-8")
            r = _run("--check", str(cfg))
            self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main()
