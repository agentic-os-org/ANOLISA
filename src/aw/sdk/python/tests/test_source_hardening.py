"""Hardening regressions for LanguageSource strict isolation, stream budgets and auto_instrument."""

from __future__ import annotations

import asyncio
import importlib.abc
import importlib.machinery
import inspect
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import types
import unittest

from aw_sdk import auto_instrument
from aw_sdk.sources import LanguageSource, SourceDenied
from aw_sdk.transport import AwError

from test_source_instrumentation import start_aw_service, stop_service


class PythonHardeningTests(unittest.IsolatedAsyncioTestCase):
    async def test_strict_output_isolation_lazy_iterators_and_stream_byte_budget(self) -> None:
        if "AW_TEST_BINARY" not in os.environ:
            self.skipTest("AW_TEST_BINARY not set")
        with tempfile.TemporaryDirectory(prefix="aw-py-hard-") as tmp:
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
                    async with source.session(session_id="hard-l1") as session:
                        # 1) Producer reusing one mutable dict across async stream yields
                        #    AND mutating it in finally: consumer receives audited isolated values.
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

                        # 2) Normal tool returning a mutable dict mutated later:
                        #    consumer receives isolated audited copy.
                        holder: dict[str, object] = {}

                        async def mutable_tool(_args: dict[str, str]):
                            payload = {"text": "clean", "nested": [1, 2]}
                            holder["ref"] = payload
                            return payload

                        wrapped_mut = session.wrap_tool(mutable_tool, name="mut")
                        got = await wrapped_mut({"command": "ok"})
                        holder["ref"]["text"] = "TAMPERED"  # type: ignore[index]
                        holder["ref"]["nested"].append(99)  # type: ignore[union-attr]
                        self.assertEqual(got, {"text": "clean", "nested": [1, 2]})

                        # 3) Normal tool returning opaque non-JSON object on strict route fails closed.
                        class Opaque:
                            def __init__(self) -> None:
                                self.secret = "raw"

                        async def opaque_tool(_args: dict[str, str]):
                            return Opaque()

                        with self.assertRaises(AwError) as ctx:
                            await session.wrap_tool(opaque_tool, name="opaque")({"command": "ok"})
                        self.assertEqual(ctx.exception.code, "source_result_isolation_required")
                        self.assertTrue(getattr(ctx.exception, "tool_executed", False))

                        # 4) Dynamic lazy iterator / sync generator on strict route.
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

                        # 5) P1-3: Cumulative byte budget (single-chunk, multi-chunk, exact boundary).
                        closed_count = 0

                        async def sized_stream(args: dict[str, object]):
                            nonlocal closed_count
                            try:
                                for chunk in args["chunks"]:  # type: ignore[index]
                                    yield chunk
                            finally:
                                closed_count += 1

                        # "abcd" as JSON is 6 bytes; 2 chunks = 12 bytes exact boundary.
                        exact = session.wrap_tool(
                            sized_stream,
                            name="sized",
                            buffer_stream=True,
                            max_stream_bytes=12,
                        )
                        out = [c async for c in exact({"chunks": ["abcd", "efgh"]})]
                        self.assertEqual(out, ["abcd", "efgh"])

                        received: list[object] = []
                        before_closed = closed_count
                        with self.assertRaises(AwError) as ctx:
                            async for c in exact({"chunks": ["12345678901"]}):
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
            finally:
                stop_service(proc)

    async def test_auto_instrument_descriptors_rollback_loaders_and_sitecustomize(self) -> None:
        if "AW_TEST_BINARY" not in os.environ:
            self.skipTest("AW_TEST_BINARY not set")
        with tempfile.TemporaryDirectory(prefix="aw-py-auto-hard-") as tmp:
            root = Path(tmp)
            proc, endpoint = start_aw_service(root)
            socket_path = str(endpoint)
            try:
                mod_name = f"aw_desc_mod_{os.getpid()}"
                mod = types.ModuleType(mod_name)

                class Base:
                    def inherited(self, args: dict[str, str]) -> str:
                        return f"base:{self.__class__.__name__}:{args['command']}"

                class Sub(Base):
                    @classmethod
                    def cls_tool(cls, args: dict[str, str]) -> str:
                        return f"cls:{cls.__name__}:{args['command']}"

                    @staticmethod
                    def static_tool(args: dict[str, str]) -> str:
                        return f"static:{args['command']}"

                    @property
                    def bad_prop(self) -> str:
                        return "nope"

                def ok_fn(args: dict[str, str]) -> str:
                    return f"ok:{args['command']}"

                def sync_gen(args: dict[str, str]):
                    box = {"v": args["command"]}
                    try:
                        yield box
                    finally:
                        box["v"] = "MUTATED"

                mod.Base = Base  # type: ignore[attr-defined]
                mod.Sub = Sub  # type: ignore[attr-defined]
                mod.ok_fn = ok_fn  # type: ignore[attr-defined]
                mod.sync_gen = sync_gen  # type: ignore[attr-defined]
                mod.NOT_CALLABLE = 123  # type: ignore[attr-defined]
                sys.modules[mod_name] = mod
                try:
                    meta_before = list(sys.meta_path)
                    with self.assertRaises(AwError) as ctx:
                        auto_instrument.install(
                            {
                                "endpoint": socket_path,
                                "target": "app",
                                "targets": [
                                    {"module": mod_name, "qualname": "ok_fn"},
                                    {"module": mod_name, "qualname": "typo_fn"},
                                ],
                            }
                        )
                    self.assertEqual(ctx.exception.code, "source_auto_target_missing")
                    self.assertIs(mod.ok_fn, ok_fn)
                    self.assertEqual(sys.meta_path, meta_before)

                    for bad_qual in ("Sub.bad_prop", "NOT_CALLABLE"):
                        with self.assertRaises(AwError) as ctx:
                            auto_instrument.install(
                                {
                                    "endpoint": socket_path,
                                    "target": "app",
                                    "targets": [
                                        {"module": mod_name, "qualname": "ok_fn"},
                                        {"module": mod_name, "qualname": bad_qual},
                                    ],
                                }
                            )
                        self.assertEqual(ctx.exception.code, "source_auto_target_unsupported")
                        self.assertIs(mod.ok_fn, ok_fn)
                        self.assertEqual(sys.meta_path, meta_before)

                    legacy_mod_name = f"aw_legacy_loader_{os.getpid()}"

                    class LegacyLoader:
                        pass

                    class LegacyFinder(importlib.abc.MetaPathFinder):
                        def find_spec(self, fullname, path, target=None):
                            if fullname == legacy_mod_name:
                                return importlib.machinery.ModuleSpec(fullname, LegacyLoader())
                            return None

                    legacy_finder = LegacyFinder()
                    sys.meta_path.append(legacy_finder)
                    try:
                        with auto_instrument.install(
                            {
                                "endpoint": socket_path,
                                "target": "app",
                                "targets": [{"module": legacy_mod_name, "qualname": "run"}],
                            }
                        ) as legacy_handle:
                            self.assertIn((legacy_mod_name, "run"), legacy_handle.pending_targets)
                            with self.assertRaises(AwError) as ctx:
                                legacy_handle.assert_installed()
                            self.assertEqual(ctx.exception.code, "source_auto_targets_pending")
                            with self.assertRaises(AwError) as ctx:
                                __import__(legacy_mod_name)
                            self.assertEqual(ctx.exception.code, "source_auto_target_unsupported")
                    finally:
                        sys.meta_path.remove(legacy_finder)

                    handle = auto_instrument.install(
                        {
                            "endpoint": socket_path,
                            "target": "app",
                            "replace_input": True,
                            "replace_result": True,
                            "withhold_result": True,
                            "targets": [
                                {"module": mod_name, "qualname": "Sub.inherited"},
                                {"module": mod_name, "qualname": "Sub.cls_tool"},
                                {"module": mod_name, "qualname": "Sub.static_tool"},
                                {
                                    "module": mod_name,
                                    "qualname": "sync_gen",
                                    "buffer_stream": True,
                                },
                            ],
                        }
                    )
                    try:
                        handle.assert_installed()
                        inst = Sub()
                        self.assertEqual(inst.inherited({"command": "raw:1"}), "base:Sub:safe:1")
                        self.assertEqual(Sub.cls_tool({"command": "raw:2"}), "cls:Sub:safe:2")
                        self.assertEqual(inst.cls_tool({"command": "raw:3"}), "cls:Sub:safe:3")
                        self.assertEqual(Sub.static_tool({"command": "raw:4"}), "static:safe:4")
                        with self.assertRaises(SourceDenied):
                            Sub.cls_tool({"command": "blocked"})
                        self.assertEqual(
                            list(mod.sync_gen({"command": "raw:5"})),
                            [{"v": "safe:5"}],
                        )
                    finally:
                        handle.uninstall()
                    self.assertNotIn("inherited", Sub.__dict__)
                    self.assertEqual(Sub().inherited({"command": "blocked"}), "base:Sub:blocked")
                finally:
                    sys.modules.pop(mod_name, None)

                site_dir = root / "site_fail"
                site_dir.mkdir()
                (site_dir / "sitecustomize.py").write_text(
                    "import aw_sdk.auto_instrument\naw_sdk.auto_instrument.bootstrap()\n",
                    encoding="utf-8",
                )
                sdk_src = str(Path(__file__).resolve().parents[1] / "src")
                env = dict(
                    os.environ,
                    PYTHONPATH=f"{site_dir}{os.pathsep}{sdk_src}",
                    AW_AUTO_INSTRUMENT="1",
                    AW_AUTO_INSTRUMENT_SPEC="{invalid json",
                )
                sub = subprocess.run(
                    [sys.executable, "-c", "print('SHOULD_NOT_RUN')"],
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=15,
                    check=False,
                )
                self.assertNotEqual(sub.returncode, 0)
                self.assertNotIn("SHOULD_NOT_RUN", sub.stdout)
            finally:
                stop_service(proc)

    async def test_concurrent_first_call_multiloop_cleanup_and_script_syspath(self) -> None:
        if "AW_TEST_BINARY" not in os.environ:
            self.skipTest("AW_TEST_BINARY not set")
        with tempfile.TemporaryDirectory(prefix="aw-py-conc-") as tmp:
            root = Path(tmp)
            proc, endpoint = start_aw_service(root)
            socket_path = str(endpoint)
            try:
                mod_name = f"aw_conc_mod_{os.getpid()}"
                mod = types.ModuleType(mod_name)

                async def async_tool(args: dict[str, str]) -> str:
                    return f"ok:{args['command']}"

                mod.async_tool = async_tool  # type: ignore[attr-defined]
                sys.modules[mod_name] = mod

                connect_calls = 0
                orig_connect = LanguageSource.connect

                async def counted_connect(self_src: LanguageSource):
                    nonlocal connect_calls
                    connect_calls += 1
                    return await orig_connect(self_src)

                LanguageSource.connect = counted_connect  # type: ignore[method-assign]
                try:
                    handle = auto_instrument.install(
                        {
                            "endpoint": socket_path,
                            "target": "app",
                            "replace_input": True,
                            "replace_result": True,
                            "withhold_result": True,
                            "targets": [{"module": mod_name, "qualname": "async_tool"}],
                        }
                    )
                    try:
                        results = await asyncio.gather(
                            mod.async_tool({"command": "raw:a"}),
                            mod.async_tool({"command": "raw:b"}),
                            mod.async_tool({"command": "raw:c"}),
                        )
                        self.assertEqual(results, ["ok:safe:a", "ok:safe:b", "ok:safe:c"])
                        self.assertEqual(connect_calls, 1)

                        bg_results: list[str] = []
                        bg_ready = threading.Event()
                        bg_stop = threading.Event()

                        def bg_worker() -> None:
                            loop = asyncio.new_event_loop()
                            asyncio.set_event_loop(loop)

                            async def run_bg() -> None:
                                bg_results.append(await mod.async_tool({"command": "raw:bg"}))
                                bg_ready.set()
                                while not bg_stop.is_set():
                                    await asyncio.sleep(0.01)

                            loop.run_until_complete(run_bg())
                            loop.close()

                        t = threading.Thread(target=bg_worker, daemon=True)
                        t.start()
                        self.assertTrue(bg_ready.wait(timeout=10))
                        self.assertEqual(bg_results, ["ok:safe:bg"])
                        self.assertEqual(connect_calls, 2)
                        await handle.aclose()
                        bg_stop.set()
                        t.join(timeout=5)
                        self.assertFalse(t.is_alive())
                    finally:
                        if handle.active:
                            await handle.aclose()

                    # Partial connect failure (unreachable endpoint) cleans up slot cleanly.
                    bad_handle = auto_instrument.install(
                        {
                            "endpoint": str(root / "missing.sock"),
                            "target": "app",
                            "availability": "required",
                            "targets": [{"module": mod_name, "qualname": "async_tool"}],
                        }
                    )
                    try:
                        with self.assertRaises(AwError):
                            await mod.async_tool({"command": "ok"})
                        # Second call fails cleanly without hanging on a wedged task.
                        with self.assertRaises(AwError):
                            await mod.async_tool({"command": "ok"})
                    finally:
                        bad_handle.uninstall()

                    # Sync uninstall() also closes async sessions on their owning background loop.
                    bg2_ready = threading.Event()
                    bg2_stop = threading.Event()
                    bg2_handle = auto_instrument.install(
                        {
                            "endpoint": socket_path,
                            "target": "app",
                            "replace_input": True,
                            "replace_result": True,
                            "withhold_result": True,
                            "targets": [{"module": mod_name, "qualname": "async_tool"}],
                        }
                    )

                    def bg2_worker() -> None:
                        loop = asyncio.new_event_loop()
                        asyncio.set_event_loop(loop)

                        async def run_bg2() -> None:
                            self.assertEqual(
                                await mod.async_tool({"command": "raw:sync_uninst"}),
                                "ok:safe:sync_uninst",
                            )
                            bg2_ready.set()
                            while not bg2_stop.is_set():
                                await asyncio.sleep(0.01)

                        loop.run_until_complete(run_bg2())
                        loop.close()

                    t2 = threading.Thread(target=bg2_worker, daemon=True)
                    t2.start()
                    self.assertTrue(bg2_ready.wait(timeout=10))
                    bg2_handle.uninstall()
                    bg2_stop.set()
                    t2.join(timeout=5)
                    self.assertFalse(t2.is_alive())
                finally:
                    LanguageSource.connect = orig_connect  # type: ignore[method-assign]
                    sys.modules.pop(mod_name, None)

                dir_a = root / "dir_a"
                dir_b = root / "dir_b"
                dir_a.mkdir()
                dir_b.mkdir()
                (dir_a / "sibling_helper.py").write_text(
                    "def run_cmd(args):\n    return 'from_script_dir:' + args['command']\n",
                    encoding="utf-8",
                )
                (dir_b / "sibling_helper.py").write_text(
                    "def run_cmd(args):\n    return 'from_cwd_decoy:' + args['command']\n",
                    encoding="utf-8",
                )
                (dir_a / "app.py").write_text(
                    "import json, sibling_helper\n"
                    "print(json.dumps({'out': sibling_helper.run_cmd({'command': 'raw:x'})}))\n",
                    encoding="utf-8",
                )
                spec_path = root / "syspath_spec.json"
                spec_path.write_text(
                    json.dumps(
                        {
                            "endpoint": socket_path,
                            "target": "app",
                            "replace_input": True,
                            "replace_result": True,
                            "withhold_result": True,
                            "targets": [{"module": "sibling_helper", "qualname": "run_cmd"}],
                        }
                    ),
                    encoding="utf-8",
                )
                sdk_src = str(Path(__file__).resolve().parents[1] / "src")
                env = dict(
                    os.environ,
                    PYTHONPATH=sdk_src,
                    AW_AUTO_INSTRUMENT="1",
                    AW_AUTO_INSTRUMENT_CONFIG=str(spec_path),
                )
                sub = subprocess.run(
                    [sys.executable, "-m", "aw_sdk.auto_instrument", str(dir_a / "app.py")],
                    cwd=dir_b,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=30,
                    check=False,
                )
                self.assertEqual(sub.returncode, 0, sub.stderr)
                self.assertEqual(
                    json.loads(sub.stdout.strip()), {"out": "from_script_dir:safe:x"}
                )
            finally:
                stop_service(proc)


if __name__ == "__main__":
    unittest.main()
