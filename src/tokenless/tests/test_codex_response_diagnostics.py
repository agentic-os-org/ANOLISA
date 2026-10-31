#!/usr/bin/env python3
"""Regression tests for the Codex PostToolUse diagnostics contract."""

from __future__ import annotations

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
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parent.parent
    / "adapters"
    / "tokenless"
    / "codex"
    / "scripts"
    / "response-diagnostics"
)


def _create_marker_tokenless(tmpdir: str) -> tuple[str, str]:
    """Create a tokenless stub that records any unexpected invocation."""
    marker = os.path.join(tmpdir, "tokenless-called")
    mock_script = os.path.join(tmpdir, "tokenless")
    script = textwrap.dedent(
        f"""\
        #!/usr/bin/env python3
        from pathlib import Path
        Path({marker!r}).touch()
        raise SystemExit(0)
        """
    )
    Path(mock_script).write_text(script, encoding="utf-8")
    os.chmod(
        mock_script,
        os.stat(mock_script).st_mode | stat.S_IXUSR | stat.S_IXGRP,
    )
    return mock_script, marker


def _run_hook(
    tool_response: object, tool_name: str = "Bash", **payload_fields: object
) -> subprocess.CompletedProcess[str]:
    input_data = {
        "tool_name": tool_name,
        "tool_response": tool_response,
        "session_id": "test-session",
        "tool_use_id": "toolu_test",
        **payload_fields,
    }
    return subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=json.dumps(input_data),
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )


def _run_raw_hook(payload: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=payload,
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )


def _additional_context(result: subprocess.CompletedProcess[str]) -> str:
    """Return the injected additionalContext, failing when the hook was silent."""
    assert result.stdout.strip(), f"hook stayed silent: {result.stderr}"
    output = json.loads(result.stdout)
    return output["hookSpecificOutput"]["additionalContext"]


class CodexResponseDiagnosticsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmpdir = tempfile.mkdtemp(prefix="test_codex_diagnostics_")
        self.addCleanup(shutil.rmtree, self.tmpdir, ignore_errors=True)

    def test_large_success_is_not_compressed_or_injected(self) -> None:
        """Successful output must remain untouched instead of being duplicated."""
        mock_bin, marker = _create_marker_tokenless(self.tmpdir)
        original_path = os.environ.get("PATH", "")
        with mock.patch.dict(
            os.environ,
            {
                "PATH": f"{self.tmpdir}{os.pathsep}{original_path}",
                "TOKENLESS_BIN": mock_bin,
            },
        ):
            result = _run_hook(
                {
                    "stdout": "TOKENLESS_SENTINEL_" + "x" * 10_000,
                    "stderr": "",
                    "exit_code": 0,
                }
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertFalse(
            os.path.exists(marker),
            "Codex diagnostics must not invoke the compression pipeline",
        )

    def test_environment_failure_adds_diagnostic_only(self) -> None:
        result = _run_hook(
            {
                "stdout": "",
                "stderr": "bash: frobnicate: command not found",
                "exit_code": 127,
            }
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        output = json.loads(result.stdout)
        hook_output = output["hookSpecificOutput"]
        self.assertEqual(hook_output["hookEventName"], "PostToolUse")
        context = hook_output["additionalContext"]
        self.assertIn("[tokenless:env:ENV_DEPENDENCY_MISSING]", context)
        self.assertIn(" — fix the environment first.", context)
        self.assertNotIn("[tokenless:compressed]", context)
        self.assertNotIn("updatedMCPToolOutput", hook_output)
        self.assertNotIn("suppressOutput", output)

    def test_non_object_payload_passes_through_silently(self) -> None:
        """Valid JSON that is not an object must take the silent skip path.

        The docstring promises every error path exits successfully with empty
        stdout, and the sibling schema hook already treats this input class as
        a recognised one.
        """
        for payload in (
            '[{"stdout": "bash: foo: command not found"}]',
            '"a string"',
            "42",
            "null",
            "true",
        ):
            with self.subTest(payload=payload):
                result = _run_raw_hook(payload)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")
                self.assertNotIn("Traceback", result.stderr)

    def test_unclassified_failure_passes_through(self) -> None:
        result = _run_hook(
            {"stdout": "", "stderr": "domain-specific failure", "exit_code": 1}
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_content_tools_remain_silent(self) -> None:
        result = _run_hook("permission denied", tool_name="Read")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")


class CodexEnvHintFailureEvidenceTest(unittest.TestCase):
    """The hint is failure advice, so it requires evidence the tool failed.

    Codex delivers shell output to PostToolUse as one bare string with no exit
    status, so the classifier cannot tell a failed command from a successful
    one that merely quotes error text. Every case below was observed in a live
    Codex session: the agent ran a command that succeeded and was then told not
    to retry it because the environment was broken.
    """

    def test_successful_output_quoting_error_phrases_stays_silent(self) -> None:
        successes = [
            # `git log --oneline -- src/tokenless`
            '2020f735 feat(anolisa): validate rpm install plans\n'
            '15bf1187 fix(tokenless): accept codex "not installed" row\n',
            # `gh issue view` rendering a Markdown table cell
            "| tokenless `install.sh` | yes | none \u2014 the env var does not exist |\n",
            # `cat` of a Python source file
            'try:\n    import pwd as _pwd\nexcept (ImportError, KeyError):\n    _REAL_HOME = ""\n',
            # `grep -rn` over the shell suite's own assertions
            'tests/run-all-tests.sh:502:    assert_contains "$attr_out" '
            '"ENV_DEPENDENCY_MISSING" "Attribution detects command not found"\n',
            # `sed -n` over the pattern table this hook classifies with
            '            "command not found",\n            "not installed",\n',
        ]
        for output in successes:
            with self.subTest(first_line=output.splitlines()[0]):
                result = _run_hook(output)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_unnumbered_search_results_stay_silent(self) -> None:
        """Search output prints `path:content`, which is not an error report.

        `rg --no-heading --color never "not installed" <files>` exits 0 and
        prints one `path:content` line per hit, the source line verbatim after
        the colon. Neither the file prefix nor the phrase it quotes is evidence
        that anything failed, with or without a line number.
        """
        successes = [
            # rg --no-heading --color never "not installed" hook_utils.py tests/…
            'adapters/tokenless/common/hooks/hook_utils.py:            "not installed",\n'
            'tests/test_codex_response_diagnostics.py:            "not installed",\n',
            # grep -rn over the shell suite's own assertions, line numbers off
            'tests/run-all-tests.sh:    assert_contains "$attr_out" '
            '"ENV_DEPENDENCY_MISSING" "Attribution detects command not found"\n',
            # a hit in a Markdown file and one in an extension-less Makefile
            'README.md: the installer prints "not installed" when the CLI is absent\n',
            'Makefile:\t@echo "frobnicate is not installed"\n',
        ]
        for output in successes:
            with self.subTest(first_line=output.splitlines()[0]):
                result = _run_hook(output)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_commit_metadata_quoting_error_phrases_stays_silent(self) -> None:
        """`git show` prints a commit whose message quotes the phrases.

        Observed live: reading the diff of this very fix told the agent not to
        retry a command that had succeeded. The header (`Author:`), the trailers
        (`Fixes:`, `Signed-off-by:`) and the indented message body all carry the
        `word: ` shape of a program prefix, so what separates them from a report
        is where the phrase sits: a report states its failure first, while prose
        carries a quoted phrase mid-sentence.
        """
        output = (
            "commit 157b9a2d48c9873e5924857dc1b42a3366629cb4\n"
            "Author: A. Contributor <contributor@example.com>\n"
            "Date:   Thu Sep 24 15:52:36 2026 +0800\n"
            "\n"
            "    fix(tokenless): require failure evidence for Codex env hints\n"
            "\n"
            "    session: a `git log` subject (`fix(tokenless): accept codex\n"
            '    \"not installed\" row`), an issue body ("the env var does not exist"),\n'
            "    a grep hit inside a test assertion, and a printed source file's\n"
            "    `except (ImportError, KeyError):`.\n"
            "\n"
            '    Fixes: 5f7e61079648 ("feat(tokenless): add codex adapter plugin")\n'
            "    Signed-off-by: A. Contributor <contributor@example.com>\n"
        )

        result = _run_hook(output)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_prose_and_source_lines_with_a_word_prefix_stay_silent(self) -> None:
        successes = [
            # a README paragraph printed by a successful `cat`
            "Note: frobnicate is not installed on this host by default\n",
            # an assertion in a Python suite printed by `sed -n`
            'result = _run_hook("permission denied", tool_name="Read")\n',
            # a commit trailer quoting a change it fixes
            'Fixes: 5f7e61079648 ("feat(tokenless): add codex adapter plugin")\n',
            # a diff line of the pattern table itself
            '+# ENV_PATTERNS matches text, and shell output quotes "not installed"\n',
        ]
        for output in successes:
            with self.subTest(first_line=output.splitlines()[0]):
                result = _run_hook(output)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_printed_json_stays_output_not_process_state(self) -> None:
        """A string payload is command output, whatever it parses to.

        Codex hands PostToolUse the output text, so a JSON object with no
        process state — no exit code, no output stream, no explicit error flag
        — is application data printed by a command that succeeded. Only the
        shell envelope the Common Hook parses may gate the diagnosis.
        """
        successes = [
            '{"error":"The program frobnicate is not installed"}',
            json.dumps({"error": "No module named yaml", "code": 1}),
            '{\n  "error": "The program frobnicate is not installed"\n}\n',
            json.dumps({"result": {"message": "the env var does not exist"}}),
        ]
        for output in successes:
            with self.subTest(payload=output.splitlines()[0]):
                result = _run_hook(output)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_envelope_process_state_follows_the_common_hook(self) -> None:
        """Inside the envelope contract, step 9's precedence applies.

        A reported error is a failure whatever the exit code, and a reported
        success is never diagnosed — the same reading compress_response_hook.py
        gives these fields, so the hint cannot contradict the host's own status.
        """
        failed = _run_hook(
            json.dumps(
                {
                    "stdout": "",
                    "error": "The program frobnicate is not installed",
                    "exit_code": 0,
                }
            )
        )
        self.assertIn(
            "[tokenless:env:ENV_DEPENDENCY_MISSING]", _additional_context(failed)
        )

        clean = _run_hook(
            json.dumps({"stdout": "frobnicate: command not found", "exit_code": 0})
        )
        self.assertEqual(clean.returncode, 0, clean.stderr)
        self.assertEqual(clean.stdout, "")

    def test_exit_zero_result_is_never_diagnosed(self) -> None:
        result = _run_hook(
            {
                "stdout": "ok\n",
                "stderr": "warning: rustup component frobnicate is not installed\n",
                "exit_code": 0,
            }
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_host_success_status_overrides_stderr_text(self) -> None:
        result = _run_hook(
            {"stdout": "", "stderr": "bash: frobnicate: command not found"},
            status="success",
        )

        self.assertEqual(result.stdout, "")

    def test_interrupted_run_is_not_an_environment_failure(self) -> None:
        result = _run_hook(
            {
                "stdout": "",
                "stderr": "bash: frobnicate: command not found",
                "interrupted": True,
            }
        )

        self.assertEqual(result.stdout, "")

    def test_reported_failure_keeps_permissive_matching(self) -> None:
        """With a host failure signal, prose-shaped errors are still diagnosed."""
        result = _run_hook(
            "The program frobnicate is not installed\n", is_error=True
        )

        self.assertIn(
            "[tokenless:env:ENV_DEPENDENCY_MISSING]", _additional_context(result)
        )

    def test_embedded_shell_envelope_supplies_the_failure_signal(self) -> None:
        """A JSON process result inside the text carries the missing signal."""
        failed = _run_hook(
            json.dumps(
                {
                    "stdout": "",
                    "stderr": "The program frobnicate is not installed",
                    "exit_code": 1,
                }
            )
        )
        self.assertIn(
            "[tokenless:env:ENV_DEPENDENCY_MISSING]", _additional_context(failed)
        )

        # The same envelope reporting success is not a failure, even though its
        # stdout quotes the phrase that would otherwise match.
        clean = _run_hook(
            json.dumps({"stdout": "frobnicate: command not found", "exit_code": 0})
        )
        self.assertEqual(clean.stdout, "")

    def test_genuine_failure_shapes_are_still_diagnosed(self) -> None:
        failures = [
            ("bash: frobnicate: command not found\n", "ENV_DEPENDENCY_MISSING"),
            ("/bin/sh: 1: frobnicate: not found\n", "ENV_DEPENDENCY_MISSING"),
            ("zsh: command not found: frobnicate\n", "ENV_DEPENDENCY_MISSING"),
            ("command not found: frobnicate\n", "ENV_DEPENDENCY_MISSING"),
            ("which: no frobnicate in (/usr/bin)\n", "ENV_DEPENDENCY_MISSING"),
            (
                "./configure: line 42: gcc: command not found\n",
                "ENV_DEPENDENCY_MISSING",
            ),
            ("python3.11: No module named yaml\n", "ENV_PACKAGE_MISSING"),
            (
                "/usr/bin/env: 'node': No such file or directory\n",
                "ENV_FILE_MISSING",
            ),
            (
                "ERROR: package frobnicate is not installed\n",
                "ENV_DEPENDENCY_MISSING",
            ),
            (
                "sudo: /usr/local/bin/frobnicate: permission denied\n",
                "ENV_PERMISSION",
            ),
            (
                "cat: /work/missing.txt: No such file or directory\n",
                "ENV_FILE_MISSING",
            ),
            (
                "error: command 'gcc' failed: No such file or directory\n",
                "ENV_FILE_MISSING",
            ),
            (
                "curl: (6) Could not resolve host: registry.invalid\n",
                "ENV_NETWORK",
            ),
            (
                "npm ERR! 404 Not Found - GET https://registry.invalid/pkg\n",
                "ENV_PACKAGE_MISSING",
            ),
            (
                "Traceback (most recent call last):\n"
                '  File "x.py", line 1, in <module>\n'
                "ModuleNotFoundError: No module named 'yaml'\n",
                "ENV_PACKAGE_MISSING",
            ),
            (
                "ImportError: cannot import name 'foo' from 'bar'\n",
                "ENV_PACKAGE_MISSING",
            ),
        ]
        for output, category in failures:
            with self.subTest(first_line=output.splitlines()[0]):
                result = _run_hook(output)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(
                    f"[tokenless:env:{category}]", _additional_context(result)
                )

    def test_error_line_beside_quoted_phrases_is_diagnosed(self) -> None:
        """A real error line is diagnosed even beside benign quoting lines."""
        result = _run_hook(
            'checking docs...\n'
            'README.md:12: the installer prints "command not found" when host CLI is absent\n'
            "bash: frobnicate: command not found\n"
        )

        self.assertIn(
            "[tokenless:env:ENV_DEPENDENCY_MISSING]", _additional_context(result)
        )

if __name__ == "__main__":
    unittest.main(verbosity=2)
