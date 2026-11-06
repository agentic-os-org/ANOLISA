"""Real Service, private tool subprocess and model HTTP delivery acceptance."""
import argparse
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.metadata import version

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sec_core"))
from runtime import Processes, await_condition, checked, save


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    assert version("agentscope") == "2.0.6"
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory(prefix="aw-as-private-") as temporary:
        root = Path(temporary)
        (root / "work").mkdir()
        requests = []

        class Model(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                last = request["messages"][-1]
                tool = last["role"] != "tool"
                message = {"role": "assistant", "content": "done"}
                if tool:
                    message = {"role": "assistant", "content": None, "tool_calls": [
                        {"id": "call", "type": "function", "function": {"name": "private_tool",
                         "arguments": json.dumps({"text": "unchanged native arguments"})}}]}
                body = json.dumps({"id": "fixture", "object": "chat.completion", "created": 0,
                    "model": "fixture", "choices": [{"index": 0, "message": message,
                    "finish_reason": "tool_calls" if tool else "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        env = {**{key: value for key, value in os.environ.items() if not key.lower().endswith("_proxy")},
               "AW_TEST_ROOT": str(root), "AW_TEST_MODEL": f"http://127.0.0.1:{server.server_port}/v1"}
        processes = Processes(root, output, env)
        aw = args.aw.resolve()
        config = root / "aw.json"
        document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
            "metadata": {"name": "agentscope-private"}, "spec": {
                "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(root / "state")},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {"sdk": {"adapter": "agentscope", "argv": [sys.executable, str(fixture / "private_worker.py")]}},
                "providers": {"observer": {"protocol": "native-hook/v1alpha1", "transport": {
                    "type": "stdio", "location": "agent", "argv": [sys.executable,
                    str(fixture / "private_hook.py"), str(root / "hooks.jsonl")]},
                    "timeout_ms": 1000, "max_output_bytes": 4096, "config": {}}},
                "events": {event: {"enabled": True, "steps": [{"id": "observer", "provider": "observer",
                    "native": {}, "on_error": "block" if event == "tool.before" else "report"}]}
                    for event in ("tool.before", "tool.after")}}}
        save(config, document)
        candidate = copy.deepcopy(document)
        candidate["spec"]["events"] = {}
        save(root / "candidate.json", candidate)

        def status():
            return json.loads(checked([aw, "status", "--config", config], env=env).stdout)["result"]

        def busy():
            result = subprocess.run([aw, "update", "--config", config, "--agent", "sdk", "--policy", root / "candidate.json"],
                env=env, capture_output=True, text=True, timeout=15)
            assert result.returncode and "busy" in (result.stdout + result.stderr).lower(), result

        def start(index, mode):
            (root / f"turn-{index}").write_text(mode)

        def finish(index, failed=False):
            path = root / f"result-{index}"
            await_condition(path.exists, 30, "private reply completion")
            assert path.read_text() == ("CallbackFailure" if failed else "ok"), path.read_text()
            if not failed:
                await_condition(lambda: status()["instances"][0]["requests"] == 0, 10, "private lease release")

        try:
            processes.start("daemon", [aw, "serve", "--config", config, "--state-dir", root / "state"])
            await_condition(lambda: (root / "state/aw.sock").exists(), 10, "service startup")
            worker = processes.start("worker", [aw, "run", "--config", config, "--agent", "sdk"])
            await_condition(lambda: (root / "ready").exists() or processes.exited(worker), 30, "private factory ready")
            assert not processes.exited(worker)
            start(0, "normal")
            finish(0)
            start(1, "stream")
            finish(1)
            start(2, "acquire")
            await_condition(lambda: (root / "acquiring").exists(), 15, "acquiring lease")
            busy()
            (root / "allow-acquire").touch()
            finish(2)
            start(3, "revoke")
            await_condition(lambda: (root / "revoking").exists(), 15, "revoking lease")
            busy()
            (root / "allow-revoke").touch()
            finish(3)
            start(4, "fail")
            finish(4, failed=True)
            busy()
            before = len(requests)
            start(5, "normal")
            finish(5, failed=True)
            assert len(requests) == before == 10
            assert status()["instances"][0]["requests"] == 1
            tools = (root / "tools.jsonl").read_text().splitlines()
            hooks = (root / "hooks.jsonl").read_text()
            assert len(tools) == 5 and len(hooks.splitlines()) == 10
            assert "[redacted]" in hooks and "[redacted]" in json.dumps(requests)
            assert "synthetic-task-private-value" not in hooks + json.dumps(requests)
            for request in requests:
                for message in request["messages"]:
                    if message["role"] == "tool":
                        assert "[redacted]" in json.dumps(message)
            (root / "stop").touch()
            assert processes.wait(worker, 15) == 0
            diagnostics = (output / "worker.log").read_text()
            assert "synthetic-task-private-value" not in diagnostics
            assert "Task exception was never retrieved" not in diagnostics
            assert "native_event_closed" not in diagnostics
            await_condition(lambda: status()["bindings"] == status()["events"] == 0, 10, "worker teardown")
            save(output / "result.json", {"passed": True, "sdk_version": version("agentscope"),
                "model_requests": len(requests), "tool_subprocesses": len(tools), "public_hooks": 10,
                "busy_rejections": 3, "failed_revoke_fenced": True, "bindings": 0, "events": 0})
        finally:
            processes.close()
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
