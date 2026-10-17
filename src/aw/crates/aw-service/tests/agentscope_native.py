"""Real SDK acceptance: invoke with AgentScope 2.0.6 installed, no network/model.

python tests/agentscope_native.py --aw target/debug/aw --output target/agentscope-native
The interpreter running this script also runs the fixture worker.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
from importlib.metadata import version
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    assert version("agentscope") == "2.0.6"
    aw = args.aw.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent / "fixtures/agentscope"
    with tempfile.TemporaryDirectory(prefix="s-", dir=args.output) as temporary:
        root = Path(temporary).resolve()
        state = root / "state"
        assert (
            len(os.fsencode(state / "aw.sock")) <= 107
        ), "use a short worktree/target output path for Linux Unix sockets"
        config = {
            "apiVersion": "aw/v1alpha1",
            "kind": "AWConfiguration",
            "metadata": {"name": "agentscope-native"},
            "spec": {
                "daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 55000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {
                    "sdk": {
                        "adapter": "agentscope",
                        "argv": [sys.executable, str(fixture / "worker.py")],
                    }
                },
                "providers": {},
                "events": {},
            },
        }
        for label, event, effects in [
            ("before-first", "tool.before", ["observe", "block"]),
            ("native-report", "tool.before", ["observe", "block"]),
            ("native-block", "tool.before", ["observe", "block"]),
            ("before-last", "tool.before", ["observe", "block"]),
            ("after", "tool.after", ["observe"]),
        ]:
            config["spec"]["providers"][label] = {
                "transport": {
                    "type": "stdio",
                    "location": "agent",
                    "argv": [sys.executable, str(fixture / "policy.py"), str(root), label],
                },
                "timeout_ms": 4000,
                "max_output_bytes": 4096,
                "config": {},
            }
            if label.startswith("native-"):
                config["spec"]["providers"][label]["protocol"] = "native-hook/v1alpha1"
                config["spec"]["providers"][label]["transport"]["argv"] = [
                    sys.executable,
                    str(fixture / "native_hook.py"),
                    str(root),
                    label,
                ]
            step = {
                "id": label,
                "provider": label,
                "on_error": (
                    "block" if event == "tool.before" and label != "native-report" else "report"
                ),
            }
            if label.startswith("native-"):
                step["native"] = {}
            else:
                step["effects"] = effects
            config["spec"]["events"].setdefault(
                event, {"enabled": True, "required": True, "steps": []}
            )["steps"].append(step)
        path = root / "aw.json"
        path.write_text(json.dumps(config))
        (args.output / "config.json").write_text(json.dumps(config, indent=2))
        logs = (args.output / "serve.log").open("w")
        cmd = [str(aw), "serve", "--config", str(path), "--state-dir", str(state)]
        record = {
            "command": cmd,
            "cwd": str(root),
            "ports": [],
            "log": str(logs.name),
            "lifetime_seconds": 65,
        }
        (args.output / "process.json").write_text(json.dumps(record))
        proc = subprocess.Popen(cmd, cwd=root, stdout=logs, stderr=logs, start_new_session=True)
        record.update(pid=proc.pid, stop=f"kill -TERM -- -{proc.pid}")
        (args.output / "process.json").write_text(json.dumps(record))
        try:
            deadline = time.monotonic() + 10
            while not (state / "aw.sock").exists():
                assert proc.poll() is None, "service exited"
                assert time.monotonic() < deadline, "service startup timed out"
                time.sleep(0.01)
            env = {**os.environ, "AW_TEST_ROOT": str(root)}
            # Each foreground process creates two native sessions; use repeated
            # call IDs across processes to verify binding isolation as well.
            launches = []
            # Two concurrent sessions each exercise hundreds of native callbacks.
            # Bound the full suite independently of the unchanged per-Hook budgets.
            deadline = time.monotonic() + 120
            try:
                for number in range(2):
                    stdout = (args.output / f"run-{number}.stdout").open("wb")
                    stderr = (args.output / f"run-{number}.stderr").open("wb")
                    command = [str(aw), "run", "--config", str(path), "--agent", "sdk"]
                    launch_record = {
                        "command": command,
                        "cwd": str(root),
                        "ports": [],
                        "logs": [str(stdout.name), str(stderr.name)],
                        "lifetime_seconds": 120,
                    }
                    record_path = args.output / f"launch-{number}.json"
                    record_path.write_text(json.dumps(launch_record))
                    child = subprocess.Popen(
                        command,
                        env=env,
                        cwd=root,
                        stdout=stdout,
                        stderr=stderr,
                        start_new_session=True,
                    )
                    launch_record.update(pid=child.pid, stop=f"kill -TERM -- -{child.pid}")
                    record_path.write_text(json.dumps(launch_record))
                    launches.append((child, stdout, stderr, record_path, launch_record))
                for child, *_ in launches:
                    child.wait(timeout=max(0.01, deadline - time.monotonic()))
            finally:
                for child, stdout, stderr, record_path, launch_record in launches:
                    if child.poll() is None:
                        os.killpg(child.pid, signal.SIGTERM)
                        try:
                            child.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            os.killpg(child.pid, signal.SIGKILL)
                            child.wait(timeout=3)
                    stdout.close()
                    stderr.close()
                    launch_record.update(exited=True, returncode=child.returncode)
                    record_path.write_text(json.dumps(launch_record))
            contexts = []
            for number, (child, *_) in enumerate(launches):
                assert child.returncode == 0, (args.output / f"run-{number}.stderr").read_text()
                contexts.extend(json.loads((args.output / f"run-{number}.stdout").read_bytes()))
            callbacks = [
                json.loads(line) for line in (root / "callbacks.jsonl").read_text().splitlines()
            ]
            executed = [
                json.loads(line) for line in (root / "executed.jsonl").read_text().splitlines()
            ]
            assert len(executed) == 24, executed
            assert all(item["mode"] != "deny" for item in executed)
            assert len(callbacks) == 632, len(callbacks)
            native_callbacks = [
                json.loads(line)
                for line in (root / "native-callbacks.jsonl").read_text().splitlines()
            ]
            assert len(native_callbacks) == 320, len(native_callbacks)
            instances = {item["event"]["agent"]["instance_id"] for item in callbacks}
            assert len(instances) == 2
            for instance in instances:
                for session in ["session-one", "session-two"]:
                    for mode, outcome in [
                        ("success", "success"),
                        ("deny", "denied"),
                        ("error", "error"),
                        ("cancel", "interrupted"),
                    ]:
                        selected = [
                            item
                            for item in callbacks
                            if item["event"]["agent"]["instance_id"] == instance
                            and item["event"]["session_id"] == session
                            and item["event"]["native"]["tool_call"]["id"] == "call-" + mode
                            and item["event"]["tool"]["input"]["mode"] == mode
                        ]
                        if mode == "success":
                            assert len(selected) == 9
                        else:
                            assert [item["label"] for item in selected] == (
                                ["before-first", "after"]
                                if mode == "deny"
                                else ["before-first", "before-last", "after"]
                            )
                        assert selected[-1]["event"]["tool"]["result"]["state"] == outcome, selected
                        assert all(
                            item["event"]["tool"]["input"]["uid"] == session for item in selected
                        )
                    by_call = {}
                    for item in callbacks:
                        event = item["event"]
                        if (
                            event["agent"]["instance_id"] == instance
                            and event["session_id"] == session
                        ):
                            by_call.setdefault(event["tool"]["call_id"], []).append(item)
                    assert len(by_call) == 76, len(by_call)
                    for selected in by_call.values():
                        denied = selected[0]["event"]["tool"]["input"]["mode"] in {
                            "deny",
                            "hook-error",
                            "native-exit",
                            "native-signal",
                        }
                        assert [item["label"] for item in selected] == (
                            ["before-first", "after"]
                            if denied
                            else ["before-first", "before-last", "after"]
                        )
                        assert selected[-1]["event"]["tool"]["result"]["state"] == (
                            "denied"
                            if denied
                            else {"error": "error", "cancel": "interrupted"}.get(
                                selected[0]["event"]["tool"]["input"]["mode"], "success"
                            )
                        )
                        native = [
                            item
                            for item in native_callbacks
                            if item["event"]["invocation_id"]
                            == selected[0]["event"]["tool"]["call_id"]
                        ]
                        early_denied = selected[0]["event"]["tool"]["input"]["mode"] in {
                            "deny",
                            "hook-error",
                        }
                        assert [item["label"] for item in native] == (
                            [] if early_denied else ["native-report", "native-block"]
                        )
                        if native:
                            assert native[0]["event"] == native[1]["event"]
            for context in contexts:
                results = [
                    block
                    for msg in context["context"]
                    for block in msg["content"]
                    if block["type"] == "tool_result"
                ]
                assert len(results) == 76, results
            status = json.loads(
                subprocess.check_output(
                    [str(aw), "status", "--socket", str(state / "aw.sock")], timeout=5
                )
            )["result"]
            assert status["bindings"] == 0 and status["events"] == 0, status
            assert not list(state.glob("launch-*"))
            journals = list(state.rglob("*.jsonl"))
            assert len(journals) == 610, "missing durable event journals"
            verified = []
            for journal in journals:
                audit = subprocess.run(
                    [str(aw), "request", "--socket", str(state / "aw.sock")],
                    input=json.dumps({"method": "audit", "key": journal.stem}).encode(),
                    capture_output=True,
                    timeout=5,
                    check=True,
                )
                checked = json.loads(audit.stdout)["result"]
                assert checked["terminal"] is True, checked
                if checked["records"][0]["record"]["plan"]["kind"] == "event":
                    assert checked["records"][-1]["record"]["status"] in {"closed", "completed"}
                verified.append(checked)
            (args.output / "verified-audit.json").write_text(json.dumps(verified, indent=2))
            (args.output / "callbacks.json").write_text(json.dumps(callbacks, indent=2))
            (args.output / "native-callbacks.json").write_text(
                json.dumps(native_callbacks, indent=2)
            )
            (args.output / "contexts.json").write_text(json.dumps(contexts, indent=2))
            (args.output / "journal.json").write_text(
                json.dumps({str(p.relative_to(state)): p.read_text() for p in journals}, indent=2)
            )
            (args.output / "result.json").write_text(
                json.dumps(
                    {
                        "sdk_version": version("agentscope"),
                        "callbacks": len(callbacks),
                        "native_callbacks": len(native_callbacks),
                        "executed": len(executed),
                        "status": status,
                    },
                    indent=2,
                )
            )
        finally:
            try:
                subprocess.run(
                    [str(aw), "stop", "--socket", str(state / "aw.sock")],
                    timeout=5,
                    capture_output=True,
                )
                proc.wait(timeout=5)
            except (subprocess.TimeoutExpired, FileNotFoundError):
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)
            logs.close()
            record.update(exited=True, returncode=proc.returncode)
            (args.output / "process.json").write_text(json.dumps(record))
    print("AgentScope 2.0.6 real turn/Toolkit/aw run acceptance passed")


if __name__ == "__main__":
    main()
