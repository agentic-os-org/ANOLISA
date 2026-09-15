#!/usr/bin/env python3
"""Integration tests for compress_response_hook.py.

Validates the PostToolUse hook output contract:
- Replacement semantics: updatedToolOutput replaces (not appends to) original.
- Additivity: additionalContext is reserved for env-attribution diagnostics.
- No duplicate content in the model-visible output.
- Pass-through when compression yields no size reduction.
- Legacy path for non-replacement adapters.

Uses subprocess to invoke the hook with a mock tokenless binary,
avoiding Python version issues with the hook_utils module.
"""

import importlib.machinery
import importlib.util
import json
import os
import select
import stat
import subprocess
import sys
import shutil
import tempfile
import textwrap
import time
import types
import unittest
from unittest import mock

# ``fcntl``, ``pty`` and ``termios`` are POSIX-only, and so is ``select.poll``.
# Only the pty-based host-classification tests need them, and each of those is
# guarded by ``skipIf(os.name == "nt" or pty is None, ...)``. Import them
# defensively: an unconditional top-level import would make module collection
# itself fail on a non-POSIX host, which defeats every skipIf guard below
# (the whole file errors out instead of skipping).
try:
    import fcntl
    import pty
    import termios
except ImportError:  # non-POSIX
    fcntl = None
    pty = None
    termios = None


def _make_large_json_payload(char_target: int = 500) -> dict:
    """Build a JSON payload larger than _MIN_RESPONSE_CHARS (200)."""
    return {
        "stdout": "x" * char_target,
        "stderr": "",
        "exit_code": 0,
        "interrupted": False,
    }


def _create_mock_tokenless(tmpdir: str, behavior: str = "compress") -> str:
    """Create a mock `tokenless` speaking the Protocol v2 PostTool operation.

    Every invocation appends its argv to a `spawn_log` file next to the
    binary, so tests can assert the one-subprocess contract (§5.6). The
    mock also overwrites `request.json` with the latest PostTool input. The
    mock validates the request shape: a malformed request from the
    hook exits non-zero, which the hook fails open on — surfacing
    request-construction bugs as envelope mismatches.

    Behaviors: "compress" applies string-truncation (>20 chars → first 20)
    to the content and responds applied; "no-savings" and "passthrough"
    return the original content under the matching disposition.
    """
    mock_script = os.path.join(tmpdir, "tokenless")

    prologue = textwrap.dedent("""\
        #!/usr/bin/env python3
        import json, os, sys
        with open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                               "spawn_log"), "a") as log:
            log.write(" ".join(sys.argv[1:]) + "\\n")
        if sys.argv[1:] != ["compress"]:
            sys.exit(2)
        request = json.loads(sys.stdin.read())
        if (request.get("protocol_version") != 2
                or request.get("operation") != "post_tool"
                or "capabilities" not in request.get("input", {})):
            sys.exit(2)
        operation_input = request["input"]
        content = operation_input["content"]
        with open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                               "request.json"), "w") as captured:
            json.dump(operation_input, captured)

        def respond(output, disposition, additional_context=None):
            result = {
                "output": output,
                "disposition": disposition,
                "content_type": "json",
                "applied_operations": ["json_cleanup"] if disposition == "applied" else [],
                "recoverability": "lossless",
                "before_tokens": 100,
                "after_tokens": 50 if disposition == "applied" else 100,
                "stash_keys": [],
                "tokenizer_id": "heuristic-v1",
            }
            if additional_context:
                result["additional_context"] = additional_context
            print(json.dumps({
                "protocol_version": 2,
                "operation": "post_tool",
                "attribution": request["attribution"],
                "result": result,
            }))

        if operation_input["status"] == "error":
            context = None
            if "command not found" in content.lower():
                context = "[tokenless:env] tool failed: ENV_DEPENDENCY_MISSING."
            respond(content, "tool_error", context)
            sys.exit(0)
        if (not operation_input["capabilities"]["replace_output"]
                or operation_input["content_origin"] == "file_content"
                or len(content) < 200):
            respond(content, "passthrough")
            sys.exit(0)
    """)

    if behavior == "compress":
        script = prologue + textwrap.dedent("""\
            data = json.loads(content)
            if isinstance(data, str):
                data = json.loads(data)
            compressed = {
                k: (v[:20] if isinstance(v, str) and len(v) > 20 else v)
                for k, v in data.items()
            }
            respond(json.dumps(compressed, separators=(",", ":")), "applied")
        """)
    elif behavior == "compress-text":
        # Text-slot path: the hook must have declared replace_with_text for
        # the unwrapped shell field; the deterministic head-truncation lets
        # tests assert exactly which field's text was sent.
        script = prologue + textwrap.dedent("""\
            if operation_input["capabilities"].get("replace_with_text") is not True:
                sys.exit(2)
            respond(content[:40], "applied")
        """)
    elif behavior == "no-savings":
        script = prologue + 'respond(content, "no_savings")\n'
    elif behavior == "passthrough":
        script = prologue + 'respond(content, "passthrough")\n'
    elif behavior == "wrong-protocol-version":
        script = prologue + textwrap.dedent("""\
            print(json.dumps({
                "protocol_version": 1,
                "operation": "post_tool",
                "attribution": request["attribution"],
                "result": {},
            }))
        """)
    else:
        raise ValueError(f"Unknown behavior: {behavior}")

    with open(mock_script, "w") as f:
        f.write(script)
    os.chmod(mock_script, os.stat(mock_script).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return mock_script


def _spawn_log_lines(mock_tokenless_path: str) -> list:
    """The argv lines the mock recorded, one per tokenless invocation."""
    log_path = os.path.join(os.path.dirname(mock_tokenless_path), "spawn_log")
    try:
        with open(log_path) as f:
            return [line.strip() for line in f if line.strip()]
    except OSError:
        return []


def _create_mock_claude(tmpdir: str, version: str = "2.1.121") -> str:
    """Create a mock claude binary that reports a specific version."""
    mock_script = os.path.join(tmpdir, "claude")
    script = textwrap.dedent(f"""\
        #!/usr/bin/env python3
        import sys
        if "--version" in sys.argv:
            print("{version}")
    """)
    with open(mock_script, "w") as f:
        f.write(script)
    os.chmod(mock_script, os.stat(mock_script).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return mock_script


def _hook_script_path() -> str:
    hooks_dir = os.path.normpath(os.path.join(
        os.path.dirname(__file__),
        os.pardir, "adapters", "tokenless", "common", "hooks",
    ))
    return os.path.join(hooks_dir, "compress_response_hook.py")


def _build_hook_env(agent_id: str, mock_tokenless_path: str,
                    isolated_home: str = None, extra_env: dict = None) -> dict:
    """Build the environment for a hook subprocess (shared with pty runs)."""
    env = os.environ.copy()
    env["TOKENLESS_AGENT_ID"] = agent_id
    if agent_id == "cosh-ng":
        env["COSH_NG_VERSION"] = "0.5.0"
    env["PATH"] = os.path.dirname(mock_tokenless_path) + ":" + env.get("PATH", "")
    # Isolate HOME so hook doesn't read/write ~/.tokenless/.claude-version
    if isolated_home:
        env["HOME"] = isolated_home
    for key, value in (extra_env or {}).items():
        if value is None:
            env.pop(key, None)
        else:
            env[key] = value
    return env


def _run_hook(stdin_data: dict, agent_id: str, mock_tokenless_path: str,
              isolated_home: str = None, extra_env: dict = None,
              ancestor_cmd: str = None, ancestor_args: list = None) -> dict:
    """Run the hook as a subprocess with mocked tokenless binary.

    Args:
        stdin_data: JSON payload to feed to the hook via stdin.
        agent_id: The adapter agent ID (e.g. "claude-code").
        mock_tokenless_path: Path to the mock tokenless binary.
        isolated_home: Temporary HOME directory for the subprocess to avoid
            touching the caller's ~/.tokenless state.
        extra_env: Extra environment variables for the hook subprocess; a
            None value removes the variable from the inherited environment.
        ancestor_cmd: Optional executable that runs the hook as its child,
            placing itself in the hook's process ancestry (used to exercise
            the WorkBuddy CLI host discriminator).
        ancestor_args: Extra argv entries handed to ancestor_cmd before the
            hook command (e.g. hosted-mode flags such as ``--serve``).

    Returns:
        Parsed JSON output dict from the hook, or a dict with ``_subprocess_error``
        key when the hook exits non-zero.
    """
    hook_path = _hook_script_path()
    env = _build_hook_env(agent_id, mock_tokenless_path, isolated_home,
                          extra_env)

    cmd = [sys.executable, hook_path]
    if ancestor_cmd:
        cmd = [ancestor_cmd] + list(ancestor_args or []) + cmd

    proc = subprocess.run(
        cmd,
        input=json.dumps(stdin_data),
        capture_output=True,
        text=True,
        timeout=10,
        env=env,
    )

    # Check returncode first — a non-zero exit indicates a real failure
    # (import error, runtime crash, etc.) that should not be silently
    # swallowed as an empty result.
    if proc.returncode != 0:
        return {
            "_subprocess_error": True,
            "_returncode": proc.returncode,
            "_stderr": proc.stderr,
            "_stdout": proc.stdout,
        }

    stdout = proc.stdout.strip()
    if not stdout or stdout == "{}":
        return {}
    try:
        return json.loads(stdout)
    except json.JSONDecodeError:
        return {"_raw_stdout": stdout, "_stderr": proc.stderr}


def _run_hook_in_pty(stdin_data: dict, env: dict, cmd: list) -> dict:
    """Run the hook pipeline inside a fresh controlling terminal.

    The child gets a new session whose controlling terminal is the pty —
    the shape of an interactive CLI session, so the hook's ancestor walk
    observes a controlling terminal on the CLI process regardless of
    whether the test runner itself has one. Echo and output
    post-processing are disabled before any data flows, and the hook's
    stderr goes to /dev/null so only its stdout JSON is captured.

    Callers guard with ``skipIf(os.name == "nt" or pty is None, ...)``; the
    check below keeps an unguarded caller skipping instead of raising
    ``AttributeError`` on a host without the POSIX modules.
    """
    if (
        pty is None
        or fcntl is None
        or termios is None
        or not hasattr(select, "poll")
        or not hasattr(os, "fork")
    ):
        raise unittest.SkipTest("pty-based host classification needs POSIX")
    payload = (json.dumps(stdin_data) + "\n").encode()
    master_fd, slave_fd = pty.openpty()
    attrs = termios.tcgetattr(slave_fd)
    attrs[3] &= ~termios.ECHO   # payload must not bounce into the stream
    attrs[1] &= ~termios.OPOST  # keep the hook's JSON byte-exact
    termios.tcsetattr(slave_fd, termios.TCSANOW, attrs)

    devnull = os.open(os.devnull, os.O_WRONLY)
    child_pid = os.fork()
    if child_pid == 0:
        try:
            os.setsid()
            fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)
            os.dup2(slave_fd, 0)
            os.dup2(slave_fd, 1)
            os.dup2(devnull, 2)
            for fd in (slave_fd, master_fd, devnull):
                if fd > 2:
                    try:
                        os.close(fd)
                    except OSError:
                        pass
            os.execvpe(cmd[0], cmd, env)
        finally:
            os._exit(127)

    os.close(slave_fd)
    os.close(devnull)

    deadline = time.monotonic() + 30
    buf = b""
    try:
        # Canonical mode: deliver the one-line payload, then VEOF flushes
        # EOF to the hook's stdin. poll() (not select()): sandboxed hosts
        # can allocate the master fd beyond FD_SETSIZE.
        view = memoryview(payload)
        while view:
            written = os.write(master_fd, view)
            view = view[written:]
        os.write(master_fd, b"\x04")

        poller = select.poll()
        poller.register(master_fd, select.POLLIN)
        reaped = False
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("pty hook run timed out")
            try:
                events = poller.poll(200)
            except InterruptedError:
                continue
            got = False
            for _fd, event in events:
                if event & select.POLLIN:
                    try:
                        chunk = os.read(master_fd, 65536)
                    except OSError:
                        chunk = b""
                    if chunk:
                        buf += chunk
                        got = True
            if not reaped:
                pid, _ = os.waitpid(child_pid, os.WNOHANG)
                if pid == child_pid:
                    reaped = True
            if reaped and not got:
                break
    finally:
        try:
            os.close(master_fd)
        except OSError:
            pass
    out = buf.decode("utf-8", "replace").replace("\r", "").strip()
    if not out or out == "{}":
        return {}
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return {"_raw_stdout": out}


_needs_py39 = sys.version_info < (3, 9)


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestBinaryFallbackPaths(unittest.TestCase):
    @staticmethod
    def _hook_utils() -> types.ModuleType:
        hooks_dir = os.path.normpath(os.path.join(
            os.path.dirname(__file__),
            os.pardir, "adapters", "tokenless", "common", "hooks",
        ))
        sys.path.insert(0, hooks_dir)
        try:
            import hook_utils
        finally:
            sys.path.pop(0)
        return hook_utils

    @staticmethod
    def _codex_check_tokenless() -> types.ModuleType:
        script_path = os.path.normpath(
            os.path.join(
                os.path.dirname(__file__),
                os.pardir,
                "adapters",
                "tokenless",
                "codex",
                "scripts",
                "check-tokenless",
            )
        )
        loader = importlib.machinery.SourceFileLoader(
            "codex_check_tokenless", script_path
        )
        spec = importlib.util.spec_from_loader("codex_check_tokenless", loader)
        if spec is None or spec.loader is None:
            raise RuntimeError("unable to load codex check-tokenless")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_supported_install_layouts_are_covered(self) -> None:
        paths = self._hook_utils()._known_binary_paths("rtk", "/home/alice")
        expected = (
            "/home/alice/.local/bin/rtk",
            "/home/alice/.local/lib/anolisa/libexec/tokenless/rtk",
            "/home/alice/.local/libexec/anolisa/tokenless/rtk",
            "/usr/local/bin/rtk",
            "/usr/local/libexec/anolisa/tokenless/rtk",
            "/usr/bin/rtk",
            "/usr/libexec/anolisa/tokenless/rtk",
            "/usr/lib/anolisa/tokenless/rtk",
            "/home/alice/.local/share/anolisa/tokenless/rtk",
            "/home/alice/.local/lib/anolisa/tokenless/rtk",
        )
        self.assertEqual(paths, expected)

    def test_generic_binaries_skip_tokenless_helper_dirs(self) -> None:
        paths = self._hook_utils()._known_binary_paths("docker", "/home/alice")
        self.assertIn("/home/alice/.local/bin/docker", paths)
        self.assertIn("/usr/local/bin/docker", paths)
        self.assertIn("/usr/bin/docker", paths)
        self.assertFalse(any("tokenless" in path for path in paths))

    def test_user_layouts_require_an_absolute_home(self) -> None:
        hook_utils = self._hook_utils()
        for home in ("", "relative/home"):
            with self.subTest(home=home):
                paths = hook_utils._known_binary_paths("rtk", home)
                self.assertTrue(all(os.path.isabs(path) for path in paths))
                self.assertFalse(any(".local" in path for path in paths))

    def test_codex_check_tokenless_uses_canonical_order(self) -> None:
        paths = self._codex_check_tokenless()._known_tokenless_paths("/home/alice")
        self.assertEqual(
            paths,
            (
                "/home/alice/.local/bin/tokenless",
                "/usr/local/bin/tokenless",
                "/usr/bin/tokenless",
                "/home/alice/.local/share/anolisa/tokenless/tokenless",
                "/home/alice/.local/lib/anolisa/tokenless/tokenless",
            ),
        )

    def test_codex_check_tokenless_rejects_invalid_home(self) -> None:
        check_tokenless = self._codex_check_tokenless()
        for home in ("", "relative/home"):
            with self.subTest(home=home):
                paths = check_tokenless._known_tokenless_paths(home)
                self.assertEqual(
                    paths,
                    ("/usr/local/bin/tokenless", "/usr/bin/tokenless"),
                )

    def test_resolver_finds_makefile_user_helper_without_path(self) -> None:
        hook_utils = self._hook_utils()
        with tempfile.TemporaryDirectory() as home:
            helper_dir = os.path.join(
                home, ".local", "libexec", "anolisa", "tokenless"
            )
            os.makedirs(helper_dir)
            rtk_path = os.path.join(helper_dir, "rtk")
            with open(rtk_path, "w", encoding="utf-8") as handle:
                handle.write("#!/bin/sh\n")
            os.chmod(rtk_path, 0o755)

            hook_utils._resolved_cache.clear()
            with (
                mock.patch.dict(os.environ, {"HOME": home}),
                mock.patch.object(hook_utils.shutil, "which", return_value=None),
            ):
                self.assertEqual(hook_utils.resolve_binary("rtk"), rtk_path)
            hook_utils._resolved_cache.clear()

    def test_resolver_prefers_user_layout_to_explicit_legacy_fallback(self) -> None:
        hook_utils = self._hook_utils()
        with tempfile.TemporaryDirectory() as home:
            local_bin = os.path.join(home, ".local", "bin")
            legacy_bin = os.path.join(home, "legacy")
            os.makedirs(local_bin)
            os.makedirs(legacy_bin)
            user_rtk = os.path.join(local_bin, "rtk")
            legacy_rtk = os.path.join(legacy_bin, "rtk")
            for path in (user_rtk, legacy_rtk):
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write("#!/bin/sh\n")
                os.chmod(path, 0o755)

            hook_utils._resolved_cache.clear()
            with (
                mock.patch.dict(os.environ, {"HOME": home}),
                mock.patch.object(hook_utils.shutil, "which", return_value=None),
            ):
                self.assertEqual(
                    hook_utils.resolve_binary("rtk", legacy_rtk), user_rtk
                )
            hook_utils._resolved_cache.clear()


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestRetrieveCommandClassifier(unittest.TestCase):
    @staticmethod
    def _classify(tool_name: str, command: object) -> bool:
        hook_utils = TestBinaryFallbackPaths._hook_utils()
        return hook_utils.is_tokenless_retrieve_command(
            tool_name, {"command": command}
        )

    def test_accepts_generated_and_direct_retrieve_commands(self):
        marker = "<<tokenless:0123456789abcdef01234567>>"
        commands = (
            f"tokenless retrieve '{marker}'",
            f'tokenless retrieve "{marker}"',
            "tokenless retrieve ABCDEF0123456789ABCDEF01",
        )
        for command in commands:
            with self.subTest(command=command):
                self.assertTrue(self._classify("Bash", command))

    def test_rejects_non_retrieve_shell_syntax_and_invalid_boundaries(self):
        marker = "<<tokenless:0123456789abcdef01234567>>"
        cases = (
            ("Read", f"tokenless retrieve '{marker}'"),
            ("Bash", f"relative/tokenless retrieve '{marker}'"),
            ("Bash", f"/usr/bin/tokenless retrieve '{marker}'"),
            ("Bash", f"'/usr/local/bin/tokenless' retrieve '{marker}'"),
            ("Bash", f"'/tmp/tokenless test/tokenless' retrieve '{marker}'"),
            ("Bash", f"tokenless retrieve {marker}"),
            ("Bash", "tokenless retrieve 0123456789abcdef01234567 # comment"),
            ("Bash", "tokenless retrieve 0123456789abcdef0123456\\7"),
            ("Bash", "tokenless retrieve $'0123456789abcdef01234567'"),
            ("Bash", "tokenless retrieve\n0123456789abcdef01234567"),
            ("Bash", "tokenless retrieve 0123456789abcdef01234567\u00a0"),
            ("Bash", f"tokenless retrieve '{marker}' | jq ."),
            ("Bash", f"tokenless retrieve '{marker}' > recovered.json"),
            ("Bash", f"tokenless retrieve '{marker}'; echo done"),
            ("Bash", f"tokenless retrieve '{marker}' extra"),
            ("Bash", "tokenless retrieve <<tokenless:not-a-hash>>"),
            ("Bash", "tokenless retrieve 'unterminated"),
            ("Bash", 42),
        )
        for tool_name, command in cases:
            with self.subTest(tool_name=tool_name, command=command):
                self.assertFalse(self._classify(tool_name, command))

    def test_recovery_requires_bare_tokenless_on_path(self):
        hook_utils = TestBinaryFallbackPaths._hook_utils()
        with mock.patch.object(hook_utils.shutil, "which", return_value=None):
            self.assertFalse(hook_utils.tokenless_retrieve_command_available())
        with mock.patch.object(
            hook_utils.shutil, "which", return_value="/usr/bin/tokenless"
        ):
            self.assertTrue(hook_utils.tokenless_retrieve_command_available())


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestReplacementProtocol(unittest.TestCase):
    """Verify updatedToolOutput replacement semantics."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.isolated_home = tempfile.mkdtemp(prefix="test_hook_home_")
        self.mock_bin = _create_mock_tokenless(self.tmpdir, "compress")
        self.mock_claude = _create_mock_claude(self.tmpdir)

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)
        shutil.rmtree(self.isolated_home, ignore_errors=True)

    def test_claude_code_uses_updated_tool_output(self):
        """Claude Code adapter should use updatedToolOutput, not additionalContext."""
        large_payload = _make_large_json_payload()

        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "test-session",
                "tool_use_id": "toolu_test",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertEqual(hso.get("hookEventName"), "PostToolUse")
        self.assertIn("updatedToolOutput", hso,
                       "Claude Code should use updatedToolOutput for replacement")
        self.assertNotIn("additionalContext", hso,
                         "Compressed content must not be in additionalContext (duplication)")

    def test_qoder_cli_uses_updated_tool_output(self):
        """Qoder CLI should replace tool output without version gating."""
        large_payload = _make_large_json_payload()

        result = _run_hook(
            {
                "tool_name": "run_in_terminal",
                "tool_response": large_payload,
                "session_id": "test-session",
                "tool_use_id": "toolu_test",
            },
            agent_id="qoder-cli",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertEqual(hso.get("hookEventName"), "PostToolUse")
        self.assertIn("updatedToolOutput", hso,
                      "Qoder CLI should use updatedToolOutput for replacement")
        updated_output = hso["updatedToolOutput"]
        self.assertIsInstance(
            updated_output,
            str,
            "Qoder CLI requires updatedToolOutput to be a string",
        )
        compressed_data = json.loads(updated_output)
        self.assertEqual(compressed_data["stdout"], "x" * 20)
        self.assertEqual(compressed_data["stderr"], "")
        self.assertEqual(compressed_data["exit_code"], 0)
        self.assertFalse(compressed_data["interrupted"])
        self.assertNotIn("additionalContext", hso,
                         "Qoder compressed content must not be additive")

    def test_opencode_uses_string_replacement(self):
        """OpenCode should receive a replacement that its plugin can apply."""
        large_payload = _make_large_json_payload()

        result = _run_hook(
            {
                "tool_name": "bash",
                "tool_response": json.dumps(large_payload),
                "session_id": "test-session",
                "tool_use_id": "toolu_test",
            },
            agent_id="opencode",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertEqual(hso.get("hookEventName"), "PostToolUse")
        self.assertIsInstance(hso.get("updatedToolOutput"), str)
        self.assertNotIn("additionalContext", hso,
                         "OpenCode compressed content must not be additive")

    def test_business_exit_code_is_not_a_process_failure(self):
        payload = {
            "exitCode": 1,
            "error": "business status, not a host execution failure",
            "message": "permission denied is a documented business status " * 12,
        }
        result = _run_hook(
            {
                "tool_name": "mcp__analytics_report",
                "tool_response": payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertIn("updatedToolOutput", hso)
        self.assertNotIn("additionalContext", hso)

    def test_cosh_ng_nested_shell_failure_uses_llm_content_status(self):
        result = _run_hook(
            {
                "tool_name": "run_shell_command",
                "tool_response": {
                    "llmContent": {
                        "stdout": "",
                        "stderr": "sh: rg: command not found",
                        "exitCode": 127,
                    },
                    "returnDisplay": "ran `rg pattern` (exit 127)",
                },
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="cosh-ng",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertIn("ENV_DEPENDENCY_MISSING", hso.get("additionalContext", ""))
        self.assertNotIn("updatedToolResponse", hso)

    def test_replacement_is_smaller(self):
        """The replacement output should be smaller than the original."""
        large_payload = _make_large_json_payload(1000)

        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        replacement = hso.get("updatedToolOutput", "")
        original_size = len(json.dumps(large_payload, separators=(",", ":")))
        replacement_size = (
            len(json.dumps(replacement, separators=(",", ":")))
            if isinstance(replacement, (dict, list))
            else len(str(replacement))
        )
        self.assertLess(replacement_size, original_size,
                        "Replacement should be smaller than original")

    def test_replacement_content_structure(self):
        """Replacement should contain compressed stdout and valid schema fields."""
        large_payload = _make_large_json_payload()

        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        replacement = hso.get("updatedToolOutput", "")

        # The mock compressor truncates strings > 20 chars, so stdout should
        # be truncated. Verify the compressed content is present and parseable.
        # Note: updatedToolOutput may be a JSON string or already-parsed dict
        # depending on how the hook encodes it.
        if isinstance(replacement, str):
            try:
                compressed_data = json.loads(replacement)
            except json.JSONDecodeError:
                self.fail(f"updatedToolOutput should be valid JSON, got: {replacement!r}")
        elif isinstance(replacement, (dict, list)):
            compressed_data = replacement
        else:
            self.fail(f"updatedToolOutput unexpected type: {type(replacement)}")

        # Verify stdout field was compressed to exactly 20 chars by mock
        # (mock truncates strings > 20 to their first 20 chars).
        self.assertIn("stdout", compressed_data,
                       "Compressed output should preserve stdout key")
        self.assertEqual(compressed_data["stdout"], "x" * 20,
                         "stdout should be truncated to exactly 'x' * 20")

        # Verify schema fields are preserved with correct values
        self.assertEqual(compressed_data["exit_code"], 0)
        self.assertEqual(compressed_data["interrupted"], False)

    def test_no_duplicate_content(self):
        """The original sentinel must not appear alongside compressed output."""
        sentinel = "UNIQUE_SENTINEL_12345"
        # Mock truncates strings > 20 chars; sentinel is 21 chars,
        # so truncated form is first 20 chars.
        truncated_sentinel = sentinel[:20]
        payload = {"stdout": sentinel * 30, "stderr": "", "exit_code": 0, "interrupted": False}

        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})

        # additionalContext must not contain compressed content
        additional = hso.get("additionalContext", "")
        self.assertNotIn(sentinel, additional,
                         "additionalContext must not contain compressed content")

        # updatedToolOutput should exist and contain the truncated sentinel
        self.assertIn("updatedToolOutput", hso,
                       "Claude Code should use updatedToolOutput")
        updated = hso["updatedToolOutput"]
        if isinstance(updated, str):
            updated_data = json.loads(updated)
        else:
            updated_data = updated

        # The mock truncates the sentinel (21 chars) to its first 20 chars.
        # Assert the truncated form IS present (proves content wasn't lost).
        self.assertIn("stdout", updated_data,
                       "updatedToolOutput should contain stdout field")
        self.assertEqual(updated_data["stdout"], truncated_sentinel,
                         "stdout should be the truncated sentinel (first 20 chars)")

        # Full sentinel must NOT appear (proves content wasn't duplicated)
        updated_str = json.dumps(updated) if isinstance(updated, (dict, list)) else str(updated)
        self.assertNotIn(sentinel * 30, updated_str,
                         "updatedToolOutput must not contain the full original sentinel")


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestShellEnvelopeUnwrap(unittest.TestCase):
    """Shell envelopes ride the text slot: the dominant stdout/stderr field
    is sent as plain text and the compressed text is re-injected into a
    same-shaped envelope — the host's tool protocol stays intact."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.isolated_home = tempfile.mkdtemp(prefix="test_hook_home_")
        self.mock_bin = _create_mock_tokenless(self.tmpdir, "compress-text")
        self.mock_claude = _create_mock_claude(self.tmpdir)

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)
        shutil.rmtree(self.isolated_home, ignore_errors=True)

    @staticmethod
    def _bash_envelope(stdout: str, stderr: str) -> dict:
        return {
            "stdout": stdout,
            "stderr": stderr,
            "interrupted": False,
            "isImage": False,
        }

    def test_stderr_dominant_envelope_is_rewrapped_in_place(self):
        log = "error: build failed\n" + "junk line\n" * 300
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": self._bash_envelope("", log),
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )
        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        updated = result["hookSpecificOutput"]["updatedToolOutput"]
        self.assertEqual(updated, self._bash_envelope("", log[:40]),
                         "Compressed text must replace exactly the sent field")
        self.assertEqual(len(_spawn_log_lines(self.mock_bin)), 1,
                         "Unwrapping must not add a second subprocess")

    def test_largest_field_wins_and_the_other_stays_verbatim(self):
        stdout = "info: routine progress line\n" * 100
        stderr = "warn: something odd\n" * 110
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": self._bash_envelope(stdout, stderr),
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )
        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        updated = result["hookSpecificOutput"]["updatedToolOutput"]
        self.assertEqual(updated, self._bash_envelope(stdout[:40], stderr))

    def test_qoder_rewrapped_envelope_is_a_compact_json_string(self):
        log = "npm ERR! code ELIFECYCLE\n" + "npm verbose stack line\n" * 150
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": self._bash_envelope(log, ""),
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="qoder-cli",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )
        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        updated = result["hookSpecificOutput"]["updatedToolOutput"]
        self.assertIsInstance(updated, str,
                              "Qoder requires a string updatedToolOutput")
        self.assertEqual(json.loads(updated), self._bash_envelope(log[:40], ""))


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestGrepEnvelopeUnwrap(unittest.TestCase):
    def test_claude_content_slot_is_replaced_without_changing_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = _create_mock_tokenless(directory, "compress-text")
            _create_mock_claude(directory)
            content = "crates/long_directory/file.rs:7:  matching text  \n" * 12
            envelope = {
                "mode": "content", "content": content, "numLines": 12,
                "numFiles": 0, "filenames": [], "appliedLimit": 12,
            }
            result = _run_hook(
                {"tool_name": "Grep", "tool_response": envelope},
                "claude-code", binary, directory,
            )
            self.assertEqual(
                result["hookSpecificOutput"]["updatedToolOutput"],
                dict(envelope, content=content[:40]),
            )
            with open(os.path.join(directory, "request.json")) as captured:
                request = json.load(captured)
            self.assertEqual(request["content"], content)
            self.assertEqual(request["content_origin"], "api_response")
            self.assertTrue(request["capabilities"]["replace_with_text"])
            self.assertEqual(len(_spawn_log_lines(binary)), 1)

    def test_invalid_tool_input_keeps_the_existing_route(self) -> None:
        for tool_input in [None, "grep -rn foo", ["-C"], 3]:
            with self.subTest(tool_input=tool_input), tempfile.TemporaryDirectory() as directory:
                binary = _create_mock_tokenless(directory, "compress-text")
                _create_mock_claude(directory)
                result = _run_hook(
                    {"tool_name": "Grep", "tool_input": tool_input, "tool_response": {
                        "mode": "content", "content": "file.rs:1:match\n" * 20,
                    }},
                    "claude-code", binary, directory,
                )
                self.assertEqual(result, {})
                with open(os.path.join(directory, "request.json")) as captured:
                    request = json.load(captured)
                self.assertEqual(request["content_origin"], "file_content")

    def test_other_modes_hosts_and_context_keep_the_existing_json_route(self) -> None:
        cases = [
            ("claude-code", "count", {}),
            ("claude-code", "files_with_matches", {}),
            ("qoder-cli", "content", {}),
            ("opencode", "content", {}),
            *[("claude-code", "content", {flag: 2})
              for flag in ("-A", "-B", "-C", "context")],
        ]
        for agent, mode, tool_input in cases:
            with self.subTest(agent=agent, mode=mode, tool_input=tool_input):
                with tempfile.TemporaryDirectory() as directory:
                    binary = _create_mock_tokenless(directory, "no-savings")
                    _create_mock_claude(directory)
                    envelope = {"mode": mode, "content": "file.rs:1:match\n" * 20}
                    result = _run_hook(
                        {"tool_name": "Grep", "tool_response": envelope,
                         "tool_input": tool_input},
                        agent, binary, directory,
                    )
                    self.assertEqual(result, {})
                    with open(os.path.join(directory, "request.json")) as captured:
                        request = json.load(captured)
                    self.assertEqual(json.loads(request["content"]), envelope)
                    self.assertFalse(request["capabilities"]["replace_with_text"])

    def test_no_savings_and_old_claude_do_not_emit_replacements(self) -> None:
        for version in ["2.1.120", "2.1.259"]:
            with self.subTest(version=version), tempfile.TemporaryDirectory() as directory:
                binary = _create_mock_tokenless(directory, "no-savings")
                _create_mock_claude(directory, version)
                result = _run_hook(
                    {"tool_name": "Grep", "tool_response": {
                        "mode": "content", "content": "file.rs:1:match\n" * 20,
                    }},
                    "claude-code", binary, directory,
                )
                self.assertEqual(result, {})
                with open(os.path.join(directory, "request.json")) as captured:
                    request = json.load(captured)
                self.assertEqual(
                    request["capabilities"]["replace_output"], version == "2.1.259"
                )
                self.assertEqual(request["content_origin"], "api_response")


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestPassthrough(unittest.TestCase):
    """Verify pass-through when compression yields no size reduction."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.isolated_home = tempfile.mkdtemp(prefix="test_hook_home_")
        self.mock_bin = _create_mock_tokenless(self.tmpdir, "no-savings")
        self.mock_claude = _create_mock_claude(self.tmpdir)

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)
        shutil.rmtree(self.isolated_home, ignore_errors=True)

    def test_skip_when_no_compression_savings(self):
        """When compression does not reduce size, output should be empty (skip)."""
        payload = _make_large_json_payload()

        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertEqual(result, {},
                         "Should skip when compression yields no savings")

    def test_version_skewed_response_fails_open(self):
        """A response declaring a protocol version this adapter does not
        speak must never replace model-visible output."""
        mock_dir = tempfile.mkdtemp(dir=self.tmpdir)
        mock_bin = _create_mock_tokenless(mock_dir, "wrong-protocol-version")
        _create_mock_claude(mock_dir)

        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": _make_large_json_payload(),
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertEqual(result, {},
                         "Version-skewed responses must fail open")


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestSkipTools(unittest.TestCase):
    """Verify skip-tools behavior (content retrieval tools)."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.isolated_home = tempfile.mkdtemp(prefix="test_hook_home_")
        self.mock_bin = _create_mock_tokenless(self.tmpdir, "compress")
        self.mock_claude = _create_mock_claude(self.tmpdir)

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)
        shutil.rmtree(self.isolated_home, ignore_errors=True)

    def test_skip_tools_no_replacement(self):
        """Skip-tools (Read) should not use updatedToolOutput."""
        payload = {"stdout": "file content", "stderr": "", "exit_code": 0}

        result = _run_hook(
            {
                "tool_name": "Read",
                "tool_response": payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertEqual(result, {},
                         "Skip-tools (Read) should produce empty result (pass-through)")
        hso = result.get("hookSpecificOutput", {})
        self.assertNotIn("updatedToolOutput", hso,
                         "Skip-tools should not replace tool output")

    def test_skip_tools_are_classified_by_core(self):
        """File-content policy belongs to the PostTool service."""
        result = _run_hook(
            {
                "tool_name": "Read",
                "tool_response": _make_large_json_payload(),
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertEqual(result, {})
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"])


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestNonReplacementAdapters(unittest.TestCase):
    """additionalContext-only hosts pass through (roadmap: additive
    injection would append the compressed copy beside the still-visible
    original, a net token increase)."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.isolated_home = tempfile.mkdtemp(prefix="test_hook_home_")
        self.mock_bin = _create_mock_tokenless(self.tmpdir, "compress")
        self.mock_claude = _create_mock_claude(self.tmpdir)

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)
        shutil.rmtree(self.isolated_home, ignore_errors=True)

    def test_qwencode_passes_through_via_core(self):
        """Qwen Code declares no replacement capability to Core."""
        large_payload = _make_large_json_payload()

        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="qwencode",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertEqual(result, {},
                         "Hosts without true replacement remain passthrough")
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"])

    def test_qwencode_still_receives_env_attribution(self):
        """Environment attribution is genuinely additive and stays."""
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": {"stdout": "", "stderr": "bash: rg: command not found",
                                  "exit_code": 127},
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="qwencode",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertIn("[tokenless:env]", hso.get("additionalContext", ""))
        self.assertNotIn("updatedToolOutput", hso)

    def test_shell_diagnostic_uses_short_stderr_not_large_stdout(self):
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": {
                    "stdout": "routine output\n" * 1_000,
                    "stderr": "bash: rg: command not found",
                    "exit_code": 127,
                },
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="qwencode",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertIn("ENV_DEPENDENCY_MISSING", hso.get("additionalContext", ""))
        self.assertNotIn("updatedToolOutput", hso)

    def _fake_codebuddy(self) -> str:
        """Stage a ``codebuddy`` executable that runs the hook as its child.

        The script forks the command it receives and waits, so the hook
        process really has a ``codebuddy`` ancestor — the exact shape of
        the CodeBuddy Code CLI running a settings.json hook command.
        Leading ``--flags`` stay in the process argv (hosted-mode shapes)
        but are skipped before executing the child command.
        """
        path = os.path.join(self.tmpdir, "codebuddy")
        with open(path, "w") as f:
            # Skip every leading flag (long ``--serve`` and short ``-p``
            # alike) so the remaining words are executed as the child
            # command, while the flags stay in this process' argv for the
            # ancestor walk to observe.
            f.write(
                "#!/bin/sh\n"
                "while [ $# -gt 0 ]; do\n"
                "  case \"$1\" in\n"
                "    -*) shift ;;\n"
                "    *) break ;;\n"
                "  esac\n"
                "done\n"
                "\"$@\"\n"
            )
        os.chmod(path, 0o755)
        return path

    def _pty_env(self, extra_env: dict) -> dict:
        return _build_hook_env(
            "workbuddy", self.mock_bin, self.isolated_home, extra_env
        )

    @unittest.skipIf(os.name == "nt" or pty is None,
                     "interactive-shape test needs a POSIX pty")
    def test_workbuddy_cli_uses_updated_tool_output(self):
        """CodeBuddy Code CLI host: the compressed payload replaces the result.

        The CodeBuddy CLI Hooks contract (v1.16.0+) defines
        updatedToolOutput as a full replacement for built-in and MCP
        tools; additionalContext would keep the original result and
        append. Standalone detection classifies a CLI-binary ancestor
        free of every hosted signal (launcher marker and hosted sidecar
        argv flags); this test exercises the interactive terminal shape
        under a real ``codebuddy`` ancestor in a fresh pty session, with
        all hosted signals absent.
        """
        large_payload = _make_large_json_payload()
        env = self._pty_env({
            "CODEBUDDY_FORCE_HEADLESS_BUNDLE": None,
            "CODEBUDDY_SESSION_KIND": None,
        })
        result = _run_hook_in_pty(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            env,
            [self._fake_codebuddy(), sys.executable, _hook_script_path()],
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertNotIn("_raw_stdout", result, f"unparsable output: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertIn("updatedToolOutput", hso,
                      "CodeBuddy CLI should replace the tool result")
        # The mock compresses long strings to 20 chars: the replacement is
        # the compressed payload, not a copy of the original.
        self.assertEqual(hso["updatedToolOutput"]["stdout"], "x" * 20)
        # No additive duplicate of the compressed payload beside it.
        self.assertNotIn("additionalContext", hso,
                         "CLI replacement must not append the same payload")
        # The CLI path still runs the compressor: exactly one unified
        # entry-point invocation (the one-subprocess contract).
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"])

    @unittest.skipIf(os.name == "nt", "process ancestry walk is POSIX-only")
    def test_workbuddy_headless_cli_uses_updated_tool_output(self):
        """Supported headless CLI shape (``codebuddy -p`` / ``--print``).

        The Headless Mode documents ``codebuddy -p`` for CI/CD,
        automation scripts and stdin pipelines, where the process
        legitimately owns no controlling terminal; the CLI Hooks contract
        still honors ``updatedToolOutput`` there. This test reproduces
        that shape with a real ``--print`` codebuddy ancestor and no pty:
        the host must be classified as a standalone CLI, the compressed
        payload must replace the tool result, and the compressor must run.
        """
        large_payload = _make_large_json_payload()
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="workbuddy",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
            extra_env={
                "CODEBUDDY_PROJECT_DIR": "/tmp/project",
                "CODEBUDDY_FORCE_HEADLESS_BUNDLE": None,
                "CODEBUDDY_SESSION_KIND": None,
            },
            ancestor_cmd=self._fake_codebuddy(),
            ancestor_args=["--print", "-p"],
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertNotIn("_raw_stdout", result, f"unparsable output: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertIn("updatedToolOutput", hso,
                      "headless CLI must replace the tool result even "
                      "without a controlling terminal")
        self.assertEqual(hso["updatedToolOutput"]["stdout"], "x" * 20)
        self.assertNotIn("additionalContext", hso,
                         "CLI replacement must not append the same payload")
        # The CLI path still runs the compressor: exactly one unified
        # entry-point invocation (the one-subprocess contract).
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"])

    @unittest.skipIf(os.name == "nt", "process ancestry walk is POSIX-only")
    def test_workbuddy_bg_session_uses_updated_tool_output(self):
        """First-class background session (``codebuddy --bg``).

        The Daemon Mode reference documents the background session as a
        first-class CLI task: the CLI forks it as ``--print -y`` and
        declares CODEBUDDY_SESSION_KIND=bg, which the hook inherits. The
        host must still be classified as a standalone CLI — the compressed
        payload must replace the tool result and the compressor must run —
        even though the session kind is ``bg`` and there is no TTY.
        """
        large_payload = _make_large_json_payload()
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="workbuddy",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
            extra_env={
                "CODEBUDDY_PROJECT_DIR": "/tmp/project",
                "CODEBUDDY_FORCE_HEADLESS_BUNDLE": None,
                "CODEBUDDY_SESSION_KIND": "bg",
            },
            ancestor_cmd=self._fake_codebuddy(),
            ancestor_args=["--print", "-y"],
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertNotIn("_raw_stdout", result, f"unparsable output: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertIn("updatedToolOutput", hso,
                      "bg session must replace the tool result even though "
                      "CODEBUDDY_SESSION_KIND=bg and there is no TTY")
        self.assertEqual(hso["updatedToolOutput"]["stdout"], "x" * 20)
        self.assertNotIn("additionalContext", hso,
                         "CLI replacement must not append the same payload")
        # The CLI path still runs the compressor: exactly one unified
        # entry-point invocation (the one-subprocess contract).
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"])

    @unittest.skipIf(os.name == "nt" or pty is None,
                     "interactive-shape test needs a POSIX pty")
    def test_workbuddy_web_ui_serve_uses_updated_tool_output(self):
        """User-started ``codebuddy --serve`` Web UI is a standalone CLI.

        The official Web UI reference documents users launching
        ``codebuddy --serve --port <port>`` directly; the CLI Hooks
        contract honors ``updatedToolOutput`` in that host, so the serve
        argv shape must NOT be treated as a spawned sidecar. The
        resident daemon with an identical argv shape is separated by the
        daemon session kind (covered separately). With no launcher
        marker, no daemon kind and no hosted sidecar flag, the host is a
        standalone CLI and the compressed payload replaces the result.
        """
        large_payload = _make_large_json_payload()
        env = self._pty_env({
            "CODEBUDDY_PROJECT_DIR": "/tmp/project",
            "CODEBUDDY_FORCE_HEADLESS_BUNDLE": None,
            "CODEBUDDY_SESSION_KIND": None,
        })
        result = _run_hook_in_pty(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            env,
            [self._fake_codebuddy(), "--serve",
             sys.executable, _hook_script_path()],
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertNotIn("_raw_stdout", result, f"unparsable output: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertIn("updatedToolOutput", hso,
                      "user-started serve (Web UI) must receive the CLI "
                      "replacement path")
        self.assertEqual(hso["updatedToolOutput"]["stdout"], "x" * 20)
        self.assertNotIn("additionalContext", hso,
                         "CLI replacement must not append the same payload")
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"])

    @unittest.skipIf(os.name == "nt", "process ancestry walk is POSIX-only")
    def test_workbuddy_daemon_worker_skips_replacement(self):
        """Resident daemon worker: documented session kind, ``--serve`` argv.

        ``daemon start`` forks the resident child with ``--serve``
        prepended (Daemon Mode reference), so argv alone cannot separate
        the daemon from a user-started Web UI; the documented
        ``CODEBUDDY_SESSION_KIND=daemon`` worker-type variable is the
        contract-backed discriminator and must classify the host as
        non-CLI. Compression is disabled: Core is consulted once and
        returns a passthrough.
        """
        large_payload = _make_large_json_payload()
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="workbuddy",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
            extra_env={
                "CODEBUDDY_PROJECT_DIR": "/tmp/project",
                "CODEBUDDY_FORCE_HEADLESS_BUNDLE": None,
                "CODEBUDDY_SESSION_KIND": "daemon",
            },
            ancestor_cmd=self._fake_codebuddy(),
            ancestor_args=["--serve"],
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertNotIn("updatedToolOutput", hso,
                         "daemon workers must not receive the CLI-only "
                         "field despite a CLI ancestor with --serve")
        self.assertNotIn("additionalContext", hso)
        self.assertEqual(result, {},
                         "daemon workbuddy hosts pass the result through")
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"],
                         "exactly one Core passthrough; no compression applied")

    def test_workbuddy_ide_host_skips_replacement(self):
        """IDE host: CODEBUDDY_PROJECT_DIR set, no ``codebuddy`` ancestor.

        The IDE Hooks reference lists CODEBUDDY_PROJECT_DIR among the
        environment variables available to IDE hook scripts as well, so
        the variable alone must not route the host to the CLI-only
        updatedToolOutput path. The IDE PostToolUse contract documents
        only the additive additionalContext, which keeps the original
        tool result, so compressing through it would grow the context:
        the hook fails open and passes the result through unchanged.
        """
        large_payload = _make_large_json_payload()
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="workbuddy",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
            extra_env={
                "CODEBUDDY_PROJECT_DIR": "/tmp/project",
                "CODEBUDDY_FORCE_HEADLESS_BUNDLE": None,
            },
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertNotIn("updatedToolOutput", hso,
                         "IDE hosts must not receive the CLI-only field")
        self.assertNotIn("additionalContext", hso,
                         "compressed payload must not be appended beside "
                         "the original result on non-CLI hosts")
        self.assertEqual(result, {},
                         "Non-CLI workbuddy hosts pass the result through")
        # Non-CLI hosts declare no replacement capability: Core is
        # consulted once and returns a passthrough without running the
        # compression pipeline.
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"],
                         "exactly one Core passthrough; no compression applied")

    @unittest.skipIf(os.name == "nt", "process ancestry walk is POSIX-only")
    def test_workbuddy_desktop_sidecar_host_skips_replacement(self):
        """WorkBuddy desktop: CLI sidecar ancestry + host launcher env.

        WorkBuddy desktop runs its agent inside an embedded headless
        CodeBuddy Code bundle: the host spawns ``cbc`` for the sidecar
        and prewarm processes with CODEBUDDY_FORCE_HEADLESS_BUNDLE=1
        (documented in the published CLI package's bin/codebuddy entry),
        and the hook executor merges the bundle's process environment
        into the hook environment. The hook's ancestor chain therefore
        really contains the CLI binary on the desktop host — ancestry
        alone would misroute it. The launcher variable must classify the
        host as non-CLI; compression is disabled.
        """
        large_payload = _make_large_json_payload()
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": large_payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="workbuddy",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
            extra_env={
                "CODEBUDDY_PROJECT_DIR": "/tmp/project",
                "CODEBUDDY_FORCE_HEADLESS_BUNDLE": "1",
            },
            ancestor_cmd=self._fake_codebuddy(),
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertNotIn("updatedToolOutput", hso,
                         "desktop host must not receive the CLI-only field "
                         "despite CLI ancestry in its process tree")
        self.assertNotIn("additionalContext", hso)
        self.assertEqual(result, {},
                         "desktop workbuddy hosts pass the result through")
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"],
                         "exactly one Core passthrough; no compression applied")

    def test_workbuddy_non_cli_host_keeps_env_attribution(self):
        """Non-CLI hosts still get genuinely additive env attribution."""
        payload = {
            "stdout": "x" * 500,
            "stderr": "bash: foo: command not found",
            "exit_code": 127,
        }
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": payload,
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="workbuddy",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
            extra_env={
                "CODEBUDDY_PROJECT_DIR": "/tmp/project",
                "CODEBUDDY_FORCE_HEADLESS_BUNDLE": None,
            },
        )

        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        hso = result.get("hookSpecificOutput", {})
        self.assertNotIn("updatedToolOutput", hso)
        context = hso.get("additionalContext", "")
        self.assertIn("[tokenless:env]", context,
                      "environment attribution is genuinely additive and "
                      "supported by every workbuddy host")
        # The compressed payload itself must not be appended beside the
        # original result on hosts without a replacement contract.
        self.assertNotIn("x" * 20, context)
        # Attribution is owned by the PostTool service: the hook declares
        # no replacement capability, Core's diagnosis is still delivered,
        # and the compression pipeline never runs.
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"],
                         "exactly one Core passthrough; no compression applied")


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestSingleSubprocess(unittest.TestCase):
    """One Tokenless subprocess per hook invocation (roadmap §5.6).

    TOON selection and its 500-char gate live behind the entry point now
    (see the Rust entry tests, including the non-BMP code-point cases);
    what the hook owes the contract is that everything happens in a single
    `tokenless compress` spawn.
    """

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.isolated_home = tempfile.mkdtemp(prefix="test_hook_home_")
        self.mock_bin = _create_mock_tokenless(self.tmpdir, "compress")
        self.mock_claude = _create_mock_claude(self.tmpdir)

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)
        shutil.rmtree(self.isolated_home, ignore_errors=True)

    def test_compressible_payload_spawns_exactly_once(self):
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": _make_large_json_payload(1000),
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )
        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertIn("updatedToolOutput", result.get("hookSpecificOutput", {}))
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"],
                         "exactly one tokenless subprocess per invocation")

    def test_small_payload_is_gated_by_core(self):
        result = _run_hook(
            {
                "tool_name": "Bash",
                "tool_response": {"stdout": "short", "exit_code": 0},
                "session_id": "s",
                "tool_use_id": "t",
            },
            agent_id="claude-code",
            mock_tokenless_path=self.mock_bin,
            isolated_home=self.isolated_home,
        )
        self.assertNotIn("_subprocess_error", result,
                         f"Hook subprocess failed: {result}")
        self.assertEqual(result, {})
        self.assertEqual(_spawn_log_lines(self.mock_bin), ["compress"])

@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestWorkBuddyCliDetection(unittest.TestCase):
    """Unit tests for the CodeBuddy Code CLI host discriminator."""

    @staticmethod
    def _hook() -> types.ModuleType:
        hooks_dir = os.path.normpath(os.path.join(
            os.path.dirname(__file__),
            os.pardir, "adapters", "tokenless", "common", "hooks",
        ))
        sys.path.insert(0, hooks_dir)
        try:
            loader = importlib.machinery.SourceFileLoader(
                "compress_response_hook_under_test",
                os.path.join(hooks_dir, "compress_response_hook.py"),
            )
            spec = importlib.util.spec_from_loader(
                "compress_response_hook_under_test", loader
            )
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
        finally:
            sys.path.pop(0)
        return module

    def test_cli_argv_shapes_match(self):
        m = self._hook()
        # The published CLI package registers three bin entries pointing
        # at the same entry script: codebuddy, codebuddy-code and cbc.
        for argv in (
            ["codebuddy"],
            ["codebuddy-code"],
            ["cbc"],
            ["/usr/local/bin/codebuddy", "--debug"],
            ["/bin/sh", "/tmp/hooks/codebuddy", "python3", "hook.py"],
            ["python3", "/opt/codebuddy-cli/codebuddy"],
            ["node", "/usr/lib/node_modules/@tencent-ai/codebuddy-code/bin/codebuddy"],
        ):
            self.assertTrue(m._argv_is_codebuddy_cli(argv), argv)

    def test_non_cli_argv_shapes_do_not_match(self):
        m = self._hook()
        for argv in (
            [],
            ["bash", "-c", "codebuddy"],
            ["/opt/codebuddy-ide/bin/codebuddy-ide"],
            ["vim", "codebuddy-notes.md"],
        ):
            self.assertFalse(m._argv_is_codebuddy_cli(argv), argv)

    def _cli_procs(self):
        """A standalone-CLI-shaped ancestor tree: argv lists."""
        return [
            ["/bin/bash", "-c", "python3 compress_response_hook.py"],
            ["/usr/local/bin/codebuddy"],
            ["/bin/bash", "-lc", "codebuddy"],
        ]

    def test_host_launcher_env_disables_cli_detection(self):
        """CODEBUDDY_FORCE_HEADLESS_BUNDLE marks a WorkBuddy-host spawn.

        The desktop host sets it before spawning the headless bundle and
        the hook inherits it, so even a CLI-shaped ancestor chain must
        not select the CLI-only replacement path. Values follow the
        official entry script's parsing: 1 / true (any case) count,
        anything else is ignored.
        """
        m = self._hook()
        for value in ("1", "true", "TRUE", "True"):
            with mock.patch.object(m, "_ancestor_procs",
                                   return_value=iter(self._cli_procs())):
                with mock.patch.dict(os.environ,
                                     {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": value,
                                      "CODEBUDDY_SESSION_KIND": ""}):
                    self.assertFalse(m._workbuddy_cli_host(), value)
        for value in ("0", "yes", "disabled", ""):
            with mock.patch.object(m, "_ancestor_procs",
                                   return_value=iter(self._cli_procs())):
                with mock.patch.dict(os.environ,
                                     {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": value,
                                      "CODEBUDDY_SESSION_KIND": ""}):
                    self.assertTrue(m._workbuddy_cli_host(), value)

    def test_session_kind_daemon_disables_cli_detection(self):
        """CODEBUDDY_SESSION_KIND=daemon marks the resident daemon worker.

        The official Daemon Mode reference documents the session kind as
        the worker type (interactive / bg / daemon), and the hook
        inherits it from the daemon child that ``daemon start`` forks
        with ``--serve`` prepended. The kind is the contract-backed
        signal separating the daemon from a user-started ``--serve``
        Web UI session, so it must route a CLI-shaped ancestor to the
        non-CLI path; every other value (interactive / bg / teammate /
        unset, any case) belongs to the standalone CLI's own sessions
        and never disables detection.
        """
        m = self._hook()
        for kind in ("daemon", "Daemon", "DAEMON", " daemon "):
            with mock.patch.object(m, "_ancestor_procs",
                                   return_value=iter(self._cli_procs())):
                with mock.patch.dict(os.environ,
                                     {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": "",
                                      "CODEBUDDY_SESSION_KIND": kind}):
                    self.assertFalse(m._workbuddy_cli_host(), kind)
        for kind in ("interactive", "bg", "teammate", "BG", ""):
            with mock.patch.object(m, "_ancestor_procs",
                                   return_value=iter(self._cli_procs())):
                with mock.patch.dict(os.environ,
                                     {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": "",
                                      "CODEBUDDY_SESSION_KIND": kind}):
                    self.assertTrue(m._workbuddy_cli_host(), kind)

    def test_bg_session_kind_is_standalone_cli(self):
        """A real ``codebuddy --bg`` background session is a standalone CLI.

        Reproduces the documented background-session shape: the CLI forks
        the task as ``--print -y`` and declares CODEBUDDY_SESSION_KIND=bg,
        which the hook inherits. The host must still be classified as a
        standalone CLI (the Hooks contract honors updatedToolOutput there),
        whether detected by the bg argv flag or by the declared kind.
        """
        m = self._hook()
        bg_tree = [
            ["/bin/bash", "-c", "python3 compress_response_hook.py"],
            ["/usr/local/bin/codebuddy", "--print", "-y", "--name", "task"],
        ]
        for kind in ("bg", "BG", ""):
            with mock.patch.object(m, "_ancestor_procs",
                                   return_value=iter(bg_tree)):
                with mock.patch.dict(os.environ,
                                     {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": "",
                                      "CODEBUDDY_SESSION_KIND": kind}):
                    self.assertTrue(m._workbuddy_cli_host(), kind)

    def test_hosted_argv_shapes_disable_cli_detection(self):
        """Hosted sidecar flags predate the launcher marker and must win.

        Artifacts before 2.136.0 carry no CODEBUDDY_FORCE_HEADLESS_BUNDLE
        but already ship the hosted prewarm and team-sidecar modes, so a
        CLI ancestor with one of these flags stays non-CLI even though a
        bare CLI ancestor would be treated as standalone. ``--serve`` is
        deliberately absent: the Web UI reference documents users
        starting ``codebuddy --serve`` directly (standalone CLI); the
        daemon child with the same argv is excluded by the session kind.
        """
        m = self._hook()
        for flag in ("--prewarm", "--prewarm-force", "--teammate-mode"):
            tree = [
                ["/bin/bash", "-c", "python3 compress_response_hook.py"],
                ["/usr/local/bin/codebuddy", flag, "--team-name", "t"],
            ]
            with mock.patch.object(m, "_ancestor_procs",
                                   return_value=iter(tree)):
                with mock.patch.dict(os.environ,
                                     {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": "",
                                      "CODEBUDDY_SESSION_KIND": ""}):
                    self.assertFalse(m._workbuddy_cli_host(), flag)

    def test_serve_argv_without_daemon_kind_is_standalone_cli(self):
        """``--serve`` alone is the user-started Web UI, not a sidecar."""
        m = self._hook()
        tree = [
            ["/bin/bash", "-c", "python3 compress_response_hook.py"],
            ["/usr/local/bin/codebuddy", "--serve", "--port", "7890"],
        ]
        for kind in ("interactive", ""):
            with mock.patch.object(m, "_ancestor_procs",
                                   return_value=iter(tree)):
                with mock.patch.dict(os.environ,
                                     {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": "",
                                      "CODEBUDDY_SESSION_KIND": kind}):
                    self.assertTrue(m._workbuddy_cli_host(), kind)

    def test_env_var_alone_does_not_select_cli(self):
        """CODEBUDDY_PROJECT_DIR also exists on IDE hosts (IDE Hooks
        reference), so it must not select the CLI-only replacement path."""
        m = self._hook()
        ide_tree = [["/usr/bin/codebuddy-ide", "--project", "/tmp/p"]]
        with mock.patch.object(m, "_ancestor_procs",
                               return_value=iter(ide_tree)):
            with mock.patch.dict(os.environ,
                                 {"CODEBUDDY_PROJECT_DIR": "/tmp/p",
                                  "CODEBUDDY_FORCE_HEADLESS_BUNDLE": "",
                                  "CODEBUDDY_SESSION_KIND": ""}):
                self.assertFalse(m._workbuddy_cli_host())

    def test_headless_cli_ancestor_selects_cli(self):
        """A supported headless CLI shape is standalone even without a TTY.

        The Headless Mode documents ``codebuddy -p`` / ``--print`` for
        CI/CD, automation scripts and stdin pipelines — processes that
        legitimately own no controlling terminal — and the CLI Hooks
        contract still honors ``updatedToolOutput`` there. Such an
        ancestor carries no hosted marker, no hosted session kind and no
        hosted sidecar flag, so it must be classified as a standalone
        CLI; a controlling terminal is not required.
        """
        m = self._hook()
        env = {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": "",
               "CODEBUDDY_SESSION_KIND": ""}
        for argv in (
            ["codebuddy", "-p", "fix the build"],
            ["codebuddy", "--print", "--output-format", "json"],
            ["cbc", "-p", "--resume", "abc123"],
            ["/usr/local/bin/codebuddy", "--acp"],
            ["codebuddy", "daemon", "start"],
            ["codebuddy", "--bg", "run the tests"],
        ):
            tree = [
                ["/bin/bash", "-c", "python3 compress_response_hook.py"],
                argv,
            ]
            with mock.patch.object(m, "_ancestor_procs",
                                   return_value=iter(tree)):
                with mock.patch.dict(os.environ, env):
                    self.assertTrue(m._workbuddy_cli_host(), argv)

    def test_cli_ancestor_selects_cli(self):
        m = self._hook()
        with mock.patch.object(m, "_ancestor_procs",
                               return_value=iter(self._cli_procs())):
            with mock.patch.dict(os.environ,
                                 {"CODEBUDDY_FORCE_HEADLESS_BUNDLE": "",
                                  "CODEBUDDY_SESSION_KIND": ""}):
                self.assertTrue(m._workbuddy_cli_host())

if __name__ == "__main__":
    unittest.main()
