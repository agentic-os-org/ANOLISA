"""Run the installed native Hermes command through AW against a local model."""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time


class Model(BaseHTTPRequestHandler):
    def log_message(self, *_args: object) -> None:
        pass

    def do_GET(self) -> None:
        self.reply({"data": [{"id": "aw-fixture", "object": "model"}]})

    def reply(self, value: dict) -> None:
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self) -> None:
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if not isinstance(payload.get("messages"), list):
            self.send_error(404, "Fixture supports chat completions only")
            return
        self.server.calls += 1
        last_user = max((index for index, row in enumerate(payload["messages"])
                         if row.get("role") == "user"), default=-1)
        complete = any(row.get("role") == "tool" for row in payload["messages"][last_user + 1:])
        if complete:
            self.server.deliveries.append("PRIVATE_SKILL_BODY" in json.dumps(payload["messages"][last_user + 1:]))
            self.server.turns += 1
            if self.server.label == "ALLOW":
                assert self.server.release.wait(timeout=15), "fixture completion was not released"
        auxiliary = not payload.get("tools")
        message = {"role": "assistant", "content": "Fixture complete" if complete or auxiliary else None}
        if not complete and not auxiliary:
            message["tool_calls"] = [{"id": "fixture-call", "type": "function", "function": {
                "name": "terminal", "arguments": json.dumps({"command": "printf AW_" + self.server.label + " > marker"})}}]
        if not complete and not auxiliary and self.server.skill:
            message["tool_calls"][0]["function"] = {"name": "skill_view", "arguments": json.dumps({"name": "unsigned-again" if self.server.label == "RESTORE" else "unsigned"})}
        finish = "stop" if complete or auxiliary else "tool_calls"
        if payload.get("stream"):
            if not complete and not auxiliary:
                message["tool_calls"][0]["index"] = 0
            data = {"id": "fixture", "object": "chat.completion.chunk", "created": 1,
                    "model": "aw-fixture", "choices": [{"index": 0, "delta": message, "finish_reason": finish}]}
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(("data: " + json.dumps(data) + "\n\ndata: [DONE]\n\n").encode())
        else:
            self.reply({"id": "fixture", "object": "chat.completion", "created": 1, "model": "aw-fixture",
                        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--hermes", type=Path, required=True)
    parser.add_argument("--site-packages", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--interactive", action="store_true")
    parser.add_argument("--skill-provider", type=Path)
    args = parser.parse_args()
    assert not args.skill_provider or args.interactive, "Skill update requires interactive mode"
    args.output.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    server.calls = 0
    server.turns = 0
    server.label = "ALLOW"
    server.skill = bool(args.skill_provider)
    server.deliveries = []
    server.release = threading.Event()
    if not args.interactive:
        server.release.set()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    assets = []
    daemon = None
    results = []
    try:
        with tempfile.TemporaryDirectory(prefix="aw-hermes-native-") as directory:
            root = Path(directory)
            profile, home, work, state = [root / name for name in ("profile", "home", "work", "state")]
            for path in (profile, home, work, state):
                path.mkdir(mode=0o700)
            native = {"updates": {"check": False}, "model": {"provider": "custom", "default": "aw-fixture",
                      "key_env": "AW_FIXTURE_KEY", "base_url": f"http://127.0.0.1:{server.server_port}/v1", "api_mode": "chat_completions"},
                      "terminal": {"backend": "local", "cwd": str(work)},
                      "memory": {"memory_enabled": False, "user_profile_enabled": False},
                      "plugins": {"hook_callback_timeout": 60}}
            (profile / "config.yaml").write_text(json.dumps(native))
            (profile / "config.yaml").chmod(0o600)
            argv = [str(args.hermes), "chat", "--cli", "--oneshot", "--quiet", "--max-turns", "3",
                    "--provider", "custom", "--model", "aw-fixture", "--accept-hooks", "--ignore-rules",
                    "--toolsets", "terminal", "--query", "Run the requested terminal call and finish."]
            if args.interactive:
                argv = [value for value in argv if value not in ("--oneshot", "--quiet")]
            action = Path(__file__).resolve().parent.parent / "openclaw/action.py"
            config = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "native-hermes"},
                "spec": {"daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                    "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                    "audit": {"enabled": True, "payload": "metadata_only"},
                    "agents": {"hermes": {"adapter": "hermes", "argv": argv}},
                    "providers": {"policy": {"protocol": "aw-provider/v1alpha1", "transport": {"type": "stdio", "location": "agent",
                        "argv": ["/usr/bin/python3", str(action), "--root", str(root), "--label", "policy"]},
                        "timeout_ms": 2000, "max_output_bytes": 65536, "config": {}}},
                    "events": {name: {"enabled": True, "steps": [{"id": name, "provider": "policy",
                        "operation": operation, "effects": effects, "on_error": error}]}
                        for name, operation, effects, error in (("tool.before", "check", ["observe", "block"], "block"),
                                                               ("tool.after", "record", ["observe"], "report"))}}}
            if args.skill_provider:
                config["spec"]["execution"]["default_event_budget_ms"] = 55000
                argv[argv.index("terminal")] = "skills"
                skill = profile / "skills" / "unsigned"
                skill.mkdir(parents=True)
                (skill / "SKILL.md").write_text("---\nname: unsigned\ndescription: fixture\n---\nPRIVATE_SKILL_BODY\n")
                restored = skill.with_name("unsigned-again")
                restored.mkdir()
                (restored / "SKILL.md").write_text("---\nname: unsigned-again\ndescription: fixture\n---\nPRIVATE_SKILL_BODY\n")
                config["spec"]["providers"] = {"security": {"protocol": "aw-provider/v1alpha1",
                    "transport": {"type": "stdio", "location": "agent", "argv": [str(args.skill_provider.resolve()),
                        "--cli", str(Path(__file__).with_name("skill_cli.py").resolve()), "--socket", str(root / "ledger.sock")]},
                    "timeout_ms": 800, "max_output_bytes": 4096, "config": {"version": 1, "mode": "block", "skill": True}}}
                config["spec"]["events"] = {"tool.before": {"enabled": True, "steps": [{"id": "trust", "provider": "security",
                    "operation": "check_skill", "match": {"tools": ["native:hermes:skill_view"]},
                    "effects": ["observe", "block"], "on_error": "block"}]}}
            config["spec"]["agents"]["hermes"]["evidence"] = {
                "source_id": "fixture", "endpoint": "http://127.0.0.1:1", "mode": "auto",
                "query_timeout_ms": 100, "query_bytes": 1048576, "max_items": 32, "max_bytes": 262144}
            enabled = json.loads(json.dumps(config))
            if args.interactive:
                config["spec"]["events"]["tool.before"]["enabled"] = False
            path = root / "aw.json"
            path.write_text(json.dumps(config))
            env = {key: value for key, value in os.environ.items() if not key.startswith(("HERMES_", "AW_", "OPENAI_"))}
            env.update(PATH=str(args.hermes.parent) + ":/usr/bin:/bin", HOME=str(home), HERMES_HOME=str(profile), AW_FIXTURE_KEY="local-fixture-only",
                       OPENAI_API_KEY="local-fixture-only", OPENAI_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                       PYTHONDONTWRITEBYTECODE="1", NO_PROXY="127.0.0.1,localhost")
            def run(command: list[str], label: str, environment: dict = env) -> subprocess.CompletedProcess:
                result = subprocess.run(command, cwd=work, env=environment, stdin=subprocess.DEVNULL,
                                        capture_output=True, timeout=100, check=False)
                (args.output / (label + ".log")).write_bytes(result.stdout + result.stderr)
                assert result.returncode == 0, (label, result.stderr.decode()[-2000:])
                return result
            run([str(args.aw), "install", "--config", str(path), "--agent", "hermes", "--native-profile", str(profile)], "install")
            original = (profile / "config.yaml").read_bytes()
            daemon_log = (args.output / "daemon.log").open("wb")
            daemon = subprocess.Popen([str(args.aw), "serve", "--config", str(path), "--state-dir", str(state)], cwd=work, env=env,
                                      stdout=daemon_log, stderr=subprocess.STDOUT, start_new_session=True)
            deadline = time.monotonic() + 10
            while not (state / "aw.sock").exists():
                assert daemon.poll() is None and time.monotonic() < deadline
                time.sleep(0.05)
            setup = json.loads(run([str(args.aw), "instance", "setup", "--config", str(path), "--agent", "hermes",
                "--directory", str(args.site_packages), "--native-profile", str(profile)], "setup").stdout)
            assets = [args.site_packages / name for name in ("aw-native-bootstrap.pth", "aw_native_startup.py", "aw-native-bootstrap.json")]
            native_env = {**env, **setup["environment"]}
            if args.interactive:
                from native_interactive import verify
                proof = verify(args, argv, native_env, work, enabled, server)
                assert (profile / "config.yaml").read_bytes() == original
                status = json.loads(run([str(args.aw), "status", "--config", str(path)], "status-interactive").stdout)
                assert status["result"]["bindings"] == 0 and status["result"]["events"] == 0
                assert status["result"]["observations"] == []
                proof.update(bindings_after_exit=0, events_after_exit=0, observations_after_exit=0, native_config_preserved=True)
                (args.output / "result.json").write_text(json.dumps(proof, indent=2))
                return
            for label in ("ALLOW", "DENY"):
                server.label = label
                count = server.calls
                previous = set(root.glob("provider-*.json"))
                (work / "marker").unlink(missing_ok=True)
                run(argv, label.lower(), native_env)
                executed = (work / "marker").exists()
                assert executed == (label == "ALLOW")
                assert (profile / "config.yaml").read_bytes() == original
                assert server.calls - count >= 2
                invocations = [json.loads(item.read_text()) for item in set(root.glob("provider-*.json")) - previous]
                invocations = [row for row in invocations if row.get("method") == "invoke"]
                assert {row["event"]["name"] for row in invocations} == {"tool.before", "tool.after"}
                assert all(label in row["event"]["tool"]["input"]["command"] for row in invocations)
                status = json.loads(run([str(args.aw), "status", "--config", str(path)], "status-" + label).stdout)
                assert status["result"]["bindings"] == 0
                results.append({"case": label, "model_calls": server.calls - count, "tool_executed": executed, "instance_released": True})
            (args.output / "result.json").write_text(json.dumps({"status": "passed", "native_command": True, "cases": results}, indent=2))
    finally:
        server.release.set()
        if daemon is not None and daemon.poll() is None:
            os.killpg(daemon.pid, signal.SIGTERM)
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(daemon.pid, signal.SIGKILL)
                daemon.wait(timeout=5)
        for path in assets:
            path.unlink(missing_ok=True)
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
