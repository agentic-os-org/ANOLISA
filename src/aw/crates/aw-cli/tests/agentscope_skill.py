"""Real AgentScope/AW/Provider Skill adoption with a controlled Ledger and model."""
import argparse
import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.metadata import version

from agentscope_submission import stop

CASES = ("signed", "unsigned", "drifted", "tampered", "backend-denied", "unavailable", "timeout")


async def worker():
    from agentscope.agent import InjectionConfig
    from agentscope.credential import OpenAICredential
    from agentscope.message import UserMsg
    from agentscope.model import OpenAIChatModel
    from agentscope.state import AgentState
    from agentscope.tool import Toolkit
    from aw_agentscope import AwBridge

    root = Path(os.environ["AW_TEST_ROOT"])
    bridge = AwBridge()
    models, cases = [], []
    try:
        for name in CASES:
            model = OpenAIChatModel(OpenAICredential(api_key="fixture", base_url=os.environ["AW_TEST_MODEL"]),
                                    model="fixture", stream=False, max_retries=0, context_size=128000)
            models.append(model)
            agent = bridge.create_agent(name="fixture", system_prompt="Load the requested Skill once.", model=model,
                state=AgentState(session_id=name), toolkit=Toolkit(skills_or_loaders=[str(root / name)]),
                injection_config=InjectionConfig(inject_runtime_state=False))
            (root / "case").write_text(name)
            await agent.reply(UserMsg("user", name))
            requests = [json.loads(row) for row in (root / "models.jsonl").read_text().splitlines()]
            selected = [item for item in requests if item["case"] == name]
            assert len(selected) == 2, (name, selected)
            delivered = json.dumps(selected[-1]["request"]["messages"])
            assert ("PRIVATE_SKILL_BODY" in delivered) == (name == "signed"), (name, delivered)
            status = json.loads(subprocess.check_output([os.environ["AW_TEST_BIN"], "status", "--socket",
                str(root / "state/aw.sock")], timeout=5))["result"]
            assert status["events"] == status["resources"]["usage"]["invocations"] == 0, status
            cases.append(name)
        print(json.dumps({"cases": cases, "case_count": len(cases), "sdk_version": version("agentscope")}))
    finally:
        for model in models:
            await model.client.close()


def run(aw, provider, output):
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent / "fixtures/agentscope/skill_scanner.py"
    with tempfile.TemporaryDirectory(prefix="aw-as-skill-") as temporary:
        root = Path(temporary)
        for name in CASES:
            directory = root / name
            directory.mkdir()
            (directory / "SKILL.md").write_text(f"---\nname: {name}\ndescription: fixture\n---\nPRIVATE_SKILL_BODY\n")
        class Model(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                name = (root / "case").read_text()
                with (root / "models.jsonl").open("a") as log:
                    log.write(json.dumps({"case": name, "request": request}) + "\n")
                tool = request["messages"][-1]["role"] != "tool"
                message = {"role": "assistant", "content": "done"}
                if tool:
                    message.update(content=None, tool_calls=[{"id": "call-" + name, "type": "function",
                        "function": {"name": "Skill", "arguments": json.dumps({"skill": name})}}])
                body = json.dumps({"id": "fixture", "object": "chat.completion", "created": 0, "model": "fixture",
                    "choices": [{"index": 0, "message": message, "finish_reason": "tool_calls" if tool else "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        config = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "skill-native"}, "spec": {
            "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(root / "state")},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"sdk": {"adapter": "agentscope", "argv": [sys.executable, str(Path(__file__).resolve()), "--worker"]}},
            "providers": {"security": {"protocol": "aw-provider/v1alpha1", "transport": {"type": "stdio", "location": "agent",
                "argv": [str(provider), "--cli", str(fixture), "--socket", str(root / "ledger.sock")]},
                "timeout_ms": 800, "max_output_bytes": 4096, "config": {"version": 1, "mode": "block", "skill": True}}},
            "events": {"tool.before": {"enabled": True, "required": True, "steps": [{"id": "trust", "provider": "security",
                "operation": "check_skill", "match": {"tools": ["native:agentscope:Skill"]}, "effects": ["observe", "block"], "on_error": "block"}]}}}}
        path = root / "aw.json"
        path.write_text(json.dumps(config))
        processes = []
        try:
            with (output / "serve.log").open("wb") as log:
                service = subprocess.Popen([str(aw), "serve", "--config", str(path), "--state-dir", str(root / "state")], stdout=log, stderr=log, start_new_session=True)
                processes.append(service)
                deadline = time.monotonic() + 15
                while not (root / "state/aw.sock").exists():
                    assert service.poll() is None and time.monotonic() < deadline
                    time.sleep(.02)
                with (output / "worker.stdout").open("wb") as stdout, (output / "worker.stderr").open("wb") as stderr:
                    child = subprocess.Popen([str(aw), "run", "--config", str(path), "--agent", "sdk"], cwd=root,
                        env={**{key: value for key, value in os.environ.items() if not key.lower().endswith("_proxy")},
                             "AW_TEST_ROOT": str(root), "AW_TEST_BIN": str(aw),
                             "AW_TEST_MODEL": f"http://127.0.0.1:{server.server_port}/v1"},
                        stdout=stdout, stderr=stderr, start_new_session=True)
                    processes.append(child)
                    child.wait(timeout=120)
                assert child.returncode == 0, (output / "worker.stderr").read_text()
                result = json.loads((output / "worker.stdout").read_text())
                status = json.loads(subprocess.check_output([str(aw), "status", "--socket", str(root / "state/aw.sock")], timeout=5))["result"]
                assert status["bindings"] == status["events"] == 0, status
                result.update(bindings=0, events=0)
                (output / "result.json").write_text(json.dumps(result, indent=2))
        finally:
            for process in reversed(processes):
                stop(process)
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
            for name in ["models.jsonl", "ledger.jsonl"]:
                if (root / name).exists():
                    (output / name).write_bytes((root / name).read_bytes())
            (output / "processes.json").write_text(json.dumps([{"pid": process.pid, "returncode": process.poll()}
                                                              for process in processes]))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--worker", action="store_true")
    parser.add_argument("--aw", type=Path)
    parser.add_argument("--sec-core-provider", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    assert version("agentscope") == "2.0.6"
    if args.worker:
        asyncio.run(worker())
    else:
        run(args.aw.resolve(), args.sec_core_provider.resolve(), args.output.resolve())
