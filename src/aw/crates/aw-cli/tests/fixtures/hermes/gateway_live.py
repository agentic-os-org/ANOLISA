"""Require model adoption of an AW replacement through Hermes 0.23.1 CLI/Gateway."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
import time


parser = argparse.ArgumentParser()
parser.add_argument("--aw", type=Path, required=True)
parser.add_argument("--hermes", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--attach", action="store_true")
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
requests = []
deny_marker = None
parallel = threading.Barrier(2, timeout=40)


class Model(BaseHTTPRequestHandler):
    def log_message(self, *_args: object) -> None:
        pass

    def do_GET(self) -> None:
        self.send_json({"data": [{"id": "aw-gateway-fixture", "object": "model"}]})

    def do_POST(self) -> None:
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        results = [message.get("content") for message in request.get("messages", [])
                   if message.get("role") == "tool"]
        requests.append(results)
        if results:
            if args.attach and any("AW_PARALLEL" in str(message.get("content"))
                                   for message in request.get("messages", [])
                                   if message.get("role") == "user"):
                parallel.wait()
            delta, finish = {"role": "assistant", "content": "Complete"}, "stop"
        else:
            denied = any("AW_DENY_TEST" in json.dumps(message)
                         for message in request.get("messages", []) if message.get("role") == "user")
            command = (f"printf AW_DENY_TEST > {shlex.quote(str(deny_marker))}" if denied
                       else "printf HERMES_TOOL_OK")
            delta = {"role": "assistant", "tool_calls": [{
                "index": 0, "id": "call-gateway-fixture", "type": "function",
                "function": {"name": "terminal", "arguments": json.dumps({"command": command})},
            }]}
            finish = "tool_calls"
        chunks = [
            {"id": "gateway-fixture", "object": "chat.completion.chunk", "created": 1,
             "model": "aw-gateway-fixture", "choices": [{"index": 0, "delta": delta,
                                                      "finish_reason": None}]},
            {"id": "gateway-fixture", "object": "chat.completion.chunk", "created": 1,
             "model": "aw-gateway-fixture", "choices": [{"index": 0, "delta": {},
                                                      "finish_reason": finish}]},
        ]
        body = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks)
                + "data: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def send_json(self, value: dict) -> None:
        body = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def run(argv: list[str], cwd: Path, env: dict[str, str], log: Path,
        timeout: int = 120) -> subprocess.CompletedProcess:
    with log.open("w") as stream:
        child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                 stdout=stream, stderr=subprocess.STDOUT,
                                 start_new_session=True)
        try:
            child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGTERM)
            child.wait(timeout=5)
            raise
        return subprocess.CompletedProcess(argv, child.returncode)


def test_attachment(stack, env, root, profile, work, state, config_path):
    gateway_env = {**env, "AW_HERMES_GATEWAY_CONFIG": str(config_path),
                   "AW_HERMES_GATEWAY_BINARY": str(args.aw.resolve())}
    with (args.output / "gateway.log").open("w") as stream:
        gateway = subprocess.Popen(
            [str(args.hermes), "gateway", "--accept-hooks", "run"],
            cwd=work, env=gateway_env, stdin=subprocess.DEVNULL, stdout=stream,
            stderr=subprocess.STDOUT, start_new_session=True)

    def stop_gateway():
        if gateway.poll() is None:
            os.killpg(gateway.pid, signal.SIGTERM)
            try:
                gateway.wait(timeout=40)
            except subprocess.TimeoutExpired:
                os.killpg(gateway.pid, signal.SIGKILL)
                gateway.wait(timeout=5)
                raise

    stack.callback(stop_gateway)
    log = profile / "logs" / "gateway.log"
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        if log.exists() and "AW Hermes attach ready" in log.read_text():
            break
        if gateway.poll() is not None:
            raise RuntimeError("native Hermes Gateway exited before attachment")
        time.sleep(.05)
    else:
        if log.exists():
            (args.output / "gateway-runtime.log").write_bytes(log.read_bytes())
        raise TimeoutError("native Hermes Gateway attachment")
    assert len(list(state.glob("native-*"))) == 1

    def chat(query, cwd, name):
        argv = [str(args.hermes), "chat", "--cli", "--oneshot", "--quiet",
                "--max-turns", "4", "--run-budget", "120", "--toolsets", "terminal", "--query", query]
        result = run(argv, cwd, env, args.output / (name + ".log"), 150)
        assert result.returncode == 0, (name, (args.output / (name + ".log")).read_text())

    with ThreadPoolExecutor(max_workers=2) as pool:
        left = pool.submit(chat, "AW_PARALLEL left terminal", work, "left")
        right = pool.submit(chat, "AW_PARALLEL right terminal", work, "right")
        left.result(timeout=160)
        right.result(timeout=160)
    original = config_path.read_bytes()
    candidate = json.loads(original)
    candidate["spec"]["providers"]["policy"]["config"]["replacement"] = "UPDATED_POLICY_REPLACEMENT"
    next_policy = root / "next-policy.json"
    next_policy.write_text(json.dumps(candidate))
    update = run([str(args.aw), "update", "--config", str(config_path), "--policy",
                  str(next_policy), "--agent", "hermes"], work, env, args.output / "update.log", 35)
    assert update.returncode == 0, (args.output / "update.log").read_text()
    workspace = root / "other-workspace"
    workspace.mkdir()
    chat("Use terminal from another workspace", workspace, "workspace")
    events = [json.loads(line) for line in (root / "provider.jsonl").read_text().splitlines()]
    before = [event for event in events if event["name"] == "tool.before"]
    after = [event for event in events if event["name"] == "tool.after"]
    cwd = [event["native"]["cwd"] for event in before]
    scopes = [event["agent"]["instance_id"] for event in before]
    model_visible = [output for group in requests for output in group]
    if log.exists():
        (args.output / "gateway-runtime.log").write_bytes(log.read_bytes())
    (args.output / "model-requests.json").write_text(json.dumps(requests, indent=2))
    status = run([str(args.aw), "service", "status", "--config", str(config_path)],
                 work, env, args.output / "scope-status.log", 20)
    assert status.returncode == 0
    bindings = json.loads((args.output / "scope-status.log").read_text())["result"]["bindings"]
    scopes_released = bindings == 1 and not list(state.glob("native-*/workspace-*"))
    # The native stop command's process scan recognizes the adjacent "gateway run" form.
    gateway.send_signal(signal.SIGINT)
    gateway.wait(timeout=10)
    report = {
        "status": "passed",
        "parallel_tools": len(set(scopes)) == 3 and cwd.count(str(work)) == 2,
        "alternate_workspace": cwd.count(str(workspace)) == 1
        and all(event["fixture_provider_cwd"] == event["native"]["cwd"] for event in events),
        "pre_events": len(before),
        "after_events": len(after),
        "model_saw_replacement": len(model_visible) == 3
        and all("MODEL_VISIBLE_REPLACEMENT" in str(result) for result in model_visible[:2]),
        "updated_workspace_policy": config_path.read_bytes() == original
        and "UPDATED_POLICY_REPLACEMENT" in str(model_visible[-1])
        and before[0]["fixture_revision"] != before[-1]["fixture_revision"],
        "workspace_scopes_released": scopes_released,
        "launch_assets_released": not list(state.glob("native-*")),
        "gateway_stopped": gateway.returncode == 0,
    }
    assert report == {
        "status": "passed", "parallel_tools": True, "alternate_workspace": True,
        "pre_events": 3, "after_events": 3, "model_saw_replacement": True,
        "updated_workspace_policy": True, "workspace_scopes_released": True,
        "launch_assets_released": True, "gateway_stopped": True,
    }, report
    (args.output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))


def main() -> None:
    global deny_marker
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    daemon = None
    try:
        with ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="aw-h-gw-")))
            profile, home, work, state = (root / name for name in
                                          ("profile", "home", "work", "state"))
            for path in (profile, home, work):
                path.mkdir(mode=0o700)
            deny_marker = work / "denied-marker"
            native = {
                "model": {"provider": "custom", "default": "aw-gateway-fixture",
                          "key_env": "OPENAI_API_KEY",
                          "base_url": f"http://127.0.0.1:{server.server_port}/v1",
                          "api_mode": "chat_completions"},
                "updates": {"check": False},
                "terminal": {"backend": "local", "cwd": str(work)},
                "memory": {"memory_enabled": False, "user_profile_enabled": False},
                "database": {"journal_mode": "delete"},
                "plugins": {"enabled": [], "disabled": [], "hook_callback_timeout": 80},
                "approvals": {"single_query_mode": "approve"},
            }
            (profile / "config.yaml").write_text(json.dumps(native))
            (profile / "config.yaml").chmod(0o600)
            policy = Path(__file__).with_name("gateway_policy.py").resolve()
            config = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
                      "metadata": {"name": "hermes-gateway-native"}, "spec": {
                          "daemon": {"startup": "external", "endpoint": "auto",
                                     "state_dir": str(state)},
                          "execution": {"guarantee": "native_hook",
                                        "default_event_budget_ms": 5000},
                          "audit": {"enabled": True, "payload": "metadata_only"},
                          "agents": {"hermes": {"adapter": "hermes", "argv": [
                              str(args.hermes), "chat", "--cli"]}},
                          "providers": {"policy": {"protocol": "aw-provider/v1alpha1",
                              "transport": {"type": "stdio", "location": "agent",
                                            "argv": [sys.executable, str(policy), str(root)]},
                              "timeout_ms": 2000, "max_output_bytes": 65536, "config": {}}},
                          "events": {
                              "tool.before": {"enabled": True, "required": True, "steps": [{
                                  "id": "check", "provider": "policy", "operation": "check",
                                  "effects": ["observe", "block"], "on_error": "block"}]},
                              "tool.after": {"enabled": True, "required": True, "steps": [{
                                  "id": "record", "provider": "policy", "operation": "record",
                                  "effects": ["observe", "replace_result"], "on_error": "report"}]},
                          }}}
            config_path = root / "aw.json"
            config_path.write_text(json.dumps(config))
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(("HERMES_", "OPENAI_", "AW_HERMES_"))}
            env.update(HOME=str(home), HERMES_HOME=str(profile),
                       XDG_STATE_HOME=str(root / "xdg-state"),
                       XDG_CONFIG_HOME=str(home / ".config"),
                       HERMES_SAFE_MODE="0", OPENAI_API_KEY="fixture-key",
                       PYTHONDONTWRITEBYTECODE="1",
                       NO_PROXY="127.0.0.1,localhost", no_proxy="127.0.0.1,localhost",
                       GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null")
            env["PATH"] = "/usr/bin:/bin"
            def stop_gateway() -> None:
                try:
                    run([str(args.hermes), "gateway", "stop"], work, env,
                        args.output / "gateway-cleanup.log", 30)
                except (OSError, subprocess.TimeoutExpired):
                    pass

            base = [str(args.aw), "--config", str(config_path), "--agent", "hermes",
                    "--native-profile", str(profile)]
            def checked(argv: list[str], name: str, timeout: int = 120) -> None:
                result = run(argv, work, env, args.output / f"{name}.log", timeout)
                if result.returncode:
                    for log in profile.rglob("*.log"):
                        (args.output / ("hermes-" + log.name)).write_bytes(log.read_bytes())
                    raise RuntimeError(f"{name} exited {result.returncode}: " +
                                       (args.output / f"{name}.log").read_text()[-1200:])

            checked([base[0], "connect", *base[1:]], "connect")
            with (args.output / "daemon.log").open("w") as stream:
                daemon = subprocess.Popen([str(args.aw), "service", "serve", "--config", str(config_path),
                                           "--state-dir", str(state)], cwd=work, env=env,
                                          stdin=subprocess.DEVNULL, stdout=stream,
                                          stderr=subprocess.STDOUT, start_new_session=True)
            def stop_daemon() -> None:
                if daemon.poll() is None:
                    os.killpg(daemon.pid, signal.SIGTERM)
                    daemon.wait(timeout=5)

            stack.callback(stop_daemon)
            if not args.attach:
                stack.callback(stop_gateway)
            deadline = time.monotonic() + 10
            while not (state / "aw.sock").exists() and time.monotonic() < deadline:
                if daemon.poll() is not None:
                    raise RuntimeError("AW daemon exited before readiness")
                time.sleep(0.05)
            if not (state / "aw.sock").exists():
                raise TimeoutError("AW daemon readiness timed out")
            if args.attach:
                test_attachment(stack, env, root, profile, work, state, config_path)
                return
            checked([base[0], "run", *base[1:], "--", "--oneshot", "--quiet",
                     "--max-turns", "4", "--run-budget", "60", "--toolsets", "terminal",
                     "--query", "Use the terminal tool once."], "chat", 180)
            checked([base[0], "run", *base[1:], "--", "--oneshot", "--quiet",
                     "--max-turns", "4", "--run-budget", "60", "--toolsets", "terminal",
                     "--query", "Use the terminal tool once for AW_DENY_TEST."], "chat-denied", 180)
            (args.output / "model-requests.json").write_text(json.dumps(requests, indent=2))
            for journal in state.rglob("*.jsonl"):
                (args.output / journal.name).write_bytes(journal.read_bytes())
            for log in profile.rglob("*.log"):
                (args.output / ("hermes-" + log.name)).write_bytes(log.read_bytes())
            events = [json.loads(line) for line in (root / "provider.jsonl").read_text().splitlines()]
            result_text = json.dumps(requests)
            chat_logs = [(args.output / name).read_text() for name in ("chat.log", "chat-denied.log")]
            policy_blocks = sum(
                json.loads(line).get("record", {}).get("policy_block") is True
                for journal in state.rglob("*.jsonl") for line in journal.read_text().splitlines())
            checked([str(args.hermes), "gateway", "stop"], "gateway-stop", 30)
            checked([base[0], "service", "status", "--config", str(config_path)], "status")
            status = json.loads((args.output / "status.log").read_text())
            checked([base[0], "service", "stop", "--config", str(config_path)], "aw-stop")
            daemon.wait(timeout=10)
            report = {"status": "passed", "model_saw_replacement":
                      "MODEL_VISIBLE_REPLACEMENT" in result_text,
                      "model_saw_original": "HERMES_TOOL_OK" in result_text,
                      "denied_marker_absent": not deny_marker.exists(),
                      "denied_policy_blocks": policy_blocks,
                      "gateway_confirmation_logged": all(
                          "AW hermes CLI plugin ready" in log
                          and "AW hermes Gateway callbacks active" in log for log in chat_logs),
                      "pre_events": sum(event["name"] == "tool.before" for event in events),
                      "after_events": sum(event["name"] == "tool.after" for event in events),
                      "instance_released": status["result"]["bindings"] == 0,
                      "gateway_stopped": True}
            assert report == {"status": "passed", "model_saw_replacement": True,
                              "model_saw_original": False, "denied_marker_absent": True,
                              "denied_policy_blocks": 1,
                              "gateway_confirmation_logged": True,
                              "pre_events": 2, "after_events": 1, "instance_released": True,
                              "gateway_stopped": True}, report
            (args.output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report))
    finally:
        if daemon is not None and daemon.poll() is None:
            os.killpg(daemon.pid, signal.SIGTERM)
            daemon.wait(timeout=5)
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    main()
