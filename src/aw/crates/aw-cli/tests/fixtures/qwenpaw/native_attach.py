"""Exercise AW attach/detach against two independently started stock QwenPaw Apps."""

import argparse
import concurrent.futures
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


class NativeAttachTests(unittest.TestCase):
    def test_native_first_request_isolation_busy_detach_and_reattach(self):
        evidence = Path(os.environ["AW_TEST_OUTPUT"]) / "native-attach"
        evidence.mkdir(exist_ok=True)
        report = {"passed": False, "checks": []}
        with tempfile.TemporaryDirectory(prefix="aw-native-attach-") as directory:
            root = Path(directory)
            cases = []
            ports = []
            try:
                for name in ("selected", "other"):
                    case = root / name
                    case.mkdir(mode=0o700)
                    for child in ("home", "work", "state", "runtime"):
                        (case / child).mkdir(mode=0o700)
                    output = evidence / name
                    output.mkdir(exist_ok=True)
                    environment = {k: v for k, v in os.environ.items()
                                   if not k.startswith(("AW_", "QWENPAW_", "QODER_", "AGENT_SEC_"))}
                    environment["XDG_RUNTIME_DIR"] = str(case / "runtime")
                    processes = Processes(case, output, environment)
                    argv, native_ports = configure_qwenpaw(case, self.qwenpaw, environment, processes)
                    marker = case / "work/marker"
                    (case / "tool-command.txt").write_text("printf attached > " + shlex.quote(str(marker)))
                    child = processes.start("native-app", argv, native_ports[:1])
                    cases.append((case, processes, environment, child, native_ports, argv))
                    ports.extend(native_ports)
                    def listening():
                        self.assertFalse(processes.exited(child))
                        with socket.socket() as connection:
                            return connection.connect_ex(("127.0.0.1", native_ports[0])) == 0
                    await_condition(listening, 45, "native App startup")
                    with urllib.request.urlopen(f"http://127.0.0.1:{native_ports[0]}/api/plugins", timeout=5) as response:
                        response.read()
                case, processes, environment, child, native_ports, argv = cases[0]
                other = cases[1][0]
                unchanged = (other / "home/config.json").read_bytes()
                identity = Path(f"/proc/{child.pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
                config = case / "aw.json"
                document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "attach"}, "spec": {
                    "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(case / "state")},
                    "execution": {"guarantee": "native_hook", "default_event_budget_ms": 1000},
                    "audit": {"enabled": True, "payload": "metadata_only"},
                    "agents": {"native": {"adapter": "qwenpaw", "argv": argv}},
                    "providers": {"deny": {"protocol": "native-hook/v1alpha1", "transport": {
                        "type": "stdio", "location": "agent", "argv": ["/bin/sh", "-c", "exit 2"]},
                        "timeout_ms": 500, "max_output_bytes": 4096, "config": {}}},
                    "events": {"tool.before": {"enabled": True, "steps": [
                        {"id": "deny", "provider": "deny", "native": {}, "on_error": "block"}]}},
                }}
                save(config, document)
                aw = self.binary
                daemon = processes.start("daemon", [aw, "serve", "--config", config, "--state-dir", case / "state"])
                await_condition(lambda: (case / "state/aw.sock").exists() or processes.exited(daemon), 5, "AW startup")
                def status():
                    return json.loads(checked([aw, "status", "--config", config], env=environment).stdout)["result"]
                def install(remove=False, success=True):
                    command = [aw, "install", "--config", config, "--agent", "native", "--pid", str(child.pid)]
                    if remove:
                        command.append("--remove")
                    result = __import__("subprocess").run(list(map(str, command)), env=environment, capture_output=True, timeout=45)
                    self.assertEqual(result.returncode == 0, success, result.stderr.decode())
                    return result
                def turn(index, session):
                    selected = cases[index]
                    (selected[0] / "tool-call-id.txt").write_text("call_" + __import__("uuid").uuid4().hex)
                    payload = {"session_id": session, "user_id": "aw-test", "input": [
                        {"role": "user", "content": [{"type": "text", "text": "Use execute_shell_command once then stop."}]}]}
                    request = urllib.request.Request(f"http://127.0.0.1:{selected[4][0]}/api/console/chat",
                        data=json.dumps(payload).encode(), headers={"Content-Type": "application/json", "X-Agent-Id": "default"})
                    with urllib.request.urlopen(request, timeout=45) as response:
                        data = response.read(2 * 1024 * 1024)
                    frames = [json.loads(line[6:]) for line in data.splitlines() if line.startswith(b"data: {")]
                    completed = [frame for frame in frames if frame.get("object") == "response" and frame.get("status") == "completed"]
                    self.assertEqual(len(completed), 1, data.decode())
                    output = [message for message in completed[0]["output"] if message["type"] == "plugin_call_output"]
                    self.assertEqual(len(output), 1, data.decode())
                    return output[0]["content"][0]["data"]["state"]
                attached = json.loads(install().stdout)
                self.assertTrue(attached["attached"])
                self.assertFalse((case / "model-requests.jsonl").exists())
                self.assertEqual(turn(0, "existing-session"), "denied")
                self.assertFalse((case / "work/marker").exists())
                self.assertEqual(turn(1, "other-session"), "success")
                self.assertTrue((other / "work/marker").exists())
                self.assertEqual((other / "home/config.json").read_bytes(), unchanged)
                install(success=False)
                report["checks"].extend(["first-request-denied", "other-profile-unaffected", "duplicate-rejected"])
                allowed = copy.deepcopy(document)
                allowed["spec"]["events"] = {}
                save(config, allowed)
                checked([aw, "update", "--config", config, "--agent", "native"], env=environment)
                report["checks"].append("single-config-update")
                (case / "tool-command.txt").write_text("sleep 2; printf attached > " + shlex.quote(str(case / "work/marker")))
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    future = pool.submit(turn, 0, "busy-session")
                    await_condition(lambda: status()["instances"][0]["tools"] > 0, 10, "active native tool")
                    rejected = install(remove=True, success=False)
                    self.assertIn(b"busy", rejected.stderr)
                    self.assertTrue((case / "home/plugins/aw-native").is_dir())
                    self.assertEqual(future.result(timeout=45), "success")
                await_condition(lambda: status()["instances"][0]["requests"] == 0, 5, "request cleanup")
                plugin_tree = case / "home/plugins/aw-native"
                moved_tree = case / "home/plugins/moved-aw-native"
                plugin_tree.rename(moved_tree)
                missing = install(remove=True, success=False)
                self.assertIn(b"unload cannot be verified", missing.stderr)
                ownership = json.loads((case / "home/.aw-attachment.json").read_text())
                self.assertTrue(Path(ownership["binding"]).is_file())
                self.assertTrue(Path(ownership["artifacts"]).is_dir())
                moved_tree.rename(plugin_tree)
                self.assertTrue(json.loads(install(remove=True).stdout)["detached"])
                self.assertEqual(status()["bindings"], 0)
                self.assertFalse((case / "home/plugins/aw-native").exists())
                self.assertFalse((case / "home/.aw-attachment.json").exists())
                self.assertFalse(list((case / "state").glob("attach-*")))
                self.assertEqual(turn(0, "existing-session"), "success")
                report["checks"].extend(["busy-detach-rejected", "native-cleanup", "existing-session-after-detach"])
                install()
                self.assertEqual(turn(0, "reattached-session"), "success")
                install(remove=True)
                self.assertEqual(Path(f"/proc/{child.pid}/stat").read_text().rsplit(")", 1)[1].split()[19], identity)
                self.assertFalse(processes.exited(child))
                report["checks"].extend(["reattach", "pid-start-ticks-unchanged"])
                report["passed"] = True
            finally:
                for case, processes, *_ in reversed(cases):
                    processes.close()
                for port in ports:
                    with socket.socket() as connection:
                        self.assertNotEqual(connection.connect_ex(("127.0.0.1", port)), 0)
                save(evidence / "result.json", report)
        self.assertFalse(root.exists())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--qwenpaw", type=Path, default=shutil.which("qwenpaw"))
    args, remaining = parser.parse_known_args()
    NativeAttachTests.binary = args.bin.resolve() / "aw"
    NativeAttachTests.qwenpaw = args.qwenpaw.resolve()
    unittest.main(argv=[sys.argv[0], *remaining])
