#!/usr/bin/env python3
"""Tests for Cosh-NG compatibility of the tokenless hooks.

Covers the Cosh-NG hook contract (issue #1615 acceptance criteria) against
the unified-entry architecture (roadmap §5.4):

- Runtime detection from the ``COSH_NG_VERSION`` / ``COSH_RUNTIME``
  environment variables the host injects into hook processes, including
  unknown-version sentinels and whitespace-padded version strings.
- Stats attribution resolves to the ``cosh-ng`` agent ID and overrides the
  manifest-declared ``TOKENLESS_AGENT_ID``.
- PostToolUse: only the wrapper's ``llmContent`` (model-visible content) is
  compressed — ``returnDisplay`` never reaches the compressor nor the output
  envelope — and the replacement is delivered through
  ``hookSpecificOutput.updatedToolResponse``, the key Cosh-NG's
  ``pick_updated_tool_response`` reads.
- Unknown/unsupported Cosh-NG versions fail open (compression disabled) so
  hosts that cannot replace the response never receive a duplicate payload.
- Attribution-only paths emit exactly one JSON document.
- PreToolUse: the rewritten command is emitted both as the ``tool_input``
  partial patch Cosh-NG merges and the legacy ``updatedInput`` full
  replacement, unchanged when running under Cosh-NG env markers.
- Cross-host regression: the string-envelope error classification in
  compress_response_hook.py is deliberately not Cosh-NG gated — it restores
  the v1 hook-side classification for every host — so copilot-shell string
  envelopes keep their error attribution, and envelope-shaped output of a
  successful command is not misclassified as an error.

Uses subprocesses with mock ``tokenless`` binaries, following the pattern of
test_compress_response_hook.py and test_rewrite_hook.py.  The three
integration classes run through one shared harness
(:class:`HookIntegrationTestCase`) so env scrubbing, PATH layout, timeouts
and output parsing are defined exactly once.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

_HOOKS_DIR = (
    Path(__file__).resolve().parent.parent
    / "adapters"
    / "tokenless"
    / "common"
    / "hooks"
)

_spec = importlib.util.spec_from_file_location(
    "hook_utils", _HOOKS_DIR / "hook_utils.py"
)
assert _spec and _spec.loader
hook_utils = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(hook_utils)

COMPRESS_HOOK = _HOOKS_DIR / "compress_response_hook.py"
REWRITE_HOOK = _HOOKS_DIR / "rewrite_hook.py"
# Canonical Protocol v2 transport mock shared with the contract suite: Core
# owns the PreTool rewrite decision, so the hook is exercised against the
# same mock `tokenless` the contract goldens use.
MOCK_TOKENLESS = Path(__file__).resolve().parent / "contract" / "mock_tokenless.py"


def _write_exec(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _make_large_llm_content(char_target: int = 500) -> str:
    """Return a JSON object string above the mock's 200-char compress gate."""
    return json.dumps({"stdout": "x" * char_target, "exit_code": 0})


def _create_mock_tokenless(tmpdir: Path, behavior: str = "compress") -> Path:
    """Create a mock `tokenless` speaking the Protocol v2 PostTool operation.

    Protocol v2 lifecycle (roadmap §5.4): the hook sends one request —
    ``protocol_version`` 2, ``operation`` "post_tool", ``attribution`` and
    an ``input`` object carrying ``content`` / ``status`` /
    ``content_origin`` / ``capabilities`` — and reads the replacement from
    ``result.output`` plus optional ``result.additional_context`` in the
    response.  Core owns the policy the v1 entry point ran itself: error
    results are diagnosed (never compressed) and sub-threshold content
    passes through unchanged.

    Behaviors:
      - "compress": truncate strings longer than 20 chars to their first 20
        (JSON object content field-by-field, plain text as a whole) and
        respond "applied".
      - "no-savings": return the content unchanged with disposition
        "no_savings".
      - "passthrough": echo stdin for any subcommand (existence probe for
        hooks that only check the binary is installed).
    """
    mock_script = tmpdir / "tokenless"

    prologue = textwrap.dedent("""\
        #!/usr/bin/env python3
        import json, sys
        if len(sys.argv) < 2 or sys.argv[1] != "compress":
            sys.exit(2)
        request = json.loads(sys.stdin.read())
        operation_input = request.get("input", {})
        if (request.get("protocol_version") != 2
                or request.get("operation") != "post_tool"
                or "capabilities" not in operation_input):
            sys.exit(2)
        content = operation_input["content"]

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

        if operation_input.get("status") == "error":
            context = None
            if "command not found" in content.lower():
                context = (
                    "[tokenless:env] %s failed: ENV_DEPENDENCY_MISSING "
                    "(Install the missing dependency or ask the user for "
                    "guidance.)." % operation_input.get("tool_name", "tool")
                )
            respond(content, "tool_error", context)
            sys.exit(0)
        if (not operation_input["capabilities"]["replace_output"]
                or operation_input.get("content_origin") == "file_content"
                or len(content) < 200):
            respond(content, "passthrough")
            sys.exit(0)
    """)

    if behavior == "passthrough":
        script = textwrap.dedent("""\
            #!/usr/bin/env python3
            import sys
            print(sys.stdin.read())
        """)
    elif behavior == "no-savings":
        script = prologue + 'respond(content, "no_savings")\n'
    elif behavior == "compress":
        script = prologue + textwrap.dedent("""\
            try:
                data = json.loads(content)
                if isinstance(data, str):
                    data = json.loads(data)
            except (TypeError, ValueError):
                data = None
            if isinstance(data, dict):
                compressed = {
                    k: (v[:20] if isinstance(v, str) and len(v) > 20 else v)
                    for k, v in data.items()
                }
                respond(json.dumps(compressed, separators=(",", ":")), "applied")
            else:
                # Plain-text content: truncate as a whole string.
                truncated = content[:20] if len(content) > 20 else content
                respond(truncated, "applied")
        """)
    else:
        raise ValueError(f"Unknown behavior: {behavior}")

    _write_exec(mock_script, script)
    return mock_script


class CoshNGEnvTestCase(unittest.TestCase):
    """Base class that scrubs Cosh-NG markers from the process environment."""

    def setUp(self) -> None:
        self._saved_env = os.environ.copy()
        os.environ.pop("COSH_NG_VERSION", None)
        os.environ.pop("COSH_RUNTIME", None)
        os.environ.pop("TOKENLESS_AGENT_ID", None)

    def tearDown(self) -> None:
        os.environ.clear()
        os.environ.update(self._saved_env)


class HookIntegrationTestCase(CoshNGEnvTestCase):
    """Shared harness for the hook subprocess integration tests.

    Owns the temp sandbox, the scrubbed Cosh-NG environment (inherited from
    :class:`CoshNGEnvTestCase`) and the one ``_run_hook`` implementation, so
    env handling, timeouts and output parsing cannot drift between the
    integration classes — the rewrite-marker leak caught in the first review
    round was exactly such a drift.  Subclasses describe their sandbox with
    ``_prepare_sandbox`` / ``_path_entries``, pick the hook under test via
    ``hook_script`` and add class-wide env vars through ``extra_env``.
    """

    hook_script: Path = COMPRESS_HOOK

    def setUp(self) -> None:
        super().setUp()
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.extra_env: dict[str, str] = {}
        self._prepare_sandbox()

    def tearDown(self) -> None:
        self.tmp.cleanup()
        super().tearDown()

    def _prepare_sandbox(self) -> None:
        """Lay out the sandbox and set ``self.home`` (subclass hook)."""
        raise NotImplementedError

    def _path_entries(self, env: dict[str, str]) -> str:
        """Return the ``PATH`` the hook subprocess runs with (subclass hook)."""
        raise NotImplementedError

    def _run_hook(self, stdin_data: dict, env_overrides: dict | None = None) -> dict:
        env = os.environ.copy()
        env.pop("COSH_NG_VERSION", None)
        env.pop("COSH_RUNTIME", None)
        env["HOME"] = str(self.home)
        env["PATH"] = self._path_entries(env)
        env["TOKENLESS_AGENT_ID"] = "copilot-shell"
        env.update(self.extra_env)
        if env_overrides:
            env.update(env_overrides)
        proc = subprocess.run(
            [sys.executable, str(self.hook_script)],
            input=json.dumps(stdin_data),
            capture_output=True,
            text=True,
            timeout=15,
            env=env,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        stdout = proc.stdout.strip()
        if not stdout or stdout == "{}":
            return {}
        return json.loads(stdout)


class CompressHookTestCase(HookIntegrationTestCase):
    """Sandbox for compress_response_hook.py: mock ``tokenless`` inside HOME."""

    def _prepare_sandbox(self) -> None:
        self.home = self.root
        self.mock_tokenless = _create_mock_tokenless(self.home)

    def _path_entries(self, env: dict[str, str]) -> str:
        return str(self.mock_tokenless.parent) + ":" + env.get("PATH", "")


class TestCoshNGRuntimeDetection(CoshNGEnvTestCase):
    """Cosh-NG detection from the host-injected environment variables."""

    def test_detect_version_from_cosh_ng_version(self):
        """COSH_NG_VERSION yields the parsed version tuple."""
        os.environ["COSH_NG_VERSION"] = "0.21.0"
        self.assertEqual(hook_utils.detect_cosh_ng_runtime(), (0, 21, 0))

    def test_detect_version_tolerates_surrounding_whitespace(self):
        """A padded version string still parses (regex search, not match)."""
        os.environ["COSH_NG_VERSION"] = "  0.6.0 "
        self.assertEqual(hook_utils.detect_cosh_ng_runtime(), (0, 6, 0))

    def test_unparseable_version_is_unknown_sentinel(self):
        """Detected but unparseable version maps to the (0, 0, 0) sentinel."""
        os.environ["COSH_NG_VERSION"] = "not-a-version"
        self.assertEqual(hook_utils.detect_cosh_ng_runtime(), (0, 0, 0))

    def test_whitespace_only_version_is_unknown_sentinel(self):
        """A whitespace-only version is detected but unsupported, not a crash."""
        os.environ["COSH_NG_VERSION"] = "   "
        self.assertEqual(hook_utils.detect_cosh_ng_runtime(), (0, 0, 0))

    def test_runtime_marker_without_version_is_unknown_sentinel(self):
        """COSH_RUNTIME=cosh-ng without a version maps to (0, 0, 0)."""
        os.environ["COSH_RUNTIME"] = "cosh-ng"
        self.assertEqual(hook_utils.detect_cosh_ng_runtime(), (0, 0, 0))

    def test_version_wins_over_runtime_marker(self):
        """When both markers are present the version string is authoritative."""
        os.environ["COSH_NG_VERSION"] = "0.7.1"
        os.environ["COSH_RUNTIME"] = "cosh-ng"
        self.assertEqual(hook_utils.detect_cosh_ng_runtime(), (0, 7, 1))

    def test_not_cosh_ng_when_markers_absent(self):
        """No markers means the hook is not running under Cosh-NG."""
        self.assertIsNone(hook_utils.detect_cosh_ng_runtime())

    def test_other_runtime_marker_is_not_cosh_ng(self):
        """COSH_RUNTIME values other than cosh-ng are ignored."""
        os.environ["COSH_RUNTIME"] = "copilot-shell"
        self.assertIsNone(hook_utils.detect_cosh_ng_runtime())


class TestCoshNGAgentAttribution(CoshNGEnvTestCase):
    """Stats attribution resolves to the cosh-ng agent ID."""

    def test_cosh_ng_version_overrides_declared_agent_id(self):
        """The host env beats the manifest-declared TOKENLESS_AGENT_ID."""
        os.environ["TOKENLESS_AGENT_ID"] = "copilot-shell"
        os.environ["COSH_NG_VERSION"] = "0.21.0"
        self.assertEqual(hook_utils.resolve_agent_id(), "cosh-ng")

    def test_runtime_marker_overrides_declared_agent_id(self):
        """COSH_RUNTIME alone is enough to re-attribute to cosh-ng."""
        os.environ["TOKENLESS_AGENT_ID"] = "copilot-shell"
        os.environ["COSH_RUNTIME"] = "cosh-ng"
        self.assertEqual(hook_utils.resolve_agent_id(), "cosh-ng")

    def test_unknown_version_still_attributes_to_cosh_ng(self):
        """Even the fail-open sentinel keeps stats attribution correct."""
        os.environ["COSH_RUNTIME"] = "cosh-ng"
        self.assertEqual(hook_utils.resolve_agent_id(), "cosh-ng")

    def test_declared_agent_id_used_outside_cosh_ng(self):
        """Without Cosh-NG markers the declared agent ID is kept."""
        os.environ["TOKENLESS_AGENT_ID"] = "copilot-shell"
        self.assertEqual(hook_utils.resolve_agent_id(), "copilot-shell")

    def test_default_used_when_nothing_declared(self):
        """No markers and no declaration fall back to the visible default."""
        self.assertEqual(hook_utils.resolve_agent_id(), "unknown")


class TestCoshNGCompressResponseIntegration(CompressHookTestCase):
    """Integration tests for compress_response_hook.py under Cosh-NG.

    The unified-entry hook (roadmap §5.4) detects Cosh-NG from the
    ``COSH_NG_VERSION`` / ``COSH_RUNTIME`` environment variables injected
    by the host, extracts only ``llmContent`` from the wrapped response,
    and emits the replacement through ``updatedToolResponse``.
    """

    def test_cosh_ng_replacement_field_emitted(self):
        """Cosh-NG path emits updatedToolResponse with compressed llmContent."""
        llm_content = _make_large_llm_content(500)
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": {
                "llmContent": llm_content,
                "returnDisplay": "Bash completed",
            },
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_NG_VERSION": "0.6.0"},
        )
        self.assertTrue(out.get("suppressOutput"))
        specific = out.get("hookSpecificOutput", {})
        self.assertEqual(specific.get("hookEventName"), "PostToolUse")
        self.assertIn("updatedToolResponse", specific)
        # returnDisplay must never leak into the replacement output.
        self.assertNotIn("returnDisplay", json.dumps(specific))
        self.assertNotIn("Bash completed", specific["updatedToolResponse"])

    def test_cosh_ng_pre_060_version_still_replaces(self):
        """Pre-0.6.0 versions still receive the replacement field.

        The unified-entry envelope carries the replacement exclusively in
        ``updatedToolResponse``; hosts that do not read it keep the
        original output unchanged (cosh-core falls back to the original
        when the field is absent), so no duplicate injection is possible
        and no version gate is needed beyond the unknown-version sentinel.
        """
        llm_content = _make_large_llm_content(500)
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": {
                "llmContent": llm_content,
                "returnDisplay": "Bash completed",
            },
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_NG_VERSION": "0.5.0"},
        )
        specific = out.get("hookSpecificOutput", {})
        self.assertIn("updatedToolResponse", specific)

    def test_cosh_ng_fail_open_unknown_runtime_version(self):
        """Cosh-NG detected without a parseable version disables compression."""
        llm_content = _make_large_llm_content(500)
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": {
                "llmContent": llm_content,
                "returnDisplay": "Bash completed",
            },
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_RUNTIME": "cosh-ng"},
        )
        self.assertEqual(out, {})

    def test_cosh_ng_fail_open_unparseable_version(self):
        """Unparseable COSH_NG_VERSION disables compression (fail open)."""
        llm_content = _make_large_llm_content(500)
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": {
                "llmContent": llm_content,
                "returnDisplay": "Bash completed",
            },
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_NG_VERSION": "not-a-version"},
        )
        self.assertEqual(out, {})

    def test_cosh_ng_small_response_skipped(self):
        """Small Cosh-NG responses are skipped."""
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": {
                "llmContent": '{"stdout":"hi"}',
                "returnDisplay": "Bash completed",
            },
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_NG_VERSION": "0.6.0"},
        )
        self.assertEqual(out, {})

    def test_cosh_ng_env_attribution_only(self):
        """Cosh-NG env attribution is emitted even when response is skipped."""
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": {
                "llmContent": '{"stderr":"command not found: foobar","exit_code":127}',
                "returnDisplay": "Bash failed",
            },
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_NG_VERSION": "0.6.0"},
        )
        specific = out.get("hookSpecificOutput", {})
        self.assertNotIn("updatedToolResponse", specific)
        self.assertIn("additionalContext", specific)
        self.assertIn("ENV_DEPENDENCY_MISSING", specific["additionalContext"])

    def test_cosh_ng_plain_text_llm_content(self):
        """Plain text llmContent is compressed as text."""
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": {
                "llmContent": "x" * 500,
                "returnDisplay": "Bash completed",
            },
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_NG_VERSION": "0.6.0"},
        )
        specific = out.get("hookSpecificOutput", {})
        self.assertIn("updatedToolResponse", specific)
        self.assertEqual(specific["updatedToolResponse"], "x" * 20)

    def test_cosh_ng_wrapped_string_response(self):
        """A JSON-string wrapper is unwrapped; only llmContent is compressed."""
        llm_content = _make_large_llm_content(500)
        wrapper = json.dumps(
            {"llmContent": llm_content, "returnDisplay": "Bash completed"}
        )
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": wrapper,
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_NG_VERSION": "0.6.0"},
        )
        specific = out.get("hookSpecificOutput", {})
        self.assertIn("updatedToolResponse", specific)
        self.assertNotIn("Bash completed", specific["updatedToolResponse"])

    def test_cosh_ng_no_savings_passes_through(self):
        """When compression yields no savings the original passes through."""
        self.mock_tokenless = _create_mock_tokenless(self.home, behavior="no-savings")
        llm_content = _make_large_llm_content(500)
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": {
                "llmContent": llm_content,
                "returnDisplay": "Bash completed",
            },
        }
        out = self._run_hook(
            stdin_data,
            env_overrides={"COSH_NG_VERSION": "0.6.0"},
        )
        self.assertEqual(out, {})


class TestCopilotShellEnvelopeClassification(CompressHookTestCase):
    """Cross-host regression for string-envelope error classification.

    Protocol v2 moved environment diagnosis into Core, gated on the
    hook-supplied status, so compress_response_hook.py parses string shell
    envelopes (``try_parse_json``, double-unwrap) without a Cosh-NG gate —
    restoring the host-agnostic classification the v1 hook ran hook-side.
    copilot-shell delivers shell output as a plain string envelope, so it
    must keep working through the same parse, and envelope-shaped text
    without error markers must not be misclassified.
    """

    def test_copilot_shell_string_envelope_error_keeps_attribution(self):
        """A failing copilot-shell string envelope is classified as error."""
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": json.dumps(
                {"stderr": "command not found: foobar", "exit_code": 127}
            ),
        }
        out = self._run_hook(stdin_data)
        specific = out.get("hookSpecificOutput", {})
        self.assertNotIn("updatedToolOutput", specific)
        self.assertIn("additionalContext", specific)
        self.assertIn("ENV_DEPENDENCY_MISSING", specific["additionalContext"])

    def test_copilot_shell_envelope_shaped_success_not_misclassified(self):
        """Envelope-shaped output of a successful command stays success."""
        stdin_data = {
            "tool_name": "Bash",
            "tool_response": json.dumps(
                {
                    "exit_code": 0,
                    "stdout": "s" * 300,
                    "stderr": "warning: skipped 2 entries",
                }
            ),
        }
        self.assertEqual(self._run_hook(stdin_data), {})


class TestCoshNGRewriteIntegration(HookIntegrationTestCase):
    """Integration tests for rewrite_hook.py under Cosh-NG."""

    hook_script = REWRITE_HOOK

    def _prepare_sandbox(self) -> None:
        self.home = self.root / "home"
        self.bin_dir = self.root / "bin"
        self.home.mkdir()
        self.bin_dir.mkdir()
        tokenless = self.bin_dir / "tokenless"
        shutil.copy(MOCK_TOKENLESS, tokenless)
        tokenless.chmod(
            tokenless.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH
        )
        # The mock's shebang resolves through PATH: pin it to this interpreter.
        (self.bin_dir / "python3").symlink_to(sys.executable)
        self.request_log = self.root / "requests.jsonl"
        # Keep the run hermetic: no stats/SLS side channels.
        self.extra_env.update(
            {
                "TOKENLESS_STATS_ENABLED": "0",
                "TOKENLESS_SLS_ENABLED": "0",
                "TOKENLESS_MOCK_BEHAVIOR": "applied",
                "TOKENLESS_MOCK_REQUEST_LOG": str(self.request_log),
            }
        )

    def _path_entries(self, env: dict[str, str]) -> str:
        return f"{self.bin_dir}:/usr/bin:/bin"

    def _requests(self) -> list[dict]:
        with self.request_log.open(encoding="utf-8") as handle:
            return [json.loads(line) for line in handle if line.strip()]

    def test_cosh_ng_pre_tool_emits_tool_input(self):
        """Cosh-NG PreToolUse output uses tool_input patch field."""
        stdin_data = {
            "session_id": "session-1",
            "tool_use_id": "call-1",
            "tool_name": "Bash",
            "hook_event_name": "PreToolUse",
            "tool_input": {"command": "git status"},
        }
        out = self._run_hook(stdin_data, env_overrides={"COSH_NG_VERSION": "0.6.0"})
        specific = out.get("hookSpecificOutput", {})
        self.assertEqual(specific.get("hookEventName"), "PreToolUse")
        self.assertIn("tool_input", specific)
        self.assertIn("updatedInput", specific)
        self.assertEqual(
            specific["tool_input"]["command"],
            specific["updatedInput"]["command"],
        )
        self.assertTrue(
            specific["tool_input"]["command"].endswith("git status")
        )
        # The Cosh-NG runtime markers still win over the manifest-declared
        # agent ID on the PreTool request Core receives.
        requests = self._requests()
        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0]["operation"], "pre_tool")
        self.assertEqual(requests[0]["attribution"]["agent_id"], "cosh-ng")

if __name__ == "__main__":
    unittest.main(verbosity=2)
