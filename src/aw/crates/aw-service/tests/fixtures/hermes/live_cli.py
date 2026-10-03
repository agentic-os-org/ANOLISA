"""Run official Hermes chat against a deterministic local model endpoint and AW."""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time


parser = argparse.ArgumentParser()
parser.add_argument("--aw", type=Path, required=True)
parser.add_argument("--provider", type=Path, required=True)
parser.add_argument("--hermes", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--source", type=Path, required=True)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
sys.dont_write_bytecode = True
sys.path.insert(0, str(args.source))
import hermes_yaml


class Model(BaseHTTPRequestHandler):
    def log_message(self, *_args: object) -> None:
        pass

    def do_GET(self) -> None:
        data = json.dumps({"data": [{"id": "aw-fixture", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self) -> None:
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        call = {"id": "call-fixture", "type": "function", "function": {
            "name": "terminal", "arguments": json.dumps({"command": "printf adopted > marker.txt"}),
        }}
        complete = any(message.get("role") == "tool" for message in payload.get("messages", []))
        message = {"role": "assistant", "content": "Fixture complete" if complete else None}
        if not complete:
            message["tool_calls"] = [call]
        self.server.requests.append({"path": self.path, "complete": complete})
        if payload.get("stream"):
            if not complete:
                call["index"] = 0
            event = {"id": "fixture", "object": "chat.completion.chunk", "created": 1,
                     "model": "aw-fixture", "choices": [{"index": 0, "delta": message,
                     "finish_reason": "stop" if complete else "tool_calls"}]}
            data = ("data: " + json.dumps(event) + "\n\ndata: [DONE]\n\n").encode()
            content_type = "text/event-stream"
        else:
            data = json.dumps({"id": "fixture", "object": "chat.completion", "created": 1,
                "model": "aw-fixture", "choices": [{"index": 0, "message": message,
                "finish_reason": "stop" if complete else "tool_calls"}],
                "usage": {"prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30}}).encode()
            content_type = "application/json"
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main() -> None:
    owned = []
    records = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    server.requests = []
    server.timeout = 0.5
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    ledger = args.output / "processes.json"

    def save() -> None:
        ledger.write_text(json.dumps({"model_fixture_pid": os.getpid(),
            "model_fixture_port": server.server_port, "processes": records}, indent=2))

    def start(argv: list[str], name: str, cwd: Path, env: dict) -> subprocess.Popen:
        log = (args.output / (name + ".log")).open("w")
        child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        record = {"name": name, "command": argv, "cwd": str(cwd), "pid": child.pid,
            "process_group": child.pid, "ports": [], "log": str(log.name),
            "deadline_seconds": 120, "stop": f"kill -TERM -- -{child.pid}"}
        records.append(record)
        owned.append((child, record, log))
        save()
        return child

    def run(argv: list[str], name: str, cwd: Path, env: dict) -> int:
        return start(argv, name, cwd, env).wait(timeout=120)

    try:
        with tempfile.TemporaryDirectory(prefix="aw-hermes-cli-") as temporary:
            root = Path(temporary)
            profile = root / "profile"
            profile.mkdir(mode=0o700)
            home = root / "home"
            home.mkdir(mode=0o700)
            work = root / "work"
            work.mkdir()
            trace = root / "trace.jsonl"
            command = work / "existing-hook"
            shutil.copyfile(Path(__file__).with_name("native_command.py"), command)
            command.chmod(0o700)
            native = {
                "updates": {"check": False},
                "display": {"interface": "tui"},
                "model": {"provider": "custom", "default": "aw-fixture", "key_env": "AW_FIXTURE_KEY",
                    "base_url": f"http://127.0.0.1:{server.server_port}/v1", "api_mode": "chat_completions"},
                "terminal": {"backend": "local", "cwd": str(work)},
                "memory": {"memory_enabled": False, "user_profile_enabled": False},
                "database": {"journal_mode": "delete"},
                "plugins": {"enabled": [], "disabled": ["aw-native-hooks", "keep-disabled"]},
                "hooks": {name: [{"command": f"./existing-hook hook --step existing-{label}", "timeout": 5}]
                    for name, label in (("pre_tool_call", "before"), ("post_tool_call", "after"))},
                "unknown_config": {"keep": [1, True, "value"]},
            }
            original = hermes_yaml.safe_dump(native, sort_keys=False) + '''
# Keep this operator comment and native scalar meanings.
unknown_yaml:
  text_yes: "yes"
  flag_yes: yes
  text_on: "on"
  flag_on: on
  text_y: "y"
  flag_y: y
  text_n: "n"
  flag_n: n
  octal: 012
  text_octal: "012"
  duration: 1:20
  text_duration: "1:20"
  template: "${AW_FIXTURE_KEY}"
'''
            (profile / "config.yaml").write_text(original)
            (profile / "auth.json").write_text("{}\n")
            (profile / ".env").write_text("AW_HERMES_PROFILE_SENTINEL=existing\n")
            (profile / "sessions").mkdir()
            (profile / "sessions/keep.txt").write_text("existing session")
            (profile / "active_profile").write_text("unrelated-profile")
            prompt = root / "prompt.txt"
            prompt.write_text("Run the requested terminal call once and then finish.")
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(("HERMES_", "OPENAI_", "AW_HERMES_"))}
            env.update(HOME=str(home), HERMES_HOME=str(profile), AW_HERMES_TRACE=str(trace),
                HERMES_TUI="1",
                AW_FIXTURE_KEY="local-fixture-not-a-secret", OPENAI_API_KEY="local-fixture-not-a-secret",
                OPENAI_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                PYTHONDONTWRITEBYTECODE="1", NO_PROXY="127.0.0.1,localhost", no_proxy="127.0.0.1,localhost")
            results = []
            for label, blocked in (("allow", False), ("block", True)):
                state = root / ("state-" + label)
                document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": label},
                    "spec": {"daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                        "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                        "audit": {"enabled": True, "payload": "metadata_only"},
                        "agents": {"hermes": {"adapter": "hermes", "argv": [str(args.hermes), "chat",
                            "--oneshot", "--max-turns", "3", "--run-budget", "80", "--provider", "custom",
                            "--model", "aw-fixture", "--accept-hooks", "--ignore-rules", "--toolsets", "terminal",
                            "--quiet", "--query-file", str(prompt)]}}, "providers": {}, "events": {}}}
                providers = document["spec"]["providers"]
                providers["policy"] = {"protocol": "aw-provider/v1alpha1", "transport": {"type": "stdio",
                    "location": "agent", "argv": [str(args.provider)]}, "timeout_ms": 2000,
                    "max_output_bytes": 65536, "config": {"blocked_tools": ["terminal"] if blocked else []}}
                for event, suffix in (("tool.before", "before"), ("tool.after", "after")):
                    steps = []
                    for index in (1, 2):
                        identifier = f"aw-{suffix}-{index}"
                        providers[identifier] = {"protocol": "native-hook/v1alpha1", "transport": {
                            "type": "stdio", "location": "agent", "argv": ["./existing-hook", "hook", "--step", identifier,
                                "--literal", "literal ; $(touch NEVER) ' spaces"]},
                            "timeout_ms": 2000, "max_output_bytes": 65536, "config": {}}
                        steps.append({"id": identifier, "provider": identifier, "native": {}, "on_error": "report"})
                    steps.append({"id": "policy-" + suffix, "provider": "policy", "operation": "check",
                        "effects": ["observe", "block"] if suffix == "before" else ["observe"],
                        "on_error": "block" if suffix == "before" else "report"})
                    document["spec"]["events"][event] = {"enabled": True, "required": True, "steps": steps}
                config = root / (label + ".json")
                config.write_text(json.dumps(document))
                base = [str(args.aw), "--config", str(config), "--agent", "hermes", "--native-profile", str(profile)]
                if label == "allow":
                    assert run([base[0], "run", *base[1:]], "uninstalled", work, env) != 0
                    assert run([base[0], "install", *base[1:]], "install", work, env) == 0
                    installed = (profile / "config.yaml").read_bytes()
                    before = hermes_yaml.safe_load(original)
                    after = hermes_yaml.safe_load(installed)
                    assert after["plugins"]["enabled"] == ["aw-native-hooks"]
                    assert after["plugins"]["disabled"] == ["keep-disabled"]
                    before.pop("plugins")
                    after.pop("plugins")
                    assert before == after
                    assert b"# Keep this operator comment" in installed
                    assert b'text_yes: "yes"' in installed
                    assert not list(profile.glob(".aw-config-*"))
                    assert run([base[0], "install", *base[1:]], "install-again", work, env) == 0
                    assert (profile / "config.yaml").read_bytes() == installed
                daemon = start([str(args.aw), "serve", "--config", str(config), "--state-dir", str(state)],
                               "daemon-" + label, work, env)
                deadline = time.monotonic() + 10
                while not (state / "aw.sock").exists() and time.monotonic() < deadline:
                    assert daemon.poll() is None
                    time.sleep(0.05)
                assert (state / "aw.sock").exists()
                trace.unlink(missing_ok=True)
                (work / "marker.txt").unlink(missing_ok=True)
                assert run([base[0], "run", *base[1:]], label, work, env) == 0
                rows = [json.loads(line) for line in trace.read_text().splitlines()]
                result = {"case": label, "tool_executed": (work / "marker.txt").exists(),
                    "installation_preserved_yaml11": True,
                    "steps": [row["step"] for row in rows],
                    "after_status": [row["payload"]["extra"].get("status") for row in rows
                                     if row["step"] == "existing-after"],
                    "profile_retained": (profile / "sessions/keep.txt").read_text() == "existing session",
                    "config_unchanged_during_run": (profile / "config.yaml").read_bytes() == installed,
                    "native_environment_preserved": all(row["profile_sentinel"] == "existing" for row in rows),
                    "launch_directories_removed": not list(state.glob("launch-*"))}
                (args.output / (label + "-trace.json")).write_text(json.dumps(rows, indent=2))
                assert result["tool_executed"] != blocked, result
                assert result["steps"] == ["existing-before", "aw-before-1", "aw-before-2",
                                           "existing-after", "aw-after-1", "aw-after-2"], result
                assert result["after_status"] == ["blocked" if blocked else "ok"], result
                assert result["profile_retained"] and result["config_unchanged_during_run"], result
                assert result["native_environment_preserved"], result
                assert len({row["tmpdir"] for row in rows}) == 1 and rows[0]["tmpdir"]
                assert len({row["home"] for row in rows}) == 1
                assert result["launch_directories_removed"], result
                assert not (work / "NEVER").exists()
                assert all(row["literal"] == "literal ; $(touch NEVER) ' spaces" for row in rows
                           if row["step"].startswith("aw-"))
                results.append(result)
                assert run([str(args.aw), "stop", "--config", str(config)], "stop-" + label, work, env) == 0
                assert daemon.wait(timeout=10) == 0
                for journal in state.rglob("*.jsonl"):
                    shutil.copyfile(journal, args.output / (label + "-" + journal.name))
            (args.output / "result.json").write_text(json.dumps({"results": results,
                "model_requests": server.requests, "real_model_used": False}, indent=2))
            print(json.dumps(results))
    finally:
        for child, record, log in reversed(owned):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=5)
            record["exit_code"] = child.returncode
            record["pid_absent"] = not Path(f"/proc/{child.pid}").exists()
            log.close()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        save()


if __name__ == "__main__":
    def interrupted(_signal: int, _frame: object) -> None:
        raise KeyboardInterrupt("Hermes CLI fixture interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    main()
