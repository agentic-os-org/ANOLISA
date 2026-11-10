#!/usr/bin/env python3
"""Protocol v2 and cross-operation tests for the Common PreTool hook."""

import json
import os
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
REPO_ROOT = TESTS_DIR.parent
CONTRACT_DIR = TESTS_DIR / "contract"
sys.path.insert(0, str(CONTRACT_DIR))

import contract_runner
import corpus

sys.path.insert(0, corpus.HOOKS_DIR)
import hook_utils

RESPONSE_HOOK = REPO_ROOT / "adapters/tokenless/common/hooks/compress_response_hook.py"
MOCK_TOKENLESS = CONTRACT_DIR / "mock_tokenless.py"
QWEN_MANIFEST = REPO_ROOT / "adapters/tokenless/qwencode/qwen-extension.json.in"


def pre_tool_payload(call_id: str = "call-1") -> dict:
    payload = {
        "session_id": "session-1",
        "tool_name": "Bash",
        "tool_input": {"command": "grep error log", "timeout": 30},
    }
    if call_id:
        payload["tool_use_id"] = call_id
    return payload


def post_tool_payload(call_id: str) -> dict:
    return {
        "session_id": "session-1",
        "tool_use_id": call_id,
        "tool_name": "Bash",
        "tool_response": {"output": "x" * 80},
    }


class PreToolContractTest(unittest.TestCase):
    def run_case(self, behavior: str | None, call_id: str = "call-1"):
        return contract_runner.run_case(
            corpus.PRE_TOOL_HOOK,
            json.dumps(pre_tool_payload(call_id)),
            {"TOKENLESS_AGENT_ID": "qoder-cli"},
            behavior,
        )

    def test_applied_rewrite_uses_protocol_v2(self) -> None:
        result = self.run_case("applied")
        self.assertEqual(result.spawns, [corpus.PRE_TOOL_COMMAND])
        self.assertEqual(
            result.envelope,
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "tool_input": {"command": "/mock/rtk grep error log"},
                    "updatedInput": {
                        "command": "/mock/rtk grep error log",
                        "timeout": 30,
                    },
                }
            },
        )
        self.assertEqual(len(result.requests), 1)
        request = result.requests[0]
        self.assertEqual(request["protocol_version"], 2)
        self.assertEqual(request["operation"], "pre_tool")
        self.assertEqual(
            request["attribution"],
            {
                "agent_id": "qoder-cli",
                "session_id": "session-1",
                "tool_use_id": "call-1",
            },
        )
        self.assertEqual(
            request["input"],
            {
                "tool_name": "Bash",
                "arguments": {"command": "grep error log", "timeout": 30},
                "command_field": "command",
                "capabilities": {
                    "replace_arguments": True,
                    "block_and_suggest": False,
                },
            },
        )

    def test_passthrough_and_failure_classes_fail_open(self) -> None:
        for behavior in [
            "no_savings",
            "passthrough",
            "error_disposition",
            "nonzero_exit",
            "malformed_stdout",
            "old_response",
            "future_response",
        ]:
            with self.subTest(behavior=behavior):
                result = self.run_case(behavior)
                self.assertEqual(result.envelope, {})
                self.assertEqual(result.spawns, [corpus.PRE_TOOL_COMMAND])

    def test_missing_binary_and_missing_call_id_do_not_spawn(self) -> None:
        self.assertEqual(self.run_case(None).envelope, {})
        missing_id = self.run_case("applied", call_id="")
        self.assertEqual(missing_id.envelope, {})
        self.assertEqual(missing_id.spawns, [])


class HookLifecycleStateTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.home = root / "home"
        self.bin_dir = root / "bin"
        self.home.mkdir()
        self.bin_dir.mkdir()
        tokenless = self.bin_dir / "tokenless"
        shutil.copy(MOCK_TOKENLESS, tokenless)
        tokenless.chmod(tokenless.stat().st_mode | stat.S_IXUSR)
        self.request_log = root / "requests.jsonl"
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "PATH": f"{self.bin_dir}:/usr/bin:/bin",
            "LC_ALL": "C.UTF-8",
            "TOKENLESS_AGENT_ID": "qoder-cli",
            "TOKENLESS_STATS_ENABLED": "0",
            "TOKENLESS_SLS_ENABLED": "0",
            "TOKENLESS_MOCK_BEHAVIOR": "applied",
            "TOKENLESS_MOCK_REQUEST_LOG": str(self.request_log),
        }

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_hook(self, hook: Path, payload: dict, *hook_args: str, env: dict | None = None) -> dict:
        proc = subprocess.run(
            [sys.executable, str(hook), *hook_args],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            env=self.env if env is None else env,
            timeout=15,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout or "{}")

    def requests(self) -> list[dict]:
        with self.request_log.open() as request_log:
            return [json.loads(line) for line in request_log if line.strip()]

    def test_rtk_state_reaches_post_tool_once(self) -> None:
        rewritten = self.run_hook(corpus.PRE_TOOL_HOOK, pre_tool_payload())
        self.assertIn("hookSpecificOutput", rewritten)

        first = self.run_hook(RESPONSE_HOOK, post_tool_payload("call-1"))
        self.assertEqual(first, {})
        self.assertEqual(self.requests()[-1]["input"]["output_optimization"], "rtk")
        self.assertEqual(self.requests()[-1]["input"]["capabilities"]["recovery"]["kind"], "none")

        second = self.run_hook(RESPONSE_HOOK, post_tool_payload("call-1"))
        self.assertIn("hookSpecificOutput", second)
        self.assertEqual(self.requests()[-1]["input"]["output_optimization"], "none")

    def test_state_is_isolated_by_tool_call(self) -> None:
        self.run_hook(corpus.PRE_TOOL_HOOK, pre_tool_payload("call-a"))

        other = self.run_hook(RESPONSE_HOOK, post_tool_payload("call-b"))
        self.assertIn("hookSpecificOutput", other)
        self.assertEqual(self.requests()[-1]["input"]["output_optimization"], "none")

        matching = self.run_hook(RESPONSE_HOOK, post_tool_payload("call-a"))
        self.assertEqual(matching, {})
        self.assertEqual(self.requests()[-1]["input"]["output_optimization"], "rtk")

    def test_rewrite_is_not_applied_when_state_cannot_be_written(self) -> None:
        tokenless_dir = self.home / ".tokenless"
        tokenless_dir.mkdir()
        (tokenless_dir / "hook-state").write_text("not a directory")
        output = self.run_hook(corpus.PRE_TOOL_HOOK, pre_tool_payload())
        self.assertEqual(output, {})

    def test_command_agent_id_keeps_lifecycle_state_stable(self) -> None:
        env = {key: value for key, value in self.env.items() if key != "TOKENLESS_AGENT_ID"}
        self.run_hook(
            corpus.PRE_TOOL_HOOK,
            pre_tool_payload(),
            "--agent-id",
            "copilot-shell",
            env=env,
        )
        self.run_hook(
            RESPONSE_HOOK,
            post_tool_payload("call-1"),
            "--agent-id",
            "copilot-shell",
            env=env,
        )
        requests = self.requests()
        self.assertEqual(requests[-2]["attribution"]["agent_id"], "copilot-shell")
        self.assertEqual(requests[-1]["attribution"]["agent_id"], "copilot-shell")
        self.assertEqual(requests[-1]["input"]["output_optimization"], "rtk")

    def test_qwenwork_rewrite_and_response_share_rtk_state(self) -> None:
        rewritten = self.run_hook(
            corpus.PRE_TOOL_HOOK, pre_tool_payload(), "--agent-id", "qwenwork"
        )
        self.assertEqual(
            rewritten["hookSpecificOutput"]["updatedInput"],
            {"command": "/mock/rtk grep error log", "timeout": 30},
        )
        output = self.run_hook(
            RESPONSE_HOOK, post_tool_payload("call-1"), "--agent-id", "qwenwork"
        )
        self.assertEqual(output, {})
        requests = self.requests()
        self.assertEqual(len(requests), 2)
        for request in requests:
            self.assertEqual(request["attribution"]["agent_id"], "qwenwork")
            self.assertEqual(request["attribution"]["tool_use_id"], "call-1")
        self.assertEqual(requests[-1]["input"]["output_optimization"], "rtk")
        self.assertEqual(requests[-1]["input"]["capabilities"]["recovery"]["kind"], "none")

    def test_abandoned_state_is_bounded_on_next_rewrite(self) -> None:
        state_dir = self.home / ".tokenless" / "hook-state"
        state_dir.mkdir(parents=True)
        stale = state_dir / "stale"
        stale.write_text("rtk\n")
        stale_time = stale.stat().st_mtime - 25 * 60 * 60
        os.utime(stale, (stale_time, stale_time))
        claimed = state_dir / "claimed.consuming.1234"
        claimed.write_text("rtk\n")
        crashed = state_dir / "crashed.consuming.5678"
        crashed.write_text("rtk\n")
        os.utime(crashed, (stale_time, stale_time))
        for index in range(1025):
            (state_dir / f"abandoned-{index:04}").write_text("rtk\n")

        self.run_hook(corpus.PRE_TOOL_HOOK, pre_tool_payload())

        self.assertFalse(stale.exists())
        self.assertTrue(claimed.exists())
        self.assertFalse(crashed.exists())
        unclaimed = [path for path in state_dir.iterdir() if ".consuming." not in path.name]
        self.assertLessEqual(len(unclaimed), 1024)

    def test_qwen_lifecycle_commands_pin_agent_id(self) -> None:
        manifest = json.loads(QWEN_MANIFEST.read_text().replace("@VERSION@", "test"))
        rewrite = manifest["hooks"]["PreToolUse"][1]["hooks"][0]["command"]
        response = manifest["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
        self.assertIn("rewrite_hook.py --agent-id qwencode", rewrite)
        self.assertIn("compress_response_hook.py --agent-id qwencode", response)


@unittest.skipUnless(os.name == "posix", "POSIX process groups required")
class IsolatedRewriteProcessTest(unittest.TestCase):
    def test_child_owns_its_session_and_keeps_all_streams(self) -> None:
        code = (
            "import json,os,sys; "
            "print(json.dumps([os.getpid(),os.getpgrp(),os.getsid(0),sys.stdin.read()])); "
            "sys.stderr.write('diagnostic')"
        )
        result = hook_utils._run_in_group([sys.executable, "-c", code], "request", 3)
        pid, group, session, received = json.loads(result.stdout)
        self.assertEqual((group, session), (pid, pid))
        self.assertNotEqual(group, os.getpgrp())
        self.assertEqual(received, "request")
        self.assertEqual(result.stderr, "diagnostic")
        self.assertEqual(result.returncode, 0)

    def test_outer_timeout_ends_descendants_holding_output_pipes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            marker = Path(tmp) / "survived"
            ready = Path(tmp) / "ready"
            descendant = (
                "import pathlib,time; time.sleep(1); "
                f"pathlib.Path({str(marker)!r}).write_text('survived')"
            )
            code = (
                "import pathlib,subprocess,sys,time; "
                f"child=subprocess.Popen([sys.executable,'-c',{descendant!r}]); "
                f"pathlib.Path({str(ready)!r}).write_text(str(child.pid)); "
                "time.sleep(60)"
            )
            started = time.monotonic()
            result = hook_utils.run(
                [sys.executable, "-c", code], "", timeout=0.3, isolated=True
            )
            self.assertIsNone(result)
            self.assertTrue(ready.is_file(), "descendant did not start")
            self.assertLess(time.monotonic() - started, 2)
            time.sleep(1.1)
            self.assertFalse(marker.exists(), "descendant survived the timeout")

    def test_timeout_does_not_drain_pipes_held_by_detached_descendants(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            marker = Path(tmp) / "detached-pid"
            child = (
                "import pathlib,subprocess,sys,time; "
                "p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],"
                "start_new_session=True); "
                f"pathlib.Path({str(marker)!r}).write_text(str(p.pid)); "
                "time.sleep(60)"
            )
            probe = (
                "import sys; "
                f"sys.path.insert(0,{corpus.HOOKS_DIR!r}); "
                "import hook_utils; "
                f"result=hook_utils.run([sys.executable,'-c',{child!r}],"
                "'',timeout=1,isolated=True); print(result is None)"
            )
            with subprocess.Popen(
                [sys.executable, "-c", probe],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            ) as supervisor:
                try:
                    stdout, stderr = supervisor.communicate(timeout=4)
                    self.assertEqual(supervisor.returncode, 0, stderr)
                    self.assertEqual(stdout.strip(), "True")
                    self.assertTrue(marker.is_file(), "detached child did not start")
                finally:
                    if supervisor.poll() is None:
                        supervisor.kill()
                        supervisor.wait(timeout=1)
                    if marker.is_file():
                        try:
                            os.kill(int(marker.read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass


@unittest.skipUnless(
    os.path.exists(corpus.DEBUG_TOKENLESS_BIN),
    "tokenless debug binary not built",
)
class RealCorePreToolTest(unittest.TestCase):

    @unittest.skipUnless(os.name == "posix", "POSIX process groups required")
    def test_rewrite_deadline_survives_a_killed_python_hook(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            if any((parent / ".claude").exists() for parent in [root, *root.parents]):
                self.skipTest("temporary directory has a .claude ancestor")
            bin_dir = root / "bin"
            bin_dir.mkdir()
            pids = root / "pids"
            git = bin_dir / "git"
            git.write_text(
                '#!/bin/sh\necho "$PPID $$" > "$TOKENLESS_TEST_PIDS"\nexec sleep 60\n'
            )
            git.chmod(0o700)
            env = {
                **os.environ,
                "PATH": f"{bin_dir}:/usr/bin:/bin",
                "TOKENLESS_DATA_DIR": str(root / "data"),
                "TOKENLESS_TEST_PIDS": str(pids),
                "TOKENLESS_STATS_ENABLED": "0",
                "TOKENLESS_SLS_ENABLED": "0",
            }
            request = hook_utils.build_pre_tool_request(
                {"command": "git status"}, "qwenwork", "Bash", "command"
            )
            code = (
                "import json,sys; "
                f"sys.path.insert(0,{corpus.HOOKS_DIR!r}); "
                "from hook_utils import run_compress; "
                f"run_compress({corpus.DEBUG_TOKENLESS_BIN!r},json.load(sys.stdin),8,'pre_tool')"
            )
            with subprocess.Popen(
                [sys.executable, "-c", code],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=env,
                cwd=root,
            ) as hook:
                try:
                    hook.stdin.write(json.dumps(request))
                    hook.stdin.close()
                    deadline = time.monotonic() + 3
                    while (
                        not pids.exists() or not pids.stat().st_size
                    ) and time.monotonic() < deadline:
                        time.sleep(0.02)
                    self.assertTrue(pids.exists(), "RTK did not start the stalled git")
                    rewrite_pid, git_pid = map(int, pids.read_text().split())
                    hook.kill()
                    hook.wait(timeout=1)
                    deadline = time.monotonic() + 7
                    while time.monotonic() < deadline:
                        proc_stat = Path(f"/proc/{git_pid}/stat")
                        if proc_stat.exists():
                            # Containers may leave dead orphans unreaped.
                            state = proc_stat.read_text().rsplit(")", 1)[1].split()[0]
                            if state in {"Z", "X"}:
                                break
                        try:
                            os.kill(git_pid, 0)
                        except ProcessLookupError:
                            break
                        time.sleep(0.05)
                    else:
                        os.killpg(rewrite_pid, signal.SIGKILL)
                        self.fail("stalled git survived the rewrite deadline")
                finally:
                    if hook.poll() is None:
                        hook.kill()
                        hook.wait(timeout=1)

    def test_core_owns_rtk_execution_and_path_anchoring(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            bin_dir = root / "bin"
            home.mkdir()
            bin_dir.mkdir()
            os.symlink(corpus.DEBUG_TOKENLESS_BIN, bin_dir / "tokenless")
            # RTK is embedded: a stray PATH rtk must neither rewrite nor run.
            rtk = bin_dir / "rtk"
            rtk.write_text("#!/bin/sh\nexit 97\n")
            rtk.chmod(rtk.stat().st_mode | stat.S_IXUSR)
            env = {
                **os.environ,
                "HOME": str(home),
                "PATH": f"{bin_dir}:/usr/bin:/bin",
                "TOKENLESS_DATA_DIR": str(root / "data"),
                "TOKENLESS_AGENT_ID": "qoder-cli",
                "TOKENLESS_STATS_ENABLED": "0",
                "TOKENLESS_SLS_ENABLED": "0",
            }
            payload = pre_tool_payload()
            payload["tool_input"]["command"] = "git status"
            proc = subprocess.run(
                [sys.executable, corpus.PRE_TOOL_HOOK],
                input=json.dumps(payload),
                capture_output=True,
                text=True,
                env=env,
                timeout=15,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            output = json.loads(proc.stdout)["hookSpecificOutput"]
            command = output["tool_input"]["command"]
            tokens = shlex.split(command)
            rtk_index = tokens.index("rtk")
            launcher = Path(tokens[rtk_index - 1])
            self.assertTrue(launcher.is_absolute())
            self.assertEqual(
                launcher.resolve(), Path(corpus.DEBUG_TOKENLESS_BIN).resolve()
            )
            self.assertEqual(tokens[rtk_index:], ["rtk", "git", "status"])
            self.assertIn("TOKENLESS_AGENT_ID=qoder-cli", command)
            self.assertEqual(output["updatedInput"]["timeout"], 30)
            for command in ["grep error log", "env LANG=C timeout 30 rg --files"]:
                payload["tool_input"]["command"] = command
                proc = subprocess.run(
                    [sys.executable, corpus.PRE_TOOL_HOOK], input=json.dumps(payload),
                    capture_output=True, text=True, env=env, timeout=15,
                )
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertEqual(json.loads(proc.stdout), {})


if __name__ == "__main__":
    unittest.main()
