"""Layer 1 LanguageSource mutation, withholding, strict isolation and byte-budget tests."""

from __future__ import annotations

import inspect
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

from aw_sdk.sources import (
    LanguageSource,
    SourceDenied,
    SourceResultWithheld,
    confirm_source_routes,
    source_capabilities,
)
from aw_sdk.transport import AwError

PROVIDER_SCRIPT = r"""import json, sys
from pathlib import Path
LOG = Path(sys.argv[1])
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
    with LOG.open('a', encoding='utf-8') as fh:
        fh.write(json.dumps({'event': ev['name'], 'op': op, 'tool': ev.get('tool', {}).get('name')}) + '\n')
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


def start_aw_service(root: Path) -> tuple[subprocess.Popen[bytes], Path, Path]:
    binary = os.environ["AW_TEST_BINARY"]
    state = root / "s"
    state.mkdir(mode=0o700)
    provider = root / "provider.py"
    log_path = root / "provider.jsonl"
    provider.write_text(PROVIDER_SCRIPT, encoding="utf-8")
    config = {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "layer1-mutation-py"},
        "spec": {
            "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 4000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"app": {"adapter": "sdk", "argv": ["/bin/true"]}},
            "providers": {
                "p": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [sys.executable, str(provider), str(log_path)],
                    },
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
    return proc, endpoint, log_path


def stop_service(proc: subprocess.Popen[bytes]) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def read_provider_log(path: Path) -> list[dict[str, object]]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text("utf-8").splitlines() if line.strip()]


class CapabilityTests(unittest.TestCase):
    def test_opt_in_capabilities_and_lifecycle_compatibility(self) -> None:
        base = source_capabilities(["tool.before", "tool.after"])
        self.assertNotIn("failure_actions", base)
        self.assertFalse(confirm_source_routes(base, {"hooks": [{"event": "tool.before", "replace_input": True}]}))
        self.assertFalse(confirm_source_routes(base, {"hooks": [{"event": "tool.after", "replace_result": True}]}))
        self.assertFalse(confirm_source_routes(base, {"hooks": [{"event": "tool.after", "on_error": "withhold_result"}]}))

        full = source_capabilities(
            ["session.start", "turn.stop", "session.end", "input.submit", "tool.before", "tool.after"],
            replace_input=True,
            replace_result=True,
            withhold_result=True,
        )
        self.assertEqual(full["events"]["session.start"], ["observe"])
        self.assertEqual(full["events"]["turn.stop"], ["observe", "block"])
        self.assertEqual(full["events"]["session.end"], ["observe"])
        self.assertIn("replace_input", full["events"]["tool.before"])
        self.assertIn("replace_result", full["events"]["tool.after"])
        self.assertEqual(full["failure_actions"], {"tool.after": ["withhold_result"]})
        self.assertTrue(
            confirm_source_routes(
                full,
                {
                    "hooks": [
                        {"event": "session.start"},
                        {"event": "turn.stop"},
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


@unittest.skipUnless("AW_TEST_BINARY" in os.environ, "requires AW_TEST_BINARY")
class LanguageSourceMutationTests(unittest.IsolatedAsyncioTestCase):
    async def test_rewritten_input_requires_matching_json_types(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awp-", dir="/tmp") as tmp:
            proc, endpoint, _ = start_aw_service(Path(tmp))
            try:
                async with LanguageSource.create(
                    endpoint=str(endpoint),
                    target="app",
                    availability="required",
                    replace_input=True,
                    replace_result=True,
                    withhold_result=True,
                ) as source:
                    async with source.session(session_id="py-rewrite-types") as session:
                        executed: list[dict[str, object]] = []

                        async def tool(args: dict[str, object]) -> None:
                            executed.append(args)

                        for approved, rebuilt in ((False, 0), (True, 1), (1, 1.0)):
                            with self.subTest(approved=approved, rebuilt=rebuilt):
                                wrapped = session.wrap_tool(
                                    tool,
                                    apply_input=lambda updated, _args, _kwargs, wrong=rebuilt: (
                                        ({**updated, "options": [{"enabled": wrong}]},),
                                        {},
                                    ),
                                )
                                with self.assertRaisesRegex(
                                    AwError, "^source_input_replace_mismatch$"
                                ):
                                    await wrapped(
                                        {"command": "raw:types", "options": [{"enabled": approved}]}
                                    )
                        self.assertEqual(executed, [])
            finally:
                stop_service(proc)

    async def test_rewrite_withhold_and_streaming_boundaries(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awp-", dir="/tmp") as tmp:
            proc, endpoint, _ = start_aw_service(Path(tmp))
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
                        self.assertEqual(await wrapped({"command": "raw:hello"}), "out:safe:hello")
                        self.assertEqual(executed, ["safe:hello"])

                        with self.assertRaises(SourceDenied) as denied:
                            await wrapped({"command": "rewrite-to-deny"})
                        self.assertEqual(denied.exception.event, "tool.before")
                        self.assertEqual(executed, ["safe:hello"])

                        self.assertEqual(await wrapped({"command": "SECRET"}), "out:[REDACTED]")

                        async def struct_tool(args: dict) -> dict:
                            return {"stdout": f"data:{args['command']}", "exit_code": 0}

                        wrapped_struct = session.wrap_tool(struct_tool, name="struct")
                        self.assertEqual(
                            await wrapped_struct({"command": "SECRET"}),
                            {"stdout": "data:[REDACTED]", "exit_code": 0},
                        )

                        with self.assertRaises(SourceResultWithheld) as withheld:
                            await wrapped({"command": "FAULT"})
                        self.assertTrue(withheld.exception.tool_executed)
                        self.assertNotIn("FAULT", str(withheld.exception))

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

                        stream_runs = 0

                        async def stream_tool(args: dict):
                            nonlocal stream_runs
                            stream_runs += 1
                            yield f"chunk:{args['command']}"

                        unbuffered = session.wrap_tool(stream_tool, name="stream")
                        with self.assertRaisesRegex(AwError, "^source_stream_withhold_requires_buffering$"):
                            _ = [c async for c in unbuffered({"command": "ok"})]
                        self.assertEqual(stream_runs, 0)

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

    async def test_strict_isolation_lazy_iterators_and_unified_extract_byte_budget(self) -> None:
        with tempfile.TemporaryDirectory(prefix="awp-h-", dir="/tmp") as tmp:
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
                    async with source.session(session_id="py-hard") as session:
                        # 1) Producer reusing one mutable dict across async stream yields + finally.
                        shared = {"v": -1}

                        async def reusing_stream(_args: dict[str, str]):
                            try:
                                for i in range(3):
                                    shared["v"] = i
                                    yield shared
                            finally:
                                shared["v"] = 999

                        wrapped_stream = session.wrap_tool(
                            reusing_stream, name="reusing", buffer_stream=True
                        )
                        seen = [item async for item in wrapped_stream({"command": "ok"})]
                        self.assertEqual(seen, [{"v": 0}, {"v": 1}, {"v": 2}])

                        # 2) Normal tool returning a mutable dict mutated later.
                        holder: dict[str, object] = {}

                        async def mutable_tool(_args: dict[str, str]):
                            payload = {"text": "clean", "nested": [1, 2]}
                            holder["ref"] = payload
                            return payload

                        got = await session.wrap_tool(mutable_tool, name="mut")({"command": "ok"})
                        holder["ref"]["text"] = "TAMPERED"  # type: ignore[index]
                        holder["ref"]["nested"].append(99)  # type: ignore[union-attr]
                        self.assertEqual(got, {"text": "clean", "nested": [1, 2]})

                        # 3) Dynamic lazy iterator / sync generator on strict route.
                        gen_closed = 0

                        def lazy_gen(_args: dict[str, str]):
                            nonlocal gen_closed
                            d = {"n": 0}
                            try:
                                for i in (1, 2):
                                    d["n"] = i
                                    yield d
                            finally:
                                d["n"] = 999
                                gen_closed += 1

                        started = 0

                        def sync_gen_fn(_args: dict[str, str]):
                            nonlocal started
                            started += 1
                            yield "part"

                        with self.assertRaises(AwError) as ctx:
                            await session.wrap_tool(sync_gen_fn, name="sgen")({"command": "ok"})
                        self.assertEqual(
                            ctx.exception.code, "source_stream_withhold_requires_buffering"
                        )
                        self.assertEqual(started, 0)

                        created_gens: list[object] = []

                        def returns_lazy(args: dict[str, str]):
                            g = lazy_gen(args)
                            created_gens.append(g)
                            return g

                        with self.assertRaises(AwError) as ctx:
                            await session.wrap_tool(returns_lazy, name="lazy")({"command": "ok"})
                        self.assertEqual(
                            ctx.exception.code, "source_stream_withhold_requires_buffering"
                        )
                        self.assertTrue(getattr(ctx.exception, "tool_executed", False))
                        self.assertEqual(inspect.getgeneratorstate(created_gens[-1]), "GEN_CLOSED")

                        buffered_iter = await session.wrap_tool(
                            returns_lazy, name="lazy_buf", buffer_stream=True
                        )({"command": "ok"})
                        self.assertEqual(list(buffered_iter), [{"n": 1}, {"n": 2}])
                        self.assertEqual(gen_closed, 1)

                        # 4) Exact JSON array byte boundary, single-chunk & multi-chunk overflow.
                        closed_count = 0

                        async def sized_stream(args: dict[str, object]):
                            nonlocal closed_count
                            try:
                                for chunk in args["chunks"]:  # type: ignore[index]
                                    yield chunk
                            finally:
                                closed_count += 1

                        # '["abcd","efgh"]' is 15 UTF-8 JSON bytes (exact boundary).
                        exact = session.wrap_tool(
                            sized_stream,
                            name="sized",
                            buffer_stream=True,
                            max_stream_bytes=15,
                        )
                        out = [c async for c in exact({"chunks": ["abcd", "efgh"]})]
                        self.assertEqual(out, ["abcd", "efgh"])

                        received: list[object] = []
                        before_closed = closed_count
                        with self.assertRaises(AwError) as ctx:
                            async for c in exact({"chunks": ["123456789012"]}):
                                received.append(c)
                        self.assertEqual(ctx.exception.code, "source_stream_result_too_large")
                        self.assertTrue(getattr(ctx.exception, "tool_executed", False))
                        self.assertEqual(received, [])
                        self.assertEqual(closed_count, before_closed + 1)

                        received.clear()
                        before_closed = closed_count
                        with self.assertRaises(AwError) as ctx:
                            async for c in exact({"chunks": ["abcd", "efgh", "x"]}):
                                received.append(c)
                        self.assertEqual(ctx.exception.code, "source_stream_result_too_large")
                        self.assertTrue(getattr(ctx.exception, "tool_executed", False))
                        self.assertEqual(received, [])
                        self.assertEqual(closed_count, before_closed + 1)

                        # 5) P1: Small plain JSON object extracted into oversized audited payload
                        #    must fail closed under the same max_stream_bytes budget, evaluate
                        #    extract_result exactly once, deliver zero chunks and not call tool.after.
                        extract_calls = 0

                        def expand_payload(chunks: list[dict[str, int]]) -> str:
                            nonlocal extract_calls
                            extract_calls += 1
                            return "x" * 10000

                        before_after_logs = [
                            e for e in read_provider_log(log_path) if e["event"] == "tool.after"
                        ]
                        expanded_async = session.wrap_tool(
                            sized_stream,
                            name="expand_async",
                            buffer_stream=True,
                            max_stream_bytes=64,
                            extract_result=expand_payload,
                        )
                        leaked_async: list[object] = []
                        with self.assertRaises(AwError) as ctx:
                            async for c in expanded_async({"chunks": [{"k": 1}]}):
                                leaked_async.append(c)
                        self.assertEqual(ctx.exception.code, "source_stream_result_too_large")
                        self.assertTrue(getattr(ctx.exception, "tool_executed", False))
                        self.assertEqual(leaked_async, [])
                        self.assertEqual(extract_calls, 1)

                        def small_sync_gen(_args: dict[str, str]):
                            yield {"k": 1}

                        extract_calls = 0
                        expanded_sync = session.wrap_tool(
                            small_sync_gen,
                            name="expand_sync",
                            buffer_stream=True,
                            max_stream_bytes=64,
                            extract_result=expand_payload,
                        )
                        with self.assertRaises(AwError) as ctx:
                            await expanded_sync({"command": "ok"})
                        self.assertEqual(ctx.exception.code, "source_stream_result_too_large")
                        self.assertTrue(getattr(ctx.exception, "tool_executed", False))
                        self.assertEqual(extract_calls, 1)

                        after_logs = [
                            e for e in read_provider_log(log_path) if e["event"] == "tool.after"
                        ]
                        self.assertEqual(len(after_logs), len(before_after_logs))

                        # 6) P1: extract_result mutating its argument cannot tamper with delivered chunks.
                        extract_calls = 0

                        def mutating_extract(chunks: list[dict[str, str]]) -> str:
                            nonlocal extract_calls
                            extract_calls += 1
                            text = chunks[0]["v"]
                            chunks[0]["v"] = "MUTATED_BY_EXTRACTOR"
                            return text

                        isolated_extract = session.wrap_tool(
                            sized_stream,
                            name="mut_ext",
                            buffer_stream=True,
                            max_stream_bytes=128,
                            extract_result=mutating_extract,
                        )
                        self.assertEqual(
                            [c async for c in isolated_extract({"chunks": [{"v": "clean"}]})],
                            [{"v": "clean"}],
                        )
                        self.assertEqual(extract_calls, 1)

                        # 7) P1: Unmeterably opaque custom types are rejected on strict routes
                        #    even when extract_result (or opaque snapshot_result) is supplied.
                        class BigBox:
                            def __init__(self, data: str) -> None:
                                self.data = data

                            def __str__(self) -> str:
                                return "Box"

                        async def opaque_normal(_args: dict[str, str]):
                            return BigBox("x" * 10000)

                        async def opaque_async_stream(_args: dict[str, str]):
                            yield BigBox("x" * 10000)

                        def opaque_sync_stream(_args: dict[str, str]):
                            yield BigBox("x" * 10000)

                        for fn_name, fn, extra in [
                            ("op_plain", opaque_normal, {}),
                            ("op_ext", opaque_normal, {"extract_result": lambda b: b.data}),
                            (
                                "op_sync",
                                opaque_sync_stream,
                                {
                                    "buffer_stream": True,
                                    "max_stream_bytes": 64,
                                    "extract_result": lambda bs: "".join(b.data for b in bs),
                                },
                            ),
                            (
                                "op_sync_snap",
                                opaque_sync_stream,
                                {
                                    "buffer_stream": True,
                                    "max_stream_bytes": 64,
                                    "snapshot_result": lambda b: BigBox(b.data),
                                    "extract_result": lambda bs: "".join(b.data for b in bs),
                                },
                            ),
                        ]:
                            with self.assertRaises(AwError, msg=fn_name) as ctx:
                                await session.wrap_tool(fn, name=fn_name, **extra)({"command": "ok"})
                            self.assertEqual(ctx.exception.code, "source_result_isolation_required")
                            self.assertTrue(getattr(ctx.exception, "tool_executed", False))

                        for fn_name, extra in [
                            (
                                "op_astream",
                                {
                                    "buffer_stream": True,
                                    "max_stream_bytes": 64,
                                    "extract_result": lambda bs: "".join(b.data for b in bs),
                                },
                            ),
                            (
                                "op_astream_snap",
                                {
                                    "buffer_stream": True,
                                    "max_stream_bytes": 64,
                                    "snapshot_result": lambda b: BigBox(b.data),
                                    "extract_result": lambda bs: "".join(b.data for b in bs),
                                },
                            ),
                        ]:
                            wrapped_op = session.wrap_tool(opaque_async_stream, name=fn_name, **extra)
                            with self.assertRaises(AwError, msg=fn_name) as ctx:
                                _ = [c async for c in wrapped_op({"command": "ok"})]
                            self.assertEqual(ctx.exception.code, "source_result_isolation_required")
                            self.assertTrue(getattr(ctx.exception, "tool_executed", False))

                    # 8) P2: Sync strict replay stops on session.close() after first chunk
                    #    for both sync generator functions and dynamic sync iterators.
                    for label, as_dynamic in (("sync_gen", False), ("dynamic_iter", True)):
                        cleaned = 0

                        def close_sync_gen(_args: dict[str, str]):
                            nonlocal cleaned
                            try:
                                yield {"part": 1}
                                yield {"part": 2}
                            finally:
                                cleaned += 1

                        def returns_dynamic_iter(args: dict[str, str]):
                            return close_sync_gen(args)

                        tool_fn = returns_dynamic_iter if as_dynamic else close_sync_gen
                        close_session = source.session(session_id=f"py-close-{label}")
                        try:
                            after_before = len(
                                [e for e in read_provider_log(log_path) if e["event"] == "tool.after"]
                            )
                            wrapped_close = close_session.wrap_tool(
                                tool_fn, name=f"close_{label}", buffer_stream=True
                            )
                            replay = await wrapped_close({"command": "ok"})
                            self.assertEqual(cleaned, 1)
                            after_buffered = len(
                                [e for e in read_provider_log(log_path) if e["event"] == "tool.after"]
                            )
                            self.assertEqual(after_buffered, after_before + 1)
                            self.assertEqual(next(replay), {"part": 1})
                            await close_session.close()
                            with self.assertRaises(AwError, msg=label) as ctx:
                                next(replay)
                            self.assertEqual(ctx.exception.code, "source_session_closed")
                            self.assertTrue(getattr(ctx.exception, "tool_executed", False))
                            self.assertIsNone(next(replay, None))
                            self.assertEqual(cleaned, 1)
                            after_closed = len(
                                [e for e in read_provider_log(log_path) if e["event"] == "tool.after"]
                            )
                            self.assertEqual(after_closed, after_before + 1)
                        finally:
                            await close_session.close()
            finally:
                stop_service(proc)


if __name__ == "__main__":
    unittest.main()
