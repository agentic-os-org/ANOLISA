"""Exercise direct and cooperative ProfileSource bindings with a real AW Service."""

from __future__ import annotations

import asyncio
import hashlib
import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from typing import Any
from unittest.mock import patch

from aw_sdk import AwError
from aw_sdk._profile_bootstrap import bootstrap_profile
from aw_sdk.profile_source import ProfileSource
from aw_sdk.sources import SourceDenied, SourceResultWithheld
from aw_sdk.transport import rpc
from test_profile_eval import capability, document, field, target
from test_profile_source import MODULE, load_module, tool_target
from test_source_mutation import start_aw_service, stop_service


def submit_target() -> dict:
    return target(
        MODULE,
        "prompt",
        call="async",
        hooks={"input.submit": capability("block", actions=("block",), mode="control")},
        fields={
            "prompt": field("arg.0"),
            "session_id": field("arg.1"),
            "turn_id": field("arg.2"),
        },
    )


@unittest.skipUnless("AW_TEST_BINARY" in os.environ, "requires AW_TEST_BINARY")
class ProfileServiceTests(unittest.IsolatedAsyncioTestCase):
    async def test_real_service_direct_and_cooperative_descriptor(self) -> None:
        module = load_module()
        self.addCleanup(sys.modules.pop, MODULE, None)
        native_calls = []

        async def prompt(text: str, session_id: str, turn_id: str) -> str:
            return text

        async def run(payload: dict[str, Any], session_id: str) -> str:
            command = payload["command"]
            native_calls.append(command)
            return f"result:{command}"

        module.prompt, module.run = prompt, run
        tool = tool_target(result=field("result"))
        tool["fields"]["session_id"] = field("kwarg.session_id")
        for cooperative in (False, True):
            with (
                self.subTest(cooperative=cooperative),
                tempfile.TemporaryDirectory(prefix="awprofile-", dir="/tmp") as tmp,
            ):
                root = Path(tmp)
                service, endpoint, _ = await asyncio.to_thread(start_aw_service, root)
                config = root / "aw.json"
                descriptor = root / "deployment.json"
                descriptor.write_text(
                    json.dumps(
                        {
                            "version": "aw-deployment/v1alpha1",
                            "configuration_sha256": hashlib.sha256(
                                config.read_bytes()
                            ).hexdigest(),
                            "target": "app",
                            "endpoint": str(endpoint),
                            "profile": {
                                "adapter": "sdk",
                                "version": "aw-hook-source/v1",
                                "entrypoint": "application",
                            },
                            "availability": "required",
                            "wait_for_input_submit": True,
                        }
                    )
                )
                dist = root / "profile_demo-1.4.dist-info"
                dist.mkdir()
                (dist / "METADATA").write_text("Name: profile-demo\nVersion: 1.4\n")
                profile_file = root / "profile.json"
                profile_file.write_text(json.dumps(document(submit_target(), tool)))
                for trusted_file in (config, descriptor, profile_file):
                    trusted_file.chmod(0o600)
                output = io.StringIO()
                try:
                    with (
                        redirect_stdout(output),
                        patch.object(sys, "path", [tmp, *sys.path]),
                    ):
                        profile = await bootstrap_profile(
                            str(profile_file),
                            str(config),
                            str(descriptor),
                            cooperative=cooperative,
                        )
                        try:
                            if cooperative:
                                await self.attach(config)
                            async with profile.request():
                                with self.assertRaises(SourceDenied) as denied:
                                    await module.prompt(
                                        "deny", "session-denied", "turn-1"
                                    )
                            self.assertEqual(
                                denied.exception.session_id, "session-denied"
                            )
                            self.assertEqual(denied.exception.turn_id, "turn-1")
                            async with profile.request():
                                with self.assertRaises(SourceDenied):
                                    await module.run(
                                        {"command": "rewrite-to-deny"}, "session-2"
                                    )
                            original = {"command": "raw:SECRET"}
                            async with profile.request() as first:
                                self.assertEqual(
                                    await module.run(original, "session-3"),
                                    "result:safe:[REDACTED]",
                                )
                            self.assertEqual(original, {"command": "raw:SECRET"})
                            self.assertTrue(first.task.closed)
                            async with profile.request() as second:
                                self.assertNotEqual(
                                    first.task.binding["instance_id"],
                                    second.task.binding["instance_id"],
                                )
                                with self.assertRaises(SourceResultWithheld):
                                    await module.run(
                                        {"command": "raw:FAULT"}, "session-4"
                                    )
                        finally:
                            await profile.teardown()
                        self.assertIs(module.run, run)
                        status = await rpc(
                            str(endpoint), None, {"method": "status"}, 1000
                        )
                        self.assertEqual(status["result"]["bindings"], 0)
                    self.assertEqual(output.getvalue(), "")
                except Exception as error:
                    error.add_note((root / "service.log").read_text())
                    raise
                finally:
                    await asyncio.to_thread(stop_service, service)
        self.assertEqual(native_calls, ["safe:SECRET", "safe:FAULT"] * 2)

    async def test_cancelled_owner_retains_service_scope_through_filtered_result(
        self,
    ) -> None:
        module = load_module()
        self.addCleanup(sys.modules.pop, MODULE, None)
        entered, finish = asyncio.Event(), asyncio.Event()

        async def run(payload):
            entered.set()
            await finish.wait()
            return payload["command"]

        async def prompt(text, session_id, turn_id):
            return text

        module.run, module.prompt = run, prompt
        with tempfile.TemporaryDirectory(prefix="awprofile-", dir="/tmp") as tmp:
            service, endpoint, _ = await asyncio.to_thread(start_aw_service, Path(tmp))
            try:
                with patch("aw_sdk.profile_source.version", return_value="1.5.2"):
                    profile = ProfileSource.probe(
                        document(submit_target(), tool_target(result=field("result")))
                    )
                profile.install()
                profile.verify()
                try:
                    source = await profile.activate(
                        endpoint=str(endpoint), target="app"
                    )
                    session = source.session("borrowed", timeout_ms=500)
                    started = asyncio.get_running_loop().create_future()

                    async def serve():
                        async with session, profile.request(session):
                            started.set_result(
                                asyncio.create_task(
                                    module.run({"command": "raw:SECRET"})
                                )
                            )
                            await asyncio.Event().wait()

                    owner = asyncio.create_task(serve())
                    worker = await asyncio.wait_for(started, 3)
                    try:
                        await asyncio.wait_for(entered.wait(), 3)
                        owner.cancel()
                        with self.assertRaisesRegex(AwError, "source_close_pending"):
                            await asyncio.wait_for(owner, 3)
                        status = await rpc(
                            str(endpoint), None, {"method": "status"}, 1000
                        )
                        self.assertEqual(status["result"]["bindings"], 2)
                    finally:
                        finish.set()
                        if not owner.done():
                            owner.cancel()
                        await asyncio.gather(owner, return_exceptions=True)
                        result = await asyncio.wait_for(worker, 4)
                        await session.close()
                    self.assertEqual(result, "safe:[REDACTED]")
                    status = await rpc(str(endpoint), None, {"method": "status"}, 1000)
                    self.assertEqual(status["result"]["bindings"], 1)
                finally:
                    await profile.teardown()
            finally:
                await asyncio.to_thread(stop_service, service)

    async def test_borrowed_scope_lives_until_instrumented_stream_cleanup_finishes(
        self,
    ) -> None:
        module = load_module()
        self.addCleanup(sys.modules.pop, MODULE, None)
        entered, release = asyncio.Event(), asyncio.Event()
        reading = asyncio.Event()
        deadlines = []
        results = []

        async def run(payload):
            entered.set()
            await release.wait()
            return payload["command"]

        async def prompt(text, session_id, turn_id):
            async with asyncio.timeout(None) as deadline:
                deadlines.append(deadline)
                try:
                    yield text
                    reading.set()
                    await asyncio.Event().wait()
                finally:
                    results.append(await module.run({"command": "raw:SECRET"}))

        module.run, module.prompt = run, prompt
        stream_target = submit_target()
        stream_target["call"] = "async_iter"
        with tempfile.TemporaryDirectory(prefix="awprofile-", dir="/tmp") as tmp:
            service, endpoint, _ = await asyncio.to_thread(start_aw_service, Path(tmp))
            try:
                with patch("aw_sdk.profile_source.version", return_value="1.5.2"):
                    profile = ProfileSource.probe(
                        document(stream_target, tool_target(result=field("result")))
                    )
                profile.install()
                profile.verify()
                try:
                    source = await profile.activate(
                        endpoint=str(endpoint), target="app"
                    )
                    for action in ("close", "cancel", "deadline"):
                        with self.subTest(action=action):
                            reading.clear()
                            entered.clear()
                            release.clear()
                            results.clear()
                            session = source.session("A", timeout_ms=500)
                            with self.assertRaisesRegex(
                                AwError, "source_close_pending"
                            ):
                                async with profile.request(session):
                                    stream = module.prompt("ok", "A", "turn-a")
                                    self.assertEqual(await anext(stream), "ok")
                                    if action != "close":
                                        reader = asyncio.create_task(anext(stream))
                                        await asyncio.wait_for(reading.wait(), 3)
                            with self.assertRaisesRegex(
                                AwError, "source_close_pending"
                            ):
                                await session.close(timeout_ms=100)
                            if action == "deadline":
                                deadlines[-1].reschedule(
                                    asyncio.get_running_loop().time()
                                )
                            elif action == "cancel":
                                reader.cancel()
                            else:
                                reader = asyncio.create_task(stream.aclose())
                            try:
                                await asyncio.wait_for(entered.wait(), 3)
                                status = await rpc(
                                    str(endpoint), None, {"method": "status"}, 1000
                                )
                                self.assertEqual(status["result"]["bindings"], 2)
                                with self.assertRaisesRegex(
                                    AwError, "source_session_closed"
                                ):
                                    await session.submit("new work")
                            finally:
                                release.set()
                                if action == "close":
                                    await reader
                                else:
                                    expected = (
                                        TimeoutError
                                        if action == "deadline"
                                        else asyncio.CancelledError
                                    )
                                    with self.assertRaises(expected):
                                        await reader
                                await session.close()
                            await stream.aclose()
                            self.assertEqual(results, ["safe:[REDACTED]"])
                            status = await rpc(
                                str(endpoint), None, {"method": "status"}, 1000
                            )
                            self.assertEqual(status["result"]["bindings"], 1)
                finally:
                    await profile.teardown()
            finally:
                await asyncio.to_thread(stop_service, service)

    async def attach(self, config: Path) -> None:
        process = await asyncio.create_subprocess_exec(
            os.environ["AW_TEST_BINARY"],
            "connect",
            "--config",
            str(config),
            "--sdk-pid",
            str(os.getpid()),
            "--timeout-ms",
            "5000",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        try:
            stdout, stderr = await asyncio.wait_for(process.communicate(), 8)
            self.assertEqual(process.returncode, 0, (stdout, stderr))
        finally:
            if process.returncode is None:
                process.kill()
                await process.wait()
