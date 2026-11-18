"""Shared availability scenarios against a real AW Service."""

import asyncio
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from aw_sdk import AwError, Instance
from aw_sdk.transport import rpc

BINARY = os.environ["AW_TEST_BINARY"]
FIXTURE = json.loads(Path(os.environ["AW_CONTRACT_FIXTURE"]).read_text())


class Lifecycle(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="aw-python-sdk-")
        self.root = Path(self.temporary.name)
        state = self.root / "state"
        state.mkdir(mode=0o700)
        self.endpoint = str(state / "aw.sock")
        self.config = self.root / "config.json"
        self.config.write_text(
            json.dumps(
                {
                    "apiVersion": "aw/v1alpha1",
                    "kind": "AWConfiguration",
                    "metadata": {"name": "sdk"},
                    "spec": {
                        "daemon": {
                            "startup": "on_demand",
                            "endpoint": "auto",
                            "state_dir": str(state),
                        },
                        "execution": {"guarantee": "native_hook", "default_event_budget_ms": 1000},
                        "audit": {"enabled": True, "payload": "metadata_only"},
                        "agents": {"app": {"adapter": "qoder-sdk", "argv": ["/bin/true"]}},
                        "providers": {},
                        "events": {
                            name: {"enabled": True, "required": False, "steps": []}
                            for name in ["tool.before", "tool.after"]
                        },
                    },
                }
            )
        )
        self.service = None
        self.instances = []

    def instance(self, **options: object) -> Instance:
        settings = dict(
            endpoint=self.endpoint,
            target="app",
            availability="native_fallback",
            install=lambda _: True,
            timeout_ms=500,
            capabilities={
                "adapter": "qoder-sdk",
                "version": "public-hooks/v1",
                "entrypoint": "sdk-worker",
                "events": {"tool.before": ["observe"], "tool.after": ["observe"]},
            },
        )
        settings.update(options)
        instance = Instance(**settings)
        self.instances.append(instance)
        return instance

    async def start(self) -> None:
        self.service = subprocess.Popen(
            [
                BINARY,
                "service",
                "serve",
                "--config",
                str(self.config),
                "--state-dir",
                str(self.root / "state"),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        async with asyncio.timeout(10):
            while True:
                try:
                    await rpc(self.endpoint, None, {"method": "status"}, 500)
                    return
                except AwError as error:
                    if not error.unavailable:
                        raise
                await asyncio.sleep(0.01)

    def stop(self) -> None:
        if self.service and self.service.poll() is None:
            self.service.terminate()
            try:
                self.service.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.service.kill()
                self.service.wait(timeout=5)

    async def asyncTearDown(self) -> None:
        try:
            for instance in self.instances:
                await instance.close()
        finally:
            self.stop()
            self.temporary.cleanup()

    async def test_concurrent_task_and_instance_close(self) -> None:
        await self.start()
        instance = self.instance(availability="required")
        await instance.connect()
        task = await instance.task()
        async with asyncio.timeout(5):
            results = await asyncio.gather(
                task.close(),
                task.__aexit__(None, None, None),
                instance.close(),
                return_exceptions=True,
            )
        self.assertEqual(results, [None, None, None])
        self.assertTrue(task.closed)
        self.assertNotIn(task, instance.tasks)
        await task.close()

    async def test_absence_loss_recovery_and_independent_instances(self) -> None:
        instance = self.instance()
        self.assertFalse(await instance.connect())
        absent = await instance.task()
        self.assertTrue(absent.fallback)
        await self.start()
        self.assertTrue(await instance.connect())
        task = await instance.task()
        other = self.instance()
        self.assertTrue(await other.connect())
        second = await other.task()
        await second.close()
        await other.close()
        hook = task.routes["hooks"][0]
        payload = dict(FIXTURE["native_before"], cwd=os.getcwd())
        self.assertEqual(await task.invoke_native(hook, payload), (0, b"", b""))
        self.stop()
        self.assertEqual(await task.invoke_native(hook, payload), (0, b"", b""))
        self.assertTrue(task.fallback)
        async with asyncio.timeout(0.1):
            for _ in range(10):
                await task.invoke_native(hook, payload)
        await self.start()
        self.assertTrue(await instance.connect())
        self.assertTrue(task.fallback)
        self.assertTrue(absent.fallback)
        fresh = await instance.task()
        self.assertFalse(fresh.fallback)
        await fresh.close()

    async def test_required_background_recovery_keeps_old_tasks_failed(self) -> None:
        instance = self.instance(availability="required", reconnect_ms=100)
        instance.start()
        await self.until(lambda: instance.last_error is not None)
        await self.start()
        await self.until(lambda: instance.state == "ready")
        active, sibling = await instance.task(), await instance.task()
        original = instance.connection["identity"]["generation"]
        payload = dict(FIXTURE["native_before"], cwd=os.getcwd())
        self.stop()
        with self.assertRaises(AwError) as caught:
            await active.invoke_native(active.routes["hooks"][0], payload)
        self.assertTrue(caught.exception.unavailable)
        self.assertEqual(instance.state, "unavailable")
        self.assertFalse(active.fallback)
        self.assertFalse(sibling.fallback)
        with self.assertRaisesRegex(AwError, "instance_not_ready"):
            await instance.task()
        await asyncio.sleep(0.25)
        self.assertFalse(instance._loop.done())
        await self.start()
        await self.until(lambda: instance.state == "ready")
        self.assertNotEqual(instance.connection["identity"]["generation"], original)
        for task in (active, sibling):
            with self.assertRaises(AwError) as caught:
                await task.invoke_native(task.routes["hooks"][0], payload)
            self.assertTrue(caught.exception.unavailable)
            self.assertFalse(task.fallback)
            await task.close()
        fresh = await instance.task()
        self.assertEqual(
            await fresh.invoke_native(fresh.routes["hooks"][0], payload), (0, b"", b"")
        )
        await fresh.close()

    async def test_required_recovery_on_same_service_drains_old_registration(self) -> None:
        await self.start()
        instance = self.instance(availability="required")
        await instance.connect()
        task = await instance.task()
        original = instance.connection
        socket, moved = Path(self.endpoint), Path(self.endpoint + ".held")
        socket.rename(moved)
        try:
            with self.assertRaisesRegex(AwError, "ENOENT"):
                await task.invoke_native(
                    task.routes["hooks"][0], dict(FIXTURE["native_before"], cwd=os.getcwd())
                )
        finally:
            moved.rename(socket)
        await instance.connect()
        self.assertEqual(instance.connection["identity"], original["identity"])
        self.assertNotEqual(instance.connection["handle"], original["handle"])
        await task.close()
        fresh = await instance.task()
        self.assertEqual(
            await fresh.invoke_native(
                fresh.routes["hooks"][0], dict(FIXTURE["native_before"], cwd=os.getcwd())
            ),
            (0, b"", b""),
        )
        await fresh.close()

    async def test_fallback_close_and_detach_join_before_root_release(self) -> None:
        await self.start()
        instance = self.instance()
        await instance.connect()
        task = await instance.task()
        instance.lost(task.connection, AwError("ECONNRESET", unavailable=True))
        root_released, finish_detach = asyncio.Event(), asyncio.Event()
        original = instance._registration

        async def paused(identity, command):
            result = await original(identity, command)
            root_released.set()
            await finish_detach.wait()
            return result

        instance._registration = paused
        detaching = asyncio.create_task(instance.detach_control())
        try:
            async with asyncio.timeout(5):
                await root_released.wait()
                await task.close()
                self.assertTrue(task.closed)
                self.assertNotIn(task, instance.tasks)
        finally:
            finish_detach.set()
            await detaching
        await instance.close()

    async def test_fallback_detach_releases_retained_tasks(self) -> None:
        await self.start()
        instance = self.instance()
        await instance.connect()
        task = await instance.task()
        socket, moved = Path(self.endpoint), Path(self.endpoint + ".held")
        socket.rename(moved)
        try:
            self.assertEqual(
                await task.invoke_native(
                    task.routes["hooks"][0], dict(FIXTURE["native_before"], cwd=os.getcwd())
                ),
                (0, b"", b""),
            )
        finally:
            moved.rename(socket)
        self.assertTrue(task.fallback)
        await instance.detach_control()
        await task.close()
        self.assertTrue(task.closed)
        self.assertNotIn(task, instance.tasks)
        await instance.close()
        self.assertEqual(
            (await rpc(self.endpoint, None, {"method": "status"}, 500))["result"]["bindings"], 0
        )

    async def cli(self, action: str, *args: str, configuration: bool = False) -> dict:
        selection = ("--config", str(self.config)) if configuration else ("--socket", self.endpoint)
        process = await asyncio.create_subprocess_exec(
            BINARY,
            "instance",
            action,
            *selection,
            *args,
            env=dict(os.environ, XDG_RUNTIME_DIR=str(self.root)),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        async with asyncio.timeout(10):
            stdout, stderr = await process.communicate()
        if process.returncode:
            raise RuntimeError(stdout.decode() or stderr.decode())
        return json.loads(stdout)

    async def test_task_context_is_private_and_copied(self) -> None:
        await self.start()
        instance = self.instance(environment={"ROOT_ONLY": "root"})
        self.assertTrue(await instance.connect())
        workspace = self.root / "workspace"
        workspace.mkdir()
        environment = {"TASK_ONLY": "clean"}
        calls = []

        async def capture(endpoint, identity, operation, timeout_ms):
            if operation["method"] == "open_scope":
                calls.append(operation)
            return await rpc(endpoint, identity, operation, timeout_ms)

        with patch("aw_sdk.client.rpc", side_effect=capture):
            task = await instance.task(cwd=str(workspace), environment=environment)
        environment["TASK_ONLY"] = "changed"
        self.assertEqual(calls[0]["cwd"], str(workspace))
        self.assertEqual(calls[0]["environment"], {"TASK_ONLY": "clean"})
        self.assertEqual(instance.environment, {"ROOT_ONLY": "root"})
        await task.close()

    async def test_config_pid_attach_refreshes_restarted_service(self) -> None:
        instance = self.instance(reconnect_ms=100)
        instance.listen()
        for _ in range(2):
            await self.start()
            await self.cli("attach", "--sdk-pid", str(os.getpid()), configuration=True)
            self.assertEqual(instance.state, "ready")
            self.stop()
            await self.until(lambda: instance.state != "ready")

    async def test_config_pid_attach_rejects_edited_configuration(self) -> None:
        await self.start()
        instance = self.instance(reconnect_ms=100)
        instance.listen()
        await self.cli("attach", "--sdk-pid", str(os.getpid()), configuration=True)
        identity = instance.connection["identity"]
        original = self.config.read_bytes()
        edited = json.loads(original)
        edited["spec"]["events"]["tool.before"]["enabled"] = False
        for contents in (json.dumps(edited).encode(), b"{invalid json", original + b"\n"):
            with self.subTest(contents=contents):
                self.config.write_bytes(contents)
                await self.cli(
                    "detach", "--sdk-instance", instance.application_id, configuration=True
                )
                self.assertEqual(instance.state, "detached")
                with self.assertRaisesRegex(RuntimeError, "configuration.*does not match"):
                    await self.cli("attach", "--sdk-pid", str(os.getpid()), configuration=True)
                self.assertEqual(instance.state, "detached")
                self.assertEqual(
                    (await rpc(self.endpoint, None, {"method": "status"}, 500))["result"][
                        "bindings"
                    ],
                    0,
                )
                self.config.write_bytes(original)
                await self.cli("attach", "--sdk-pid", str(os.getpid()), configuration=True)
                self.assertEqual(instance.connection["identity"], identity)
                self.assertEqual(instance.state, "ready")

    async def test_pid_selection_rejects_multiple_instances(self) -> None:
        await self.start()
        first, second = self.instance(reconnect_ms=100), self.instance(reconnect_ms=100)
        first.listen()
        second.listen()
        await self.until(lambda: first.application_id and second.application_id)
        with self.assertRaisesRegex(RuntimeError, "sdk_control_ambiguous_process"):
            await self.cli("attach", "--sdk-pid", str(os.getpid()))
        await second.stop()
        result = await self.cli("attach", "--sdk-pid", str(os.getpid()))
        self.assertEqual(result["result"]["state"], "completed")
        self.assertEqual(first.state, "ready")

    async def until(self, check) -> None:
        async with asyncio.timeout(8):
            while not check():
                await asyncio.sleep(0.01)

    async def test_cooperative_selection_busy_and_restart(self) -> None:
        first, second = self.instance(reconnect_ms=100), self.instance(reconnect_ms=100)
        first.listen()
        second.listen()
        absent = await first.task()
        await self.start()
        await self.until(lambda: first.application_id and second.application_id)
        self.assertEqual(len((await self.cli("list"))["applications"]), 2)
        with self.assertRaisesRegex(AwError, "already_listening"):
            first.listen()
        with self.assertRaisesRegex(AwError, "already_listening"):
            first.start()
        await second.stop()
        self.assertEqual(len((await self.cli("list"))["applications"]), 1)
        second.listen()
        async with asyncio.timeout(8):
            while len((await self.cli("list"))["applications"]) != 2:
                await asyncio.sleep(0.01)
        original_id = first.application_id
        await self.cli("attach", "--sdk-instance", first.application_id)
        await self.until(lambda: first.state == "ready")
        self.assertEqual(second.state, "detached")
        self.assertTrue(absent.fallback)
        task = await first.task()
        with self.assertRaisesRegex(RuntimeError, "sdk_control_busy"):
            await self.cli("detach", "--sdk-instance", first.application_id)
        self.assertEqual(first.state, "ready")
        self.assertFalse(task.fallback)
        await task.close()
        await self.cli("detach", "--sdk-instance", first.application_id)
        await self.until(lambda: first.state == "detached")
        await self.cli("attach", "--sdk-instance", second.application_id)
        await self.until(lambda: second.state == "ready")
        retained = await second.task()
        self.stop()
        await self.until(lambda: retained.fallback)
        await self.start()
        await self.until(lambda: second.state == "ready" and first.application_id != original_id)
        self.assertTrue(retained.fallback)
        self.assertEqual(first.state, "detached")
        with self.assertRaises(RuntimeError):
            await self.cli("attach", "--sdk-instance", original_id)
        await first.close()
        await second.close()
        self.assertEqual((await self.cli("list"))["applications"], [])

    async def test_expired_ready_never_publishes_connection(self) -> None:
        entered, release = asyncio.Event(), asyncio.Event()

        async def install(_):
            entered.set()
            await release.wait()
            return True

        await self.start()
        instance = self.instance(install=install, reconnect_ms=100)
        instance.listen()
        await self.until(lambda: instance.application_id)
        request = asyncio.create_task(
            self.cli("attach", "--sdk-instance", instance.application_id, "--timeout-ms", "150")
        )
        try:
            async with asyncio.timeout(5):
                await entered.wait()
            with self.assertRaises(RuntimeError):
                await request
            # CLI timeout can precede the Service intent deadline.
            async with asyncio.timeout(5):
                while True:
                    applications = (await self.cli("list"))["applications"]
                    current = next(
                        app
                        for app in applications
                        if app["application_id"] == instance.application_id
                    )
                    if current["request"]["state"] == "expired":
                        break
                    await asyncio.sleep(0.01)
        finally:
            release.set()
        await self.until(
            lambda: instance.state == "detached"
            and instance.controller.active is None
            and instance.pending is None
        )
        self.assertIsNone(instance.connection)
        self.assertTrue((await instance.task()).fallback)
        status = await rpc(self.endpoint, None, {"method": "status"}, 500)
        self.assertEqual(status["result"]["instances"], [])
        await self.cli("attach", "--sdk-instance", instance.application_id)
        await self.until(lambda: instance.state == "ready")

    async def test_explicit_error_is_not_fallback(self) -> None:
        await self.start()
        instance = self.instance(target="missing")
        with self.assertRaises(AwError) as raised:
            await instance.connect()
        self.assertFalse(raised.exception.unavailable)
        with self.assertRaises(AwError):
            await instance.task()

    async def test_ready_confirmation_and_mode(self) -> None:
        await self.start()
        instance = self.instance(install=lambda _: False)
        with self.assertRaisesRegex(AwError, "ready_not_confirmed"):
            await instance.connect()
        with self.assertRaises(AttributeError):
            instance.availability = "required"


if __name__ == "__main__":
    unittest.main()
