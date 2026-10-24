"""Pinned native resident acceptance; one configuration, two named targets."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from native_cleanup import own_children, reap_children


def assert_reaped(pid: int) -> None:
    deadline = time.monotonic() + 5
    while Path(f"/proc/{pid}").exists():
        try:
            os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            pass
        if not Path(f"/proc/{pid}").exists():
            return
        assert time.monotonic() < deadline, f"SDK child {pid} was not reaped"
        time.sleep(0.01)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--agent", choices=["pi", "claude"], required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--npm", type=Path)
    parser.add_argument("--scenario", choices=["lifecycle", "stop-failure", "cancel", "startup",
                        "messages", "eof-stop-failure", "close-timeout", "observation"], default="lifecycle")
    parser.add_argument("--evidence-mode", choices=["auto", "off"])
    args = parser.parse_args()
    aw, output = args.aw.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent / "fixtures/resident"
    application = fixture / "pi.mjs"
    if args.agent == "pi":
        assert args.npm is not None
        application = args.npm.resolve() / "resident.mjs"
        shutil.copyfile(fixture / "pi.mjs", application)
        for name in ("pi_startup.mjs", "pi_cancel.mjs"):
            shutil.copyfile(fixture / name, application.parent / name)
    # AF_UNIX uses an OS path limit independent of the evidence directory length.
    state_parent = Path(tempfile.mkdtemp(prefix="aw-resident-"))
    config = output / "aw.json"
    document = {
        "apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
        "metadata": {"name": "resident-sdk"},
        "spec": {
            "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": str(state_parent / "s")},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {
                "claude": {"adapter": "claude-sdk", "argv": [sys.executable, "-m", "aw_claude", "--", str(fixture / "claude.py")]},
                "pi": {"adapter": "pi", "argv": [shutil.which("node") or "/usr/bin/node", str(application)]},
            },
            "providers": {"custom": {"transport": {"type": "stdio", "location": "agent",
                "argv": [sys.executable, str(fixture / "policy.py")]}, "timeout_ms": 1000, "max_output_bytes": 4096}},
            "events": {event: {"enabled": True, "required": True, "steps": [{
                "id": "custom", "provider": "custom", "on_error": "report",
                "effects": ["observe", "block"] if event == "tool.before" else ["observe"],
            }]} for event in ("tool.before", "tool.after")},
        },
    }
    if args.evidence_mode is not None or args.scenario == "observation":
        document["spec"]["agents"][args.agent]["evidence"] = {
            "source_id": "fixture", "endpoint": "http://127.0.0.1:1",
            "mode": args.evidence_mode or "auto", "query_timeout_ms": 100,
            "query_bytes": 1048576, "max_items": 32, "max_bytes": 262144,
        }
    config.write_text(json.dumps(document))
    env = {**os.environ, "AW_TEST_ROOT": str(output), "AW_TEST_SCENARIO": args.scenario, "PYTHONDONTWRITEBYTECODE": "1",
           "CLAUDE_CONFIG_DIR": str(output / "host-profile"), "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
           "AW_TEST_CLAUDE_FIXTURE": str(fixture.parents[2] / "crates/aw-cli/tests/fixtures/claude-sdk/native_worker.py")}
    command = [str(aw), "run", "--config", str(config), "--agent", args.agent]
    record = {"command": command, "cwd": str(output), "ports": "per-task endpoint.json",
              "timeout_seconds": 600, "state": str(state_parent), "pid": None,
              "stop": [str(aw), "stop", "--config", str(config)]}
    (output / "process.json").write_text(json.dumps(record))
    process = None
    own_children()
    try:
        with (output / "stdout.log").open("w") as stdout, (output / "stderr.log").open("w") as stderr:
            process = subprocess.Popen(command, cwd=output, env=env, stdout=stdout, stderr=stderr, start_new_session=True)
            record.update(pid=process.pid, stop_worker=f"kill -TERM -- -{process.pid}")
            (output / "process.json").write_text(json.dumps(record))
            expected = (-9 if args.scenario in {"stop-failure", "eof-stop-failure"}
                        else 1 if args.scenario in {"close-timeout", "observation"} else 0)
            assert process.wait(timeout=600) == expected, (output / "stderr.log").read_text()[-6000:]
        for endpoint in output.glob("*/endpoint.json"):
            with socket.socket() as probe:
                probe.settimeout(1)
                assert probe.connect_ex(("127.0.0.1", json.loads(endpoint.read_text())["port"])) != 0
        for evidence in output.glob("*/sdk-process.json"):
            assert_reaped(json.loads(evidence.read_text())["pid"])
        if args.scenario == "stop-failure":
            requests = json.loads((output / "0/requests.json").read_text())
            assert len([value for value in requests if "messages" in value]) == 1
            assert not (output / "0/effect").exists()
            (output / "result.json").write_text(json.dumps({"tasks": 1, "passed": True}))
        if args.scenario == "eof-stop-failure":
            assert "AW interrupt acknowledgement failed: RuntimeError" in (output / "stderr.log").read_text()
            assert not list(output.glob("*/effect"))
            (output / "result.json").write_text(json.dumps({"tasks": 1, "passed": True}))
        if args.scenario == "observation":
            diagnostic = (output / "stderr.log").read_text()
            assert "does not support AgentSight evidence" in diagnostic, diagnostic
            assert "native hooks ready" not in diagnostic, diagnostic
            assert not (output / "application-started").exists()
            (output / "result.json").write_text(json.dumps({"rejected_before_ready": True, "passed": True}))
        status = json.loads(subprocess.check_output([str(aw), "status", "--config", str(config)], env=env, timeout=10))["result"]
        assert status["bindings"] == status["events"] == 0, status
        result = json.loads((output / "result.json").read_text())
        result["status"] = status
        (output / "result.json").write_text(json.dumps(result))
    finally:
        try:
            if process is not None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=10)
            stopped = subprocess.run([str(aw), "stop", "--config", str(config)], env=env, capture_output=True, timeout=15)
            assert stopped.returncode == 0, stopped.stderr.decode()
            deadline = time.monotonic() + 15
            while (state_parent / "s/aw.sock").exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            assert not (state_parent / "s/aw.sock").exists()
        finally:
            try:
                reap_children()
                shutil.copytree(state_parent, output / "service-evidence", dirs_exist_ok=True)
            finally:
                shutil.rmtree(state_parent)
                assert not state_parent.exists()


if __name__ == "__main__":
    main()
