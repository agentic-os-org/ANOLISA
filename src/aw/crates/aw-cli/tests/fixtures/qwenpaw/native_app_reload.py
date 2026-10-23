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
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sec_core"))
from agents import configure_qwenpaw
from runtime import Processes, await_condition, checked, save


class AppReloadTests(unittest.TestCase):
    def test_policy_updates_after_successful_and_error_native_tools(self):
        evidence = Path(os.environ["AW_TEST_OUTPUT"]) / "app-reload"
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
                child = processes.start("app", [aw, "run", "--config", config, "--agent", "native", "--reloadable"], ports[:1])

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
                    {"id": "deny", "provider": "deny", "native": {}, "on_error": "block"},
                ]}
                readable = root / "work/readable.txt"
                readable.write_text("native read success")
                cases = (
                    (blocked, "execute_shell_command", "denied", None),
                    (document, "execute_shell_command", "success", None),
                    (document, "read_file", "success", readable),
                    (document, "read_file", "error", root / "work/absent.txt"),
                    (blocked, "execute_shell_command", "denied", None),
                )
                for index, (policy, tool, expected_state, path) in enumerate(cases):
                    tool_request = root / "tool-call.json"
                    if path is None:
                        tool_request.unlink(missing_ok=True)
                    else:
                        save(tool_request, {"name": tool, "arguments": {"file_path": str(path)}})
                    candidate = root / "next.json"
                    save(candidate, policy)
                    changed = json.loads(checked([aw, "update", "--config", config, "--agent", "native", "--policy", candidate], env=environment).stdout)
                    payload = {"session_id": f"reload-{index}", "user_id": "aw-test", "input": [
                        {"role": "user", "content": [{"type": "text", "text": f"Use {tool} once then stop."}]},
                    ]}
                    request = urllib.request.Request(
                        f"http://127.0.0.1:{ports[0]}/api/console/chat",
                        data=json.dumps(payload).encode(),
                        headers={"Content-Type": "application/json", "X-Agent-Id": "default"},
                    )
                    with urllib.request.urlopen(request, timeout=45) as response:
                        events = response.read(2 * 1024 * 1024)
                    (evidence / f"turn-{index}.log").write_bytes(events)
                    frames = [json.loads(line[6:]) for line in events.splitlines() if line.startswith(b"data: {")]
                    completed = [frame for frame in frames if frame.get("object") == "response" and frame.get("status") == "completed"]
                    self.assertEqual(len(completed), 1)
                    outputs = [message for message in completed[0]["output"] if message["type"] == "plugin_call_output"]
                    self.assertEqual(len(outputs), 1)
                    result = outputs[0]["content"][0]["data"]
                    self.assertEqual(result["name"], tool)
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
                processes.stop(child)
                self.assertEqual(status()["bindings"], 0)
                save(evidence / "result.json", {"passed": True, "same_process": True, "turns": snapshots})
            finally:
                processes.close()
                self.assertFalse((root / "state/aw.sock").exists())
                self.assertFalse((root / "home/plugins/aw-native").exists())
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
