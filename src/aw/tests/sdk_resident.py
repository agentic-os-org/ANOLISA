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
                        "messages", "eof-stop-failure", "close-timeout", "observation", "mcp", "mcp-failure", "long", "drain", "drain-timeout", "store", "denial", "boundaries"], default="lifecycle")
    parser.add_argument("--evidence-mode", choices=["auto", "off"])
    parser.add_argument("--tokenless", type=Path)
    parser.add_argument("--tokenless-provider", type=Path)
    args = parser.parse_args()
    aw, output = args.aw.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent / "fixtures/resident"
    application = fixture / "pi.mjs"
    if args.agent == "pi":
        assert args.npm is not None
        application = args.npm.resolve() / "resident.mjs"
        for name in ("pi_startup.mjs", "pi_cancel.mjs", "pi_drain.mjs"):
            shutil.copyfile(fixture / name, application.parent / name)
        shutil.copyfile(fixture / ("long_pi.mjs" if args.scenario == "long" else "pi.mjs"), application)
        if args.scenario == "long":
            shutil.copyfile(fixture / "pi.mjs", application.with_name("resident-fixture.mjs"))
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
    if args.scenario in {"mcp", "mcp-failure"}:
        assert args.agent == "claude"
        document["spec"]["providers"]["custom"]["timeout_ms"] = 4000
    if args.scenario in {"mcp", "mcp-failure", "long"}:
        assert args.tokenless and args.tokenless_provider
        document["spec"]["agents"]["claude"]["argv"][-1] = str(fixture / ("long_claude.py" if args.scenario == "long" else "mcp.py"))
        document["spec"]["providers"]["tokenless"] = {
            "protocol": "aw-provider/v1alpha1", "transport": {"type": "stdio", "location": "agent",
            "argv": [str(args.tokenless_provider.resolve()), "--cli", str(args.tokenless.resolve()) if args.scenario != "mcp-failure" else "/bin/false", "--data-dir", str(state_parent / "tokenless")]},
            "timeout_ms": 2000, "max_output_bytes": 1048576, "config": {"version": 1}}
        for event, operation, effects in (("tool.before", "pre_tool", ["block"]), ("tool.after", "post_tool", ["replace_result"])):
            document["spec"]["events"][event]["steps"].append({"id": "tokenless", "provider": "tokenless",
                "operation": operation, "effects": effects, "on_error": "block" if event == "tool.before" else "report"})
    config.write_text(json.dumps(document))
    env = {**os.environ, "AW_TEST_ROOT": str(output), "TOKENLESS_STATS_ENABLED": "0", "TOKENLESS_SLS_ENABLED": "0", "AW_TEST_SCENARIO": args.scenario, "PYTHONDONTWRITEBYTECODE": "1",
           "CLAUDE_CONFIG_DIR": str(output / "host-profile"), "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
           "AW_TEST_CLAUDE_FIXTURE": str(fixture.parents[2] / "crates/aw-cli/tests/fixtures/claude-sdk/native_worker.py")}
    timeout = 1800 if args.scenario == "long" else 600
    command = [str(aw), "run", "--config", str(config), "--agent", args.agent]
    record = {"command": command, "cwd": str(output), "ports": "per-task endpoint.json",
              "timeout_seconds": timeout, "state": str(state_parent), "pid": None,
              "stop": [str(aw), "stop", "--config", str(config)]}
    (output / "process.json").write_text(json.dumps(record))
    process = None
    own_children()
    try:
        with (output / "stdout.log").open("w") as stdout, (output / "stderr.log").open("w") as stderr:
            process = subprocess.Popen(command, cwd=output, env=env, stdout=stdout, stderr=stderr, start_new_session=True)
            record.update(pid=process.pid, stop_worker=f"kill -TERM -- -{process.pid}")
            (output / "process.json").write_text(json.dumps(record))
            expected = (-9 if args.scenario in {"stop-failure", "eof-stop-failure", "drain-timeout"}
                        else 1 if args.scenario in {"close-timeout", "observation"} else 0)
            assert process.wait(timeout=timeout) == expected, (output / "stderr.log").read_text()[-6000:]
        for endpoint in output.glob("*/endpoint.json"):
            with socket.socket() as probe:
                probe.settimeout(1)
                assert probe.connect_ex(("127.0.0.1", json.loads(endpoint.read_text())["port"])) != 0
        for evidence in output.glob("*/sdk-process.json"):
            detail = json.loads(evidence.read_text())
            assert_reaped(detail["pid"])
            if "relay_port" in detail:
                with socket.socket() as probe:
                    probe.settimeout(1)
                    assert probe.connect_ex(("127.0.0.1", detail["relay_port"])) != 0
        if args.scenario == "drain-timeout":
            assert (output / "drain-timeout-barrier").exists()
            assert not (output / "result.json").exists()
            (output / "result.json").write_text(json.dumps({"tasks": 1, "passed": True,
                "cases": ["undrained-callback-terminates-owned-group"]}))
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
        if args.scenario in {"mcp", "mcp-failure"}:
            events = []
            for journal in (state_parent / "s/journal").glob("*.jsonl"):
                data = journal.read_text()
                assert "stable fixture data" not in data
                for line in data.splitlines():
                    event = json.loads(line)["record"].get("plan", {}).get("event")
                    if event is not None:
                        events.append(event)
            assert events and all(event["correlation_source"] == "claude-sdk-mcp/v1" for event in events)
        result = json.loads((output / "result.json").read_text())
        result["status"] = status
        result["scenario"] = args.scenario
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
