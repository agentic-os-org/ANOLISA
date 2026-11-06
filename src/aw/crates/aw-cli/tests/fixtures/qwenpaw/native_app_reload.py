"""Require stock App HTTP tool turns and repeated policy updates in one process."""

import argparse
import copy
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sec_core"))
from agents import configure_qwenpaw
from runtime import Processes, await_condition, checked, save
from native_app_config import ready as native_ready, stage_user_configuration, turn, dynamic_mcp


class AppReloadTests(unittest.TestCase):
    def test_policy_updates_after_successful_and_error_native_tools(self):
        self.check_policy_updates(reloadable=True)

    def test_resident_policy_updates_preserve_native_configuration(self):
        self.check_policy_updates(reloadable=False)

    def check_policy_updates(self, *, reloadable):
        evidence = Path(os.environ["AW_TEST_OUTPUT"]) / ("app-reload" if reloadable else "app-resident")
        evidence.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="aw-app-reload-") as directory:
            root = Path(directory)
            for name in ("work", "home", "state"):
                (root / name).mkdir(mode=0o700)
            marker = root / "work/marker.txt"
            (root / "tool-command.txt").write_text("printf '42\\n' > " + shlex.quote(str(marker)))
            environment = {k: v for k, v in os.environ.items()
                           if not k.startswith(("AW_", "QWENPAW_", "QODER_", "QODERCN_", "AGENT_SEC_"))}
            processes = Processes(root, evidence, environment)
            ports, snapshots = [], []
            aw = self.binary
            config = root / "current.json"
            try:
                argv, ports = configure_qwenpaw(root, self.qwenpaw, environment, processes)
                user_files = stage_user_configuration(root) if not reloadable else {}
                readable = root / "work/readable.txt"
                readable.write_text("native read success")
                native_config = None
                baseline = None
                if not reloadable:
                    native = processes.start("native-baseline", argv, ports[:1])
                    await_condition(lambda: native_ready(ports[0]), 45, "native App readiness")
                    baseline = turn(self, root, evidence, ports[0], "baseline", "read_file", {"file_path": str(readable)})
                    self.assertEqual(baseline["state"], "success")
                    dynamic_mcp(self, root, evidence, ports[0], self.qwenpaw.parent / "python", "baseline", processes)
                    processes.stop(native)
                    baseline_order = (root / "work/native-order.log").read_text().splitlines()
                    native_config = (root / "home/config.json").read_bytes()
                    # Native first startup may normalize its own schema. Compare AW
                    # against that native result, retaining plugin extension fields.
                    self.assertEqual(json.loads(native_config)["plugins"]["native-observer"]["future"], [1, 2])
                document = {
                    "apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
                    "metadata": {"name": "app-reload"}, "spec": {
                        "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(root / "state")},
                        "execution": {"guarantee": "native_hook", "default_event_budget_ms": 1000},
                        "audit": {"enabled": True, "payload": "metadata_only"},
                        "agents": {"native": {"adapter": "qwenpaw", "argv": argv}},
                        "providers": {}, "events": {},
                    },
                }
                save(config, document)
                daemon = processes.start("daemon", [aw, "serve", "--config", config, "--state-dir", root / "state"])
                await_condition(lambda: (root / "state/aw.sock").exists() or processes.exited(daemon), 5, "AW startup")
                self.assertFalse(processes.exited(daemon))
                child = processes.start("app", [aw, "run", "--config", config, "--agent", "native", *(["--reloadable"] if reloadable else [])], ports[:1])

                def status():
                    return json.loads(checked([aw, "status", "--config", config], env=environment).stdout)["result"]

                def ready():
                    self.assertFalse(processes.exited(child), "App exited before readiness")
                    rows = status()["instances"]
                    return len(rows) == 1 and rows[0]["ready"]

                await_condition(ready, 45, "QwenPaw bridge readiness")
                identity = status()["instances"][0]["process"]
                blocked = copy.deepcopy(document)
                blocked["spec"]["providers"]["deny"] = {
                    "protocol": "native-hook/v1alpha1",
                    "transport": {"type": "stdio", "location": "agent", "argv": ["/bin/sh", "-c", "exit 2"]},
                    "timeout_ms": 500, "max_output_bytes": 4096, "config": {},
                }
                blocked["spec"]["events"]["tool.before"] = {"enabled": True, "steps": [
                    {"id": "deny", "provider": "deny", "native": {}, "on_error": "block", "match": {"tools": ["native:qwenpaw:execute_shell_command"]}},
                ]}
                cases = (
                    (blocked, "execute_shell_command", "denied", None),
                    (document, "execute_shell_command", "success", None),
                    (blocked, "read_file", "success", readable),
                    (blocked, "read_file", "error", root / "work/absent.txt"),
                    (blocked, "execute_shell_command", "denied", None),
                )
                for index, (policy, tool, expected_state, path) in enumerate(cases):
                    candidate = root / "next.json"
                    save(candidate, policy)
                    changed = json.loads(checked([aw, "update", "--config", config, "--agent", "native", "--policy", candidate], env=environment).stdout)
                    arguments = {"command": (root / "tool-command.txt").read_text(), "timeout": 5} if path is None else {"file_path": str(path)}
                    result = turn(self, root, evidence, ports[0], str(index), tool, arguments)
                    if baseline is not None and path == readable:
                        self.assertEqual(result["output"], baseline["output"])
                    self.assertEqual(result["state"], expected_state)
                    self.assertEqual(marker.exists(), index == 1)

                    def idle():
                        snapshot = status()
                        state = snapshot["instances"][0]
                        return snapshot["events"] == state["requests"] == state["tools"] == 0

                    await_condition(idle, 5, "completed App request retained AW occupancy")
                    state = status()["instances"][0]
                    self.assertEqual(state["process"], identity)
                    self.assertEqual(state["policy_revision"], changed["policy_revision"])
                    snapshots.append(state)
                    marker.unlink(missing_ok=True)
                if not reloadable:
                    dynamic_mcp(self, root, evidence, ports[0], self.qwenpaw.parent / "python", "managed", processes)
                processes.stop(child)
                self.assertEqual(status()["bindings"], 0)
                if native_config is not None:
                    self.assertEqual((root / "home/config.json").read_bytes(), native_config)
                    for path, original in user_files.items():
                        self.assertEqual(path.read_bytes(), original)
                    order = (root / "work/native-order.log").read_text().splitlines()
                    self.assertEqual(order.count("before:read_file"), 3)
                    self.assertEqual(order.count("after:read_file"), baseline_order.count("after:read_file") + 2)
                save(evidence / "result.json", {"passed": True, "same_process": True, "turns": snapshots})
            finally:
                processes.close()
                self.assertFalse((root / "state/aw.sock").exists())
                self.assertFalse(list((root / "home/plugins").glob("aw-native-*")))
                for port in ports:
                    with socket.socket() as connection:
                        connection.settimeout(0.2)
                        self.assertNotEqual(connection.connect_ex(("127.0.0.1", port)), 0)
        self.assertFalse(root.exists())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--qwenpaw", type=Path, default=shutil.which("qwenpaw"))
    args, remaining = parser.parse_known_args()
    AppReloadTests.binary = args.bin.resolve() / "aw"
    AppReloadTests.qwenpaw = args.qwenpaw.resolve()
    unittest.main(argv=[sys.argv[0], *remaining])
