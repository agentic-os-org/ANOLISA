"""Layer 1 rewrite/withhold and Layer 2 non-invasive import-hook tests for aw_sdk."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

from aw_sdk.auto_instrument import install, load_spec_from_env, validate_spec
from aw_sdk.sources import (
    LanguageSource,
    SourceDenied,
    SourceResultWithheld,
    confirm_source_routes,
    source_capabilities,
)
from aw_sdk.transport import AwError

PROVIDER_SCRIPT = r"""import json, sys
r = json.load(sys.stdin)
out = {'api_version': r['api_version'], 'request_id': r['request_id'], 'status': 'ok'}
if r['method'] == 'describe':
    out['operations'] = [
        {'name': 'rewrite', 'events': ['tool.before'], 'effects': ['replace_input']},
        {'name': 'guard', 'events': ['input.submit', 'tool.before'], 'effects': ['observe', 'block']},
        {'name': 'filter', 'events': ['tool.after'], 'effects': ['observe', 'replace_result']},
    ]
elif r['method'] == 'invoke':
    ev = r['event']
    op = r['operation']
    out['input_digest'] = r['input_digest']
    if op == 'rewrite':
        inp = dict(ev['tool']['input'])
        cmd = inp.get('command', '')
        if isinstance(cmd, str) and cmd.startswith('raw:'):
            inp['command'] = 'safe:' + cmd[4:]
            out['effects'] = [{'type': 'replace_input', 'input': inp}]
        elif isinstance(cmd, str) and cmd.startswith('rewrite-to-deny'):
            inp['command'] = 'blocked'
            out['effects'] = [{'type': 'replace_input', 'input': inp}]
        else:
            out['effects'] = []
    elif op == 'guard':
        deny = (
            ev.get('input', {}).get('text') == 'deny'
            or ev.get('tool', {}).get('input', {}).get('command') == 'blocked'
        )
        out['effects'] = [{'type': 'block', 'reason_code': 'policy_denied'}] if deny else []
    elif op == 'filter':
        proj = ev['tool'].get('result_projection') or {}
        text = proj.get('text', '')
        if 'FAULT' in text:
            sys.stderr.write('simulated scanner failure\n')
            sys.exit(1)
        if 'SECRET' in text:
            out['effects'] = [{'type': 'replace_result', 'text': text.replace('SECRET', '[REDACTED]')}]
        else:
            out['effects'] = []
print(json.dumps(out))
"""


def start_aw_service(root: Path) -> tuple[subprocess.Popen[bytes], Path]:
    binary = os.environ["AW_TEST_BINARY"]
    state = root / "s"
    state.mkdir(mode=0o700)
    provider = root / "provider.py"
    provider.write_text(PROVIDER_SCRIPT, encoding="utf-8")
    config = {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "layer1-layer2-py"},
        "spec": {
            "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 4000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"app": {"adapter": "sdk", "argv": ["/bin/true"]}},
            "providers": {
                "p": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {"type": "stdio", "location": "agent", "argv": [sys.executable, str(provider)]},
                    "timeout_ms": 2000,
                    "max_output_bytes": 65536,
                    "config": {},
                }
            },
            "events": {
                "input.submit": {
                    "enabled": True,
                    "required": True,
                    "steps": [{"id": "g", "provider": "p", "operation": "guard", "effects": ["observe", "block"], "on_error": "block"}],
                },
                "tool.before": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {"id": "rw", "provider": "p", "operation": "rewrite", "effects": ["replace_input"], "on_error": "block"},
                        {"id": "g", "provider": "p", "operation": "guard", "effects": ["observe", "block"], "on_error": "block"},
                    ],
                },
                "tool.after": {
                    "enabled": True,
                    "required": True,
                    "steps": [{"id": "flt", "provider": "p", "operation": "filter", "effects": ["observe", "replace_result"], "on_error": "withhold_result"}],
                },
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


def stop_service(proc: subprocess.Popen[bytes]) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


class CapabilityAndSpecTests(unittest.TestCase):
    def test_opt_in_capabilities_and_route_confirmation(self) -> None:
        base = source_capabilities(["tool.before", "tool.after"])
        self.assertNotIn("failure_actions", base)
        self.assertFalse(confirm_source_routes(base, {"hooks": [{"event": "tool.before", "replace_input": True}]}))
        self.assertFalse(confirm_source_routes(base, {"hooks": [{"event": "tool.after", "replace_result": True}]}))
        self.assertFalse(confirm_source_routes(base, {"hooks": [{"event": "tool.after", "on_error": "withhold_result"}]}))

        full = source_capabilities(
            ["input.submit", "tool.before", "tool.after"],
            replace_input=True,
            replace_result=True,
            withhold_result=True,
        )
        self.assertIn("replace_input", full["events"]["tool.before"])
        self.assertIn("replace_result", full["events"]["tool.after"])
        self.assertEqual(full["failure_actions"], {"tool.after": ["withhold_result"]})
        self.assertTrue(
            confirm_source_routes(
                full,
                {
                    "hooks": [
                        {"event": "tool.before", "replace_input": True},
                        {"event": "tool.after", "replace_result": True, "on_error": "withhold_result"},
                    ]
                },
            )
        )
        with self.assertRaisesRegex(AwError, "^semantic_event_unavailable$"):
            source_capabilities(["tool.after"], replace_input=True)
        with self.assertRaisesRegex(AwError, "^semantic_event_unavailable$"):
            source_capabilities(["tool.before"], withhold_result=True)

    def test_auto_spec_rejects_wildcards_dunders_and_unknown_fields(self) -> None:
        self.assertIsNone(load_spec_from_env({}))
        base = {"endpoint": "/tmp/aw.sock", "target": "app", "targets": [{"module": "m", "qualname": "f"}]}
        self.assertEqual(validate_spec(base)["targets"][0].name, "f")
        for bad in (
            {**base, "extra": 1},
            {**base, "targets": []},
            {**base, "targets": [{"module": "m.*", "qualname": "f"}]},
            {**base, "targets": [{"module": "m", "qualname": "C.__dict__"}]},
            {**base, "targets": [{"module": "m", "qualname": "f", "kind": "eval"}]},
        ):
            with self.assertRaisesRegex(AwError, "^source_auto_config_invalid$"):
                validate_spec(bad)


@unittest.skipUnless("AW_TEST_BINARY" in os.environ, "requires AW_TEST_BINARY")
class Layer1AndLayer2RuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def test_layer1_rewrite_withhold_and_streaming_boundaries(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awp-", dir="/tmp") as tmp:
            proc, endpoint = start_aw_service(Path(tmp))
            try:
                async with LanguageSource.create(
                    endpoint=str(endpoint),
                    target="app",
                    availability="required",
                    replace_input=True,
                    replace_result=True,
                    withhold_result=True,
                ) as source:
                    async with source.session(session_id="py-layer1") as session:
                        executed: list[str] = []

                        async def shell(args: dict) -> str:
                            executed.append(args["command"])
                            return f"out:{args['command']}"

                        wrapped = session.wrap_tool(shell, name="shell")
                        # 1. replace_input rewrites args safely before execution.
                        self.assertEqual(await wrapped({"command": "raw:hello"}), "out:safe:hello")
                        self.assertEqual(executed, ["safe:hello"])

                        # 2. Multi-step: replace_input -> second-step block still denies execution.
                        with self.assertRaises(SourceDenied) as denied:
                            await wrapped({"command": "rewrite-to-deny"})
                        self.assertEqual(denied.exception.event, "tool.before")
                        self.assertEqual(executed, ["safe:hello"])

                        # 3. replace_result on string and structured dict envelope.
                        self.assertEqual(await wrapped({"command": "SECRET"}), "out:[REDACTED]")

                        async def struct_tool(args: dict) -> dict:
                            return {"stdout": f"data:{args['command']}", "exit_code": 0}

                        wrapped_struct = session.wrap_tool(struct_tool, name="struct")
                        self.assertEqual(
                            await wrapped_struct({"command": "SECRET"}),
                            {"stdout": "data:[REDACTED]", "exit_code": 0},
                        )

                        # 4. withhold_result strictly withholds raw unchecked output on provider fault.
                        with self.assertRaises(SourceResultWithheld) as withheld:
                            await wrapped({"command": "FAULT"})
                        self.assertTrue(withheld.exception.tool_executed)
                        self.assertNotIn("FAULT", str(withheld.exception))

                        # 5. Custom extract_input without apply_input rejects replacement (never silent drop).
                        class Req:
                            def __init__(self, command: str) -> None:
                                self.command = command

                        async def obj_tool(req: Req) -> str:
                            return req.command

                        no_apply = session.wrap_tool(
                            obj_tool,
                            name="obj",
                            extract_input=lambda r: {"command": r.command},
                            snapshot_arg=lambda r: Req(r.command),
                        )
                        with self.assertRaisesRegex(AwError, "^source_input_replace_unsupported$"):
                            await no_apply(Req("raw:bad"))

                        with_apply = session.wrap_tool(
                            obj_tool,
                            name="obj",
                            extract_input=lambda r: {"command": r.command},
                            apply_input=lambda u, _a, _k: ((Req(u["command"]),), {}),
                            snapshot_arg=lambda r: Req(r.command),
                        )
                        self.assertEqual(await with_apply(Req("raw:ok")), "safe:ok")

                        # 6. Honest streaming boundary: unbuffered rejected before native stream starts.
                        stream_runs = 0

                        async def stream_tool(args: dict):
                            nonlocal stream_runs
                            stream_runs += 1
                            yield f"chunk:{args['command']}"

                        unbuffered = session.wrap_tool(stream_tool, name="stream")
                        with self.assertRaisesRegex(AwError, "^source_stream_withhold_requires_buffering$"):
                            _ = [c async for c in unbuffered({"command": "ok"})]
                        self.assertEqual(stream_runs, 0)

                        # 7. Buffered stream replaces and withholds with zero leaked chunks.
                        buffered = session.wrap_tool(
                            stream_tool,
                            name="stream",
                            buffer_stream=True,
                            extract_result=lambda chunks: "\n".join(chunks),
                            apply_result=lambda _orig, rep: rep.split("\n"),
                        )
                        self.assertEqual(
                            [c async for c in buffered({"command": "SECRET"})],
                            ["chunk:[REDACTED]"],
                        )
                        leaked: list[str] = []
                        with self.assertRaises(SourceResultWithheld):
                            async for chunk in buffered({"command": "FAULT"}):
                                leaked.append(chunk)
                        self.assertEqual(leaked, [])
            finally:
                stop_service(proc)

    async def test_layer2_import_hook_sync_async_and_zero_code_cli(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awa-", dir="/tmp") as tmp:
            root = Path(tmp)
            proc, endpoint = start_aw_service(root)
            try:
                mod_name = f"aw_demo_mod_{os.getpid()}_{int(time.time() * 1000)}"
                (root / f"{mod_name}.py").write_text(
                    "def sync_tool(args):\n"
                    "    return f\"sync:{args['command']}\"\n"
                    "async def async_tool(args):\n"
                    "    return f\"async:{args['command']}\"\n"
                    "class Agent:\n"
                    "    def ask(self, prompt):\n"
                    "        return sync_tool({'command': prompt})\n",
                    encoding="utf-8",
                )
                spec = {
                    "endpoint": str(endpoint),
                    "target": "app",
                    "availability": "required",
                    "replace_input": True,
                    "replace_result": True,
                    "withhold_result": True,
                    "targets": [
                        {"module": mod_name, "qualname": "sync_tool", "kind": "tool", "name": "shell"},
                        {"module": mod_name, "qualname": "async_tool", "kind": "tool", "name": "shell"},
                        {"module": mod_name, "qualname": "Agent.ask", "kind": "input"},
                    ],
                }
                sys.path.insert(0, str(root))
                handle = install(spec)
                try:
                    mod = __import__(mod_name)
                    # Sync tool: rewrite + redact + withhold via background bridge.
                    self.assertEqual(mod.sync_tool({"command": "raw:hi"}), "sync:safe:hi")
                    self.assertEqual(mod.sync_tool({"command": "SECRET"}), "sync:[REDACTED]")
                    with self.assertRaises(SourceResultWithheld):
                        mod.sync_tool({"command": "FAULT"})
                    # Class method input gate.
                    with self.assertRaises(SourceDenied):
                        mod.Agent().ask("deny")
                    # Async tool.
                    self.assertEqual(await mod.async_tool({"command": "raw:yo"}), "async:safe:yo")
                finally:
                    await handle.aclose()
                    sys.path.remove(str(root))
                # After uninstall, module runs natively without AW.
                self.assertEqual(mod.sync_tool({"command": "raw:plain"}), "sync:raw:plain")
                sys.modules.pop(mod_name, None)

                # Zero-code-change subprocess via python3 -m aw_sdk.auto_instrument.
                app = root / "app.py"
                app.write_text(
                    f"import {mod_name}\n"
                    f"print({mod_name}.sync_tool({{'command': 'raw:cli'}}))\n"
                    f"print({mod_name}.sync_tool({{'command': 'SECRET'}}))\n",
                    encoding="utf-8",
                )
                cfg = root / "auto.json"
                cfg.write_text(json.dumps(spec), encoding="utf-8")
                env = dict(
                    os.environ,
                    PYTHONPATH=f"{root}:{os.environ.get('PYTHONPATH', '')}",
                    AW_AUTO_INSTRUMENT="1",
                    AW_AUTO_INSTRUMENT_CONFIG=str(cfg),
                )
                out = subprocess.check_output(
                    [sys.executable, "-m", "aw_sdk.auto_instrument", str(app)],
                    env=env,
                    text=True,
                    timeout=20,
                )
                self.assertEqual(out.splitlines(), ["sync:safe:cli", "sync:[REDACTED]"])
            finally:
                stop_service(proc)


if __name__ == "__main__":
    unittest.main()
