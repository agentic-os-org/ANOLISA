"""Pinned native SDK submission acceptance with a controlled HTTP model."""
import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.metadata import version
from pathlib import Path


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def run(aw, output, report, security=None):
    fixture = Path(__file__).resolve().parent / "fixtures/agentscope"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="aw-as-submit-") as temporary:
        root = Path(temporary)
        class Model(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                with (root / "models.jsonl").open("a") as out:
                    out.write(json.dumps(request) + "\n")
                user = next((m["content"] for m in reversed(request["messages"]) if m["role"] == "user"), "")
                if isinstance(user, list):
                    user = "".join(block.get("text", "") for block in user)
                message = {"role": "assistant", "content": "done"}
                tool = isinstance(user, str) and user.startswith("tool-") and request["messages"][-1]["role"] != "tool"
                if tool:
                    message = {"role": "assistant", "content": None, "tool_calls": [{"id": "call", "type": "function",
                        "function": {"name": "local_tool", "arguments": json.dumps({"mode": user[5:]})}}]}
                if request.get("stream"):
                    chunks = [
                        {"id": "fixture", "object": "chat.completion.chunk", "created": 0,
                         "model": "fixture", "choices": [{"index": 0, "delta": delta,
                                                          "finish_reason": finish}]}
                        for delta, finish in [({"role": "assistant", "content": "done"}, None), ({}, "stop")]
                    ]
                    body = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks)
                            + "data: [DONE]\n\n").encode()
                    content_type = "text/event-stream"
                else:
                    body = json.dumps({"id": "fixture", "object": "chat.completion", "created": 0,
                        "model": "fixture", "choices": [{"index": 0, "message": {
                            **message}, "finish_reason": "tool_calls" if tool else "stop"}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
                    content_type = "application/json"
                self.send_response(200)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except BrokenPipeError:
                    pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        config = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
                  "metadata": {"name": "agentscope-submit"}, "spec": {
            "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(root / "state")},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"sdk": {"adapter": "agentscope", "argv": [sys.executable, str(fixture / "submission_worker.py")]}},
            "providers": {"scan": {"protocol": "aw-provider/v1alpha1", "transport": {
                "type": "stdio", "location": "agent", "argv": [sys.executable, str(fixture / "submission_provider.py"), str(root)]},
                "timeout_ms": 600, "max_output_bytes": 4096, "config": {}}},
            "events": {"input.submit": {"enabled": True, "required": True, "steps": [
                {"id": name, "provider": "scan", "operation": name, "effects": ["observe", "block"],
                 "on_error": "report" if report else "block"} for name in ["scan_prompt", "scan_user_input"]
            ]}}}}
        for event, policy in [("tool.before", "block"), ("tool.after", "withhold_result")]:
            config["spec"]["events"][event] = {"enabled": True, "steps": [{
                "id": "tool", "provider": "scan", "operation": "tool", "effects": ["observe"], "on_error": policy}]}
        if security is not None:
            config["spec"]["providers"]["security"] = {
                "protocol": "aw-provider/v1alpha1", "transport": {"type": "stdio", "location": "agent",
                    "argv": [str(security), "--cli", str(fixture / "submission_scanner.py"), "--socket", str(root / "scanner.sock")]},
                "timeout_ms": 600, "max_output_bytes": 4096, "config": {"version": 1, "mode": "block",
                    "prompt": {"mode": "block", "scan_mode": "fast"}, "pii": {"input_mode": "block", "output_mode": "observe"}}}
            for step in config["spec"]["events"]["input.submit"]["steps"]:
                step["provider"] = "security"
        path = root / "aw.json"
        path.write_text(json.dumps(config))
        processes = []
        records = []
        try:
            with (output / "serve.log").open("wb") as log:
                service = subprocess.Popen([str(aw), "serve", "--config", str(path), "--state-dir", str(root / "state")],
                                           stdout=log, stderr=log, start_new_session=True)
                processes.append(service)
                records.append({"pid": service.pid, "role": "service"})
                deadline = time.monotonic() + 15
                while not (root / "state/aw.sock").exists():
                    assert service.poll() is None and time.monotonic() < deadline
                    time.sleep(.02)
                with (output / "worker.stdout").open("wb") as stdout, (output / "worker.stderr").open("wb") as stderr:
                    worker = subprocess.Popen([str(aw), "run", "--config", str(path), "--agent", "sdk"],
                        cwd=root, env={**{key: value for key, value in os.environ.items() if not key.lower().endswith("_proxy")}, "AW_TEST_ROOT": str(root), "AW_TEST_BIN": str(aw),
                            "AW_TEST_REPORT": str(int(report)), "AW_TEST_SECURITY": str(int(security is not None)),
                            "AW_TEST_MODEL": f"http://127.0.0.1:{server.server_port}/v1"},
                        stdout=stdout, stderr=stderr, start_new_session=True)
                    processes.append(worker)
                    records.append({"pid": worker.pid, "role": "worker"})
                    (output / "processes.json").write_text(json.dumps(records))
                    worker.wait(timeout=180)
                diagnostics = (output / "worker.stderr").read_text()
                assert worker.returncode == 0, diagnostics
                for failure in ["ignored GeneratorExit", "was created in a different Context",
                                "Task exception was never retrieved"]:
                    assert failure not in diagnostics, diagnostics
                status = json.loads(subprocess.check_output(
                    [str(aw), "status", "--socket", str(root / "state/aw.sock")], timeout=5
                ))["result"]
                assert status["bindings"] == status["events"] == 0, status
                result = json.loads((output / "worker.stdout").read_text())
                result.update(sdk_version=version("agentscope"), bindings=0, events=0, case_count=len(result["cases"]))
                (output / "result.json").write_text(json.dumps(result, indent=2))
        finally:
            for process in reversed(processes):
                stop(process)
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
            for record, process in zip(records, processes):
                record.update(exited=process.poll() is not None, returncode=process.returncode)
            (output / "processes.json").write_text(json.dumps(records))
            for name in ["scans.jsonl", "models.jsonl", "tools.jsonl", "scanner.jsonl"]:
                if (root / name).exists():
                    (output / name).write_bytes((root / name).read_bytes())


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sec-core-provider", type=Path)
    args = parser.parse_args()
    assert version("agentscope") == "2.0.6"
    for report in [False, True]:
        run(args.aw.resolve(), args.output / ("report" if report else "block"), report)
    if args.sec_core_provider:
        for report in [False, True]:
            run(args.aw.resolve(), args.output / ("security-report" if report else "security-block"),
                report, args.sec_core_provider.resolve())
    print("AgentScope 2.0.6 factory submission acceptance passed")
