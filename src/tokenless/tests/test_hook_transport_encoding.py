"""The Core JSON pipes use UTF-8 even when Python's locale does not."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HOOKS = Path(__file__).resolve().parents[1] / "adapters/tokenless/common/hooks"

# Run a real child through the real transport. Only the locale lookup is
# controlled; disabling UTF-8 mode makes this independent of the test runner.
DRIVER = """
import io, json, locale, subprocess, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import hook_utils
locale.getencoding = lambda: sys.argv[2]
if sys.flags.utf8_mode:
    raise SystemExit('UTF-8 mode is on; the simulated locale is inert')
with subprocess.Popen([sys.executable, '-c', 'pass'],
        stdout=subprocess.PIPE, text=True) as subprocess_probe:
    if subprocess_probe.stdout.encoding.lower() != sys.argv[2].lower():
        raise SystemExit('locale simulation is inert')
request = json.loads(sys.stdin.buffer.read().decode('utf-8'))
if sys.argv[3] == 'hook':
    sys.path.insert(0, str(Path(sys.argv[1]).parents[1]))
    from hook_dispatch import load_adapter
    import hook_runtime
    hook_runtime.resolve_binary = lambda *args: sys.executable
    hook_runtime.consume_output_optimization = lambda *args: 'none'
    payload = {'tool_name': 'Bash', 'tool_input': {'command': 'build'},
               'tool_response': request['input']['content']}
    sys.stdin = io.TextIOWrapper(io.BytesIO(json.dumps(payload).encode('ascii')),
                                encoding=sys.argv[2], errors='strict')
    sys.stdout.reconfigure(encoding=sys.argv[2], errors='strict')
    hook_runtime.post_tool(load_adapter('qoder-cli'), 'qoder-cli')
    raise SystemExit(0)
if sys.argv[3] == 'warn':
    diagnostic = io.BytesIO()
    sys.stderr = io.TextIOWrapper(diagnostic, encoding=sys.argv[2], errors='strict')
    hook_utils.warn(request.get('warning_text', 'diagnostic ' + chr(0x1f680)))
    sys.stderr.flush()
    print(json.dumps({'warning': diagnostic.getvalue().decode(sys.argv[2])}))
    raise SystemExit(0)
if sys.argv[3] == 'isolated':
    proc = hook_utils.run([sys.executable, 'compress'],
        json.dumps(request, ensure_ascii=False), 8, isolated=True)
    result = json.loads(proc.stdout)['result'] if proc is not None else None
else:
    result = hook_utils.run_compress(sys.executable, request, 8, 'post_tool')
print(json.dumps(result, ensure_ascii=True))
"""

# A stand-in for Core's strict UTF-8 stdin and stdout, not a compression mock.
CORE = """
import json, sys
request = json.loads(sys.stdin.buffer.read().decode('utf-8'))
output = request.get('response', request['input']['content'])
if request.get('emit_unicode'):
    output = 'caf' + chr(233)
response = {'protocol_version': 2, 'operation': 'post_tool',
            'result': {'output': output}}
if request.get('protocol_version') == 2:
    response['result']['disposition'] = 'applied'
raw = json.dumps(response, ensure_ascii=False).encode('utf-8')
if request.get('invalid_utf8'):
    raw = raw.replace(b'INVALID', bytes([255]))
sys.stdout.buffer.write(raw)
"""


class HookTransportEncodingTests(unittest.TestCase):
    def round_trip(self, encoding: str, mode: str = "normal", **payload) -> object:
        request = {"input": {"content": "plain ASCII"}, **payload}
        with tempfile.TemporaryDirectory(prefix="hook_encoding_") as directory:
            Path(directory, "compress").write_text(CORE, encoding="utf-8")
            proc = subprocess.run(
                [
                    sys.executable,
                    "-X",
                    "utf8=0",
                    "-c",
                    DRIVER,
                    str(HOOKS),
                    encoding,
                    mode,
                ],
                input=json.dumps(request).encode("utf-8"),
                capture_output=True,
                timeout=15,
                cwd=directory,
            )
        self.assertEqual(proc.returncode, 0, proc.stderr.decode("utf-8", "replace"))
        return json.loads(proc.stdout)

    def test_unicode_requests_round_trip_under_legacy_locales(self):
        content = "\u4e2d\u6587 \U0001f680"
        for encoding in ("cp1252", "cp936"):
            with self.subTest(encoding=encoding):
                self.assertEqual(
                    self.round_trip(encoding, input={"content": content}),
                    {"output": content},
                )

    def test_utf8_response_is_not_silently_decoded_as_locale_text(self):
        for encoding in ("cp1252", "cp936"):
            with self.subTest(encoding=encoding):
                self.assertEqual(
                    self.round_trip(encoding, emit_unicode=True),
                    {"output": "caf\u00e9"},
                )

    def test_isolated_pipe_path_preserves_unicode(self):
        # This covers pipe I/O only, not POSIX process-group supervision.
        content = "caf\u00e9"
        for encoding in ("cp1252", "cp936"):
            with self.subTest(encoding=encoding):
                self.assertEqual(
                    self.round_trip(encoding, "isolated", input={"content": content}),
                    {"output": content},
                )

    def test_invalid_utf8_response_fails_open(self):
        for encoding in ("cp1252", "cp936"):
            with self.subTest(encoding=encoding):
                self.assertIsNone(
                    self.round_trip(encoding, response="INVALID", invalid_utf8=True)
                )

    def test_post_tool_emits_lossless_json_through_legacy_stdout(self):
        content = "\u4e2d\u6587 \U0001f680 caf\u00e9\n" * 30
        for encoding in ("cp1252", "cp936"):
            with self.subTest(encoding=encoding):
                result = self.round_trip(encoding, "hook", input={"content": content})
                self.assertEqual(
                    result["hookSpecificOutput"]["updatedToolOutput"], content
                )

    def test_warnings_remain_writable_through_host_replaced_strict_stderr(self):
        """Harden host-replaced strict streams; stock stderr uses backslashreplace."""
        for encoding in ("cp1252", "cp936"):
            with self.subTest(encoding=encoding):
                result = self.round_trip(encoding, "warn")
                self.assertIn("[tokenless] WARNING: diagnostic", result["warning"])
                self.assertIn("\\U0001f680", result["warning"])

    def test_warning_handles_non_roundtripping_euc_kr_character(self):
        result = self.round_trip("euc_kr", "warn", warning_text="core said: \u3164 end")
        self.assertIn("[tokenless] WARNING: core said:", result["warning"])
        self.assertIn("\\xa4", result["warning"])


if __name__ == "__main__":
    unittest.main()
