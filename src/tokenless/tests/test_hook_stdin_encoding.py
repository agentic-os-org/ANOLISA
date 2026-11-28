"""Shared lifecycle ingress preserves UTF-8 without reconfiguring host streams.

The real Qwen Code adapter and request builders run. Binary discovery, Core
responses and optimization-state persistence are isolated from these tests.
"""

import json
import subprocess
import sys
import unittest
from pathlib import Path

HOOKS = Path(__file__).resolve().parents[1] / "adapters/tokenless/common/hooks"
TEXT = "\u4e2d\u6587 \U0001f680 caf\u00e9"
CODECS = ("cp1252", "cp936", "ascii")

DRIVER = """
import importlib.util, io, json, sys
from pathlib import Path
hooks = Path(sys.argv[1])
sys.path.insert(0, str(hooks))
import hook_runtime, schema_runtime
spec = importlib.util.spec_from_file_location('host_adapter',
    hooks.parents[1] / 'qwencode/hook_adapter.py')
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)
options = json.loads(sys.stdin.buffer.read().decode('utf-8'))
raw = bytes.fromhex(options['raw'])
if options['text_only']:
    stream = io.StringIO(raw.decode('utf-8'))
else:
    stream = io.TextIOWrapper(io.BytesIO(raw), encoding=options['codec'], errors='strict')
sys.stdin = stream
requests = []
def core(binary, request, timeout, operation):
    requests.append(request)
    if operation == 'pre_tool':
        return {'action': 'replace_arguments', 'output_optimization': 'rtk',
                'arguments': dict(request['input']['arguments'], command='rewritten')}
    if operation == 'before_model':
        return {'tools': request['input']['tools']}
    return {'disposition': 'applied', 'output': 'short output'}
for module in (hook_runtime, schema_runtime):
    module.resolve_binary = lambda *args: 'isolated-core'
    module.run_compress = core
hook_runtime.mark_rtk_optimized = lambda *args: None
hook_runtime.consume_output_optimization = lambda *args: 'none'
schema_runtime._should_warn_no_tools = lambda *args: False
output = io.StringIO()
sys.stdout = output
try:
    hook_runtime.run_hook(options['operation'], adapter, 'qwencode')
except SystemExit as error:
    if error.code not in (0, None):
        raise
finally:
    sys.stdout = sys.__stdout__
assert sys.stdin is stream, 'input stream was replaced'
if not options['text_only']:
    assert stream.encoding == options['codec'], 'input stream was reconfigured'
print(json.dumps({'requests': requests, 'output': json.loads(output.getvalue())}))
"""


class HookStdinEncodingTests(unittest.TestCase):
    def invoke(
        self, operation: str, raw: bytes, codec: str, text_only: bool = False
    ) -> dict:
        result = subprocess.run(
            [sys.executable, "-B", "-X", "utf8=0", "-c", DRIVER, str(HOOKS)],
            input=json.dumps(
                {
                    "operation": operation,
                    "codec": codec,
                    "raw": raw.hex(),
                    "text_only": text_only,
                }
            ).encode("ascii"),
            capture_output=True,
            timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("ascii", "replace"))
        self.assertNotIn(b"Traceback", result.stderr)
        return json.loads(result.stdout)

    def payload(self, operation: str, text: str = TEXT) -> dict:
        data = {
            "tool_name": "Bash",
            "session_id": text,
            "tool_use_id": "call-1",
            "tool_input": {"command": text, "metadata": {"path": text}},
            "tool_response": text * 20,
        }
        if operation == "before_model":
            data["llm_request"] = {
                "messages": [{"role": "user", "content": text}],
                "config": {"tools": [{"name": "lookup", "description": text}]},
            }
        return data

    def check_request(self, operation: str, result: dict, payload: dict) -> None:
        self.assertEqual(len(result["requests"]), 1, result)
        request = result["requests"][0]
        self.assertEqual(request["attribution"]["session_id"], payload["session_id"])
        if operation == "pre_tool":
            self.assertEqual(request["input"]["arguments"], payload["tool_input"])
            updated = result["output"]["hookSpecificOutput"]["updatedInput"]
            self.assertEqual(
                updated,
                {"command": "rewritten", "metadata": payload["tool_input"]["metadata"]},
            )
        elif operation == "post_tool":
            self.assertEqual(request["input"]["content"], payload["tool_response"])
            self.assertEqual(
                request["input"]["command"], payload["tool_input"]["command"]
            )
        else:
            expected = payload["llm_request"]["config"]["tools"]
            self.assertEqual(request["input"]["tools"], expected)
            self.assertEqual(
                result["output"]["hookSpecificOutput"]["llm_request"]["config"][
                    "tools"
                ],
                expected,
            )

    def test_raw_utf8_preserves_all_three_lifecycle_requests(self) -> None:
        for operation in ("pre_tool", "post_tool", "before_model"):
            for codec in CODECS:
                with self.subTest(operation=operation, codec=codec):
                    payload = self.payload(operation)
                    raw = json.dumps(payload, ensure_ascii=False).encode("utf-8")
                    self.check_request(
                        operation, self.invoke(operation, raw, codec), payload
                    )

    def test_ascii_escaped_host_json_retains_existing_behavior(self) -> None:
        for operation in ("pre_tool", "post_tool", "before_model"):
            with self.subTest(operation=operation):
                payload = self.payload(operation)
                raw = json.dumps(payload).encode("ascii")
                self.check_request(
                    operation, self.invoke(operation, raw, "ascii"), payload
                )

    def test_text_only_host_streams_remain_supported(self) -> None:
        for operation in ("pre_tool", "post_tool", "before_model"):
            with self.subTest(operation=operation):
                payload = self.payload(operation)
                raw = json.dumps(payload, ensure_ascii=False).encode("utf-8")
                result = self.invoke(operation, raw, "ascii", text_only=True)
                self.check_request(operation, result, payload)

    def test_invalid_utf8_does_not_reach_core(self) -> None:
        for operation in ("pre_tool", "post_tool", "before_model"):
            for codec in CODECS:
                with self.subTest(operation=operation, codec=codec):
                    raw = json.dumps(self.payload(operation, "INVALID")).encode("ascii")
                    raw = raw.replace(b"INVALID", b"\xff")
                    result = self.invoke(operation, raw, codec)
                    self.assertEqual(result["requests"], [])
                    self.assertEqual(result["output"], {})

    def test_empty_malformed_and_non_object_json_still_pass_through(self) -> None:
        for operation in ("pre_tool", "post_tool", "before_model"):
            for raw in (b"", b"{", b"[]", b"null"):
                with self.subTest(operation=operation, raw=raw):
                    result = self.invoke(operation, raw, "cp1252")
                    self.assertEqual(result["requests"], [])
                    self.assertEqual(result["output"], {})


if __name__ == "__main__":
    unittest.main()
