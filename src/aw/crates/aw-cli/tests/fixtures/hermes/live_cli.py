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

YAML11_PLUGIN_NAMES = ["on", "off", "yes", "no", "Y", "N", "TRUE", "False", "null", "~"]
LONG_PLUGIN_NAMES = ["${AW_OTHER_PLUGIN}", *[f"retained-plugin-{index:04d}" for index in range(6000)], *["${AW_OTHER_PLUGIN}"] * 3000]


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
            shadowed = work / "shadowed-import"
            shadow = (f"from pathlib import Path\nPath({str(shadowed)!r}).touch()\n"
                      "raise RuntimeError('project module must not be imported')\n")
            (work / "utils.py").write_text(shadow)
            (work / "hermes_cli").mkdir()
            (work / "hermes_cli/__init__.py").write_text(shadow)
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
                "hooks": {name: [{"command": f"./existing-hook hook --step existing-{label}", "timeout": 5}]
                    for name, label in (("pre_tool_call", "before"), ("post_tool_call", "after"))},
                "unknown_config": {"keep": [1, True, "value"]},
            }
            original = hermes_yaml.safe_dump(native, sort_keys=False) + '''
plugin_defaults: &plugin_defaults
  enabled: [retained-plugin, '${AW_PLUGIN}']
  disabled: ['${env:AW_PLUGIN}', keep-disabled]
  hook_callback_timeout: 1:20
plugins:
  <<: *plugin_defaults
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
            quoted_names = ", ".join(repr(name) for name in YAML11_PLUGIN_NAMES)
            original = original.replace("'${AW_PLUGIN}']", "'${AW_PLUGIN}', " + quoted_names + "]")
            original = original.replace("keep-disabled]", "keep-disabled, " + quoted_names + "]")
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
            short_path = env["PATH"]
            long_path = os.pathsep.join([short_path] * 100)
            byte_path = os.pathsep.join((os.fsdecode(os.fsencode(root) + b"/\xff"), long_path))
            assert len(long_path.encode()) > 4096
            results = []
            for label, blocked in (("allow", False), ("block", True), ("empty", False),
                                   ("profile-safe", False), ("parent-safe", False),
                                   ("profile-override", False), ("reload-block", True),
                                   ("rewrite-block", True)):
                env["PATH"] = byte_path if label == "allow" else short_path
                safe = label in ("profile-safe", "parent-safe")
                reloading = label == "reload-block"
                server.tool_calls = 2 if reloading else 1
                if reloading:
                    reloader = profile / "plugins/reload-fixture"
                    reloader.mkdir(mode=0o700)
                    (reloader / "plugin.yaml").write_text(
                        "name: reload-fixture\nversion: 0.1.0\nprovides_hooks: [pre_api_request]\n")
                    (reloader / "__init__.py").write_text('''from pathlib import Path
import json
from typing import Any

def register(ctx: Any) -> None:
    def reload_once(**payload: Any) -> None:
        if not any(message.get("role") == "tool" for message in payload["request_messages"]):
            return
        marker = Path.cwd() / "native-reloaded.json"
        if marker.exists():
            return
        marker.write_text("pending")
        from tools.voice_mode import _check_plugin_stt_provider
        from hermes_cli.plugins import get_plugin_manager
        _check_plugin_stt_provider("missing-aw-reload-fixture")
        plugin = next(p for p in get_plugin_manager().list_plugins() if p["name"] == "aw-native-hooks")
        marker.write_text(json.dumps(plugin))
    ctx.register_hook("pre_api_request", reload_once)
''')
                    native_reload = hermes_yaml.safe_load(installed)
                    native_reload["plugins"]["enabled"].append("reload-fixture")
                    installed = hermes_yaml.safe_dump(native_reload, sort_keys=False).encode()
                    (profile / "config.yaml").write_bytes(installed)
                if label == "rewrite-block":
                    rewritten = hermes_yaml.safe_load(installed)
                    rewritten["hooks"]["pre_tool_call"][0]["command"] = "./existing-hook hook --step modify"
                    installed = hermes_yaml.safe_dump(rewritten, sort_keys=False).encode()
                    (profile / "config.yaml").write_bytes(installed)
                other_plugin = "retained-template" + "x" * 1024
                profile_env = f"AW_PLUGIN=aw-native-hooks\nAW_OTHER_PLUGIN={other_plugin}\nAW_HERMES_PROFILE_SENTINEL=existing\nAW_HERMES_LAUNCH=/missing/stale-launch.json\n"
                if label == "block":
                    profile_env += f"PATH={long_path}\n"
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
                    "max_output_bytes": 65536, "config": {"blocked_tools":
                        ["terminal"] if blocked and label != "rewrite-block" else []}}
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
                    managed = root / "managed"
                    managed.mkdir(mode=0o700)
                    (managed / "config.yaml").write_text("plugins: {disabled: [aw-native-hooks]}\n")
                    for key, value in (("HERMES_MANAGED", "nixos"),
                                       ("HERMES_MANAGED_DIR", str(managed))):
                        for source in ("parent", "dotenv"):
                            name = "install-" + key.lower() + "-" + source
                            dotenv = profile_env + (f"{key}={value}\n" if source == "dotenv" else "")
                            (profile / ".env").write_text(dotenv)
                            managed_env = {**env, key: value} if source == "parent" else env
                            assert run([base[0], "install", *base[1:]], name, work, managed_env) != 0, name
                            assert not shadowed.exists(), name
                            assert "native config writer failed" in (args.output / (name + ".log")).read_text()
                            assert (profile / ".env").read_text() == dotenv
                            assert (profile / "config.yaml").read_bytes() == original.encode()
                            assert not (profile / "plugins/aw-native-hooks").exists()
                            assert not list(profile.glob("config.yaml.aw-backup-*"))
                            assert not list(profile.glob(".aw-config-*"))
                    (profile / ".env").write_text(profile_env)
                    if os.geteuid() != 0:
                        cleanup_profile = root / "cleanup-profile"
                        cleanup_profile.mkdir(mode=0o700)
                        cleanup_original = b"plugins: {enabled: [], disabled: []}\n"
                        (cleanup_profile / "config.yaml").write_bytes(cleanup_original)
                        (cleanup_profile / "config.yaml").chmod(0o600)
                        imports = root / "cleanup-imports"
                        imports.mkdir(mode=0o700)
                        cache_marker = root / "cleanup-cache-path"
                        (imports / "sitecustomize.py").write_text(
                            "import atexit, sys\nfrom pathlib import Path\n"
                            "def deny_cleanup():\n"
                            "    if sys.argv[-4:-2] != ['config', 'set'] or sys.argv[-2] != 'plugins.enabled':\n"
                            "        return\n"
                            "    cache = Path(sys._xoptions['pycache_prefix'])\n"
                            "    denied = cache / 'fixture-denied'\n"
                            "    denied.mkdir()\n"
                            "    (denied / 'keep').touch()\n"
                            f"    Path({str(cache_marker)!r}).write_text(str(cache))\n"
                            "    denied.chmod(0)\n"
                            "atexit.register(deny_cleanup)\n")
                        try:
                            cleanup_base = [*base]
                            cleanup_base[-1] = str(cleanup_profile)
                            assert run([base[0], "install", *cleanup_base[1:]], "install-cleanup-error",
                                       work, {**env, "PYTHONPATH": str(imports)}) != 0
                            cleanup_log = (args.output / "install-cleanup-error.log").read_text()
                            cleanup_report = json.loads(cleanup_log.splitlines()[0])
                            assert cleanup_report["status"] == "installed"
                            assert "Python cache cleanup failed" in cleanup_log
                            assert (cleanup_profile / "plugins/aw-native-hooks/.aw-owned").is_file()
                            assert hermes_yaml.safe_load((cleanup_profile / "config.yaml").read_bytes())["plugins"]["enabled"] == ["aw-native-hooks"]
                            for field in ("backup", "displaced"):
                                recovery = Path(cleanup_report[field])
                                assert recovery.read_bytes() == cleanup_original
                                assert recovery.stat().st_mode & 0o777 == 0o600
                        finally:
                            if cache_marker.exists():
                                cache = Path(cache_marker.read_text())
                                assert cache.parent == Path(tempfile.gettempdir()) and cache.name.startswith("aw-hermes-python-")
                                (cache / "fixture-denied").chmod(0o700)
                                shutil.rmtree(cache)
                                assert not cache.exists()
                                (args.output / "cache-cleanup.json").write_text(json.dumps({"root": str(cache), "removed": True}))
                    long_names = ", ".join(repr(name) for name in LONG_PLUGIN_NAMES)
                    original = original.replace(quoted_names + "]", quoted_names + ", " + long_names + "]")
                    assert len(json.dumps(LONG_PLUGIN_NAMES).encode()) > 128 * 1024
                    expanded = [other_plugin if name == "${AW_OTHER_PLUGIN}" else name for name in LONG_PLUGIN_NAMES]
                    assert len(original.encode()) < 4 * 1024 * 1024
                    assert len(json.dumps([expanded, expanded]).encode()) > 4 * 1024 * 1024
                    (profile / "config.yaml").write_text(original)
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
                    assert after["plugins"]["enabled"] == ["retained-plugin", "aw-native-hooks", *YAML11_PLUGIN_NAMES, *LONG_PLUGIN_NAMES], after["plugins"]
                    assert after["plugins"]["disabled"] == ["keep-disabled", *YAML11_PLUGIN_NAMES, *LONG_PLUGIN_NAMES]
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
                if label == "rewrite-block":
                    expected_steps[0] = "modify"
                    assert not (work / "rewritten.txt").exists(), result
                    assert all("modification is unsupported" in row["payload"]["extra"]["error_message"]
                               for row in rows if row["step"] == "existing-after"), rows
                    assert all(row["payload"]["tool_input"]["command"] == "printf adopted > marker.txt"
                               for row in rows if row["step"].startswith("aw-")), rows
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
                assert not shadowed.exists(), result
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
                        if reloading or label == "rewrite-block":
                            policy = [entry for entry in entries
                                      if entry.get("call", {}).get("provider") == "policy"]
                            assert len(policy) == 1 and policy[0]["status"] == "ok", entries
                            assert policy[0]["policy_block"] == (reloading and plan["event"]["name"] == "tool.before")
                if label == "rewrite-block":
                    assert len(audit_events) == 2, audit_events
                    assert {event["tool"]["call_id"] for event in audit_events} == {audit_events[0]["tool"]["call_id"]}
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
