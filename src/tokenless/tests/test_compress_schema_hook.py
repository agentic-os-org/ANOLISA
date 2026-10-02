#!/usr/bin/env python3
"""Integration tests for compress_schema_hook.py.

Validates the BeforeModel hook contract:
- Tool declarations are read from the canonical ``llm_request.config.tools``
  position, with the older top-level ``llm_request.tools`` still accepted.
- Compressed declarations are written back to the canonical position.
- Stats attribution: when the host announces itself via ``COSH_RUNTIME`` /
  ``COSH_NG_VERSION``, the agent ID is ``cosh-ng`` even though the shared
  extension manifest sets ``TOKENLESS_AGENT_ID=copilot-shell``.

Uses subprocess to invoke the hook with a mock tokenless binary,
avoiding Python version issues with the hook_utils module.
"""

import importlib.util
import json
import os
import stat
import subprocess
import sys
import shutil
import tempfile
import textwrap
import threading
import unittest

_TOOLS = [
    {
        "name": "shell",
        "description": "Run a shell command. " * 20,
        "parameters": {
            "type": "object",
            "properties": {"command": {"type": "string"}},
        },
    }
]


def _hook_path() -> str:
    """Absolute path of compress_schema_hook.py in the shared adapter tree."""
    hooks_dir = os.path.normpath(os.path.join(
        os.path.dirname(__file__),
        os.pardir, "adapters", "tokenless", "common", "hooks",
    ))
    return os.path.join(hooks_dir, "compress_schema_hook.py")


def _create_mock_tokenless(
    tmpdir: str, argv_log: str, disposition: str = "applied"
) -> str:
    """Mock `tokenless` speaking the Protocol v2 BeforeModel operation.

    Truncates every tool description in the request content and records the
    argv plus the decoded request to ``argv_log`` so tests can assert the
    agent ID the hook attributed the invocation to. A non-"applied"
    ``disposition`` responds with the original content untouched, matching
    the entry point's fail-open contract.
    """
    mock_script = os.path.join(tmpdir, "tokenless")
    script = textwrap.dedent(f"""\
        #!/usr/bin/env python3
        import json, sys
        request = json.loads(sys.stdin.read())
        with open({argv_log!r}, "w") as log:
            log.write(json.dumps({{"argv": sys.argv[1:], "request": request}}))
        if (sys.argv[1:] != ["compress"]
                or request.get("protocol_version") != 2
                or request.get("operation") != "before_model"):
            sys.exit(2)
        disposition = {disposition!r}
        tools = request["input"]["tools"]
        if disposition == "applied":
            for tool in tools:
                tool["description"] = "compressed"
        print(json.dumps({{
            "protocol_version": 2,
            "operation": "before_model",
            "attribution": request["attribution"],
            "result": {{
                "tools": tools,
                "visible_markers": [],
            }},
        }}))
    """)
    with open(mock_script, "w") as handle:
        handle.write(script)
    os.chmod(
        mock_script,
        os.stat(mock_script).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH,
    )
    return mock_script


def _run_hook(stdin_data: dict, mock_tokenless_path: str, extra_env: dict) -> dict:
    """Run the hook as a subprocess with a mocked tokenless binary."""
    hooks_dir = os.path.normpath(os.path.join(
        os.path.dirname(__file__),
        os.pardir, "adapters", "tokenless", "common", "hooks",
    ))
    hook_path = os.path.join(hooks_dir, "compress_schema_hook.py")

    env = os.environ.copy()
    # The host-owned variables must not leak in from the caller's environment.
    env.pop("COSH_RUNTIME", None)
    env.pop("COSH_NG_VERSION", None)
    env["PATH"] = os.path.dirname(mock_tokenless_path) + ":" + env.get("PATH", "")
    env.update(extra_env)

    proc = subprocess.run(
        [sys.executable, hook_path],
        input=json.dumps(stdin_data),
        capture_output=True,
        text=True,
        timeout=10,
        env=env,
    )
    if proc.returncode != 0:
        return {
            "_subprocess_error": True,
            "_returncode": proc.returncode,
            "_stderr": proc.stderr,
        }

    stdout = proc.stdout.strip()
    if not stdout or stdout == "{}":
        return {}
    try:
        return json.loads(stdout)
    except json.JSONDecodeError:
        return {"_raw_stdout": stdout, "_stderr": proc.stderr}


def _run_hook_raw(stdin_text: str, mock_tokenless_path: str, extra_env: dict):
    """Run the hook as a subprocess and return the CompletedProcess.

    Like ``_run_hook`` but hands back stdout and stderr untouched, for tests
    that assert on diagnostics rather than only on the hook output JSON.
    """
    hooks_dir = os.path.normpath(os.path.join(
        os.path.dirname(__file__),
        os.pardir, "adapters", "tokenless", "common", "hooks",
    ))
    hook_path = os.path.join(hooks_dir, "compress_schema_hook.py")

    env = os.environ.copy()
    env.pop("COSH_RUNTIME", None)
    env.pop("COSH_NG_VERSION", None)
    env["PATH"] = os.path.dirname(mock_tokenless_path) + ":" + env.get("PATH", "")
    env.update(extra_env)

    return subprocess.run(
        [sys.executable, hook_path],
        input=stdin_text,
        capture_output=True,
        text=True,
        timeout=10,
        env=env,
    )



_needs_py39 = sys.version_info < (3, 9)


@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestSchemaCompressionProtocol(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.argv_log = os.path.join(self.tmpdir, "argv.json")
        self.mock_bin = _create_mock_tokenless(self.tmpdir, self.argv_log)

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def _recorded_agent_id(self) -> str:
        with open(self.argv_log) as handle:
            logged = json.load(handle)
        return logged["request"]["attribution"]["agent_id"]

    def test_reads_and_writes_canonical_config_tools(self):
        result = _run_hook(
            {"session_id": "s1", "llm_request": {"config": {"tools": _TOOLS}}},
            self.mock_bin,
            {"TOKENLESS_AGENT_ID": "copilot-shell"},
        )

        tools = result["hookSpecificOutput"]["llm_request"]["config"]["tools"]
        self.assertEqual(tools[0]["name"], "shell")
        self.assertEqual(tools[0]["description"], "compressed")

    def test_lossless_only_result_wraps_original(self):
        """Common Hooks accept an unchanged lossless-only result."""
        mock_dir = tempfile.mkdtemp(dir=self.tmpdir)
        mock_bin = _create_mock_tokenless(
            mock_dir, os.path.join(mock_dir, "argv.json"),
            disposition="passthrough",
        )

        proc = _run_hook_raw(
            json.dumps(
                {"session_id": "s1", "llm_request": {"config": {"tools": _TOOLS}}}
            ),
            mock_bin,
            {"TOKENLESS_AGENT_ID": "copilot-shell"},
        )

        self.assertEqual(proc.returncode, 0, proc.stderr)
        tools = json.loads(proc.stdout)["hookSpecificOutput"]["llm_request"][
            "config"]["tools"]
        self.assertEqual(tools[0]["description"], _TOOLS[0]["description"],
                         "the original schemas must pass through untouched")
        self.assertNotIn("stash unavailable", proc.stderr)

    def test_accepts_legacy_top_level_tools(self):
        result = _run_hook(
            {"session_id": "s1", "llm_request": {"tools": _TOOLS}},
            self.mock_bin,
            {"TOKENLESS_AGENT_ID": "copilot-shell"},
        )

        # Read from the legacy position, but always written back to the
        # canonical one.
        tools = result["hookSpecificOutput"]["llm_request"]["config"]["tools"]
        self.assertEqual(tools[0]["description"], "compressed")

    def test_canonical_position_wins_over_legacy_when_both_present(self):
        result = _run_hook(
            {
                "session_id": "s1",
                "llm_request": {
                    "config": {"tools": _TOOLS},
                    "tools": [{
                        "name": "legacy-only",
                        "description": "stale",
                        "parameters": {"type": "object"},
                    }],
                },
            },
            self.mock_bin,
            {"TOKENLESS_AGENT_ID": "copilot-shell"},
        )

        tools = result["hookSpecificOutput"]["llm_request"]["config"]["tools"]
        self.assertEqual([tool["name"] for tool in tools], ["shell"])

    def test_empty_canonical_tools_does_not_fall_back_to_legacy(self):
        """An explicitly empty canonical array means "no tools this request"."""
        result = _run_hook(
            {
                "session_id": "s1",
                "llm_request": {
                    "config": {"tools": []},
                    "tools": _TOOLS,
                },
            },
            self.mock_bin,
            {"TOKENLESS_AGENT_ID": "copilot-shell"},
        )

        self.assertEqual(result, {})

    def test_skips_when_no_tools_present(self):
        # No tool declarations anywhere: the hook passes through unchanged
        # but surfaces a once-per-session systemMessage so the silent-skip
        # era ("0 records, no idea why") is diagnosable.
        home = os.path.join(self.tmpdir, "home")
        os.makedirs(home, exist_ok=True)
        result = _run_hook(
            {"session_id": "s1", "llm_request": {"model": "m", "messages": []}},
            self.mock_bin,
            {"TOKENLESS_AGENT_ID": "copilot-shell", "HOME": home},
        )

        self.assertNotIn("hookSpecificOutput", result)
        self.assertIn("no tool declarations", result.get("systemMessage", ""))

    def test_cosh_ng_runtime_wins_over_manifest_agent_id(self):
        _run_hook(
            {"session_id": "s1", "llm_request": {"config": {"tools": _TOOLS}}},
            self.mock_bin,
            {
                "TOKENLESS_AGENT_ID": "copilot-shell",
                "COSH_RUNTIME": "cosh-ng",
                "COSH_NG_VERSION": "0.13.0",
            },
        )

        self.assertEqual(self._recorded_agent_id(), "cosh-ng")

    def test_manifest_agent_id_still_serves_copilot_shell(self):
        _run_hook(
            {"session_id": "s1", "llm_request": {"config": {"tools": _TOOLS}}},
            self.mock_bin,
            {"TOKENLESS_AGENT_ID": "copilot-shell"},
        )

        self.assertEqual(self._recorded_agent_id(), "copilot-shell")



@unittest.skipIf(_needs_py39, "hook_utils requires Python 3.9+")
class TestSchemaHookNoToolsWarning(unittest.TestCase):
    """The hook must surface — once per session — when a BeforeModel event
    carries nothing schema compression can work on. The historical silent
    skip made "0 schema records" indistinguishable from "hook never ran"."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.argv_log = os.path.join(self.tmpdir, "argv.json")
        self.mock_bin = _create_mock_tokenless(self.tmpdir, self.argv_log)
        # Isolate ~/.tokenless so the dedup marker never touches real state.
        self.home = os.path.join(self.tmpdir, "home")
        os.makedirs(self.home, exist_ok=True)
        self.env = {"TOKENLESS_AGENT_ID": "copilot-shell", "HOME": self.home}

    def tearDown(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def _marker_path(self) -> str:
        return os.path.join(self.home, ".tokenless", ".schema-hook-nowarn-session")

    def test_warns_when_event_carries_no_tools(self):
        proc = _run_hook_raw(
            json.dumps({"session_id": "s1", "llm_request": {"model": "m", "messages": []}}),
            self.mock_bin,
            self.env,
        )

        self.assertEqual(proc.returncode, 0)
        self.assertIn("no tool declarations", proc.stdout)
        self.assertIn("no tool declarations", proc.stderr)
        self.assertNotIn("hookSpecificOutput", proc.stdout)
        self.assertTrue(os.path.isfile(self._marker_path()))

    def test_warns_when_payload_lacks_llm_request(self):
        proc = _run_hook_raw(
            json.dumps({"session_id": "s1"}),
            self.mock_bin,
            self.env,
        )

        self.assertEqual(proc.returncode, 0)
        self.assertIn("no llm_request", proc.stdout)
        self.assertIn("no llm_request", proc.stderr)
        self.assertNotIn("hookSpecificOutput", proc.stdout)

    def test_no_tools_warning_deduped_per_session(self):
        payload = json.dumps(
            {"session_id": "s1", "llm_request": {"model": "m", "messages": []}}
        )
        first = _run_hook_raw(payload, self.mock_bin, self.env)
        second = _run_hook_raw(payload, self.mock_bin, self.env)

        self.assertIn("no tool declarations", first.stderr)
        self.assertIn("no tool declarations", first.stdout)
        self.assertEqual(second.stderr, "")
        self.assertEqual(second.stdout.strip(), "{}")

        # A different session warns again.
        other = _run_hook_raw(
            json.dumps(
                {"session_id": "s2", "llm_request": {"model": "m", "messages": []}}
            ),
            self.mock_bin,
            self.env,
        )
        self.assertIn("no tool declarations", other.stderr)

    def test_empty_canonical_tools_skips_silently(self):
        proc = _run_hook_raw(
            json.dumps(
                {
                    "session_id": "s1",
                    "llm_request": {"model": "m", "config": {"tools": []}},
                }
            ),
            self.mock_bin,
            self.env,
        )

        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout.strip(), "{}")
        self.assertEqual(proc.stderr, "")
        self.assertFalse(os.path.exists(self._marker_path()))

    def test_no_warning_when_tools_present(self):
        proc = _run_hook_raw(
            json.dumps(
                {
                    "session_id": "s1",
                    "llm_request": {"model": "m", "config": {"tools": _TOOLS}},
                }
            ),
            self.mock_bin,
            self.env,
        )

        self.assertEqual(proc.returncode, 0)
        self.assertNotIn("WARNING", proc.stderr)
        self.assertFalse(os.path.exists(self._marker_path()))

    def test_non_object_payload_warns_and_passes_through(self):
        proc = _run_hook_raw("[1, 2, 3]", self.mock_bin, self.env)

        self.assertEqual(proc.returncode, 0)
        self.assertIn("not a JSON object", proc.stdout)
        self.assertIn("not a JSON object", proc.stderr)

    def test_concurrent_sessions_do_not_invalidate_each_other(self):
        # Two sessions under the same HOME warn in turn; afterwards both must
        # stay silent. With a single-value marker, session B's warning would
        # overwrite session A's key and make A warn again on its next turn.
        payload_a = json.dumps(
            {"session_id": "s-a", "llm_request": {"model": "m", "messages": []}}
        )
        payload_b = json.dumps(
            {"session_id": "s-b", "llm_request": {"model": "m", "messages": []}}
        )

        first_a = _run_hook_raw(payload_a, self.mock_bin, self.env)
        first_b = _run_hook_raw(payload_b, self.mock_bin, self.env)
        second_a = _run_hook_raw(payload_a, self.mock_bin, self.env)
        second_b = _run_hook_raw(payload_b, self.mock_bin, self.env)

        self.assertIn("no tool declarations", first_a.stderr)
        self.assertIn("no tool declarations", first_b.stderr)
        self.assertEqual(second_a.stderr, "")
        self.assertEqual(second_a.stdout.strip(), "{}")
        self.assertEqual(second_b.stderr, "")
        self.assertEqual(second_b.stdout.strip(), "{}")

    def test_marker_bounded_to_recent_sessions(self):
        # The marker keeps one key per line, bounded to the newest entries:
        # eviction trims the oldest keys, so active sessions keep their state
        # and the file cannot grow without limit.
        marker = self._marker_path()
        os.makedirs(os.path.dirname(marker), exist_ok=True)
        with open(marker, "w", encoding="utf-8") as handle:
            for index in range(100):
                handle.write("old-%d\n" % index)

        proc = _run_hook_raw(
            json.dumps(
                {"session_id": "s-new", "llm_request": {"model": "m", "messages": []}}
            ),
            self.mock_bin,
            self.env,
        )

        self.assertIn("no tool declarations", proc.stderr)
        with open(marker, encoding="utf-8") as handle:
            keys = [line.strip() for line in handle if line.strip()]
        self.assertLessEqual(len(keys), 64)
        self.assertEqual(keys[-1], "s-new")
        self.assertIn("old-99", keys)  # newest seeded entries survive
        self.assertNotIn("old-0", keys)  # oldest entries are trimmed first

    def test_racing_first_warnings_are_serialized_by_marker_lock(self):
        # Forced read-read-write-write interleave: a barrier inside the read
        # makes both workers finish reading before either may write. With an
        # unlocked read-modify-write both then write back over each other and
        # one session key is lost. The marker lock must serialize the pair:
        # the blocked worker cannot reach its read until the first write has
        # landed, the barrier breaks, and both keys survive.
        spec = importlib.util.spec_from_file_location(
            "compress_schema_hook_under_test", _hook_path()
        )
        hook = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(hook)
        hook._NO_TOOLS_WARN_MARKER = self._marker_path()

        barrier = threading.Barrier(2, timeout=2)
        real_read = hook._read_warned_sessions

        def synchronized_read():
            keys = real_read()
            try:
                barrier.wait()
            except threading.BrokenBarrierError:
                pass  # the lock kept the second worker out — expected
            return keys

        hook._read_warned_sessions = synchronized_read
        results = {}

        def worker(session_id):
            results[session_id] = hook._should_warn_no_tools(session_id)

        threads = [
            threading.Thread(target=worker, args=(session_id,))
            for session_id in ("race-a", "race-b")
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=15)
        hook._read_warned_sessions = real_read

        for thread in threads:
            self.assertFalse(thread.is_alive(), "worker wedged on marker lock")
        self.assertEqual(results, {"race-a": True, "race-b": True})
        with open(self._marker_path(), encoding="utf-8") as handle:
            keys = sorted(line.strip() for line in handle if line.strip())
        self.assertEqual(keys, ["race-a", "race-b"])
        # Both recorded sessions stay silent on their next turn.
        self.assertFalse(hook._should_warn_no_tools("race-a"))
        self.assertFalse(hook._should_warn_no_tools("race-b"))

    def test_parallel_hook_processes_keep_every_session_key(self):
        # True concurrency across processes: several hook invocations warn
        # for distinct sessions at the same time. Every session key must
        # survive the parallel first warnings and stay silent afterwards,
        # which the historical sequential A/B/A/B coverage never exercised.
        session_ids = ["par-a", "par-b", "par-c", "par-d", "par-e", "par-f"]
        payloads = {
            session_id: json.dumps(
                {
                    "session_id": session_id,
                    "llm_request": {"model": "m", "messages": []},
                }
            )
            for session_id in session_ids
        }

        env = os.environ.copy()
        env.pop("COSH_RUNTIME", None)
        env.pop("COSH_NG_VERSION", None)
        env["PATH"] = os.path.dirname(self.mock_bin) + ":" + env.get("PATH", "")
        env.update(self.env)

        procs = [
            subprocess.Popen(
                [sys.executable, _hook_path()],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=env,
            )
            for _ in session_ids
        ]
        outputs = {
            session_id: proc.communicate(input=payloads[session_id], timeout=30)
            for session_id, proc in zip(session_ids, procs)
        }
        for session_id, proc in zip(session_ids, procs):
            self.assertEqual(proc.returncode, 0, outputs[session_id][1])
            self.assertIn(
                "no tool declarations",
                outputs[session_id][1],
                "session %s should warn exactly once" % session_id,
            )

        with open(self._marker_path(), encoding="utf-8") as handle:
            keys = {line.strip() for line in handle if line.strip()}
        self.assertEqual(keys, set(session_ids))

        for session_id in session_ids:
            repeat = _run_hook_raw(payloads[session_id], self.mock_bin, self.env)
            self.assertEqual(repeat.stderr, "", "session %s warned twice" % session_id)
            self.assertEqual(repeat.stdout.strip(), "{}")



if __name__ == "__main__":
    unittest.main()
