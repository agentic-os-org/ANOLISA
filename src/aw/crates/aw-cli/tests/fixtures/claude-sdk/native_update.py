"""Native Python host, real bundled CLI and loopback model verify idle policy updates."""

import argparse
import asyncio
from contextlib import aclosing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from importlib.metadata import version
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time


def aw_call(aw, args, *, check=True):
    return subprocess.run([aw, *args], text=True, capture_output=True, timeout=40, check=check)


async def worker(aw):
    prepared = json.loads(aw_call(aw, ["instance", "prepare", "--agent", "claude"]).stdout)
    os.environ.update(prepared["environment"])
    sys.path[:0] = prepared["environment"]["PYTHONPATH"].split(os.pathsep)
    from aw_resident import prepare_native
    from aw_claude import create_client
    from claude_agent_sdk import ClaudeAgentOptions, ResultMessage

    await prepare_native()
    requests = []
    sdk_pids = []
    observations = []

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if self.path.endswith("count_tokens"):
                data, kind = b'{"input_tokens":1}', "application/json"
            else:
                requests.append(body)
                done = len(requests) % 2 == 0
                block = ({"type": "text", "text": "done"} if done else
                         {"type": "tool_use", "id": "native-call", "name": "Bash",
                          "input": {"command": "touch native-effect"}})
                message = {"id": "fixture", "type": "message", "role": "assistant",
                           "model": "claude-sonnet-4-6", "content": [], "stop_reason": None,
                           "usage": {"input_tokens": 1, "output_tokens": 1}}
                text = block["type"] == "text"
                events = [
                    {"type": "message_start", "message": message},
                    {"type": "content_block_start", "index": 0,
                     "content_block": {"type": "text", "text": ""} if text else {**block, "input": {}}},
                    {"type": "content_block_delta", "index": 0,
                     "delta": {"type": "text_delta", "text": "done"} if text else
                     {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}},
                    {"type": "content_block_stop", "index": 0},
                    {"type": "message_delta", "delta": {"stop_reason": "end_turn" if done else "tool_use"},
                     "usage": {"output_tokens": 1}},
                    {"type": "message_stop"},
                ]
                data = "".join(f'event: {e["type"]}\ndata: {json.dumps(e)}\n\n' for e in events).encode()
                kind = "text/event-stream"
            self.send_response(200)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    options = ClaudeAgentOptions(cwd=str(Path.cwd()), setting_sources=[], tools=["Bash"],
        allowed_tools=["Bash"], permission_mode="acceptEdits", env={
            "CLAUDE_CONFIG_DIR": str(Path.cwd() / "profile"),
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
            "ANTHROPIC_API_KEY": "local-fixture", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        })
    update = ["instance", "update", "--instance", prepared["instance_id"], "--policy", "deny.json"]
    try:
        for denied in (False, True):
            client = create_client(options)
            try:
                await client.connect()
                sdk_pids.append(client._transport.delegate._process.pid)
                if not denied:
                    busy = aw_call(aw, update, check=False)
                    assert busy.returncode != 0 and "busy" in busy.stderr.lower(), busy.stderr
                await client.query("Use Bash to touch native-effect exactly once, then finish.")
                results = []
                async with aclosing(client.receive_response()) as messages:
                    async for message in messages:
                        if isinstance(message, ResultMessage):
                            results.append(message)
                assert len(results) == 1
                assert Path("native-effect").exists() is not denied
                state = json.loads(aw_call(aw, ["status", "--config", "aw.yaml"]).stdout)["result"]
                observation = state["observations"][0]
                assert observation["instance_id"] != prepared["instance_id"]
                assert observation["coverage"] == "unknown"
                if denied:
                    assert observation["callbacks"] > 0
                    assert observation["association"]["status"] == "unverified"
                    assert "native_identity_unverified" in observation["gaps"]
                observations.append({"callbacks": observation["callbacks"], "coverage": observation["coverage"],
                    "association": observation.get("association", {}).get("status", "absent")})
            finally:
                await client.disconnect()
            if not denied:
                Path("native-effect").unlink()
                aw_call(aw, update)
        assert all(not Path(f"/proc/{pid}").exists() for pid in sdk_pids), sdk_pids
        Path("result.json").write_text(json.dumps({"passed": True,
            "cases": ["native-start", "busy-rejected", "allow-side-effect", "updated-denial", "sdk-cleanup", "task-observation-unknown"], "observations": observations,
            "model_requests": len(requests), "sdk_processes_stopped": True,
            "python": sys.version.split()[0], "sdk": version("claude-agent-sdk")}))
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
        aw_call(aw, ["instance", "release", "--binding", prepared["binding"]])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--worker", action="store_true")
    args = parser.parse_args()
    aw = str(args.aw.resolve())
    if args.worker:
        asyncio.run(worker(aw))
        return
    assert args.output is not None
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="aw-native-claude-") as temporary:
        root = Path(temporary)
        (root / "home").mkdir(mode=0o700)
        document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
            "metadata": {"name": "native-claude"}, "spec": {
                "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": str(root / "s")},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {"claude": {"adapter": "claude-sdk", "argv": [sys.executable, str(Path(__file__).resolve())]}},
                "providers": {}, "events": {}}}
        document["spec"]["agents"]["claude"]["evidence"] = {
            "source_id": "fixture", "endpoint": "http://127.0.0.1:1", "mode": "auto",
            "query_timeout_ms": 100, "query_bytes": 1048576, "max_items": 32, "max_bytes": 262144}
        (root / "aw.yaml").write_text(json.dumps(document))
        document["spec"]["providers"]["deny"] = {"protocol": "native-hook/v1alpha1",
            "transport": {"type": "stdio", "location": "agent", "argv": [sys.executable, "-c",
                "import sys; sys.stderr.write('fixture policy denial'); sys.exit(2)"]},
            "timeout_ms": 1000, "max_output_bytes": 4096, "config": {}}
        document["spec"]["events"]["tool.before"] = {"enabled": True, "required": True,
            "steps": [{"id": "deny", "provider": "deny", "native": {}, "on_error": "block"}]}
        (root / "deny.json").write_text(json.dumps(document))
        env = {"PATH": os.environ["PATH"], "HOME": str(root / "home"), "LANG": "C.UTF-8"}
        with (args.output / "service.log").open("wb") as log:
            service = subprocess.Popen([aw, "serve", "--config", root / "aw.yaml", "--state-dir", root / "s"],
                                       cwd=root, env=env, stdout=log, stderr=log, start_new_session=True)
            try:
                deadline = time.monotonic() + 15
                while not (root / "s/aw.sock").exists():
                    assert service.poll() is None and time.monotonic() < deadline
                    time.sleep(0.05)
                child = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "--aw", aw, "--worker"],
                    cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
                try:
                    stdout, stderr = child.communicate(timeout=150)
                finally:
                    if child.poll() is None:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait(timeout=5)
                (args.output / "worker.log").write_bytes(stdout + stderr)
                assert child.returncode == 0, stderr.decode()
                (args.output / "result.json").write_text((root / "result.json").read_text())
                state = json.loads(aw_call(aw, ["status", "--config", str(root / "aw.yaml")]).stdout)["result"]
                assert state["bindings"] == state["events"] == 0, state
            finally:
                if service.poll() is None:
                    os.killpg(service.pid, signal.SIGTERM)
                try:
                    service.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    os.killpg(service.pid, signal.SIGKILL)
                    service.wait(timeout=5)


if __name__ == "__main__":
    main()
