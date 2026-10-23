#!/usr/bin/env python3
"""Native Windows lifecycle and registered PowerShell hook smoke tests.

Run after scripts/build-windows.ps1 and npm/scripts/package-npm.js --target win32-x64.
"""

import importlib.util
from contextlib import closing
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "npm/dist/tokenless/adapters/tokenless/qwenwork"
BIN = Path(os.environ.get("TOKENLESS_TEST_BIN", ROOT / "target/npm-prebuilt/win32-x64"))


@unittest.skipUnless(sys.platform == "win32", "requires native Windows")
class WindowsHookTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(shutil.which("tokenless", path=str(BIN)), "build/install Windows binaries first")
        self.assertTrue((SOURCE / "scripts/lifecycle.py").is_file(), "package adapters first")
        sandbox = tempfile.TemporaryDirectory(prefix="tokenless 中文 ' ")
        self.addCleanup(sandbox.cleanup)
        self.root = Path(sandbox.name)
        self.data = self.root / "desktop"
        self.plugin = self.data / "plugins-custom/tokenless"
        spec = importlib.util.spec_from_file_location("windows_lifecycle", SOURCE / "scripts/lifecycle.py")
        self.lifecycle = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.lifecycle)
        self.env = {
            "HOME": str(self.root), "USERPROFILE": str(self.root),
            "TOKENLESS_QWENWORK_HOME": str(self.data),
            "TOKENLESS_DATA_DIR": str(self.root / "databases"),
            "RTK_DB_PATH": str(self.root / "rtk.db"),
            "PATH": str(BIN) + os.pathsep + os.environ["PATH"],
            "QODER_PLUGIN_ROOT": str(self.plugin),
            "PYTHONDONTWRITEBYTECODE": "1",
            "TOKENLESS_COMPRESSION_ENABLED": "1",
            "QWENWORK_PLUGIN_HOOK_COMPATIBILITY": "",
        }
        patcher = mock.patch.dict(os.environ, self.env)
        patcher.start()
        self.addCleanup(patcher.stop)

    def install(self):
        with mock.patch.object(self.lifecycle, "_application", return_value=(self.root, "1.3.2")):
            self.assertEqual(self.lifecycle.main(["install"]), 0)

    def hook(self, event, payload):
        config = json.loads((self.plugin / "hooks/hooks.json").read_text(encoding="utf-8"))
        hook = config["hooks"][event][0]["hooks"][0]
        if event == "PreToolUse":
            self.assertIsNotNone(re.fullmatch(config["hooks"][event][0]["matcher"], payload["tool_name"]))
        self.assertEqual(hook["shell"], "powershell")
        result = subprocess.run(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", hook["command"]],
            input=json.dumps(payload), capture_output=True, text=True, encoding="utf-8", timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_install_repeat_uninstall_and_preserve_foreign_files(self):
        self.data.mkdir()
        settings = self.data / "settings.json"
        settings.write_text('{"disabled":true}', encoding="utf-8")
        self.install()
        shipped = json.loads((SOURCE / "hooks/hooks.json").read_text(encoding="utf-8"))
        installed = json.loads((self.plugin / "hooks/hooks.json").read_text(encoding="utf-8"))
        self.assertEqual(installed["hooks"]["PreToolUse"][0]["matcher"],
                         shipped["hooks"]["PreToolUse"][0]["matcher"] + "|PowerShell")
        marker = self.plugin / self.lifecycle.MARKER
        before = marker.stat().st_mtime_ns
        self.install()
        self.assertEqual(marker.stat().st_mtime_ns, before)
        self.assertEqual(self.lifecycle.main(["uninstall"]), 0)
        self.assertFalse(self.plugin.exists())
        self.assertEqual(settings.read_text(encoding="utf-8"), '{"disabled":true}')

    def test_native_rewrite_execution_and_posttool_passthrough(self):
        self.install()
        payload = self.assert_native_call("PowerShell", "native-call")
        payload["tool_use_id"] = "unsupported-command"
        payload["tool_input"]["command"] = "Write-Output 'unchanged'"
        self.assertEqual(self.hook("PreToolUse", payload), {})

    @unittest.skipUnless(os.environ.get("TOKENLESS_TEST_QWENWORK_APP"), "optional installed QwenWork host contract")
    def test_managed_bash_uses_installed_host_manifest(self):
        app = Path(os.environ["TOKENLESS_TEST_QWENWORK_APP"])
        manifest = json.loads((app / "resources/agent-sdk/engine-runtime-manifest.json").read_text(encoding="utf-8"))
        with mock.patch.dict(os.environ, {"TOKENLESS_QWENWORK_APP": str(app)}):
            self.assertEqual(self.lifecycle.main(["install"]), 0)
        for engine_name in ("opencode", "qwencode"):
            # Import the environment supplied by the installed host; do not invent its compatibility flag.
            engine_env = manifest["engines"][engine_name]["env"]
            self.assertEqual(engine_env["QWENWORK_PLUGIN_HOOK_COMPATIBILITY"], "managed-tool-hooks-v1")
            with mock.patch.dict(os.environ, engine_env):
                for tool in ("Bash", "mcp__workspace__bash", "mcp__qw-builtin__mcp_workspace_bash"):
                    with self.subTest(engine=engine_name, tool=tool):
                        self.assert_native_call(tool, engine_name + "-" + tool)

    @unittest.skipUnless(os.environ.get("TOKENLESS_TEST_QWENWORK_APP"), "optional installed QwenWork hook runner")
    def test_installed_host_executes_registered_hooks(self):
        self.install()
        workspace = self.root / "host workspace"
        workspace.mkdir()
        subprocess.run(["git", "init", "--quiet"], cwd=workspace, check=True, capture_output=True)
        sdk = Path(os.environ["TOKENLESS_TEST_QWENWORK_APP"]) / "resources/agent-sdk"
        result = subprocess.run(
            ["node", str(ROOT / "tests/qwenwork_host_hooks.mjs"), str(sdk), str(self.plugin), str(workspace)],
            capture_output=True, text=True, encoding="utf-8", timeout=60,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        with closing(sqlite3.connect(self.root / "databases/stats.db")) as database:
            attribution = database.execute(
                "SELECT agent_id, session_id, operation FROM stats WHERE tool_use_id = 'host-git-status'"
            ).fetchall()
        self.assertEqual(attribution, [("qwenwork", "host-runner", "rewrite-command")])

    def assert_native_call(self, tool_name, call_id):
        workspace = self.root / "workspace"
        workspace.mkdir(exist_ok=True)
        subprocess.run(["git", "init", "--quiet"], cwd=workspace, check=True, capture_output=True)
        (workspace / "sample.txt").write_text("untracked fixture\n", encoding="utf-8")
        payload = {
            "session_id": "windows-session", "tool_use_id": call_id, "cwd": str(workspace),
            "tool_name": tool_name, "tool_input": {"command": "git status", "timeout": 30},
        }
        rewritten = self.hook("PreToolUse", payload)["hookSpecificOutput"]["updatedInput"]
        self.assertEqual(rewritten["timeout"], 30)
        self.assertIn("Set-Alias -Name rtk", rewritten["command"])
        result = subprocess.run(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", rewritten["command"]],
            cwd=workspace, capture_output=True, text=True, encoding="utf-8", timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout)
        payload["tool_response"] = {"stdout": result.stdout, "stderr": "", "exitCode": 0}
        self.assertEqual(self.hook("PostToolUse", payload), {})
        data = workspace / ".tokenless" if tool_name.startswith("mcp__") else self.root / "databases"
        with closing(sqlite3.connect(data / "stats.db")) as database:
            attribution = database.execute(
                "SELECT agent_id, session_id, operation FROM stats WHERE tool_use_id = ?", (call_id,)
            ).fetchall()
        self.assertEqual(attribution, [("qwenwork", "windows-session", "rewrite-command")])
        return payload

    def test_legacy_bash_keeps_posix_binding(self):
        self.install()
        payload = {
            "session_id": "windows-legacy", "tool_use_id": "bash-call",
            "tool_name": "Bash", "tool_input": {"command": "git status"},
        }
        with mock.patch.dict(os.environ, {"QWENWORK_PLUGIN_HOOK_COMPATIBILITY": ""}):
            rewritten = self.hook("PreToolUse", payload)["hookSpecificOutput"]["updatedInput"]["command"]
        self.assertTrue(rewritten.startswith("env TOKENLESS_AGENT_ID=qwenwork "))
        self.assertNotIn("Set-Alias", rewritten)

    @unittest.skipUnless(os.environ.get("TOKENLESS_TEST_BASH"), "optional Git Bash compatibility")
    def test_legacy_bash_execution_preserves_stats_path(self):
        self.install()
        for invocation in ("command", "script"):
            with self.subTest(invocation=invocation):
                payload = {
                    "session_id": "windows-legacy", "tool_use_id": invocation,
                    "tool_name": "Bash", "tool_input": {"command": "git status"},
                }
                with mock.patch.dict(os.environ, {"QWENWORK_PLUGIN_HOOK_COMPATIBILITY": ""}):
                    command = self.hook("PreToolUse", payload)["hookSpecificOutput"]["updatedInput"]["command"]
                script = self.root / "command.sh"
                script.write_text(command + "\n", encoding="utf-8", newline="\n")
                arguments = ["-c", command] if invocation == "command" else [str(script)]
                result = subprocess.run(
                    [os.environ["TOKENLESS_TEST_BASH"], "--noprofile", "--norc", *arguments],
                    cwd=ROOT, capture_output=True, text=True, encoding="utf-8", timeout=20,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                with closing(sqlite3.connect(self.root / "databases/stats.db")) as database:
                    attribution = database.execute(
                        "SELECT agent_id, session_id FROM stats WHERE tool_use_id = ?", (invocation,)
                    ).fetchall()
                self.assertEqual(attribution, [("qwenwork", "windows-legacy")])
                payload["tool_response"] = {"stdout": result.stdout, "stderr": "", "exitCode": 0}
                self.assertEqual(self.hook("PostToolUse", payload), {})

    def test_powershell_output_compression_and_read_protection(self):
        self.install()
        content = json.dumps([{"id": i, "name": "example", "enabled": True} for i in range(100)])
        payload = {
            "session_id": "windows-response", "tool_use_id": "json-output",
            "tool_name": "PowerShell", "tool_input": {"command": "Write-Output $records"},
            "tool_response": {"stdout": content, "stderr": "", "exit_code": 0},
        }
        response = self.hook("PostToolUse", payload)
        self.assertIn("updatedToolOutput", response["hookSpecificOutput"])
        self.assertLess(len(json.dumps(response)), len(content))
        payload["tool_name"] = "Read"
        payload["tool_use_id"] = "protected-read"
        self.assertEqual(self.hook("PostToolUse", payload), {})

    @unittest.skipUnless(os.environ.get("TOKENLESS_TEST_NODE"), "optional installed npm launcher runtime")
    def test_npm_launchers_support_selected_node_and_preserve_exit_code(self):
        for binary in ("tokenless", "rtk"):
            result = subprocess.run(
                [os.environ["TOKENLESS_TEST_NODE"], str(BIN / binary), "--version"],
                capture_output=True, text=True, timeout=20,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(binary, result.stdout)
        result = subprocess.run(
            [os.environ["TOKENLESS_TEST_NODE"], str(BIN / "tokenless"), "invalid-test-command"],
            capture_output=True, text=True, timeout=20,
        )
        self.assertEqual(result.returncode, 2, result.stderr)


@unittest.skipUnless(sys.platform == "win32", "requires native Windows")
class WindowsPackagingTests(unittest.TestCase):
    def test_text_symlink_materialization_excludes_build_artifacts(self):
        shared = ROOT / "adapters/tokenless/qwenwork/hooks/shared"
        if shared.is_symlink():
            self.skipTest("requires a core.symlinks=false checkout")
        self.assertEqual(shared.read_text(encoding="utf-8").strip(), "../../common/hooks")
        common = ROOT / "adapters/tokenless/common/hooks"
        with tempfile.TemporaryDirectory(prefix=".package-cache-test-", dir=common) as directory:
            fixture = Path(directory)
            for name in ("__pycache__/hook.cpython-312.pyc", "legacy.pyc", "legacy.pyo",
                         "node_modules/fixture/index.js", "package-lock.json", ".gitignore"):
                path = fixture / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("# excluded packaging fixture\n", encoding="utf-8")
            (fixture / "keep.py").write_text("# retained packaging fixture\n", encoding="utf-8")
            result = subprocess.run(
                ["node", "npm/scripts/package-npm.js", "--target", "win32-x64"], cwd=ROOT,
                capture_output=True, text=True, encoding="utf-8", timeout=180,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            package = ROOT / "npm/dist/tokenless"
            for relative in ("common/hooks", "qwenwork/hooks/shared"):
                copied = package / "adapters/tokenless" / relative / fixture.name
                self.assertEqual(sorted(p.name for p in copied.iterdir()), ["keep.py"])
            archive_path, = package.glob("*.tgz")
            with tarfile.open(archive_path) as archive:
                files = {member.name for member in archive if member.isfile()}
            for relative in ("common/hooks", "qwenwork/hooks/shared"):
                prefix = f"package/adapters/tokenless/{relative}/{fixture.name}/"
                self.assertEqual({name for name in files if name.startswith(prefix)}, {prefix + "keep.py"})


if __name__ == "__main__":
    unittest.main()
