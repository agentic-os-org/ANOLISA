"""Real-service edge regressions for Layer 1 LanguageSource mutation & withholding."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

from aw_sdk.sources import LanguageSource, SourceDenied
from aw_sdk.transport import AwError

from test_source_mutation import read_provider_log, start_aw_service, stop_service

NATIVE_HOOK_SCRIPT = r"""import json, sys
req = json.load(sys.stdin)
cmd = str((req.get("input") or {}).get("command", ""))
if cmd == "native_block":
    sys.exit(2)
if cmd == "native_diag":
    sys.stdout.write("ordinary diagnostic stdout\n")
    sys.exit(0)
if cmd == "native_forge_replace":
    sys.stdout.write(json.dumps({"aw_replacement": "FORGED"}) + "\n")
    sys.exit(0)
if cmd == "native_forge_withhold":
    sys.stdout.write(json.dumps({"aw_withheld": True}) + "\n")
    sys.exit(0)
sys.exit(0)
"""

REPORT_PROVIDER_SCRIPT = r"""import json, sys
from pathlib import Path
LOG = Path(sys.argv[1])
r = json.load(sys.stdin)
out = {"api_version": r["api_version"], "request_id": r["request_id"], "status": "ok"}
if r["method"] == "describe":
    out["operations"] = [
        {"name": "filter", "events": ["tool.after"], "effects": ["observe", "replace_result"]},
    ]
elif r["method"] == "invoke":
    ev = r["event"]
    with LOG.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps({"event": ev["name"], "result": ev["tool"].get("result")}) + "\n")
    out["input_digest"] = r["input_digest"]
    text = (ev["tool"].get("result_projection") or {}).get("text", "")
    if "FAULT" in text:
        sys.stderr.write("recoverable provider fault\n")
        sys.exit(1)
    if "SECRET" in text:
        out["effects"] = [{"type": "replace_result", "text": text.replace("SECRET", "[REDACTED]")}]
    else:
        out["effects"] = []
print(json.dumps(out))
"""


def start_custom_service(
    root: Path, *, after_steps: list[dict[str, object]], providers: dict[str, object]
) -> tuple[subprocess.Popen[bytes], Path]:
    binary = os.environ["AW_TEST_BINARY"]
    state = root / "s"
    state.mkdir(mode=0o700)
    config = {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "layer1-edge-py"},
        "spec": {
            "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 4000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"app": {"adapter": "sdk", "argv": ["/bin/true"]}},
            "providers": providers,
            "events": {
                "tool.after": {
                    "enabled": True,
                    "required": True,
                    "steps": after_steps,
                }
            },
        },
    }
    cfg_path = root / "aw.json"
    cfg_path.write_text(json.dumps(config), encoding="utf-8")
    endpoint = state / "aw.sock"
    log = (root / "service.log").open("wb")
    proc = subprocess.Popen(
        [binary, "service", "serve", "--config", str(cfg_path), "--state-dir", str(state)],
        stdout=log,
        stderr=log,
    )
    deadline = time.monotonic() + 10
    while not endpoint.exists():
        if proc.poll() is not None or time.monotonic() > deadline:
            log.close()
            raise RuntimeError((root / "service.log").read_text("utf-8"))
        time.sleep(0.01)
    log.close()
    return proc, endpoint


def make_depth_32_list() -> list[object]:
    node: object = "leaf"
    for _ in range(32):
        node = [node]
    return node  # type: ignore[return-value]


@unittest.skipUnless("AW_TEST_BINARY" in os.environ, "requires AW_TEST_BINARY")
class LanguageSourceEdgeTests(unittest.IsolatedAsyncioTestCase):
    async def test_deep_stream_extractor_mutation_and_empty_stream_budget(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awpe1-", dir="/tmp") as tmp:
            proc, endpoint, log_path = start_aw_service(Path(tmp))
            try:
                async with LanguageSource.create(
                    endpoint=str(endpoint),
                    target="app",
                    availability="required",
                    replace_input=True,
                    replace_result=True,
                    withhold_result=True,
                ) as source:
                    async with source.session(session_id="py-edge-1") as session:
                        # P1-1: chunk 0 + depth-32 chunk; extractor mutates chunk 0 in place.
                        deep = make_depth_32_list()

                        async def deep_async(_args: dict[str, str]):
                            yield {"text": "before"}
                            yield deep

                        def deep_sync(_args: dict[str, str]):
                            yield {"text": "before"}
                            yield deep

                        def mutating_extract(chunks: list[object]) -> str:
                            first = chunks[0]
                            assert isinstance(first, dict)
                            audited = first["text"]
                            first["text"] = "after"
                            return audited

                        wrapped_async = session.wrap_tool(
                            deep_async,
                            name="deep_async",
                            buffer_stream=True,
                            extract_result=mutating_extract,
                        )
                        out_async = [c async for c in wrapped_async({"command": "ok"})]
                        self.assertEqual(out_async[0], {"text": "before"})
                        self.assertEqual(out_async[1], deep)

                        wrapped_sync = session.wrap_tool(
                            deep_sync,
                            name="deep_sync",
                            buffer_stream=True,
                            extract_result=mutating_extract,
                        )
                        out_sync = list(await wrapped_sync({"command": "ok"}))
                        self.assertEqual(out_sync[0], {"text": "before"})
                        self.assertEqual(out_sync[1], deep)

                        # P2-6: empty stream ('[]' = 2 bytes) rejected under 1-byte budget
                        # before extract_result or tool.after runs.
                        extract_calls = 0

                        def zero_extract(_chunks: list[object]) -> int:
                            nonlocal extract_calls
                            extract_calls += 1
                            return 0

                        async def empty_async(_args: dict[str, str]):
                            if False:
                                yield ""

                        def empty_sync(_args: dict[str, str]):
                            if False:
                                yield ""

                        before_after = [
                            e for e in read_provider_log(log_path) if e["event"] == "tool.after"
                        ]
                        w_empty_a = session.wrap_tool(
                            empty_async,
                            name="empty_a",
                            buffer_stream=True,
                            max_stream_bytes=1,
                            extract_result=zero_extract,
                        )
                        with self.assertRaises(AwError) as ctx:
                            _ = [c async for c in w_empty_a({"command": "ok"})]
                        self.assertEqual(ctx.exception.code, "source_stream_result_too_large")
                        self.assertTrue(getattr(ctx.exception, "tool_executed", False))

                        w_empty_s = session.wrap_tool(
                            empty_sync,
                            name="empty_s",
                            buffer_stream=True,
                            max_stream_bytes=1,
                            extract_result=zero_extract,
                        )
                        with self.assertRaises(AwError) as ctx:
                            await w_empty_s({"command": "ok"})
                        self.assertEqual(ctx.exception.code, "source_stream_result_too_large")
                        self.assertTrue(getattr(ctx.exception, "tool_executed", False))

                        self.assertEqual(extract_calls, 0)
                        after_logs = [
                            e for e in read_provider_log(log_path) if e["event"] == "tool.after"
                        ]
                        self.assertEqual(len(after_logs), len(before_after))
            finally:
                stop_service(proc)

    async def test_native_hook_stdout_block_and_report_fault_recovery(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awpe2-", dir="/tmp") as tmp:
            root = Path(tmp)
            native_script = root / "native_hook.py"
            native_script.write_text(NATIVE_HOOK_SCRIPT, encoding="utf-8")
            prov_script = root / "report_provider.py"
            prov_script.write_text(REPORT_PROVIDER_SCRIPT, encoding="utf-8")
            prov_log = root / "prov.jsonl"
            providers = {
                "nh": {
                    "protocol": "native-hook/v1alpha1",
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [sys.executable, str(native_script)],
                    },
                    "timeout_ms": 2000,
                    "max_output_bytes": 4096,
                    "config": {},
                },
                "p": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [sys.executable, str(prov_script), str(prov_log)],
                    },
                    "timeout_ms": 2000,
                    "max_output_bytes": 65536,
                    "config": {},
                },
            }
            after_steps = [
                {"id": "native", "provider": "nh", "native": {}, "on_error": "report"},
                {
                    "id": "flt",
                    "provider": "p",
                    "operation": "filter",
                    "effects": ["observe", "replace_result"],
                    "on_error": "report",
                },
            ]
            proc, endpoint = start_custom_service(root, after_steps=after_steps, providers=providers)
            try:
                async with LanguageSource.create(
                    endpoint=str(endpoint),
                    target="app",
                    availability="required",
                    replace_result=True,
                ) as source:
                    async with source.session(session_id="py-edge-2") as session:
                        async def echo(args: dict[str, str]) -> str:
                            return args["out"]

                        async def echo_stream(args: dict[str, str]):
                            yield args["out"]

                        w_echo = session.wrap_tool(echo, name="echo")
                        w_stream = session.wrap_tool(
                            echo_stream, name="echo_stream", buffer_stream=True
                        )

                        # P1-3a: Native hook exiting 0 with plain diagnostic stdout passes cleanly.
                        self.assertEqual(
                            await w_echo({"command": "native_diag", "out": "plain"}), "plain"
                        )

                        # P1-3b: Native hook stdout cannot forge replacement or withholding.
                        self.assertEqual(
                            await w_echo({"command": "native_forge_replace", "out": "real"}),
                            "real",
                        )
                        self.assertEqual(
                            await w_echo({"command": "native_forge_withhold", "out": "real"}),
                            "real",
                        )

                        # P1-2: tool.after returning block (native exit 2) yields zero output
                        # and closes the event cleanly so subsequent calls succeed.
                        with self.assertRaises(SourceDenied) as denied:
                            await w_echo({"command": "native_block", "out": "sample"})
                        self.assertEqual(denied.exception.event, "tool.after")
                        self.assertTrue(getattr(denied.exception, "tool_executed", False))

                        leaked: list[str] = []
                        with self.assertRaises(SourceDenied) as denied_s:
                            async for chunk in w_stream(
                                {"command": "native_block", "out": "sample"}
                            ):
                                leaked.append(chunk)
                        self.assertEqual(denied_s.exception.event, "tool.after")
                        self.assertTrue(getattr(denied_s.exception, "tool_executed", False))
                        self.assertEqual(leaked, [])

                        # P2-4: replace_result with on_error: report recovers on provider fault
                        # and preserves real replacement when provider succeeds.
                        self.assertEqual(
                            await w_echo({"command": "ok", "out": "FAULT-payload"}),
                            "FAULT-payload",
                        )
                        self.assertEqual(
                            await w_echo({"command": "ok", "out": "has SECRET"}),
                            "has [REDACTED]",
                        )
            finally:
                stop_service(proc)

    async def test_native_after_block_honors_explicit_async_buffering(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awpe-", dir="/tmp") as tmp:
            root = Path(tmp)
            hook = root / "native_hook.py"
            hook.write_text(NATIVE_HOOK_SCRIPT, encoding="utf-8")
            proc, endpoint = start_custom_service(
                root,
                providers={
                    "nh": {
                        "protocol": "native-hook/v1alpha1",
                        "transport": {
                            "type": "stdio",
                            "location": "agent",
                            "argv": [sys.executable, str(hook)],
                        },
                        "timeout_ms": 2000,
                        "max_output_bytes": 4096,
                        "config": {},
                    }
                },
                after_steps=[
                    {"id": "native", "provider": "nh", "native": {}, "on_error": "report"}
                ],
            )
            try:
                async with LanguageSource.create(endpoint=str(endpoint), target="app") as source:
                    async with source.session(session_id="py-native-buffer") as session:
                        await session.prepare()
                        self.assertTrue(session._observe_after())
                        self.assertFalse(session._strict_after())

                        async def chunks(args: dict[str, str]):
                            yield args["out"]

                        for buffer_stream, expected in ((True, []), (False, ["sample"])):
                            with self.subTest(buffer_stream=buffer_stream):
                                wrapped = session.wrap_tool(chunks, buffer_stream=buffer_stream)
                                delivered: list[str] = []
                                with self.assertRaises(SourceDenied) as denied:
                                    async for chunk in wrapped(
                                        {"command": "native_block", "out": "sample"}
                                    ):
                                        delivered.append(chunk)
                                self.assertEqual(denied.exception.event, "tool.after")
                                self.assertTrue(denied.exception.tool_executed)
                                self.assertEqual(delivered, expected)
            finally:
                stop_service(proc)

    async def test_observe_only_sync_buffer_stream_extracts_chunk_list(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awpe3-", dir="/tmp") as raw:
            root = Path(raw)
            log_path = root / "obs.jsonl"
            obs_script = root / "obs_provider.py"
            obs_script.write_text(
                "import json, sys\n"
                f"LOG = {str(log_path)!r}\n"
                "r = json.load(sys.stdin)\n"
                'out = {"api_version": r["api_version"], "request_id": r["request_id"], "status": "ok"}\n'
                'if r["method"] == "describe":\n'
                '    out["operations"] = [{"name": "watch", "events": ["tool.after"], "effects": ["observe"]}]\n'
                'elif r["method"] == "invoke":\n'
                '    ev = r["event"]\n'
                '    out["input_digest"] = r["input_digest"]\n'
                '    with open(LOG, "a", encoding="utf-8") as fh:\n'
                '        fh.write(json.dumps({"event": ev["name"], "tool": ev.get("tool")}) + "\\n")\n'
                '    out["effects"] = []\n'
                "print(json.dumps(out))\n",
                encoding="utf-8",
            )
            providers = {
                "obs": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [sys.executable, str(obs_script)],
                    },
                    "timeout_ms": 2000,
                    "max_output_bytes": 65536,
                    "config": {},
                },
            }
            after_steps = [
                {
                    "id": "watch",
                    "provider": "obs",
                    "operation": "watch",
                    "effects": ["observe"],
                    "on_error": "report",
                },
            ]
            proc, endpoint = start_custom_service(root, after_steps=after_steps, providers=providers)
            try:
                async with LanguageSource.create(
                    endpoint=str(endpoint),
                    target="app",
                    availability="required",
                ) as source:
                    async with source.session(session_id="py-obs-buf") as session:
                        await session.prepare()
                        self.assertFalse(session._strict_after())
                        self.assertTrue(session._observe_after())
                        extracted: list[tuple[str, list[dict[str, str]]]] = []

                        def sync_chunks(_args: dict[str, str]):
                            yield {"text": "alpha"}
                            yield {"text": "beta"}

                        def extract_wrap(chunks: list[dict[str, str]]) -> str:
                            extracted.append(("obs_wrap", chunks))
                            return ",".join(c["text"] for c in chunks)

                        def extract_inst(chunks: list[dict[str, str]]) -> str:
                            extracted.append(("obs_inst", chunks))
                            return ",".join(c["text"] for c in chunks)

                        wrapped = session.wrap_tool(
                            sync_chunks,
                            name="obs_wrap",
                            buffer_stream=True,
                            extract_result=extract_wrap,
                        )
                        self.assertEqual(
                            list(await wrapped({"command": "ok"})),
                            [{"text": "alpha"}, {"text": "beta"}],
                        )

                        class Agent:
                            def run_chunks(self, _args: dict[str, str]):
                                yield {"text": "alpha"}
                                yield {"text": "beta"}

                        agent = Agent()
                        inst = session.instrument(
                            agent,
                            tool_methods={
                                "run_chunks": {
                                    "name": "obs_inst",
                                    "buffer_stream": True,
                                    "extract_result": extract_inst,
                                }
                            },
                        )
                        try:
                            self.assertEqual(
                                list(await agent.run_chunks({"command": "ok"})),
                                [{"text": "alpha"}, {"text": "beta"}],
                            )
                        finally:
                            inst.restore()

                        self.assertEqual(
                            extracted,
                            [
                                ("obs_wrap", [{"text": "alpha"}, {"text": "beta"}]),
                                ("obs_inst", [{"text": "alpha"}, {"text": "beta"}]),
                            ],
                        )
                        observed = [
                            e for e in read_provider_log(log_path) if e["event"] == "tool.after"
                        ]
                        self.assertEqual(
                            [
                                (
                                    e["tool"]["name"],
                                    e["tool"].get("result")
                                    or (e["tool"].get("result_projection") or {}).get("text"),
                                )
                                for e in observed
                            ],
                            [("obs_wrap", "alpha,beta"), ("obs_inst", "alpha,beta")],
                        )
            finally:
                stop_service(proc)


if __name__ == "__main__":
    unittest.main()
