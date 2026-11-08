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
from path_list_view import FLAT_HEADER, GROUPED_HEADER, restore_path_list


class PathListViewDecoding(unittest.TestCase):
    # Hand-built views test the decoder contract; PathListHooks below exercises
    # the real Rust encoder through the binary and host hooks.
    def test_both_formats_preserve_filenames_and_endings(self) -> None:
        names = ['plain.rs', 'Dir="other"', 'Base=other', '"quoted.rs', '',
                 '中文 quote"back\\slash.rs', 'line\u2028separator.rs',
                 'line\u2029separator.rs', 'plain.rs']
        for ending in ("\n", "\r\n"):
            for trailing in (False, True):
                metadata = {"base": "./项目/", "line_ending": ending,
                            "trailing_newline": trailing}
                paths = [directory + name for directory in ("", '目录"/nested/', "")
                         for name in names]
                flat = FLAT_HEADER + "\n" + json.dumps({**metadata, "paths": paths})
                grouped = GROUPED_HEADER + "\n" + json.dumps(metadata) + "\n"
                for directory in ("", '目录"/nested/', ""):
                    grouped += "Dir=" + json.dumps(directory) + "\n"
                    for name in names:
                        quoted = not name or name.startswith(('"', 'Dir=', 'Base='))
                        grouped += (json.dumps(name) if quoted else name) + "\n"
                expected = ending.join(metadata["base"] + path for path in paths)
                if trailing:
                    expected += ending
                for view in (flat, grouped):
                    with self.subTest(ending=ending, trailing=trailing, header=view.split("\n")[0]):
                        restored, decoded_paths = restore_path_list(view)
                        self.assertEqual(restored, expected)
                        self.assertEqual(decoded_paths, [metadata["base"] + path for path in paths])

    def test_unknown_header_and_missing_directory_are_rejected(self) -> None:
        with self.assertRaises(ValueError):
            restore_path_list("unknown\n{}")
        metadata = {"base": "src/", "line_ending": "\n", "trailing_newline": True}
        with self.assertRaises(ValueError):
            restore_path_list(GROUPED_HEADER + "\n" + json.dumps(metadata) + "\nfile.rs\n")


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

    def test_qoder_payloads_outside_stdout_remain_untouched(self):
        # Short or empty stdout must not make Bash metadata or stderr the
        # compression target, regardless of their size.
        for name in ("bash_array_truncation", "build_log_cargo_failure"):
            with self.subTest(fixture=name):
                source = Path(corpus.fixture_path("post_tool", name)).read_text()
                proc = corpus.run_hook(
                    corpus.RESPONSE_HOOK, source, corpus.RESPONSE_AGENTS["qoder-cli"]
                )
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertEqual(json.loads(proc.stdout), {})

    def test_domain_compressors_receive_native_shell_content(self) -> None:
        cases = {
            "toon": json.dumps({"items": [{"id": i, "name": "repeated record"} for i in range(60)]}, indent=4),
            "build_log_reduction": "".join(
                f"tests/test_module_{i:03}.py ........................................ [ 75%]\n"
                for i in range(12)
            ) + "===================== 480 passed in 2.00s =====================\n",
            "tabular_compaction": "id,name,description\n" + "".join(
                f'{i},record-{i},"description without embedded delimiters number {i}"\n'
                for i in range(40)
            ),
            "search_path_sharing": "".join(
                f"/workspace/long_project_name/data/records/partition-{i}/record.json\n"
                for i in range(60)
            ),
            "html_extraction": '<!DOCTYPE html><html><head><title>Report</title></head><body><nav>'
            + '<a href="/menu">menu</a>' * 100
            + '</nav><main><h1>Results</h1><p>' + 'Relevant report content. ' * 20
            + '</p></main></body></html>',
            "diff_reduction": "diff --git a/f b/f\nindex 123..456 100644\n--- a/f\n+++ b/f\n"
            + "@@ -1,12 +1,12 @@\n" + (" " + "unchanged context " * 20 + "\n") * 10
            + "-old\n+new\n tail\n",
        }
        agents = {**corpus.RESPONSE_AGENTS, "qwenwork": {"TOKENLESS_AGENT_ID": "qwenwork"},
                  "codex": {"TOKENLESS_AGENT_ID": "codex"}}
        with tempfile.TemporaryDirectory() as data:
            env = {**os.environ, "TOKENLESS_DATA_DIR": data, "TOKENLESS_COMPRESSION_ENABLED": "1",
                   "TOKENLESS_STATS_ENABLED": "0", "TOKENLESS_SLS_ENABLED": "0",
                   "TOKENLESS_DIFF_COMPRESSION_ENABLED": "", "TOKENLESS_HTML_EXTRACTION_ENABLED": "1",
                   "TOKENLESS_SEARCH_PATH_SHARING_ENABLED": "1"}
            env.pop("TOKENLESS_STASH_DB", None)
            env.pop("TOKENLESS_STATS_DB", None)
            for operation, content in cases.items():
                request = {
                    "protocol_version": 2, "operation": "post_tool",
                    "attribution": {"agent_id": "coverage", "session_id": "coverage", "tool_use_id": operation},
                    "input": {"result_kind": "tool", "tool_name": "Bash", "content": content,
                              "status": "success", "content_origin": "command_output", "output_optimization": "none",
                              "capabilities": {"replace_output": True, "replace_with_text": True, "recovery": {"kind": "shell"}}},
                }
                proc = subprocess.run([corpus.DEBUG_TOKENLESS_BIN, "compress"], input=json.dumps(request),
                                      capture_output=True, text=True, env=env, check=True)
                expected = json.loads(proc.stdout)["result"]
                self.assertEqual(expected["applied_operations"], [operation])
                shell = {"stdout": content, "stderr": "", "exit_code": 0, "duration_ms": 123}
                for agent, agent_env in agents.items():
                    with self.subTest(operation=operation, agent=agent):
                        response = shell
                        if agent == "cosh-ng":
                            response = {"llmContent": json.dumps(shell), "returnDisplay": "display-only"}
                        elif agent == "opencode":
                            response = content
                        payload = {"tool_name": "Bash", "tool_response": response,
                                   "session_id": "coverage", "tool_use_id": operation}
                        actual = corpus.run_hook(corpus.RESPONSE_HOOK, json.dumps(payload), agent_env,
                                                 extra_env={"TOKENLESS_COMPRESSION_ENABLED": "1"})
                        self.assertEqual(actual.returncode, 0, actual.stderr)
                        envelope = json.loads(actual.stdout)
                        if agent in {"codex", "qwencode"}:
                            self.assertEqual(envelope, {})
                            continue
                        fields = envelope["hookSpecificOutput"]
                        self.assertNotIn("additionalContext", fields)
                        if agent == "cosh-ng":
                            self.assertIsInstance(fields["updatedToolResponse"], str)
                            replaced = json.loads(fields["updatedToolResponse"])
                        else:
                            replaced = fields["updatedToolOutput"]
                        if agent == "opencode":
                            self.assertEqual(replaced, expected["output"])
                        else:
                            self.assertEqual(replaced, {**shell, "stdout": expected["output"]})


@unittest.skipUnless(os.path.exists(corpus.DEBUG_TOKENLESS_BIN), "tokenless debug binary not built")
class PathListHooks(unittest.TestCase):
    @staticmethod
    def paths():
        return "".join(
            f'/workspace/project with spaces/中文/long_directory/records/file{separator}name "{i}".json\n'
            for i in range(60)
            for separator in ["\u2028" if i == 0 else "\u2029" if i == 1 else " "]
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

    def restore(self, view: str) -> str:
        return restore_path_list(view)[0]

    def test_directory_interleaving_retains_the_flat_view(self) -> None:
        source = "".join(
            f"/workspace/project/component_{i % 3}/file_{i}.json\n" for i in range(90)
        )
        view = self.replacement(self.run_response(source))
        self.assertEqual(view.split("\n", 1)[0], FLAT_HEADER)
        self.assertEqual(self.restore(view), source)

    def test_content_routes_across_hosts_and_tool_names(self):
        source = self.paths()
        for agent in ("qwenwork", "qoder-cli", "claude-code", "opencode", "cosh-ng"):
            for tool in ("Bash", "mcp__files__list_paths"):
                with self.subTest(agent=agent, tool=tool):
                    output = self.run_response(source, agent=agent, tool=tool)
                    view = self.replacement(output)
                    self.assertEqual(view.split("\n", 1)[0], GROUPED_HEADER)
                    self.assertEqual(self.restore(view), source)
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
                separator = "\u2028" if index == 0 else "\u2029" if index == 1 else " "
                (target / f"document{separator}name {index}.json").write_text(str(index), encoding="utf-8")
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
                    restored, paths = restore_path_list(self.replacement(output))
                    self.assertEqual(restored, executed.stdout)
                    self.assertEqual(len(paths), 60)
                    for path in paths:
                        self.assertEqual(Path(path).read_text(), Path(path).stem.split()[-1])


if __name__ == "__main__":
    unittest.main()
