"""Exercise the pinned Gateway's native plugin loader and deferred turn seams."""

import asyncio
from contextlib import contextmanager, ExitStack
import importlib.util
import os
from pathlib import Path
import shutil
import sys
from threading import Event, RLock
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from native_support import NativeFixture


class GatewayAttachTests(NativeFixture):
    def setUp(self):
        super().setUp()
        from hermes_logging import _reset_queued_handlers

        self.addCleanup(_reset_queued_handlers)
        spec = importlib.util.spec_from_file_location(
            "aw_hermes_attach_fixture", self.plugin_path / "aw_gateway_attach.py")
        self.attach = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.attach)

    def test_native_parser_and_bad_options_leave_tools_guarded(self):
        from gateway import run_runtime, session_finite, session_runtime_workers

        for module, name in ((run_runtime, "publish_gateway_runtime_ready"),
                             (run_runtime, "settle_gateway_runtime"),
                             (session_finite, "execute_finite_admission"),
                             (session_runtime_workers, "start_turn_worker")):
            self.addCleanup(setattr, module, name, getattr(module, name))
        shutil.copytree(self.plugin_path, self.home / "plugins/aw-native-hooks")
        (self.home / "config.yaml").write_text("plugins: {enabled: [aw-native-hooks]}\n")
        manager = self.manager()
        with (patch.object(sys, "argv", ["hermes", "gateway", "--accept-hooks", "run"]),
              patch.dict(os.environ, {"AW_HERMES_GATEWAY_CONFIG": str(self.root / "aw.json"),
                                      "AW_HERMES_GATEWAY_WAIT_MS": "invalid"})):
            manager.discover_and_load()
        callback, = manager._hooks["pre_tool_call"]
        gateway = sys.modules[callback.__module__]
        attachment = gateway._attachment
        self.assertIsNotNone(attachment)
        with self.assertLogs(level="ERROR"):
            attachment.prepare()
        self.assertTrue(attachment.settled.is_set())
        self.assertIsNone(attachment.launch)
        self.assertEqual(callback(session_id="missing")["action"], "block")
        gateway_run = sys.modules[gateway.__package__]._gateway_run
        for argv in (["--accept-hooks", "gateway", "run"], ["gateway", "run", "--accept-hooks"],
                     ["gateway", "--accept-hooks"], ["gateway"]):
            self.assertTrue(gateway_run(argv), argv)
        self.assertFalse(gateway_run(["gateway", "--accept-hooks", "status"]))
        self.assertFalse(gateway_run(["chat", "--query", "gateway run"]))

    def test_first_tool_uses_workspace_binding_and_releases_it(self):
        workspace = self.root / "workspace"
        workspace.mkdir()
        owner = self.attach.Attachment(str(self.root / "aw.json"),
                                       SimpleNamespace(session_cwd=lambda: str(workspace)), None, None)
        owner.settled.set()
        root = self.root / "binding.json"
        owner.launch = "root-launch", {"cwd": str(self.root), "binding": str(root)}
        owner.workspace = Mock(return_value=b"receipt")
        turn = self.attach.Turn(owner, "session", "request", "admission")
        binding = turn.resolve(lambda path, launch, *_, workspace, scope: SimpleNamespace(
            path=path, launch=launch, workspace=workspace, scope=Path(scope), check=lambda: None))
        self.assertEqual(binding.path, "root-launch")
        self.assertEqual(binding.workspace, workspace)
        self.assertEqual(binding.scope.read_bytes(), b"receipt")
        self.assertEqual(owner.workspace.call_args_list[0].args[3], workspace)
        turn.finish()
        self.assertFalse(binding.scope.exists())
        self.assertEqual([call.args[:2] for call in owner.workspace.call_args_list],
                         [("open", str(root)), ("activate", str(binding.scope)),
                          ("close", str(binding.scope))])

    def test_managed_rejection_passes_through_native_claim_settlement(self):
        from gateway import session_managed_worker
        from gateway.session_authority import SessionAuthority

        ref = SimpleNamespace(session_id="session")
        live = SimpleNamespace(review=None, event_stream=SimpleNamespace(lock=RLock()))
        authority = SimpleNamespace(executing={ref.session_id: "admission"})
        owner = self.attach.Attachment(str(self.root / "aw.json"), None, None, None)
        with (patch.object(session_managed_worker, "managed_policy", return_value=object()),
              patch("gateway.session_open.settled", new_callable=AsyncMock),
              self.install(owner)):
            result = asyncio.run(SessionAuthority._execute_claimed(
                authority, ref, live, {"admission_id": "admission"}))
        self.assertEqual(result[1], "failed")
        self.assertNotIn(ref.session_id, authority.executing)

    @contextmanager
    def install(self, owner, turns=None):
        from gateway import run_runtime, session_finite, session_runtime_workers

        with ExitStack() as stack:
            for module, name in ((run_runtime, "publish_gateway_runtime_ready"),
                                 (run_runtime, "settle_gateway_runtime"),
                                 (session_finite, "execute_finite_admission"),
                                 (session_runtime_workers, "start_turn_worker")):
                stack.enter_context(patch.object(module, name, getattr(module, name)))
            turns = {} if turns is None else turns
            owner.install(lambda: None, lambda turn: turns.update({turn.session: turn}),
                          lambda turn: turns.pop(turn.session), lambda: not turns)
            yield

    def test_cancellation_releases_only_after_native_worker_exits(self):
        from gateway import session_finite, session_runtime_workers

        turns = {}
        worker = SimpleNamespace(worker_done=Event())
        started = asyncio.Event()
        owner = self.attach.Attachment(str(self.root / "aw.json"),
                                       SimpleNamespace(session_cwd=lambda: str(self.root)), None, None)
        owner.launch = "launch", {"cwd": str(self.root), "binding": str(self.root / "binding.json")}
        owner.workspace = Mock(return_value=b"receipt")
        owner.settled.set()

        async def execute(authority, ref, row):
            session_runtime_workers.start_turn_worker(None, worker, None, None)
            turns[ref.session_id].resolve(lambda *_, **_kwargs: SimpleNamespace(check=lambda: None))
            started.set()
            await asyncio.Event().wait()

        async def run():
            task = asyncio.create_task(session_finite.execute_finite_admission(
                SimpleNamespace(), SimpleNamespace(session_id="session"), {"admission_id":"admission"}))
            await started.wait()
            task.cancel()
            await asyncio.sleep(0)
            self.assertIn("session", turns)
            self.assertEqual(owner.workspace.call_count, 2)
            worker.worker_done.set()
            with self.assertRaises(asyncio.CancelledError):
                await task
            self.assertEqual(owner.workspace.call_args.args[0], "close")
            self.assertEqual(turns, {})

        with (patch.object(session_finite, "execute_finite_admission", execute),
              patch.object(session_runtime_workers, "start_turn_worker"),
              patch("gateway.session_managed_worker.managed_policy", return_value=None),
              self.install(owner, turns)):
            asyncio.run(run())

    def test_native_shutdown_releases_ready_binding(self):
        from gateway import run_runtime

        owner = self.attach.Attachment(str(self.root / "aw.json"), None, None, None)
        owner.launch = "launch", {"binding": "root-binding"}
        owner.release = Mock()
        owner.settled.set()

        async def settled(*_):
            owner.release.assert_not_called()

        with (patch.object(run_runtime, "settle_gateway_runtime", settled), self.install(owner)):
            asyncio.run(run_runtime.settle_gateway_runtime(object()))
        owner.release.assert_called_once_with("root-binding")
