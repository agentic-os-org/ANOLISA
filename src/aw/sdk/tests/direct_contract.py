"""Exercise public SDK registration and callbacks from a real application process."""

import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import time

FIXTURE = json.loads((Path(__file__).parent / "fixtures/contract.json").read_text())


def exchange(endpoint: Path, identity: dict | None, operation: dict) -> dict:
    deadline = time.monotonic_ns() + 5_000_000_000
    request = json.dumps({"api_version": FIXTURE["service_protocol"], "identity": identity,
                          "deadline_ns": deadline, "operation": operation}).encode()
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(5)
        connection.connect(str(endpoint))
        _, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        assert uid == os.getuid()
        connection.sendall(struct.pack("!I", len(request)) + request)

        def receive(size: int) -> bytearray:
            data = bytearray()
            while len(data) < size:
                piece = connection.recv(size - len(data))
                if not piece:
                    raise EOFError("incomplete RPC response")
                data.extend(piece)
            return data

        length, = struct.unpack("!I", receive(4))
        assert length <= 9 * 1024 * 1024
        response = json.loads(receive(length))
        assert time.monotonic_ns() < deadline
        assert response["api_version"] == FIXTURE["service_protocol"]
        if identity is not None:
            assert response["identity"] == identity
        return response


def run(binary: str) -> None:
    with tempfile.TemporaryDirectory(prefix="aw-sdk-contract-") as temporary:
        root = Path(temporary)
        config = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
                  "metadata": {"name": "sdk-contract"}, "spec": {
                      "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": "auto"},
                      "execution": {"guarantee": "native_hook", "default_event_budget_ms": 1000},
                      "audit": {"enabled": True, "payload": "metadata_only"},
                      "agents": {"app": {"adapter": "qoder-sdk", "argv": ["/bin/true"]}},
                      "providers": {}, "events": {
                          name: {"enabled": True, "required": False, "steps": []}
                          for name in ("tool.before", "tool.after")}}}
        path = root / "config.json"
        path.write_text(json.dumps(config))
        state = root / "state"
        state.mkdir(mode=0o700)
        endpoint = state / "aw.sock"
        with (root / "service.log").open("wb") as log:
            service = subprocess.Popen([binary, "service", "serve", "--config", str(path),
                                        "--state-dir", str(state)], stdout=log, stderr=log)
            try:
                limit = time.monotonic() + 10
                while not endpoint.exists():
                    if service.poll() is not None or time.monotonic() > limit:
                        raise RuntimeError((root / "service.log").read_text())
                    time.sleep(0.01)
                status = exchange(endpoint, None, {"method": "status"})
                identity = status["identity"]
                assert status["result"]["sdk_protocol"] == FIXTURE["sdk_protocol"]

                def call(operation: dict) -> dict:
                    response = exchange(endpoint, identity, operation)
                    assert response["error"] is None, response
                    return response["result"]

                def register(command: dict) -> dict:
                    return call({"method": "registration", "command": command})

                owner = {"pid": os.getpid(), "start_ticks": Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19]}
                key = {"request_id": "sdk-contract", "owner": owner}
                prepared = register({"action": "register_sdk", "sdk_version": FIXTURE["sdk_protocol"],
                                     "availability": "native_fallback", "key": key, "target": "app",
                                     "capabilities": {"adapter": "qoder-sdk", "version": "public-hooks/v1",
                                                      "entrypoint": "sdk-worker", "events": {
                                                          "tool.before": ["observe"], "tool.after": ["observe"]}},
                                     "cwd": str(root), "environment": {}})
                assert prepared["state"] == "pending_ready"
                assert register({"action": "query", "key": key})["handle"] == prepared["handle"]
                register({"action": "ready", "handle": prepared["handle"], "policy_revision": prepared["policy_revision"]})
                scope = call({"method": "open_scope", "parent_id": prepared["handle"]["binding"]["instance_id"],
                              "pid": os.getpid(), "cwd": str(root), "environment": {}})
                sid = scope["instance_id"]
                policy = call({"method": "scope_policy", "instance_id": sid})
                call({"method": "acknowledge_scope", "instance_id": sid})
                for event, name in [("tool.before", "PreToolUse"), ("tool.after", "PostToolUse")]:
                    native = dict(FIXTURE["native_before"], cwd=str(root), hook_event_name=name)
                    if event == "tool.after":
                        native["tool_response"] = "sdk"
                    step = next(iter(policy["events"][event]["steps"]))
                    response = call({"method": "sdk_callback", "instance_id": sid, "event": event,
                                     "step_id": step, "native_input": native, "close": False})
                    assert response == FIXTURE["neutral_callback"]
                call({"method": "release_instance", "instance_id": sid})
                assert register({"action": "release", "handle": prepared["handle"]})["state"] == "released"
                assert call({"method": "status"})["bindings"] == 0
                call({"method": "stop"})
                service.wait(timeout=10)
                assert service.returncode == 0
                facts = []
                for journal in (state / "journal").glob("*.jsonl"):
                    for line in journal.read_text().splitlines():
                        plan = json.loads(line)["record"].get("plan", {})
                        if "executionEvent" in plan:
                            facts.append(plan["executionEvent"])
                for event in ("tool.before", "tool.after"):
                    assert any(fact["data"]["name"] == event for fact in facts), facts
                print(json.dumps({"status": "passed", "protocol": FIXTURE["sdk_protocol"],
                                  "cases": ["direct_peer", "register", "query", "ready", "scope", "before", "after", "release", "public_native_facts"]}))
            finally:
                if service.poll() is None:
                    service.terminate()
                    try:
                        service.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        service.kill()
                        service.wait(timeout=5)


if __name__ == "__main__":
    run(str(Path(sys.argv[1]).resolve()))
