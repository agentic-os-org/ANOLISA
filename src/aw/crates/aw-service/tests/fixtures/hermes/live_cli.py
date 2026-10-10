"""Run official Hermes chat against a deterministic local model endpoint and AW."""

import argparse
from collections import Counter
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
        completed_tools = sum(message.get("role") == "tool" for message in payload.get("messages", []))
        call = {"id": f"call-fixture-{completed_tools}", "type": "function", "function": {
            "name": "terminal", "arguments": json.dumps({"command": "printf adopted > marker.txt"}),
        }}
        complete = completed_tools >= self.server.tool_calls
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
    server.tool_calls = 1
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
                "plugins": {"enabled": [], "disabled": ["aw-native-hooks", "keep-disabled"],
                            "hook_callback_timeout": 80},
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
            original = original.replace("hook_callback_timeout: 80\n", "hook_callback_timeout: 1:20\n")
            assert "hook_callback_timeout: 1:20\n" in original
            (profile / "config.yaml").write_text(original)
            (profile / "config.yaml").chmod(0o600)
            (profile / "auth.json").write_text("{}\n")
            (profile / ".env").write_text("AW_HERMES_PROFILE_SENTINEL=existing\n")
            (profile / ".env").chmod(0o600)
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
            shadow = root / "shadow-bin"
            shadow.mkdir(mode=0o700)
            (shadow / "hermes").write_text("#!/bin/sh\nexit 99\n")
            (shadow / "hermes").chmod(0o600)
            env["PATH"] = os.pathsep.join((str(shadow), str(args.hermes.parent), env["PATH"]))
            results = []
            for label, blocked in (("allow", False), ("block", True), ("empty", False),
                                   ("profile-safe", False), ("parent-safe", False),
                                   ("profile-override", False), ("reload-block", True)):
                safe = label in ("profile-safe", "parent-safe")
                reloading = label == "reload-block"
                server.tool_calls = 2 if reloading else 1
                if reloading:
                    reloader = profile / "plugins/reload-fixture"
                    reloader.mkdir(mode=0o700)
                    (reloader / "plugin.yaml").write_text(
                        "name: reload-fixture\nversion: 0.1.0\nprovides_hooks: [post_tool_call]\n")
                    (reloader / "__init__.py").write_text('''from pathlib import Path
import json
from typing import Any

def register(ctx: Any) -> None:
    def reload_once(**payload: Any) -> None:
        marker = Path.cwd() / "native-reloaded.json"
        if marker.exists():
            return
        marker.write_text("pending")
        from tools.voice_mode import _check_plugin_stt_provider
        from hermes_cli.plugins import get_plugin_manager
        _check_plugin_stt_provider("missing-aw-reload-fixture")
        plugin = next(p for p in get_plugin_manager().list_plugins() if p["name"] == "aw-native-hooks")
        marker.write_text(json.dumps(plugin))
    ctx.register_hook("post_tool_call", reload_once)
''')
                    native_reload = hermes_yaml.safe_load(installed)
                    native_reload["plugins"]["enabled"].append("reload-fixture")
                    installed = hermes_yaml.safe_dump(native_reload, sort_keys=False).encode()
                    (profile / "config.yaml").write_bytes(installed)
                profile_env = "AW_HERMES_PROFILE_SENTINEL=existing\nAW_HERMES_LAUNCH=/missing/stale-launch.json\n"
                if label == "profile-safe":
                    profile_env += 'export HERMES_SAFE_MODE="yes"\n'
                elif label == "profile-override":
                    profile_env += "HERMES_SAFE_MODE=0\n"
                (profile / ".env").write_text(profile_env)
                env["HERMES_SAFE_MODE"] = "1" if label in ("parent-safe", "profile-override") else "0"
                state = root / ("state-" + label)
                document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": label},
                    "spec": {"daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                        "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                        "audit": {"enabled": True, "payload": "metadata_only"},
                        "agents": {"hermes": {"adapter": "hermes", "argv": [str(args.hermes), "chat",
                            "--oneshot", "--max-turns", "3", "--run-budget", "80", "--provider", "custom",
                            "--model", "aw-fixture", "--accept-hooks", "--ignore-rules", "--toolsets", "terminal",
                            "--quiet", "--query-file", str(prompt)]}}, "providers": {}, "events": {}}}
                native_args = []
                if label == "allow":
                    native_args = document["spec"]["agents"]["hermes"]["argv"][1:]
                    document["spec"]["agents"]["hermes"]["argv"] = ["hermes"]
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
                if label == "empty":
                    document["spec"]["providers"] = {}
                    document["spec"]["events"] = {}
                config = root / (label + ".json")
                config.write_text(json.dumps(document))
                base = [str(args.aw), "--config", str(config), "--agent", "hermes", "--native-profile", str(profile)]
                if label == "allow":
                    assert run([base[0], "run", *base[1:], "--", *native_args], "uninstalled", work, env) != 0
                    assert run([base[0], "install", *base[1:]], "install", work, env) == 0
                    installed = (profile / "config.yaml").read_bytes()
                    report = json.loads((args.output / "install.log").read_text())
                    backup, displaced = Path(report["backup"]), Path(report["displaced"])
                    assert displaced == Path(str(backup) + ".displaced")
                    for recovery in (backup, displaced):
                        assert recovery.read_bytes() == original.encode()
                        assert recovery.stat().st_mode & 0o777 == 0o600
                    recovery_files = set(profile.glob("config.yaml.aw-backup-*"))
                    assert recovery_files == {backup, displaced}
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
                    repeated = json.loads((args.output / "install-again.log").read_text())
                    assert repeated["backup"] is None and repeated["displaced"] is None
                    assert set(profile.glob("config.yaml.aw-backup-*")) == recovery_files
                    # Native round-trip writers may normalize the spelling; keep
                    # this valid YAML 1.1 form at the actual AW launch boundary.
                    installed = installed.replace(b"hook_callback_timeout: 80\n",
                                                  b"hook_callback_timeout: 1:20\n")
                    assert b"hook_callback_timeout: 1:20\n" in installed
                    (profile / "config.yaml").write_bytes(installed)
                daemon = start([str(args.aw), "serve", "--config", str(config), "--state-dir", str(state)],
                               "daemon-" + label, work, env)
                deadline = time.monotonic() + 10
                while not (state / "aw.sock").exists() and time.monotonic() < deadline:
                    assert daemon.poll() is None
                    time.sleep(0.05)
                assert (state / "aw.sock").exists()
                trace.unlink(missing_ok=True)
                (work / "marker.txt").unlink(missing_ok=True)
                request_count = len(server.requests)
                exit_code = run([base[0], "run", *base[1:], "--", *native_args], label, work, env)
                assert (exit_code != 0) == safe, (label, exit_code)
                rows = [json.loads(line) for line in trace.read_text().splitlines()] if trace.exists() else []
                result = {"case": label, "tool_executed": (work / "marker.txt").exists(),
                    "chat_from_launcher_arguments": bool(native_args),
                    "installation_preserved_yaml11": True,
                    "steps": [row["step"] for row in rows],
                    "after_status": [row["payload"]["extra"].get("status") for row in rows
                                     if row["step"] == "existing-after"],
                    "profile_retained": (
                        (profile / "sessions/keep.txt").read_text() == "existing session"
                        and (profile / "auth.json").read_text() == "{}\n"
                        and (profile / ".env").read_text() == profile_env
                        and (profile / "active_profile").read_text() == "unrelated-profile"),
                    "config_unchanged_during_run": (profile / "config.yaml").read_bytes() == installed,
                    "native_environment_preserved": all(row["profile_sentinel"] == "existing" for row in rows),
                    "launch_directories_removed": not list(state.glob("launch-*"))}
                (args.output / (label + "-trace.json")).write_text(json.dumps(rows, indent=2))
                if reloading:
                    result["native_reload"] = json.loads((work / "native-reloaded.json").read_text())
                assert result["tool_executed"] == (not blocked and not safe), result
                if safe:
                    assert len(server.requests) == request_count, result
                expected_steps = ([] if safe else ["existing-before", "existing-after"] if label == "empty"
                                  else ["existing-before", "aw-before-1", "aw-before-2",
                                        "existing-after", "aw-after-1", "aw-after-2"])
                if reloading:
                    assert Counter(result["steps"]) == Counter(expected_steps * 2), result
                    refreshed = result["native_reload"]
                    assert refreshed["enabled"] and refreshed["error"] is None, refreshed
                else:
                    assert result["steps"] == expected_steps, result
                outcomes = [] if safe else ["blocked" if blocked else "ok"] * server.tool_calls
                assert result["after_status"] == outcomes, result
                assert result["profile_retained"] and result["config_unchanged_during_run"], result
                assert result["native_environment_preserved"], result
                if rows:
                    assert len({row["tmpdir"] for row in rows}) == 1 and rows[0]["tmpdir"]
                    assert len({row["home"] for row in rows}) == 1
                assert result["launch_directories_removed"], result
                assert not (work / "NEVER").exists()
                assert all(row["literal"] == "literal ; $(touch NEVER) ' spaces" for row in rows
                           if row["step"].startswith("aw-"))
                assert run([str(args.aw), "status", "--config", str(config)],
                           "status-" + label, work, env) == 0
                status = json.loads((args.output / ("status-" + label + ".log")).read_text())
                result["instance_released"] = status["result"]["bindings"] == 0
                assert result["instance_released"], result
                results.append(result)
                assert run([str(args.aw), "stop", "--config", str(config)], "stop-" + label, work, env) == 0
                assert daemon.wait(timeout=10) == 0
                audit_events = []
                for journal in state.rglob("*.jsonl"):
                    shutil.copyfile(journal, args.output / (label + "-" + journal.name))
                    entries = [json.loads(line)["record"] for line in journal.read_text().splitlines()]
                    plan = entries[0].get("plan", {})
                    if plan.get("kind") == "event":
                        audit_events.append(plan["event"])
                        if reloading:
                            policy = [entry for entry in entries
                                      if entry.get("call", {}).get("provider") == "policy"]
                            assert len(policy) == 1 and policy[0]["status"] == "ok", entries
                            assert policy[0]["policy_block"] == (plan["event"]["name"] == "tool.before")
                if reloading:
                    calls = {name: {event["tool"]["call_id"] for event in audit_events if event["name"] == name}
                             for name in ("tool.before", "tool.after")}
                    assert len(audit_events) == 4 and len(calls["tool.before"]) == 2, audit_events
                    assert calls["tool.before"] == calls["tool.after"], audit_events
                    result["audited_tool_calls"] = 2
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
