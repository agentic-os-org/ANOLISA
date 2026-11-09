#!/usr/bin/env python3
"""Native Hermes Skill delivery through AW/Provider and a controlled Ledger/model."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
import time

# The real Provider invokes this controlled Ledger CLI; no signing is simulated.
if "skill-ledger" in sys.argv:
    assert sys.argv[-3:-1] == ["skill-ledger", "check"]
    source = Path(sys.argv[-1])
    assert source.is_absolute() and (source / "SKILL.md").is_file()
    if source.name == "timeout":
        time.sleep(30)
    if source.name == "unavailable":
        sys.exit(3)
    status = {"signed": "pass", "unsigned": "none", "backend-denied": "error"}.get(source.name, source.name)
    print(json.dumps({"status": status}))
    sys.exit(0 if status == "pass" else 1)

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "sec_core"))
from runtime import Processes, await_condition, checked, save

CASES = ("signed", "unsigned", "drifted", "tampered", "backend-denied", "unavailable", "timeout",
         "input-denied", "input-allow-skill-denied", "input-allow-skill-allow")


class Model(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        self.server.posts += 1
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if "messages" not in request:
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"{}")
            return
        if "tools" in request:
            self.server.requests.append(request)
        complete = "tools" not in request or any(message.get("role") == "tool" for message in request["messages"])
        message = {"role": "assistant", "content": "done" if complete else None}
        if not complete:
            message["tool_calls"] = [{"id": "skill-call", "type": "function", "function": {
                "name": "skill_view", "arguments": json.dumps({"name": self.server.case})}}]
        choice = {"index": 0, "finish_reason": "stop" if complete else "tool_calls"}
        if request.get("stream"):
            if not complete:
                message["tool_calls"][0]["index"] = 0
            body = {"id": "fixture", "object": "chat.completion.chunk", "created": 1, "model": "fixture",
                    "choices": [{**choice, "delta": message}]}
            data = ("data: " + json.dumps(body) + "\n\ndata: [DONE]\n\n").encode()
            content_type = "text/event-stream"
        else:
            data = json.dumps({"id": "fixture", "object": "chat.completion", "created": 1, "model": "fixture",
                "choices": [{**choice, "message": message}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
            content_type = "application/json"
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main(args):
    sys.path.insert(0, str(args.source))
    import hermes_yaml
    args.output.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    server.requests = []
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="aw-hermes-skills-") as temporary:
            root = Path(temporary)
            for name in ("work", "home", "profile"):
                (root / name).mkdir(mode=0o700)
            profile = root / "profile"
            url = f"http://127.0.0.1:{server.server_port}/v1"
            (profile / "config.yaml").write_text(hermes_yaml.safe_dump({
                "updates": {"check": False}, "memory": {"memory_enabled": False, "user_profile_enabled": False},
                "model": {"provider": "custom", "default": "fixture", "key_env": "AW_FIXTURE_KEY", "base_url": url,
                          "api_mode": "chat_completions"}, "plugins": {"enabled": [], "hook_callback_timeout": 60}}))
            (profile / "config.yaml").chmod(0o600)
            for name in CASES:
                directory = profile / "skills" / name
                directory.mkdir(parents=True)
                (directory / "SKILL.md").write_text(f"---\nname: {name}\ndescription: fixture\n---\nPRIVATE_SKILL_BODY\n")
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(("AW_", "HERMES_", "OPENAI_")) and not key.lower().endswith("_proxy")}
            env.update(PATH=str(args.hermes.parent) + ":/usr/bin:/bin", HOME=str(root / "home"), HERMES_HOME=str(profile), AW_FIXTURE_KEY="fixture",
                       OPENAI_API_KEY="fixture", OPENAI_BASE_URL=url, PYTHONDONTWRITEBYTECODE="1", FAKE_ROOT=str(root))
            processes = Processes(root, args.output, env)
            reports = []
            assets = []
            try:
                for case in CASES:
                    combined = case.startswith("input-")
                    skill = "unsigned" if case == "input-allow-skill-denied" else "signed" if combined else case
                    server.case, server.requests, server.posts = skill, [], 0
                    state = root / ("state-" + case)
                    config = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "skill"}, "spec": {
                        "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                        "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                        "audit": {"enabled": True, "payload": "metadata_only"},
                        "agents": {"hermes": {"adapter": "hermes", "argv": [str(args.hermes), "chat", "--oneshot",
                            "--max-turns", "3", "--run-budget", "60", "--provider", "custom", "--model", "fixture",
                            "--accept-hooks", "--ignore-rules", "--toolsets", "skills", "--quiet", "--query", "Load the requested Skill once."]}},
                        "providers": {"security": {"protocol": "aw-provider/v1alpha1", "transport": {"type": "stdio", "location": "agent",
                            "argv": [str(args.provider), "--cli", str(Path(__file__).resolve()), "--socket", str(root / "ledger.sock")]},
                            "timeout_ms": 800, "max_output_bytes": 4096, "config": {"version": 1, "mode": "block", "skill": True}}},
                        "events": {"tool.before": {"enabled": True, "required": True, "steps": [{"id": "trust", "provider": "security",
                            "operation": "check_skill", "match": {"tools": ["native:hermes:skill_view"]}, "effects": ["observe", "block"], "on_error": "block"}]}}}}
                    if combined:
                        scanner = Path(__file__).resolve().parent.parent / "pi" / "submission-scanner.py"
                        config["spec"]["providers"]["input"] = {
                            "protocol": "aw-provider/v1alpha1",
                            "transport": {"type": "stdio", "location": "agent", "argv": [
                                str(args.provider), "--cli", str(scanner), "--socket", str(root / "scanner.sock")]},
                            "timeout_ms": 2000, "max_output_bytes": 4096,
                            "config": {"version": 1, "mode": "block", "prompt": {"mode": "block", "scan_mode": "fast"}}}
                        config["spec"]["events"]["input.submit"] = {
                            "enabled": True, "required": True, "steps": [{"id": "prompt", "provider": "input",
                                "operation": "scan_prompt", "effects": ["observe", "block"], "on_error": "block"}]}
                        config["spec"]["agents"]["hermes"]["argv"][-1] = "risk" if case == "input-denied" else "hello"
                    if args.native_site_packages:
                        config["spec"]["agents"]["hermes"]["argv"].insert(2, "--cli")
                    path = root / "aw.json"
                    save(path, config)
                    base = [args.aw, "--config", path, "--agent", "hermes", "--native-profile", profile]
                    if case == CASES[0]:
                        child = processes.start("install", [base[0], "install", *base[1:]])
                        assert processes.wait(child, 60) == 0
                        processes.stop(child)
                    service = processes.start("service-" + case, [args.aw, "serve", "--config", path, "--state-dir", state])
                    await_condition(lambda: (state / "aw.sock").exists(), 10, "AW socket")
                    if args.native_site_packages:
                        if not assets:
                            setup = json.loads(checked([args.aw, "instance", "setup", *base[1:], "--directory", args.native_site_packages], env=env).stdout)
                            assets = [args.native_site_packages / name for name in ("aw-native-bootstrap.pth", "aw_native_startup.py", "aw-native-bootstrap.json")]
                            env.update(setup["environment"])
                        child = processes.start("native-" + case, config["spec"]["agents"]["hermes"]["argv"])
                    else:
                        child = processes.start("native-" + case, [base[0], "run", *base[1:]])
                    assert processes.wait(child, 120) == (2 if case == "input-denied" else 0), case
                    save(args.output / (case + "-model.json"), server.requests)
                    save(args.output / (case + "-audit.json"), [json.loads(line) for file in state.rglob("*.jsonl") for line in file.read_text().splitlines()])
                    denied = case == "input-denied"
                    assert len(server.requests) == (0 if denied else 2), (case, len(server.requests))
                    if denied:
                        assert server.posts == 0, server.posts
                    if combined:
                        scans = [json.loads(line) for line in (root / "scanner.jsonl").read_text().splitlines()]
                        assert scans[-1]["text"] == ("risk" if denied else "hello"), scans
                    delivered = json.dumps(server.requests[-1]["messages"]) if server.requests else ""
                    expected_delivery = skill == "signed" and not denied
                    assert ("PRIVATE_SKILL_BODY" in delivered) == expected_delivery, (case, delivered)
                    status = json.loads(checked([args.aw, "status", "--socket", state / "aw.sock"], env=env).stdout)["result"]
                    assert status["bindings"] == status["events"] == 0, status
                    reports.append({"case": case, "instructions_delivered": expected_delivery,
                                    "model_requests": len(server.requests), "http_posts": server.posts})
                    processes.stop(child)
                    processes.stop(service)
                save(args.output / "result.json", {"case_count": len(reports), "cases": reports, "bindings": 0, "events": 0})
            finally:
                processes.close()
                for asset in assets:
                    asset.unlink(missing_ok=True)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    for name in ("aw", "provider", "hermes", "source", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--native-site-packages", type=Path)
    main(parser.parse_args())
