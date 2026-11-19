"""Pinned native resident acceptance; one configuration, two named targets."""
import argparse
from hashlib import sha256
from importlib.metadata import version
import json
import os
import re
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


def prepare_claude_wheel(
    fixture: Path, directory: Path, environment: dict, output: Path,
) -> dict:
    """Use the image installation or install the checkout wheel in a private directory."""
    if receipt := environment.get("AW_RESIDENT_PYTHON_WHEEL"):
        entries = {}
        for line in Path(receipt).read_text().splitlines():
            if not line.strip():
                continue
            digest, name = line.split()
            assert re.fullmatch(r"[0-9a-f]{64}", digest), digest
            entries[name] = digest
        core_name = f"anolisa_aw_sdk-{version('anolisa-aw-sdk')}-py3-none-any.whl"
        adapter_name = (
            f"anolisa_aw_sdk_claude-{version('anolisa-aw-sdk-claude')}-py3-none-any.whl"
        )
        assert set(entries) == {core_name, adapter_name}, entries
        environment.pop("PYTHONPATH", None)
        return {
            "name": core_name,
            "sha256": entries[core_name],
            "adapter_name": adapter_name,
            "adapter_sha256": entries[adapter_name],
            "installation": "image",
        }
    package = fixture.parents[2] / "sdk/python"
    claude_package = fixture.parents[2] / "sdk/python-adapters/claude"
    wheel_directory = directory / "wheel"
    claude_wheel_directory = directory / "wheel-claude"
    installed = directory / "installed"
    with (output / "wheel.log").open("w") as log:
        subprocess.run(
            ["uv", "build", "--wheel", "--python", sys.executable,
             "--out-dir", str(wheel_directory), str(package)],
            stdout=log, stderr=subprocess.STDOUT, check=True, timeout=120,
        )
        subprocess.run(
            ["uv", "build", "--wheel", "--python", sys.executable,
             "--out-dir", str(claude_wheel_directory), str(claude_package)],
            stdout=log, stderr=subprocess.STDOUT, check=True, timeout=120,
        )
        (wheel,) = wheel_directory.glob("*.whl")
        (claude_wheel,) = claude_wheel_directory.glob("*.whl")
        subprocess.run(
            ["uv", "pip", "install", "--python", sys.executable, "--target", str(installed),
             "--no-deps", str(wheel), str(claude_wheel)],
            stdout=log, stderr=subprocess.STDOUT, check=True, timeout=60,
        )
    environment["PYTHONPATH"] = str(installed)
    return {
        "name": wheel.name, "sha256": sha256(wheel.read_bytes()).hexdigest(),
        "adapter_name": claude_wheel.name,
        "adapter_sha256": sha256(claude_wheel.read_bytes()).hexdigest(),
        "installation": "private",
    }


def attach_claude_worker(
    process: subprocess.Popen, aw: Path, config: Path, output: Path, env: dict,
) -> None:
    deadline = time.monotonic() + 10
    while not (output / "application-started").exists():
        assert process.poll() is None, (output / "stderr.log").read_text()
        assert time.monotonic() < deadline, "native Claude worker did not start"
        time.sleep(0.025)
    # The native process starts independently. Required admission blocks the
    # model request while the application waits for the cooperative attach.
    assert not list(output.glob("*/requests.json"))
    result = subprocess.run(
        [str(aw), "connect", "--config", str(config), "--sdk-pid",
         str(process.pid), "--timeout-ms", "10000"],
        cwd=output, env=env, capture_output=True, text=True, timeout=15,
    )
    (output / "attach.log").write_text(result.stderr)
    assert result.returncode == 0, result.stderr + result.stdout
    document = json.loads(result.stdout)
    assert document["result"]["state"] == "completed", document
    (output / "attach.json").write_text(json.dumps(document))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--agent", choices=["pi", "claude"], required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--npm", type=Path)
    parser.add_argument("--scenario", choices=["lifecycle", "stop-failure", "cancel", "startup",
                        "messages", "eof-stop-failure", "close-timeout", "observation", "mcp", "mcp-failure", "long", "drain", "drain-timeout", "store", "denial", "boundaries", "settings", "report",
                        "cooperative-attach"], default="lifecycle")
    parser.add_argument("--evidence-mode", choices=["auto", "off"])
    parser.add_argument("--tokenless", type=Path)
    parser.add_argument("--tokenless-provider", type=Path)
    args = parser.parse_args()
    cooperative = args.scenario == "cooperative-attach"
    if cooperative and args.agent != "claude":
        parser.error("cooperative-attach requires --agent claude")
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
    if args.scenario == "report":
        assert args.agent == "claude" and args.tokenless and args.tokenless_provider
        document["spec"]["events"]["tool.after"].update(required=False, steps=[{
            "id": "tokenless", "provider": "tokenless", "operation": "post_tool",
            "effects": ["replace_result"], "on_error": "report",
        }])
        document["spec"]["providers"]["tokenless"] = {
            "protocol": "aw-provider/v1alpha1", "transport": {"type": "stdio", "location": "agent",
                "argv": [sys.executable, str(fixture / "report_provider.py"),
                         str(args.tokenless_provider.resolve()), str(args.tokenless.resolve()),
                         str(state_parent / "tokenless")]},
            "timeout_ms": 1200, "max_output_bytes": 1048576, "config": {"version": 1}}
    config.write_text(json.dumps(document))
    env = {**os.environ, "AW_TEST_ROOT": str(output), "TOKENLESS_STATS_ENABLED": "0", "TOKENLESS_SLS_ENABLED": "0", "AW_TEST_SCENARIO": args.scenario, "PYTHONDONTWRITEBYTECODE": "1",
           "CLAUDE_CONFIG_DIR": str(output / "host-profile"), "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
           "AW_TEST_CLAUDE_FIXTURE": str(fixture.parents[2] / "crates/aw-cli/tests/fixtures/claude-sdk/native_worker.py")}
    timeout = 1800 if args.scenario == "long" else 600
    command = [str(aw), "run", "--config", str(config), "--agent", args.agent]
    record = {"command": command, "cwd": str(output), "ports": "per-task endpoint.json",
              "timeout_seconds": timeout, "state": str(state_parent), "pid": None,
              "stop": [str(aw), "service", "stop", "--config", str(config)]}
    (output / "process.json").write_text(json.dumps(record))
    process = None
    own_children()
    try:
        if cooperative:
            # Deployment compiles wiring once from a private copy of the same bytes;
            # the worker never runs the offline parser.
            deployed, descriptor = state_parent / "aw.json", state_parent / "descriptor.json"
            shutil.copyfile(config, deployed)
            deployed.chmod(0o600)
            compiled = subprocess.run(
                [str(aw), "config", "descriptor", "--config", str(deployed), "--agent", "claude",
                 "--profile", "claude-resident-hooks"],
                env=env, capture_output=True, text=True, timeout=15,
            )
            assert compiled.returncode == 0, compiled.stderr
            descriptor.write_text(compiled.stdout)
            descriptor.chmod(0o600)
            shutil.copyfile(descriptor, output / "descriptor.json")
            env.update(AW_TEST_CONFIG=str(deployed), AW_TEST_DESCRIPTOR=str(descriptor))
            wheel = prepare_claude_wheel(fixture, state_parent, env, output)
            # Keep the adjacent mcp fixture from shadowing the installed MCP package.
            command = [sys.executable, "-P", str(fixture / "claude.py")]
            record["command"] = command
        with (output / "stdout.log").open("w") as stdout, (output / "stderr.log").open("w") as stderr:
            process = subprocess.Popen(command, cwd=output, env=env, stdout=stdout, stderr=stderr, start_new_session=True)
            record.update(pid=process.pid, stop_worker=f"kill -TERM -- -{process.pid}")
            (output / "process.json").write_text(json.dumps(record))
            if cooperative:
                attach_claude_worker(process, aw, config, output, env)
            expected = (-9 if args.scenario in {"stop-failure", "eof-stop-failure", "drain-timeout"}
                        else 1 if args.scenario == "close-timeout" else 0)
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
            assert (output / "application-started").exists()
            observed = []
            for journal in (state_parent / "s/journal").glob("*.jsonl"):
                records = [json.loads(line)["record"] for line in journal.read_text().splitlines()]
                callbacks = [record for record in records if record.get("phase") == "native_callback"]
                if not callbacks:
                    continue
                assert all(record["association"]["status"] == "unverified" for record in callbacks)
                assert records[-1]["phase"] == "closed"
                assert records[-1]["coverage"] == "unknown"
                assert "native_identity_unverified" in records[-1]["gaps"]
                observed.append(len(callbacks))
            assert len(observed) >= 2, observed
            result = json.loads((output / "result.json").read_text())
            result.update(task_observation=True, source_association="unverified", coverage="unknown",
                          closed_observation_scopes=len(observed))
            (output / "result.json").write_text(json.dumps(result))
        status = json.loads(subprocess.check_output([str(aw), "service", "status", "--config", str(config)], env=env, timeout=10))["result"]
        assert status["bindings"] == status["events"] == 0, status
        if args.scenario in {"mcp", "mcp-failure"}:
            events = []
            completed = []
            for journal in (state_parent / "s/journal").glob("*.jsonl"):
                data = journal.read_text()
                assert "stable fixture data" not in data
                for line in data.splitlines():
                    record = json.loads(line)["record"]
                    event = record.get("plan", {}).get("event")
                    if event is not None:
                        events.append(event)
                    if record.get("phase") == "completed":
                        completed.append(record)
            assert events and all(event["correlation_source"] == "claude-sdk-mcp/v1" for event in events)
            if args.scenario == "mcp-failure":
                # Report faults keep the checked result and continue the task;
                # the failed compression step stays visible in audit and logs.
                assert any(record.get("status") == "error"
                           and record.get("failure_action") == "report" for record in completed), completed
                diagnostic = (output / "stderr.log").read_text()
                assert "aw step failed (provider); recorded as report" in diagnostic, diagnostic[-3000:]
            if args.scenario == "mcp":
                # The result-control rejection of a smuggled result is recorded
                # as a report fault instead of stopping the task: after-error
                # and result-control each leave one diagnostic and audit record.
                assert any(record.get("status") == "error"
                           and record.get("failure_action") == "report" for record in completed), completed
                diagnostic = (output / "stderr.log").read_text()
                assert diagnostic.count("aw step failed (exit); recorded as report") == 2, diagnostic[-3000:]
        if args.scenario == "report":
            completed = [json.loads(line)["record"]
                         for journal in (state_parent / "s/journal").glob("*.jsonl")
                         for line in journal.read_text().splitlines()
                         if json.loads(line)["record"].get("phase") == "completed"]
            faults = [record for record in completed if record["status"] == "error"]
            assert len(faults) == 24, faults
            assert {record["failure"] for record in faults} == {"exit", "transport", "protocol", "provider"}
            assert all(record["failure_action"] == "report" for record in faults)
            assert all(record["failure_detail"] == "deadline_exceeded"
                       for record in faults if record["failure"] == "transport")
        result = json.loads((output / "result.json").read_text())
        result["status"] = status
        result["scenario"] = args.scenario
        if cooperative:
            result["wheel"] = wheel
        (output / "result.json").write_text(json.dumps(result))
    finally:
        try:
            if process is not None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=10)
            if not cooperative or (state_parent / "s/aw.sock").exists():
                stopped = subprocess.run([str(aw), "service", "stop", "--config", str(config)], env=env, capture_output=True, timeout=15)
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
