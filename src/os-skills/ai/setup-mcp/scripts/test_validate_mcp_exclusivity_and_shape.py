#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for validate_mcp.py transport exclusivity and mcpServers shape guards.

Regression tests for two defects:

1. The "Must have exactly one transport indicator" comment was never enforced:
   a server with BOTH `command` and `url` (ambiguous — cosh picks the transport
   by which field is present, per the skill's own SKILL.md documenting three
   mutually exclusive forms) passed validation as OK.
2. A list-valued `mcpServers` crashed with a raw traceback (ValueError from
   dict.update / AttributeError from .items()) instead of the clean
   `ERROR: ...` diagnostics this validator uses everywhere else — notable
   because merge() exists precisely to handle pasted external JSON.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "validate_mcp.py")


def run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT, *args],
                          capture_output=True, text=True)


def write_config(path: str, payload) -> str:
    with open(path, "w", encoding="utf-8") as f:
        json.dump(payload, f)
    return path


class TestTransportExclusivity(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.config = os.path.join(self._tmp.name, "cosh.json")

    def tearDown(self):
        self._tmp.cleanup()

    def test_conflicting_transports_rejected(self):
        """修复前：command 与 url 同时存在（互斥传输字段冲突）照样 OK。
        修复后：报 ERROR 并 exit 1。
        """
        write_config(self.config, {"mcpServers": {"conflicted": {
            "command": "npx", "args": ["-y", "x"],
            "url": "http://localhost:8080/sse"}}})
        result = run("--check", self.config)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ERROR", result.stderr)
        self.assertIn("conflicted", result.stderr)

    def test_single_transport_variants_still_pass(self):
        """正常链路保护：三种互斥形态各一个均通过。"""
        for name, cfg in (
            ("stdio", {"command": "npx", "args": ["-y", "x"]}),
            ("http", {"url": "http://localhost:8080/sse"}),
            ("httpAlt", {"httpUrl": "http://localhost:8080/sse"}),
        ):
            write_config(self.config, {"mcpServers": {name: cfg}})
            result = run("--check", self.config)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("OK", result.stdout)

    def test_missing_transport_still_rejected(self):
        """既有行为保持：一个传输字段都没有仍报错。"""
        write_config(self.config, {"mcpServers": {"bare": {"args": ["x"]}}})
        result = run("--check", self.config)
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing transport field", result.stderr)


class TestMergeShapeGuards(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.out = os.path.join(self._tmp.name, "out.json")

    def tearDown(self):
        self._tmp.cleanup()

    def test_list_mcp_servers_is_clean_error(self):
        """修复前：{"mcpServers": ["not-a-map"]} 在 merge 里 dict.update
        抛裸 ValueError 堆栈。修复后：干净 ERROR + exit 1。
        """
        result = run('{"mcpServers": ["not-a-map"]}', "--merge", self.out)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ERROR", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_check_list_mcp_servers_is_clean_error(self):
        """check 侧同款形态守卫。"""
        write_config(self.out, {"mcpServers": ["not-a-map"]})
        result = run("--check", self.out)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ERROR", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_merge_recovers_from_corrupt_existing_mcp_servers(self):
        """既有配置 mcpServers 是列表时按本文件对不可解析既有配置的
        宽容策略（WARNING 后重置）处理，merge 成功写出字典形态。
        """
        write_config(self.out, {"mcpServers": ["corrupt"]})
        result = run('{"mcpServers": {"fresh": {"command": "npx"}}}',
                     "--merge", self.out)
        self.assertEqual(result.returncode, 0, result.stderr)
        with open(self.out, encoding="utf-8") as f:
            merged = json.load(f)
        self.assertEqual(merged["mcpServers"], {"fresh": {"command": "npx"}})

    def test_valid_merge_still_works(self):
        """正常链路保护：合法输入合并进空配置。"""
        result = run('{"mcpServers": {"a": {"command": "npx"}}, '
                     '"mcp": {"k": "v"}}', "--merge", self.out)
        self.assertEqual(result.returncode, 0, result.stderr)
        with open(self.out, encoding="utf-8") as f:
            merged = json.load(f)
        self.assertEqual(merged["mcpServers"]["a"], {"command": "npx"})
        self.assertEqual(merged["mcp"], {"k": "v"})


if __name__ == "__main__":
    unittest.main()
