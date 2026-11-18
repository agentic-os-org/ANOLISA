"""Exercise the packaged Claude worker entry with actual offline CLI output."""

from copy import deepcopy
from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import ModuleType
import unittest
from unittest.mock import patch

from aw_sdk import AwError
from test_deployment import BINARY, FIXTURE


class ClaudeDeployment(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="aw-claude-descriptor-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.config = self.root / "config.json"
        self.descriptor = self.root / "descriptor.json"
        # Cooperative workers must lead their own session; emulate setsid here.
        for name in ("getpgrp", "getsid"):
            patcher = patch(f"aw_sdk.claude.os.{name}", return_value=os.getpid())
            patcher.start()
            self.addCleanup(patcher.stop)

    def write(self, input_enabled: bool) -> None:
        document = deepcopy(FIXTURE["configuration"])
        document["spec"]["agents"]["app"]["adapter"] = "claude-sdk"
        document["spec"]["daemon"]["state_dir"] = str(self.root / "state")
        document["spec"]["events"]["input.submit"] = {
            "enabled": input_enabled, "required": input_enabled, "steps": []
        }
        self.config.write_text(json.dumps(document))
        self.config.chmod(0o600)

    def compile(self) -> dict:
        result = subprocess.run(
            [BINARY, "config", "descriptor", "--config", str(self.config),
             "--agent", "app", "--profile", "claude-resident-hooks"],
            check=True, capture_output=True, timeout=10,
        )
        self.descriptor.write_bytes(result.stdout)
        self.descriptor.chmod(0o600)
        return json.loads(result.stdout)

    def test_descriptor_and_parser_select_the_same_required_worker(self) -> None:
        from aw_sdk.claude import ResidentClaude
        from aw_sdk._native.claude.aw_sdk_scope import PROFILE

        for enabled in (False, True):
            self.write(enabled)
            descriptor = self.compile()
            self.assertEqual(descriptor["profile"], PROFILE)
            self.assertEqual(descriptor["availability"], "required")
            with patch(
                "aw_sdk.configuration.subprocess.Popen",
                side_effect=AssertionError("cold-start subprocess"),
            ):
                worker = ResidentClaude.from_config(
                    str(self.config), descriptor_file=str(self.descriptor), executable="/missing/aw"
                )
            parsed = ResidentClaude.from_config(str(self.config), executable=BINARY)
            for candidate in (worker, parsed):
                instance = candidate.instance
                self.assertEqual(instance.target, "app")
                self.assertEqual(instance.endpoint, str(self.root / "state/aw.sock"))
                self.assertEqual(instance.availability, "required")
                self.assertEqual(
                    {key: instance.capabilities[key] for key in PROFILE}, PROFILE
                )
                self.assertEqual("input.submit" in instance.capabilities["events"], enabled)
                self.assertEqual(
                    instance.capabilities["events"]["tool.before"],
                    ["observe", "block", "replace_input"],
                )

    def test_descriptor_profile_and_configuration_must_match(self) -> None:
        from aw_sdk.claude import ResidentClaude

        self.write(True)
        descriptor = self.compile()
        descriptor["profile"]["version"] = "0.2.88"
        self.descriptor.write_text(json.dumps(descriptor))
        with self.assertRaisesRegex(AwError, "sdk_descriptor_profile_mismatch"):
            ResidentClaude.from_config(str(self.config), descriptor_file=str(self.descriptor))
        self.compile()
        self.config.write_text(self.config.read_text() + "\n")
        with self.assertRaisesRegex(AwError, "sdk_descriptor_stale"):
            ResidentClaude.from_config(str(self.config), descriptor_file=str(self.descriptor))

    def test_worker_rejects_shared_process_group_without_setsid(self) -> None:
        from aw_sdk.claude import ResidentClaude

        for name in ("getpgrp", "getsid"):
            with patch(f"aw_sdk.claude.os.{name}", return_value=os.getpid() + 1):
                with self.assertRaisesRegex(
                    RuntimeError, "Cooperative Claude requires an isolated process started with setsid"
                ):
                    ResidentClaude(endpoint=str(self.root / "state/aw.sock"), target="app")


class ClaudeWorkerLifecycleGuards(unittest.IsolatedAsyncioTestCase):
    async def test_closing_fences_late_open_and_unconnected_stop_skips_killpg(self) -> None:
        @dataclass
        class Options:
            cwd: str | None = None
            env: dict = field(default_factory=dict)
            mcp_servers: dict = field(default_factory=dict)
            hooks: dict | None = None
            can_use_tool: object = None
            permission_prompt_tool_name: str | None = None
            setting_sources: list | None = None

        class ClientStub:
            def __init__(self, options: Options) -> None:
                self.options = options
                self._query = None

            async def interrupt(self) -> None:
                raise RuntimeError("Not connected. Call connect() first.")

        anyio_stub = ModuleType("anyio")
        sdk_stub = ModuleType("claude_agent_sdk")
        sdk_stub.__dict__.update(
            ClaudeAgentOptions=Options,
            ClaudeSDKClient=ClientStub,
            HookMatcher=dict,
            PermissionResultDeny=dict,
            ResultMessage=type("ResultMessage", (), {}),
            SdkMcpTool=object,
            Transport=object,
            create_sdk_mcp_server=lambda *args, **kwargs: {},
        )
        internal_stub = ModuleType("claude_agent_sdk._internal")
        transport_pkg = ModuleType("claude_agent_sdk._internal.transport")
        subprocess_stub = ModuleType("claude_agent_sdk._internal.transport.subprocess_cli")
        subprocess_stub.SubprocessCLITransport = object
        pid = os.getpid()
        with (
            patch.dict(
                sys.modules,
                {
                    "anyio": anyio_stub,
                    "httpx": ModuleType("httpx"),
                    "claude_agent_sdk": sdk_stub,
                    "claude_agent_sdk._internal": internal_stub,
                    "claude_agent_sdk._internal.transport": transport_pkg,
                    "claude_agent_sdk._internal.transport.subprocess_cli": subprocess_stub,
                },
            ),
            patch("aw_sdk.claude.os.getpgrp", return_value=pid),
            patch("aw_sdk.claude.os.getsid", return_value=pid),
            patch("os.getpgrp", return_value=pid),
            patch("os.getsid", return_value=pid),
        ):
            from aw_sdk.claude import ResidentClaude
            import aw_sdk._native.claude.aw_resident as resident

            with tempfile.TemporaryDirectory(prefix="aw-claude-guard-") as temporary:
                worker = ResidentClaude(endpoint=f"{temporary}/aw.sock", target="app")
                client = worker.create_client(Options(cwd=temporary))
                # Before super().connect(None) binds _query, bridge failure must not SIGKILL.
                with patch.object(resident, "fail_stop", side_effect=AssertionError("killpg")):
                    client._aw_bridge.scope_failed(RuntimeError("lease expired"))
                    await client._aw_bridge.stop_future
                self.assertTrue(client._aw_bridge.stopped)

                # Active native work whose interrupt fails still escalates to fail_stop.
                killed = []
                client._aw_task_started = True
                client._aw_query_finished = False
                client._aw_bridge.stop_future = None
                with patch.object(resident, "fail_stop", side_effect=killed.append):
                    with self.assertRaisesRegex(RuntimeError, "Not connected"):
                        await client._aw_bridge.stop_task()
                self.assertEqual(killed, [pid])

                # A client created before close() is still fenced before opening an Admission.
                late = worker.create_client(Options(cwd=temporary))
                await worker.close()
                with self.assertRaisesRegex(RuntimeError, "Claude worker is closed"):
                    await late._aw_bridge.scope.open(temporary, {})


if __name__ == "__main__":
    unittest.main()
