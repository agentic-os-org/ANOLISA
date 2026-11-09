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

    def test_native_start_uses_ordinary_resident_policy(self):
        self.check_policy_updates(reloadable=False, native_start=True)

    def check_policy_updates(self, *, reloadable, native_start=False):
        evidence = Path(os.environ["AW_TEST_OUTPUT"]) / ("app-native-start" if native_start else "app-reload" if reloadable else "app-resident")
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
            ports, snapshots, activation_files = [], [], []
            site_permissions = None
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
                if native_start:
                    document["spec"]["agents"]["native"]["evidence"] = {
                        "mode": "auto", "source_id": "fixture", "endpoint": "http://127.0.0.1:1",
                        "token_file": str(root / "absent-source-token"),
                        "query_timeout_ms": 1000, "query_bytes": 1048576,
                        "max_items": 32, "max_bytes": 262144,
                    }
                save(config, document)
                daemon = processes.start("daemon", [aw, "serve", "--config", config, "--state-dir", root / "state"])
                await_condition(lambda: (root / "state/aw.sock").exists() or processes.exited(daemon), 5, "AW startup")
                self.assertFalse(processes.exited(daemon))
                if native_start:
                    interpreter = self.qwenpaw.parent / "python"
                    site = json.loads(checked([
                        interpreter, "-c", "import json,sys,sysconfig; print(json.dumps({'venv':sys.prefix != sys.base_prefix,'site':sysconfig.get_path('purelib')}))",
                    ], env=environment).stdout)
                    self.assertTrue(site["venv"], "native startup acceptance requires a dedicated QwenPaw venv")
                    site_path = Path(site["site"])
                    site_permissions = (site_path, site_path.stat().st_mode & 0o7777)
                    site_path.chmod(site_permissions[1] & ~0o022)
                    setup = json.loads(checked([
                        aw, "instance", "setup", "--config", config, "--agent", "native",
                        "--directory", site["site"],
                    ], env=environment).stdout)
                    activation_files = [(Path(path), Path(path).stat().st_ino) for path in setup["files"]]
                    environment.update(setup["environment"])
                    child = processes.start("app", argv, ports[:1])
                else:
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
                    command = ([aw, "instance", "update", "--instance", status()["instances"][0]["instance_id"]]
                               if native_start else [aw, "update", "--agent", "native"])
                    changed = json.loads(checked([*command, "--config", config, "--policy", candidate], env=environment).stdout)
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
                if native_start:
                    self.assertFalse(list((root / "state").glob("native-*")))
                    records = [json.loads(line)["record"] for path in (root / "state/journal").glob("*.jsonl")
                               for line in path.read_text().splitlines()]
                    callbacks = [item for item in records if item.get("phase") == "native_callback"]
                    self.assertGreater(len(callbacks), 0)
                    self.assertTrue(all(item["association"]["status"] == "verified" for item in callbacks))
                    self.assertTrue(any(item.get("phase") == "closed" for item in records))
                    self.assertEqual(status()["observations"], [])
                if native_config is not None:
                    self.assertEqual((root / "home/config.json").read_bytes(), native_config)
                    for path, original in user_files.items():
                        self.assertEqual(path.read_bytes(), original)
                    order = (root / "work/native-order.log").read_text().splitlines()
                    self.assertEqual(order.count("before:read_file"), 3)
                    self.assertEqual(order.count("after:read_file"), baseline_order.count("after:read_file") + 2)
                save(evidence / "result.json", {"passed": True, "same_process": True, "native_start": native_start, "native_mcp": not reloadable, "turns": snapshots})
            finally:
                processes.close()
                for path, inode in activation_files:
                    self.assertEqual(path.lstat().st_ino, inode, "startup asset changed during acceptance")
                    path.unlink()
                if site_permissions is not None:
                    site_permissions[0].chmod(site_permissions[1])
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
