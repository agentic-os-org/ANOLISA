# Copyright 2026 Hanabi21
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

"""Exercise literal chat-log roots through the complete command-line script."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

PROJECT_SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/skillfs/docs/skills/skillfs-mount/scripts/skill_usage_from_chat_logs.py"
)
SCRIPT = Path(os.environ.get("ANOLISA_CHAT_USAGE_SCRIPT", str(PROJECT_SCRIPT)))


class TestLiteralChatLogRoots(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write_log(
        self,
        logs_dir: Path,
        skill: str,
        *,
        project: str = "example",
        session: str = "session",
        calls: int = 1,
    ) -> None:
        chat = logs_dir / "projects" / project / "chats" / f"{session}.jsonl"
        chat.parent.mkdir(parents=True, exist_ok=True)
        event = {
            "timestamp": "2026-10-28T00:00:00Z",
            "message": {"parts": [{"functionCall": {"name": "skill", "args": {"skill": skill}}}]},
        }
        chat.write_text((json.dumps(event) + "\n") * calls, encoding="utf-8")

    def run_cli(self, logs_dir: Path) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["PYTHONIOENCODING"] = "utf-8"
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--logs-dir", str(logs_dir), "--all-tools"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=environment,
            timeout=30,
            check=False,
        )

    def test_literal_bracket_root_without_matching_sibling(self) -> None:
        logs_dir = self.root / "logs[ab]"
        self.write_log(logs_dir, "requested")
        result = self.run_cli(logs_dir)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Analyzed 1 chat sessions", result.stdout)
        self.assertIn("  requested: 1\n", result.stdout)
        self.assertIn("Total skill invocations: 1", result.stdout)

    def test_literal_root_does_not_select_glob_matching_sibling(self) -> None:
        logs_dir = self.root / "logs[ab]"
        self.write_log(logs_dir, "requested")
        self.write_log(self.root / "logsa", "decoy", calls=3)
        result = self.run_cli(logs_dir)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("1 tool calls total", result.stdout)
        self.assertIn("  requested: 1\n", result.stdout)
        self.assertNotIn("  decoy:", result.stdout)
        self.assertIn("Total skill invocations: 1", result.stdout)

    def test_ordinary_root_with_spaces(self) -> None:
        logs_dir = self.root / "ordinary logs"
        self.write_log(logs_dir, "ordinary")
        result = self.run_cli(logs_dir)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("  ordinary: 1\n", result.stdout)
        self.assertIn("Total skill invocations: 1", result.stdout)

    def test_project_and_chat_wildcards_still_discover_multiple_files(self) -> None:
        logs_dir = self.root / "logs"
        self.write_log(logs_dir, "first", project="one", session="first")
        self.write_log(logs_dir, "second", project="two", session="second", calls=2)
        result = self.run_cli(logs_dir)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Analyzed 2 chat sessions", result.stdout)
        self.assertIn("3 tool calls total", result.stdout)
        self.assertIn("  first: 1\n", result.stdout)
        self.assertIn("  second: 2\n", result.stdout)
        self.assertIn("Total skill invocations: 3", result.stdout)

    def test_missing_root_keeps_no_logs_exit_status(self) -> None:
        result = self.run_cli(self.root / "missing")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("No chat logs found under", result.stdout)


if __name__ == "__main__":
    unittest.main()
