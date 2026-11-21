"""Exercise standalone Codex hook codecs with real subprocess byte pipes.

The Core stand-ins speak UTF-8, not a native compression/RTK implementation.
Binary discovery is isolated; no dependency fixer or installed host is run.
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1] / "adapters/tokenless/codex/scripts"
TEXT = "\u4e2d\u6587 \U0001f680 caf\u00e9"
CODECS = ("cp1252", "cp936", "ascii")

DRIVER = """
import io, json, locale, os, runpy, subprocess, sys
from pathlib import Path
payload = json.loads(sys.stdin.buffer.read().decode('utf-8'))
script, codec, mode = sys.argv[1:]
# The import trust checks use a POSIX UID; Windows has none. This shim only
# lets these codec tests import the source-tree classifier, not test trust.
if not hasattr(os, 'getuid'):
    os.getuid = lambda: 0
namespace = runpy.run_path(script)
scope = namespace['main'].__globals__
if os.name == 'nt' and Path(script).name == 'response-diagnostics':
    # Windows directory mode bits fail the POSIX import trust guard. Exercise
    # the real classifier through its source path; trust policy is out of scope.
    sys.path.insert(0, str(Path(script).parents[2] / 'common/hooks'))
    from hook_utils import classify_env_error
    scope['_classify_env_error'] = classify_env_error
locale.getencoding = lambda: codec
if sys.flags.utf8_mode:
    raise SystemExit('UTF-8 mode makes locale simulation inert')
with subprocess.Popen([sys.executable, '-c', 'pass'],
        stdout=subprocess.PIPE, text=True) as probe:
    if probe.stdout.encoding.lower() != codec:
        raise SystemExit('subprocess locale simulation is inert')
sys.stdout.reconfigure(encoding=codec, errors='strict')
if mode == 'pre_tool':
    print(json.dumps(scope['_pre_tool'](sys.executable, payload)))
elif mode == 'env_check':
    os.environ['CODEC_FIXTURE_INVALID'] = str(payload.get('invalid', False))
    print(json.dumps(scope['_run_env_check'](sys.executable, 'Bash')))
else:
    scope['_find_tokenless'] = lambda: sys.executable
    if mode == 'invalid_stdin':
        raw = b'{"tool_name":"Bash", "extra":"' + bytes([255]) + b'"}'
    else:
        raw = json.dumps(payload, ensure_ascii=mode != 'raw_stdin').encode('utf-8')
    sys.stdin = io.TextIOWrapper(io.BytesIO(raw), encoding=codec, errors='strict')
    scope['main']()
"""

CORE = """
import json, sys
request = json.loads(sys.stdin.buffer.read().decode('utf-8'))
arguments = dict(request['input']['arguments'])
arguments['command'] = 'tokenless rtk ' + arguments['command']
if arguments.get('unicode_reply'):
    arguments['command'] += ' caf' + chr(233)
response = {'protocol_version': 2, 'operation': 'pre_tool', 'result': {
    'arguments': arguments, 'action': 'replace_arguments',
    'output_optimization': 'rtk'}}
raw = json.dumps(response, ensure_ascii=False).encode('utf-8')
if arguments.get('invalid'):
    raw = raw.replace(b'tokenless rtk ', bytes([255]))
sys.stdout.buffer.write(raw)
"""

ENV_CHECK = """
import json, os, sys
assert sys.argv[1:] == ['--json', '--tool', 'Bash']
raw = json.dumps({'status': 'PARTIAL', 'missing': [chr(233)]},
                 ensure_ascii=False).encode('utf-8')
if os.environ.get('CODEC_FIXTURE_INVALID') == 'True':
    raw = raw.replace(chr(233).encode('utf-8'), bytes([255]))
sys.stdout.buffer.write(raw)
"""


class CodexHookEncodingTests(unittest.TestCase):
    def run_hook(
        self, script: str, codec: str, mode: str, payload: dict
    ) -> subprocess.CompletedProcess:
        with tempfile.TemporaryDirectory(prefix="codex_codecs_") as temporary:
            Path(temporary, "compress").write_text(CORE, encoding="utf-8")
            Path(temporary, "env-check").write_text(ENV_CHECK, encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    "-X",
                    "utf8=0",
                    "-c",
                    DRIVER,
                    str(SCRIPTS / script),
                    codec,
                    mode,
                ],
                input=json.dumps(payload).encode("ascii"),
                capture_output=True,
                cwd=temporary,
                timeout=20,
            )
        self.assertEqual(result.returncode, 0, result.stderr.decode("ascii", "replace"))
        self.assertNotIn(b"Traceback", result.stderr)
        return result

    def test_pre_tool_core_pipes_preserve_unicode(self) -> None:
        for codec in CODECS:
            with self.subTest(codec=codec):
                result = self.run_hook(
                    "rewrite-hook",
                    codec,
                    "pre_tool",
                    {"input": {"arguments": {"command": TEXT, "timeout": 30}}},
                )
                self.assertIsInstance(json.loads(result.stdout), dict)
                self.assertEqual(
                    json.loads(result.stdout)["arguments"],
                    {"command": "tokenless rtk " + TEXT, "timeout": 30},
                )

    def test_pre_tool_invalid_utf8_response_fails_open(self) -> None:
        for codec in CODECS:
            with self.subTest(codec=codec):
                result = self.run_hook(
                    "rewrite-hook",
                    codec,
                    "pre_tool",
                    {"input": {"arguments": {"command": "build", "invalid": True}}},
                )
                self.assertIsNone(json.loads(result.stdout))

    def test_ascii_request_does_not_corrupt_utf8_core_response(self) -> None:
        for codec in CODECS:
            with self.subTest(codec=codec):
                result = self.run_hook(
                    "rewrite-hook",
                    codec,
                    "pre_tool",
                    {
                        "input": {
                            "arguments": {"command": "build", "unicode_reply": True}
                        }
                    },
                )
                self.assertIsInstance(json.loads(result.stdout), dict)
                self.assertEqual(
                    json.loads(result.stdout)["arguments"]["command"],
                    "tokenless rtk build caf\u00e9",
                )

    def test_env_check_decodes_utf8_response(self) -> None:
        for codec in CODECS:
            with self.subTest(codec=codec):
                result = self.run_hook("tool-ready", codec, "env_check", {})
                self.assertEqual(
                    json.loads(result.stdout),
                    {"status": "PARTIAL", "missing": ["\u00e9"]},
                )

    def test_env_check_invalid_utf8_response_fails_open(self) -> None:
        for codec in CODECS:
            with self.subTest(codec=codec):
                result = self.run_hook(
                    "tool-ready", codec, "env_check", {"invalid": True}
                )
                self.assertIsNone(json.loads(result.stdout))

    def test_rewrite_output_is_lossless_on_legacy_stdout(self) -> None:
        for codec in CODECS:
            with self.subTest(codec=codec):
                result = self.run_hook(
                    "rewrite-hook",
                    codec,
                    "escaped_stdin",
                    {"tool_name": "Bash", "tool_input": {"command": TEXT}},
                )
                self.assertTrue(result.stdout, result.stderr)
                self.assertTrue(
                    result.stdout.isascii(), "legacy stdout must emit portable JSON"
                )
                output = json.loads(result.stdout)["hookSpecificOutput"]
                self.assertEqual(
                    output["updatedInput"], {"command": "tokenless rtk " + TEXT}
                )

    def test_raw_utf8_stdin_preserves_nested_arguments(self) -> None:
        for codec in CODECS:
            with self.subTest(codec=codec):
                result = self.run_hook(
                    "rewrite-hook",
                    codec,
                    "raw_stdin",
                    {
                        "tool_name": "Bash",
                        "tool_input": {"command": TEXT, "metadata": {"path": TEXT}},
                    },
                )
                self.assertTrue(result.stdout, result.stderr)
                self.assertEqual(
                    json.loads(result.stdout)["hookSpecificOutput"]["updatedInput"],
                    {"command": "tokenless rtk " + TEXT, "metadata": {"path": TEXT}},
                )

    def test_tool_ready_emits_lossless_partial_context(self) -> None:
        for codec in CODECS:
            with self.subTest(codec=codec):
                result = self.run_hook(
                    "tool-ready", codec, "raw_stdin", {"tool_name": "Bash"}
                )
                self.assertTrue(result.stdout, result.stderr)
                self.assertTrue(
                    result.stdout.isascii(), "legacy stdout must emit portable JSON"
                )
                output = json.loads(result.stdout)["hookSpecificOutput"]
                self.assertEqual(output["permissionDecision"], "allow")
                self.assertIn("not installed: \u00e9.", output["additionalContext"])

    def test_diagnostic_context_is_writable_on_ascii_stdout(self) -> None:
        result = self.run_hook(
            "response-diagnostics",
            "ascii",
            "escaped_stdin",
            {
                "tool_name": "Bash",
                "tool_response": {"stderr": "command not found", "exit_code": 127},
            },
        )
        self.assertTrue(result.stdout, result.stderr)
        output = json.loads(result.stdout)["hookSpecificOutput"]
        self.assertEqual(output["hookEventName"], "PostToolUse")
        self.assertIn("ENV_DEPENDENCY_MISSING", output["additionalContext"])
        self.assertIn("\u2014 fix the environment first.", output["additionalContext"])

    def test_invalid_utf8_host_input_is_silent(self) -> None:
        for script in ("rewrite-hook", "tool-ready", "response-diagnostics"):
            for codec in CODECS:
                with self.subTest(script=script, codec=codec):
                    result = self.run_hook(script, codec, "invalid_stdin", {})
                    self.assertEqual(result.stdout, b"")
                    self.assertNotIn(b"Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
