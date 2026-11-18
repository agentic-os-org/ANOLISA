"""Real QwenPaw App with an installed SDK plugin and native observer."""

import json
import os
from pathlib import Path
import sys
import tempfile
import subprocess
import time
import unittest

REPO = Path(__file__).resolve().parents[4]
AW = Path(os.environ["AW_TEST_BINARY"]).resolve()
QWEN = Path(os.environ["AW_QWENPAW_BINARY"]).resolve()
sys.path.insert(0, str(REPO / "src/aw/crates/aw-cli/tests/fixtures/sec_core"))
sys.path.insert(0, str(REPO / "src/aw/crates/aw-cli/tests/fixtures/qwenpaw"))
from agents import configure_qwenpaw
from runtime import Processes, await_condition
from native_app_config import ready, stage_user_configuration, turn


class NativeApp(unittest.TestCase):
    def test_app(self) -> None:
        evidence = Path(os.environ["AW_TEST_OUTPUT"])
        evidence.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="aw-sdk-app-") as temporary:
            root = Path(temporary)
            for name in ["work", "home", "state"]:
                (root / name).mkdir(mode=0o700)
            env = {
                k: v
                for k, v in os.environ.items()
                if not k.startswith(("AW_", "QWENPAW_", "QODER_", "QODERCN_", "AGENT_SEC_"))
            }
            env["PYTHONPATH"] = os.environ["AW_SDK_PYTHONPATH"]
            processes = Processes(root, evidence, env)
            try:
                argv, ports = configure_qwenpaw(root, QWEN, env, processes)
                original = stage_user_configuration(root)
                observer = root / "home/plugins/native-observer/plugin.py"
                code = observer.read_text().replace(
                    "from pathlib import Path", "from pathlib import Path\nimport asyncio"
                )
                code = code.replace(
                    "        async for item in next_handler(**input_kwargs):",
                    """        if 'during.txt' in input_kwargs['tool_call'].input:
            Path('loss-waiting').touch()
            async with asyncio.timeout(10):
                while not Path('loss-release').exists():
                    await asyncio.sleep(0.01)
        async for item in next_handler(**input_kwargs):""",
                )
                observer.write_text(code)
                original[observer] = observer.read_bytes()
                plugin = root / "home/plugins/aw-sdk"
                plugin.mkdir()
                (plugin / "plugin.json").write_text(
                    json.dumps(
                        {
                            "id": "aw-sdk",
                            "name": "AW SDK",
                            "version": "0.1.1",
                            "type": "general",
                            "entry": {"backend": "plugin.py"},
                            "dependencies": [],
                        }
                    )
                )
                (plugin / "plugin.py").write_text("""from aw_sdk import Instance
from aw_sdk.qwenpaw import QwenPawPlugin
instance=Instance(endpoint=%r,target='app',cwd=%r,availability='native_fallback',timeout_ms=1000,
 capabilities={'adapter':'qwenpaw','version':'public-middleware/v1','entrypoint':'app','events':{'tool.before':['observe','block'],'tool.after':['observe']}},install=lambda routes:plugin.ready(routes))
plugin=QwenPawPlugin(instance)
""" % (str(root / "state/aw.sock"), str(root / "work")))
                cfg = root / "home/config.json"
                doc = json.loads(cfg.read_text())
                doc["plugins"]["aw-sdk"] = {"enabled": True}
                cfg.write_text(json.dumps(doc))
                config = root / "aw.json"
                config.write_text(
                    json.dumps(
                        {
                            "apiVersion": "aw/v1alpha1",
                            "kind": "AWConfiguration",
                            "metadata": {"name": "sdk"},
                            "spec": {
                                "daemon": {
                                    "startup": "external",
                                    "endpoint": "auto",
                                    "state_dir": str(root / "state"),
                                },
                                "execution": {
                                    "guarantee": "native_hook",
                                    "default_event_budget_ms": 1000,
                                },
                                "audit": {"enabled": True, "payload": "metadata_only"},
                                "agents": {"app": {"adapter": "qwenpaw", "argv": argv}},
                                "providers": {},
                                "events": {
                                    name: {"enabled": True, "required": False, "steps": []}
                                    for name in ["tool.before", "tool.after"]
                                },
                            },
                        }
                    )
                )
                document = json.loads(config.read_text())
                document["spec"]["providers"]["policy"] = {
                    "protocol": "aw-provider/v1alpha1",
                    "config": {"deny_file": str(root / "deny")},
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [
                            "/usr/bin/python3",
                            str(REPO / "src/aw/sdk/tests/fixtures/sdk-deny.py"),
                        ],
                    },
                    "timeout_ms": 1000,
                    "max_output_bytes": 4096,
                }
                document["spec"]["events"]["tool.before"]["steps"] = [
                    {
                        "id": "policy",
                        "provider": "policy",
                        "operation": "check",
                        "effects": ["observe", "block"],
                        "on_error": "block",
                    }
                ]
                config.write_text(json.dumps(document))
                service = processes.start(
                    "service-online",
                    [AW, "service", "serve", "--config", config, "--state-dir", root / "state"],
                )
                await_condition(lambda: (root / "state/aw.sock").exists(), 10, "AW socket")
                started = time.monotonic()
                app = processes.start("app", argv, ports[:1])
                await_condition(lambda: ready(ports[0]), 45, "native App ready")
                startup_seconds = time.monotonic() - started
                initial = json.loads(
                    subprocess.check_output(
                        [str(AW), "service", "status", "--config", str(config)], text=True, timeout=10
                    )
                )["result"]
                self.assertEqual(initial["instances"], [], initial)
                attach_seconds = None
                cases = []
                for mode in [
                    "detached-start",
                    "online",
                    "detached",
                    "attached",
                    "during",
                    "recovered",
                    "denied",
                ]:
                    if mode in ["online", "recovered"]:
                        if mode == "recovered":
                            service = processes.start(
                                "service-recovered",
                                [
                                    AW,
                                    "service",
                                    "serve",
                                    "--config",
                                    config,
                                    "--state-dir",
                                    root / "state",
                                ],
                            )
                            await_condition(
                                lambda: (root / "state/aw.sock").exists(), 10, "AW socket"
                            )
                        time.sleep(2)
                        if mode == "online":
                            listing = json.loads(
                                subprocess.check_output(
                                    [
                                        str(AW),
                                        "instance",
                                        "list",
                                        "--socket",
                                        str(root / "state/aw.sock"),
                                    ],
                                    text=True,
                                    timeout=10,
                                )
                            )
                            self.assertEqual(len(listing["applications"]), 1)
                            application = listing["applications"][0]["application_id"]
                            started = time.monotonic()
                            attached = json.loads(
                                subprocess.check_output(
                                    [
                                        str(AW),
                                        "connect",
                                        "--socket",
                                        str(root / "state/aw.sock"),
                                        "--sdk-instance",
                                        application,
                                    ],
                                    text=True,
                                    timeout=10,
                                )
                            )
                            self.assertEqual(attached["result"]["state"], "completed")
                            attach_seconds = time.monotonic() - started
                        status = json.loads(
                            subprocess.check_output(
                                [str(AW), "service", "status", "--config", str(config)], text=True, timeout=10
                            )
                        )["result"]
                        self.assertEqual(len(status["instances"]), 1, status)
                        self.assertTrue(status["instances"][0]["ready"], status)
                    if mode in ["detached", "attached"]:
                        listing = json.loads(
                            subprocess.check_output(
                                [
                                    str(AW),
                                    "instance",
                                    "list",
                                    "--socket",
                                    str(root / "state/aw.sock"),
                                ],
                                text=True,
                                timeout=10,
                            )
                        )
                        self.assertEqual(len(listing["applications"]), 1)
                        application = listing["applications"][0]["application_id"]
                        result = json.loads(
                            subprocess.check_output(
                                [
                                    str(AW),
                                    "disconnect" if mode == "detached" else "connect",
                                    "--socket",
                                    str(root / "state/aw.sock"),
                                    "--sdk-instance",
                                    application,
                                ],
                                text=True,
                                timeout=10,
                            )
                        )
                        self.assertEqual(result["result"]["state"], "completed")
                        # A detached native turn must run even while the AW policy denies.
                        if mode == "detached":
                            (root / "deny").write_text("deny")
                        else:
                            (root / "deny").unlink()
                    if mode == "denied":
                        (root / "deny").write_text("deny")
                    marker = root / "work" / f"{mode}.txt"
                    arguments = {"command": f"printf x >> {marker}", "timeout": 5}
                    if mode == "during":
                        from concurrent.futures import ThreadPoolExecutor

                        with ThreadPoolExecutor(max_workers=1) as pool:
                            future = pool.submit(
                                turn,
                                self,
                                root,
                                evidence,
                                ports[0],
                                mode,
                                "execute_shell_command",
                                arguments,
                            )
                            try:
                                await_condition(
                                    lambda: (root / "work/loss-waiting").exists(),
                                    10,
                                    "active tool boundary",
                                )
                                processes.stop(service)
                            finally:
                                (root / "work/loss-release").touch()
                            result = future.result(timeout=45)
                    else:
                        result = turn(
                            self, root, evidence, ports[0], mode, "execute_shell_command", arguments
                        )
                    self.assertEqual(
                        result["state"], "denied" if mode == "denied" else "success", result
                    )
                    if mode == "denied":
                        self.assertFalse(marker.exists())
                    else:
                        self.assertEqual(marker.read_text(), "x")
                    cases.append({"mode": mode, "passed": True})
                self.assertTrue(all(p.read_bytes() == data for p, data in original.items()))
                self.assertTrue((root / "work/native-order.log").exists())
                facts = [
                    json.loads(line)["record"]
                    for path in (root / "state/journal").glob("*.jsonl")
                    for line in path.read_text().splitlines()
                ]
                self.assertTrue(
                    any("executionEvent" in record.get("plan", {}) for record in facts),
                    "public callbacks must be recorded",
                )
                (evidence / "results.json").write_text(
                    json.dumps(
                        {
                            "passed": True,
                            "cases": cases,
                            "startup_seconds": startup_seconds,
                            "attach_seconds": attach_seconds,
                            "timing_scope": "one functional run; warm services, cold native App; no performance comparison",
                        },
                        indent=2,
                    )
                )
            finally:
                processes.close()


if __name__ == "__main__":
    unittest.main()
