"""Exercise four native tool decisions against the candidate's real sec-core daemon."""

import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import subprocess
import tempfile
import time

from agents import configure_qwenpaw, qoder_arguments, qwenpaw_turn
from runtime import Backend, Processes, await_condition, checked, save

CASES = ("allow", "scan-deny", "backend-error", "timeout-block")


def configuration(agent, argv, binaries, state, backend_socket):
    return {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "sec-core-native"},
        "spec": {
            "daemon": {
                "startup": "external",
                "endpoint": "auto",
                "state_dir": str(state),
            },
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {agent: {"adapter": agent, "argv": argv}},
            "providers": {
                "security": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [
                            str(binaries / "aw-provider-sec-core"),
                            "--cli",
                            str(binaries / "agent-sec-cli"),
                            "--socket",
                            str(backend_socket),
                        ],
                    },
                    "timeout_ms": 2000,
                    "max_output_bytes": 4096,
                    "config": {
                        "version": 1,
                        "mode": "block",
                        "tools": {
                            "Bash": {"language": "bash", "input_pointer": "/command"},
                            "execute_shell_command": {
                                "language": "bash",
                                "input_pointer": "/command",
                            },
                        },
                    },
                }
            },
            "events": {
                "tool.before": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "scan-code",
                            "provider": "security",
                            "operation": "scan_code",
                            "effects": ["observe", "block"],
                            "on_error": "block",
                        }
                    ],
                },
                "tool.after": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "observe-completion",
                            "provider": "security",
                            "operation": "observe_tool",
                            "effects": ["observe"],
                            "on_error": "report",
                        }
                    ],
                },
            },
        },
    }


def verify_outcome(report):
    """Require actual side effects, native callbacks and the expected security decision."""
    mode = report["case"]
    assert report["before"] == 1, "exactly one native before callback is required"
    assert report["tools"] == (
        ["Bash"] if report["agent"] == "qoder" else ["execute_shell_command"]
    )
    assert report["marker_exists"] is (mode == "allow"), "unexpected tool execution"
    assert report["sentinel_exists"], "denied deletion reached the native tool"
    assert report["after"] == (1 if mode == "allow" or report["agent"] == "qwenpaw" else 0)
    scans = report["scan_calls"]
    assert len(scans) == 1, "the native tool must reach exactly one security scan"
    scan = scans[0]
    if mode in ("allow", "scan-deny"):
        assert scan["status"] == "ok" and scan["failure"] is None
        assert scan["policy_block"] is (mode == "scan-deny")
        assert report["sec_verdicts"] == (
            ["warn", "warn"] if mode == "scan-deny" else ["pass", "pass"]
        )
        if mode == "scan-deny":
            assert (
                report["rule_hits"] == 2
            ), "both preflight and native scans must hit the real rule"
    else:
        assert scan["status"] == "error" and scan["failure"] in (
            "provider",
            "transport",
        )
        assert scan["failure_action"] == "block", "backend failure must fail closed"
        assert report["fault_probe_exit"] != 0
        if mode == "timeout-block":
            assert 1500 <= scan["call"]["elapsed_ms"] <= 5000, "deadline was not exercised"
    status = report["final_status"]["result"]
    assert status["audit_healthy"] and status["bindings"] == status["events"] == 0


def collect(report, case, evidence, state):
    records = [
        json.loads(line)["record"]
        for path in (state / "journal").glob("*.jsonl")
        for line in path.read_text().splitlines()
    ]
    events = [row["plan"]["event"] for row in records if "event" in row.get("plan", {})]
    before = [row for row in events if row["name"] == "tool.before"]
    report.update(
        marker_exists=(case / "work/marker.txt").exists(),
        sentinel_exists=(case / "work/delete-fixture/sentinel.txt").exists(),
        before=len(before),
        after=sum(row["name"] == "tool.after" for row in events),
        tools=[row["tool"]["name"] for row in before],
        scan_calls=[
            row
            for row in records
            if row.get("kind") == "provider" and row["call"]["step_id"] == "scan-code"
        ],
    )
    if report["case"] == "allow":
        assert (case / "work/marker.txt").read_text() == "42\n"
    if report["case"] in ("allow", "scan-deny"):
        scans = json.loads((evidence / "sec-events.json").read_text())
        assert len(scans) == 2
        assert all(str(case / "work") in row["details"]["request"]["code"] for row in scans)
        report["sec_verdicts"] = [row["details"]["result"]["verdict"] for row in scans]
        report["rule_hits"] = sum(
            any(
                hit["rule_id"] == "shell-recursive-delete"
                for hit in row["details"]["result"]["findings"]
            )
            for row in scans
        )
    if report["agent"] == "qwenpaw":
        requests = [
            json.loads(line) for line in (case / "model-requests.jsonl").read_text().splitlines()
        ]
        results = [row for request in requests for row in request["tool_results"]]
        assert len(results) == 1
        if report["case"] != "allow":
            assert "Tool blocked by AW policy" in results[0]["content"]
    else:
        results = [
            json.loads(line)
            for line in (evidence / "agent.log").read_text().splitlines()
            if line.startswith("{")
        ]
        assert report["agent_exit"] == 0 and results[-1]["num_turns"] == 2
    verify_outcome(report)


def run(args, mode, root):
    case, evidence = root / mode, args.output / mode
    case.mkdir(mode=0o700)
    evidence.mkdir()
    for name in ("work", "home", "state"):
        (case / name).mkdir(mode=0o700)
    state = case / "state"
    owned = case / "work/delete-fixture"
    owned.mkdir()
    (owned / "sentinel.txt").write_text("preserve on denial\n")
    tool_command = "printf '42\\n' > " + shlex.quote(str(case / "work/marker.txt"))
    if mode == "scan-deny":
        tool_command = "rm -r " + shlex.quote(str(owned)) + " && " + tool_command
    (case / "tool-command.txt").write_text(tool_command)
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("AW_", "QWENPAW_", "QODER_", "QODERCN_", "AGENT_SEC_"))
    }
    environment.update(PYTHONDONTWRITEBYTECODE="1", HOME=str(case / "home"))
    processes = Processes(case, evidence, environment)
    backend = Backend(case, evidence, processes, args.bin, args.image)
    aw = args.bin / "aw"
    report = {"agent": args.agent, "case": mode, "passed": False}
    ports = []
    try:
        backend.start()
        preflight = backend.scan(tool_command, timeout_ms=4000)
        save(evidence / "direct-scan.json", preflight)
        verdict = json.loads(preflight["stdout"])
        assert preflight["exit_code"] == 0 and verdict["ok"] is True
        assert verdict["verdict"] == ("warn" if mode == "scan-deny" else "pass")
        backend.inject(mode)
        if mode in ("backend-error", "timeout-block"):
            report["fault_probe_exit"] = json.loads((evidence / "fault-probe.json").read_text())[
                "exit_code"
            ]
        agent_env = {}
        if args.agent == "qwenpaw":
            argv, ports = configure_qwenpaw(case, args.agent_bin, environment, processes)
        else:
            argv = [str(args.agent_bin)]
            (case / "home/.qoder").mkdir()
            save(
                case / "home/.qoder/settings.json",
                {"general": {"enableAutoUpdate": False}},
            )
            if args.qoder_auth_dir:
                auth = case / "home/.qoder/.auth"
                auth.mkdir(mode=0o700)
                for name in ("user", "machine_id"):
                    shutil.copyfile(args.qoder_auth_dir / name, auth / name)
                    (auth / name).chmod(0o600)
            else:
                agent_env["QODER_PERSONAL_ACCESS_TOKEN"] = os.environ["QODER_PERSONAL_ACCESS_TOKEN"]
        config = evidence / "aw.json"
        save(config, configuration(args.agent, argv, args.bin, state, backend.socket))
        daemon = processes.start(
            "daemon",
            [str(aw), "serve", "--config", str(config), "--state-dir", str(state)],
        )
        await_condition(
            lambda: (state / "aw.sock").exists() or processes.exited(daemon),
            5,
            "AW startup",
        )
        assert not processes.exited(daemon), "AW exited before readiness"
        command = [str(aw), "run", "--config", str(config), "--agent", args.agent]
        if args.agent == "qoder":
            command += ["--", *qoder_arguments(tool_command)]
        child = processes.start("agent", command, ports[:1], extra_env=agent_env)
        if args.agent == "qoder":
            report["agent_exit"] = processes.wait(child, 130)
        else:
            await_condition(
                lambda: list(state.glob("launch-*/qwenpaw-ready.json")) or processes.exited(child),
                40,
                "QwenPaw plugin readiness",
            )
            assert not processes.exited(child), "QwenPaw exited before plugin readiness"
            report["plugin_receipt_seen"] = True
            qwenpaw_turn(case, evidence, ports[0], mode == "scan-deny", processes, child)
        # Reap the launcher and descendants before checking zero live bindings.
        processes.stop(child)
        report["final_status"] = json.loads(
            checked([aw, "status", "--config", config], env=environment).stdout
        )
        backend.recover()
        if mode != "backend-error":
            backend.events()
        collect(report, case, evidence, state)
        time.sleep(0.5)
        report["marker_after_recovery"] = (case / "work/marker.txt").exists()
        assert report["marker_after_recovery"] is (mode == "allow")
        report["passed"] = True
    except Exception as error:
        report["error"] = type(error).__name__ + ": " + str(error)
    finally:
        cleanup_errors = {}
        for name, close in (("backend", backend.close), ("processes", processes.close)):
            try:
                close()
            except Exception as error:
                cleanup_errors[name] = type(error).__name__ + ": " + str(error)
        try:
            if (state / "journal").exists():
                shutil.copytree(state / "journal", evidence / "journal")
            for name in ("tool-command.txt", "model-requests.jsonl"):
                if (case / name).exists():
                    shutil.copyfile(case / name, evidence / name)
        except Exception as error:
            cleanup_errors["evidence"] = type(error).__name__ + ": " + str(error)
        if cleanup_errors:
            report["cleanup_errors"] = cleanup_errors
            report["passed"] = False
        report["cleanup"] = {
            "container_removed": backend.record.get("removed", False),
            "processes_stopped": all(
                row.get("stopped") and row.get("pid_absent") for _, _, row in processes.items
            ),
            "socket_removed": not (state / "aw.sock").exists() and not backend.socket.exists(),
            "plugin_removed": not (case / "home/plugins/aw-native").exists(),
            "ports_closed": True,
        }
        for port in ports:
            with socket.socket() as connection:
                connection.settimeout(0.2)
                report["cleanup"]["ports_closed"] &= connection.connect_ex(("127.0.0.1", port)) != 0
        report["passed"] &= all(report["cleanup"].values())
        save(evidence / "result.json", report)
    print(json.dumps(report), flush=True)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", choices=("qoder", "qwenpaw"), required=True)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--agent-bin", type=Path, required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--qoder-auth-dir", type=Path, help="Local-only existing login; never uploaded"
    )
    args = parser.parse_args()
    args.bin, args.agent_bin, args.output = (
        args.bin.resolve(),
        args.agent_bin.resolve(),
        args.output.resolve(),
    )
    args.output.mkdir(parents=True, exist_ok=False)
    result = {"agent": args.agent, "cases": [], "passed": False}
    try:
        if args.agent == "qoder":
            inventory = Path(__file__).resolve().parents[5] / "tests/native-matrix.json"
            expected = json.loads(inventory.read_text())["agents"]["qoder"]["qoder_cli"]["version"]
            assert checked([args.agent_bin, "--version"]).stdout.strip() == expected
            if args.qoder_auth_dir:
                assert not os.environ.get("GITHUB_ACTIONS"), "CI must use its declared PAT Secret"
            elif not os.environ.get("QODER_PERSONAL_ACCESS_TOKEN"):
                raise ValueError("Qoder native acceptance requires QODER_PERSONAL_ACCESS_TOKEN")
        with tempfile.TemporaryDirectory(prefix="awsec-") as temporary:
            for mode in CASES:
                result["cases"].append(run(args, mode, Path(temporary)))
        result["runtime_removed"] = not Path(temporary).exists()
        result["passed"] = len(result["cases"]) == len(CASES) and all(
            row["passed"] for row in result["cases"]
        )
    finally:
        save(args.output / "results.json", result)
    if not result["passed"]:
        raise SystemExit("native sec-core acceptance failed; inspect per-case evidence")


if __name__ == "__main__":

    def interrupted(signum, _frame):
        raise KeyboardInterrupt(f"received signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    os.umask(0o077)
    main()
