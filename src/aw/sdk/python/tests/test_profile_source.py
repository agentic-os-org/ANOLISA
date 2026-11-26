"""Exercise profile lifecycle, native adoption and call draining."""

from __future__ import annotations

import asyncio
import io
import json
import sys
import threading
import types
import unittest
from collections.abc import AsyncIterator, Iterator
from contextlib import redirect_stdout
from contextvars import ContextVar
from importlib.metadata import PackageNotFoundError
from typing import Any
from unittest.mock import AsyncMock, patch
from uuid import UUID

from aw_sdk import AwError, Instance
from aw_sdk.profile_source import ProfileSource
from aw_sdk.sources import LanguageSource, SourceDenied, SourceResultWithheld
from source_fixture import FakeTask
from test_profile_eval import capability, document, field, target

MODULE = "aw_profile_runtime_demo"
CONTINUE = {"decision": "continue", "coverage": "checked"}
BLOCK = {"decision": "block", "coverage": "checked"}


def tool_target(
    name: str = "run",
    *,
    call: str = "async",
    codec: str = "json",
    result: dict[str, str] | None = None,
    **extra: Any,
) -> dict[str, Any]:
    return target(
        MODULE,
        name,
        call=call,
        hooks={
            "tool.before": capability(
                "block", "replace_input", actions=("block", "report"), mode="control"
            ),
            "tool.after": capability(
                "replace_result", actions=("withhold_result", "report"), mode="control"
            ),
        },
        fields={
            "tool_name": {"const": "run"},
            "input": field("arg.0", codec),
            "result": result or field("result", "json"),
        },
        **extra,
    )


def load_module() -> types.ModuleType:
    module = types.ModuleType(MODULE)
    sys.modules[MODULE] = module
    return module


class ProbeTests(unittest.TestCase):
    def test_valid_but_unwired_events_are_unavailable(self) -> None:
        for event, fields in (
            ("permission.request", {"tool_name": {"const": "run"}}),
            ("compact.before", {}),
            ("compact.after", {}),
            ("subagent.start", {}),
            ("subagent.stop", {}),
            ("model.before_request", {"request": field("arg.0", "json")}),
            ("security.violation", {"output": field("result")}),
        ):
            with (
                self.subTest(event),
                self.assertRaisesRegex(AwError, "^semantic_event_unavailable$"),
            ):
                ProfileSource.probe(
                    document(target(hooks={event: capability()}, fields=fields))
                )

    def test_probe_rejects_fields_the_sdk_cannot_deliver(self) -> None:
        for spec in (
            target(
                hooks={"tool.before": capability()},
                fields={"tool_name": {"const": "run"}, "input": field("arg.0")},
            ),
            target(
                hooks={"turn.stop": capability()}, fields={"output": field("result")}
            ),
        ):
            with patch(
                "aw_sdk.profile_source.version", return_value="1.5.2"
            ), self.assertRaisesRegex(AwError, "source_probe_field_unsupported"):
                ProfileSource.probe(document(spec))

    def test_package_is_resolved_without_importing_target(self) -> None:
        profile = document(target())
        with (
            patch("aw_sdk.profile_source.version", side_effect=PackageNotFoundError),
            self.assertRaisesRegex(AwError, "source_probe_package_missing"),
        ):
            ProfileSource.probe(profile)
        with (
            patch("aw_sdk.profile_source.version", return_value="2.0.0"),
            self.assertRaisesRegex(AwError, "source_probe_package_mismatch"),
        ):
            ProfileSource.probe(profile)
        with patch("aw_sdk.profile_source.version", return_value="1.4.9rc2") as version:
            ProfileSource.probe(profile)
            version.assert_called_once_with("profile-demo")
        self.assertNotIn(profile["targets"][0]["module"], sys.modules)


class ProfileFixture(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        self.module = load_module()
        self.addCleanup(sys.modules.pop, MODULE, None)
        metadata = patch("aw_sdk.profile_source.version", return_value="1.5.2")
        metadata.start()
        self.addCleanup(metadata.stop)
        self.tasks: list[FakeTask] = []

    def fake_language(
        self, profile: ProfileSource, handler: Any, routes: list[dict] | None
    ) -> LanguageSource:
        source = LanguageSource(
            Instance(
                endpoint="/unused/aw.sock",
                target="app",
                availability=profile.availability,
                capabilities=profile.capabilities,
                install=lambda _: True,
            )
        )
        source.instance.state = "ready"

        negotiated = (
            [
                {"event": event, "step": "$aw.observe", "on_error": "report"}
                for event in profile.hooks
            ]
            if routes is None
            else routes
        )
        source.instance.connection = {"routes": {"hooks": negotiated}}

        async def open_task(**options: Any) -> FakeTask:
            task = FakeTask(handler)
            task.routes["hooks"] = negotiated
            self.tasks.append(task)
            await source.instance.installed(task.routes, install=options.get("install"))
            return task

        source.connect = AsyncMock(return_value=True)
        source.instance.task = open_task
        return source

    async def activate(
        self,
        declarations: list[dict],
        handler: Any,
        *,
        routes: list[dict] | None = None,
        **options: Any,
    ) -> ProfileSource:
        profile = ProfileSource.probe(document(*declarations), **options)
        self.addAsyncCleanup(profile.teardown)
        profile.install()
        profile.verify()
        await profile.activate(self.fake_language(profile, handler, routes))
        return profile


class ProfileRuntimeTests(ProfileFixture):
    async def test_sync_stream_reserves_cleanup_before_first_native_step(self) -> None:
        identity = ContextVar("identity", default="first reader")
        seen = []

        def stream():
            seen.append(identity.get())
            yield "item"

        self.module.stream = stream
        spec = tool_target("stream", call="sync_iter")
        spec.update(hooks={"session.start": capability()}, fields={})
        profile = await self.activate([spec], AsyncMock(return_value=CONTINUE))
        async with profile.request():
            chunks = self.module.stream()

            def first_read():
                with patch.object(
                    threading.Thread, "start", side_effect=RuntimeError("no threads")
                ), self.assertRaisesRegex(RuntimeError, "no threads"):
                    next(chunks)

            await asyncio.to_thread(first_read)
            self.assertEqual(seen, [])
            token = identity.set("second reader")
            try:
                self.assertEqual(await asyncio.to_thread(next, chunks), "item")
            finally:
                identity.reset(token)
        self.assertEqual(seen, ["second reader"])

    async def test_sync_worker_bridge_and_loop_guard(self) -> None:
        runs: list[dict] = []

        def run(payload: dict) -> str:
            runs.append(payload)
            payload["native"] = True
            return payload["command"]

        async def handler(event: str, payload: dict) -> dict:
            if event == "tool.before" and payload["input"]["command"] == "deny":
                return BLOCK
            if event == "tool.before":
                return {**CONTINUE, "updated_input": {"command": "approved"}}
            return {**CONTINUE, "replaced_result": "filtered"}

        self.module.run = run
        captured = io.StringIO()
        with redirect_stdout(captured):
            profile = await self.activate(
                [tool_target(call="sync", result=field("result"))],
                handler,
                routes=[
                    {
                        "event": "tool.before",
                        "replace_input": True,
                        "on_error": "block",
                    },
                    {"event": "tool.after", "replace_result": True},
                ],
            )
            async with profile.request():
                with self.assertRaisesRegex(
                    AwError, "source_sync_call_on_activation_loop"
                ):
                    self.module.run({"command": "event loop"})
                with self.assertRaises(SourceDenied):
                    await asyncio.to_thread(self.module.run, {"command": "deny"})
                self.assertEqual(runs, [])
                original = {"command": "original"}
                self.assertEqual(
                    await asyncio.to_thread(self.module.run, payload=original),
                    "filtered",
                )
                self.assertEqual(original, {"command": "original"})
                self.assertEqual(runs, [{"command": "approved", "native": True}])
                events = self.tasks[0].events
                call_id = events[1][1]["call_id"]
                self.assertEqual(UUID(call_id).version, 4)
                self.assertEqual(events[2][1]["call_id"], call_id)
                self.assertEqual(events[2][1]["input"], {"command": "approved"})
            await profile.teardown()
        self.assertEqual(captured.getvalue(), "")

    async def test_async_json_string_writeback_and_withhold(self) -> None:
        async def run(payload: str, call_id: str) -> str:
            inputs.append(payload)
            return payload

        async def handler(event: str, payload: dict) -> dict:
            if event == "tool.before":
                return {**CONTINUE, "updated_input": {"approved": "你好"}}
            if payload["call_id"] == "withheld":
                return {"decision": "withhold", "coverage": "checked"}
            return {**CONTINUE, "replaced_result": {"approved": "result"}}

        self.module.run = run
        inputs: list[str] = []
        seam = tool_target(codec="json_string", result=field("result", "json_string"))
        seam["fields"]["call_id"] = field("kwarg.call_id")
        profile = await self.activate(
            [seam],
            handler,
            routes=[
                {"event": "tool.before", "replace_input": True},
                {
                    "event": "tool.after",
                    "replace_result": True,
                    "on_error": "withhold_result",
                },
            ],
        )
        async with profile.request():
            result = await self.module.run(
                call_id="call-1", payload='{"payload":"原始"}'
            )
            self.assertEqual(json.loads(result), {"approved": "result"})
            self.assertIn("你好", inputs[0])
            with self.assertRaises(SourceResultWithheld) as caught:
                await self.module.run('{"payload":"original"}', "withheld")
            self.assertTrue(caught.exception.tool_executed)
            with self.assertRaisesRegex(AwError, "source_tool_input_object_required"):
                await self.module.run("[]", "invalid")
        emitted = self.tasks[0].events
        self.assertEqual(
            [payload["call_id"] for _, payload in emitted],
            [
                "call-1",
                "call-1",
                "withheld",
                "withheld",
            ],
        )

    async def test_sync_and_async_buffered_streams(self) -> None:
        closed = []

        def sync_chunks(payload: dict) -> Iterator[str]:
            try:
                yield payload["value"]
                yield payload["value"]
            finally:
                closed.append("sync")
            return "private terminal"

        async def async_chunks(payload: dict) -> AsyncIterator[str]:
            try:
                yield payload["value"]
                yield payload["value"]
            finally:
                closed.append("async")

        async def handler(event: str, payload: dict) -> dict:
            if event == "tool.before":
                return {**CONTINUE, "updated_input": {"value": "approved"}}
            self.assertEqual(payload["result"], ["approved", "approved"])
            if closed[-1] == "sync":
                return {**CONTINUE, "replaced_result": ["checked"]}
            return {"decision": "withhold", "coverage": "checked"}

        self.module.sync_chunks = sync_chunks
        self.module.async_chunks = async_chunks
        stream = {"buffer": True, "max_items": 2, "max_bytes": 32}
        profile = await self.activate(
            [
                tool_target("sync_chunks", call="sync_iter", stream=stream),
                tool_target("async_chunks", call="async_iter", stream=stream),
            ],
            handler,
            routes=[
                {"event": "tool.before", "replace_input": True},
                {
                    "event": "tool.after",
                    "replace_result": True,
                    "on_error": "withhold_result",
                },
            ],
        )
        async with profile.request():
            original = {"value": "original"}

            def consume():
                chunks = self.module.sync_chunks(original)
                self.assertEqual(next(chunks), "checked")
                with self.assertRaises(StopIteration) as end:
                    next(chunks)
                self.assertIsNone(end.exception.value)

            await asyncio.to_thread(consume)
            with self.assertRaises(SourceResultWithheld):
                await anext(self.module.async_chunks(original))
        self.assertEqual(closed, ["sync", "async"])
        self.assertEqual(original, {"value": "original"})

    async def test_stream_item_and_byte_bounds_prevent_partial_release(self) -> None:
        closed: list[str] = []

        def sync_items(payload: dict) -> Iterator[str]:
            try:
                yield "one"
                yield "two"
            finally:
                closed.append("sync")

        async def async_bytes(payload: dict) -> AsyncIterator[str]:
            try:
                yield "é"  # Six UTF-8 JSON bytes including brackets and quotes.
            finally:
                closed.append("async")

        async def handler(event: str, payload: dict) -> dict:
            self.assertEqual(event, "tool.before")
            return CONTINUE

        self.module.sync_items = sync_items
        self.module.async_bytes = async_bytes
        profile = await self.activate(
            [
                tool_target(
                    "sync_items",
                    call="sync_iter",
                    stream={
                        "buffer": True,
                        "max_items": 1,
                        "max_bytes": 128,
                    },
                ),
                tool_target(
                    "async_bytes",
                    call="async_iter",
                    stream={
                        "buffer": True,
                        "max_items": 10,
                        "max_bytes": 5,
                    },
                ),
            ],
            handler,
        )
        async with profile.request():
            with self.assertRaisesRegex(
                AwError, "source_stream_result_too_large"
            ) as caught:
                await asyncio.to_thread(lambda: next(self.module.sync_items({})))
            self.assertTrue(caught.exception.tool_executed)
            with self.assertRaisesRegex(AwError, "source_stream_result_too_large"):
                await anext(self.module.async_bytes({}))
        self.assertEqual(closed, ["sync", "async"])

    async def test_stream_close_and_cancel_settle_ledger(self) -> None:
        closed = []
        waiting = asyncio.Event()
        never = asyncio.Event()

        def sync_stream(payload: dict) -> Iterator[str]:
            try:
                yield "first"
            finally:
                closed.append("sync")

        async def async_stream(payload: dict) -> AsyncIterator[str]:
            try:
                yield "first"
                waiting.set()
                await never.wait()
                yield "never"
            finally:
                closed.append("async")

        async def handler(event: str, payload: dict) -> dict:
            return CONTINUE

        self.module.sync_stream = sync_stream
        self.module.async_stream = async_stream
        seams = [
            tool_target("sync_stream", call="sync_iter"),
            tool_target("async_stream", call="async_iter"),
        ]
        for seam in seams:
            seam["hooks"]["tool.after"] = capability()
        source = await self.activate(seams, handler)
        async with source.request():
            iterator = self.module.sync_stream({})
            self.assertEqual(await asyncio.to_thread(next, iterator), "first")
            iterator.close()
            one = self.module.async_stream({})
            self.assertEqual(await anext(one), "first")
            await one.aclose()
            two = self.module.async_stream({})
            self.assertEqual(await anext(two), "first")
            pending = asyncio.create_task(anext(two))
            await asyncio.wait_for(waiting.wait(), 1)
            pending.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await pending
        await source.teardown(100)
        self.assertEqual(closed, ["sync", "async", "async"])
        self.assertEqual(
            [event for event, _ in self.tasks[0].events], ["tool.before"] * 3
        )

    async def test_epoch_isolates_calls_created_before_activation(self) -> None:
        entered, release = asyncio.Event(), asyncio.Event()
        started, finish = threading.Event(), threading.Event()
        self.addCleanup(finish.set)

        async def async_run(payload: dict) -> str:
            if payload.get("early"):
                entered.set()
                await release.wait()
            return "async"

        def sync_run(payload: dict) -> str:
            started.set()
            self.assertTrue(finish.wait(2))
            return "sync"

        async def handler(event: str, payload: dict) -> dict:
            return CONTINUE

        self.module.async_run = async_run
        self.module.sync_run = sync_run
        source = ProfileSource.probe(
            document(tool_target("async_run"), tool_target("sync_run", call="sync"))
        )
        self.addAsyncCleanup(source.teardown)
        source.install()
        source.verify()
        early = asyncio.create_task(self.module.async_run({"early": True}))
        late_await = self.module.async_run({})
        earlier_sync = asyncio.create_task(asyncio.to_thread(self.module.sync_run, {}))
        await asyncio.wait_for(entered.wait(), 1)
        self.assertTrue(await asyncio.to_thread(started.wait, 1))
        self.assertEqual(source.activation_epoch, 0)
        await source.activate(self.fake_language(source, handler, None))
        self.assertEqual(source.activation_epoch, 1)
        async with source.request():
            self.assertEqual(await self.module.async_run({}), "async")
        self.assertEqual(await late_await, "async")
        release.set()
        finish.set()
        self.assertEqual(await early, "async")
        self.assertEqual(await earlier_sync, "sync")
        self.assertEqual(
            [event for event, _ in self.tasks[0].events], ["tool.before", "tool.after"]
        )

    async def test_drain_timeout_gates_cached_wrappers_and_keeps_task_alive(
        self,
    ) -> None:
        for availability in ("required", "native_fallback"):
            with self.subTest(availability):
                entered, release = asyncio.Event(), asyncio.Event()

                async def run(
                    payload: dict,
                    started: asyncio.Event = entered,
                    finish: asyncio.Event = release,
                ) -> str:
                    if payload.get("hold"):
                        started.set()
                        await finish.wait()
                    return "native"

                async def handler(event: str, payload: dict) -> dict:
                    return CONTINUE

                self.module.run = run
                source = ProfileSource.probe(
                    document(tool_target()), availability=availability
                )
                source.install()
                source.verify()
                await source.activate(self.fake_language(source, handler, None))
                cached = self.module.run

                async def held_request(profile, run) -> str:
                    async with profile.request():
                        return await run({"hold": True})

                pending = asyncio.create_task(held_request(source, cached))
                await asyncio.wait_for(entered.wait(), 1)
                try:
                    async with source.request():
                        delayed = cached({})
                        if availability == "required":
                            with self.assertRaisesRegex(
                                AwError, "source_cleanup_unconfirmed"
                            ):
                                await source.teardown(20)
                            with self.assertRaisesRegex(AwError, "source_unavailable"):
                                cached({})
                            with self.assertRaisesRegex(AwError, "source_unavailable"):
                                await delayed
                        else:
                            await source.teardown(20)
                            self.assertEqual(await cached({}), "native")
                            self.assertEqual(await delayed, "native")
                    self.assertIs(self.module.run, cached)
                    held_task = self.tasks[-2]
                    self.assertFalse(held_task.closed)
                finally:
                    release.set()
                    self.assertEqual(await pending, "native")
                    await asyncio.wait_for(source._drain_task, 1)
                self.assertIs(self.module.run, run)
                self.assertTrue(held_task.closed)
                self.assertEqual(
                    [event for event, _ in held_task.events],
                    [
                        "tool.before",
                        "tool.after",
                    ],
                )

    async def test_rejects_negotiated_route_outside_profile_intersection(self) -> None:
        async def run(payload: dict) -> dict:
            return payload

        async def handler(event: str, payload: dict) -> dict:
            return CONTINUE

        self.module.run = self.module.other = run
        strong = tool_target()
        weak = tool_target("other")
        weak["hooks"]["tool.before"] = capability()
        profile = ProfileSource.probe(document(strong, weak))
        profile.install()
        profile.verify()
        cached = self.module.run
        with self.assertRaisesRegex(AwError, "source_route_effect_unsupported"):
            await profile.activate(
                self.fake_language(
                    profile, handler, [{"event": "tool.before", "replace_input": True}]
                )
            )
        self.addAsyncCleanup(profile.teardown)
        self.assertIs(self.module.run, cached)
        with self.assertRaisesRegex(AwError, "source_unavailable"):
            self.module.other({})
        with self.assertRaisesRegex(AwError, "source_unavailable"):
            cached({})
