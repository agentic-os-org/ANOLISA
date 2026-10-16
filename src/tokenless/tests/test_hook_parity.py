#!/usr/bin/env python3
"""Golden parity for the Common Hook Protocol v2 transport.

Replays the tests/contract corpus through the current hooks against the
real `tokenless` debug binary and compares every envelope with the Protocol
v2 goldens. The baseline includes Marker-command recovery policy for Common
Hooks and JSON/Build Log PostTool dispatch.

Requires target/debug/tokenless — run `cargo build -p tokenless-cli` first
(the Makefile target does).
"""

import json
import os
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "contract"))

import corpus


@unittest.skipUnless(
    os.path.exists(corpus.DEBUG_TOKENLESS_BIN),
    "tokenless debug binary not built",
)
class HookParity(unittest.TestCase):
    maxDiff = None

    def run_matrix(self, kind, hook, agents):
        for name in corpus.fixture_names(kind):
            with open(corpus.fixture_path(kind, name)) as f:
                stdin_text = f.read()
            for agent, env in corpus.agents_for(kind, name, agents).items():
                with self.subTest(fixture=f"{kind}/{name}", agent=agent):
                    proc = corpus.run_hook(hook, stdin_text, env)
                    self.assertEqual(
                        proc.returncode, 0, f"hook failed: {proc.stderr}"
                    )
                    actual = json.loads(proc.stdout)
                    with open(corpus.golden_path(kind, name, agent)) as f:
                        expected = json.load(f)["envelope"]
                    self.assertEqual(actual, expected)

    def test_response_hook_matches_the_goldens(self):
        self.run_matrix("post_tool", corpus.RESPONSE_HOOK, corpus.RESPONSE_AGENTS)

    def test_schema_hook_matches_the_goldens(self):
        self.run_matrix("before_model", corpus.SCHEMA_HOOK, corpus.SCHEMA_AGENTS)


@unittest.skipUnless(os.path.exists(corpus.DEBUG_TOKENLESS_BIN), "tokenless debug binary not built")
class PathListHooks(unittest.TestCase):
    @staticmethod
    def paths():
        return "".join(
            f'/workspace/project with spaces/中文/long_directory/records/file "{i}".json\n'
            for i in range(60)
        )

    def run_response(self, content, agent="qwenwork", tool="Bash", command=None, extra_env=None):
        payload = {
            "session_id": "path-list-session",
            "tool_use_id": "path-list-call",
            "tool_name": tool,
            "tool_response": content,
        }
        if command is not None:
            payload["tool_input"] = {"command": command}
        agent_env = corpus.RESPONSE_AGENTS.get(agent, {"TOKENLESS_AGENT_ID": agent})
        proc = corpus.run_hook(
            corpus.RESPONSE_HOOK, json.dumps(payload), agent_env, extra_env=extra_env
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout)

    @staticmethod
    def replacement(envelope):
        output = envelope["hookSpecificOutput"]
        return output.get("updatedToolOutput", output.get("updatedToolResponse"))

    def restore(self, view):
        header, encoded = view.split("\n", 1)
        self.assertEqual(
            header, "[Path list; all received paths retained; full path = base + each paths entry]"
        )
        data = json.loads(encoded)
        output = data["line_ending"].join(data["base"] + path for path in data["paths"])
        if data["trailing_newline"]:
            output += data["line_ending"]
        return output

    def test_content_routes_across_hosts_and_tool_names(self):
        source = self.paths()
        for agent in ("qwenwork", "qoder-cli", "claude-code", "opencode", "cosh-ng"):
            for tool in ("Bash", "mcp__files__list_paths"):
                with self.subTest(agent=agent, tool=tool):
                    output = self.run_response(source, agent=agent, tool=tool)
                    self.assertEqual(self.restore(self.replacement(output)), source)
                    self.assertNotIn("additionalContext", output["hookSpecificOutput"])

    def test_qwenwork_and_qoder_preserve_shell_envelopes_and_metadata(self):
        source = {
            "stdout": self.paths(),
            "stderr": "warning: keep this diagnostic\n",
            "exit_code": 0,
            "interrupted": False,
            "duration_ms": 123,
            "metadata": {"source": "native-find", "count": 60},
        }
        for agent in ("qwenwork", "qoder-cli"):
            output = self.replacement(self.run_response(source, agent=agent))
            if agent == "qoder-cli":
                self.assertIsInstance(output, str)
                output = json.loads(output)
            else:
                self.assertIsInstance(output, dict)
            self.assertEqual(self.restore(output["stdout"]), source["stdout"])
            self.assertEqual({**output, "stdout": source["stdout"]}, source)

    def test_files_disabled_domain_and_mixed_results_are_not_replaced(self):
        source = self.paths()
        self.assertEqual(self.run_response(source, tool="Read"), {})
        self.assertEqual(self.run_response(source, command="cat paths.txt"), {})
        self.assertEqual(self.run_response(source, command="head -60 paths.txt"), {})
        self.assertEqual(
            self.run_response(source, extra_env={"TOKENLESS_SEARCH_PATH_SHARING_ENABLED": "0"}), {}
        )
        for content in ("60\n", source + "---\n755 uploads/\n", source + "\0"):
            self.assertEqual(self.run_response(content), {})

    def test_applied_path_list_records_one_attributed_stats_row(self):
        with tempfile.TemporaryDirectory() as data:
            output = self.run_response(
                self.paths(), extra_env={"TOKENLESS_DATA_DIR": data, "TOKENLESS_STATS_ENABLED": "1"}
            )
            self.assertEqual(self.restore(self.replacement(output)), self.paths())
            with sqlite3.connect(Path(data) / "stats.db") as connection:
                rows = connection.execute(
                    "SELECT agent_id, session_id, tool_use_id, before_tokens, after_tokens FROM stats"
                ).fetchall()
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0][:3], ("qwenwork", "path-list-session", "path-list-call"))
            self.assertLess(rows[0][4], rows[0][3])

    def test_native_find_trajectory_paths_can_be_read_without_a_second_search(self):
        # PreTool counterfactuals require pinned RTK and live in test_rtk_integration.py.
        with tempfile.TemporaryDirectory(prefix="native-find-") as directory:
            root = Path(directory)
            for folder in ("query-sources", "records"):
                target = root / "app-data/local.ali-skill-workbench/data/project with spaces" / folder
                target.mkdir(parents=True)
                (target / "databoard-target.json").write_text('{"id":42}', encoding="utf-8")
            for index in range(60):
                target = root / "app-data/local.ali-skill-workbench/data/project with spaces/records"
                (target / f"document {index}.json").write_text(str(index), encoding="utf-8")
            quoted_root = shlex.quote(str(root))
            commands = (
                f'find {quoted_root} -name "databoard-*" 2>/dev/null; echo "---"; ls -la {quoted_root} | head',
                f'find {quoted_root} -path "*records*databoard-*" 2>/dev/null',
                f'find {quoted_root} -name "document*" 2>/dev/null',
            )
            for index, command in enumerate(commands):
                executed = subprocess.run(
                    ["/bin/sh", "-c", command], capture_output=True, text=True, timeout=10
                )
                self.assertEqual(executed.returncode, 0, executed.stderr)
                output = self.run_response(executed.stdout, command=command)
                if index < 2:
                    self.assertEqual(output, {})
                    paths = executed.stdout.split("---\n", 1)[0].splitlines()
                    self.assertEqual(len(paths), 2 if index == 0 else 1)
                    for path in paths:
                        self.assertEqual(Path(path).read_text(encoding="utf-8"), '{"id":42}')
                else:
                    restored = self.restore(self.replacement(output))
                    self.assertEqual(restored, executed.stdout)
                    self.assertEqual(len(restored.splitlines()), 60)
                    for path in restored.splitlines():
                        self.assertEqual(Path(path).read_text(), Path(path).stem.split()[-1])


if __name__ == "__main__":
    unittest.main()
