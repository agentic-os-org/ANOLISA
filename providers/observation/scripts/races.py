"""Exercise thread churn, busy attachment and job-control preservation on owned fixtures."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import signal
import subprocess
import time

from lifecycle import detached
from run_case import ROOT, Server, line, prepare, records, stop, wait_for


def stopped(pid: int) -> bool:
    states = [
        (task / "stat").read_text().rsplit(") ", 1)[1][0]
        for task in Path(f"/proc/{pid}/task").iterdir()
    ]
    return bool(states) and all(state in "Tt" for state in states)


def run(output: Path, assets: tuple, name: str, threads: int) -> dict:
    directory = output / f"{name}-{threads}"
    directory.mkdir(mode=0o700)
    server = Server(assets[1], assets[2])
    fixture = tracer = None
    result = {"case": name, "threads": threads}
    calls, length = 30, 128
    churn = name in ("churn", "busy_attach")
    connections = threads * (4 if churn else 1)
    binary = ROOT / "target/release/aw-observation-ptrace-poc"
    events = directory / "events.jsonl"
    try:
        fixture = subprocess.Popen(
            [
                str(assets[0]),
                str(server.port),
                str(threads),
                str(calls),
                str(length),
                "2" if churn else "0",
                "1000",
                "0",
                "0",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        line(fixture)
        permit = directory / "permit.json"
        subprocess.run(
            [str(binary), "authorize", "--pid", str(fixture.pid), "--output", str(permit)],
            check=True,
            timeout=5,
        )
        result["process"] = json.loads(permit.read_text())["process"]
        if name == "busy_attach":
            fixture.stdin.write("\n")
            fixture.stdin.flush()
            time.sleep(0.015)
        with (directory / "tracer.stderr").open("w") as error:
            tracer = subprocess.Popen(
                [
                    str(binary),
                    "observe",
                    "--permit",
                    str(permit),
                    "--output",
                    str(events),
                    "--duration",
                    "10",
                ],
                stderr=error,
            )
        wait_for(lambda: records(events) or tracer.poll() is not None)
        if name != "busy_attach":
            assert tracer.poll() is None
        if name.startswith("group_"):
            fixture.send_signal(signal.SIGSTOP)
            wait_for(lambda: stopped(fixture.pid))
            fixture.stdin.write("\n")
            fixture.stdin.flush()
            time.sleep(0.1)
            assert stopped(fixture.pid)
            assert not any(e.get("kind") == "transport.before_send" for e in records(events))
            if name == "group_detach":
                tracer.send_signal(signal.SIGINT)
                tracer.wait(5)
                assert tracer.returncode == 0
                assert detached(fixture.pid), "detach_left_tracer_owner"
                result["detach_transition_states"] = [
                    (task / "stat").read_text().rsplit(") ", 1)[1][0]
                    for task in Path(f"/proc/{fixture.pid}/task").iterdir()
                ]
                # Kernel unlink briefly schedules TRACED -> RUNNING -> STOPPED.
                # Require convergence and continued suspension before sending SIGCONT.
                wait_for(lambda: stopped(fixture.pid), timeout=1)
                time.sleep(0.1)
                assert stopped(fixture.pid), "group_stop_was_lost"
                assert not server.results, "target_completed_while_group_stopped"
            fixture.send_signal(signal.SIGCONT)
        elif name != "busy_attach":
            fixture.stdin.write("\n")
            fixture.stdin.flush()
        result["fixture"] = line(fixture, 15)
        assert result["fixture"]["failures"] == 0
        assert result["fixture"]["attempts"] == connections * calls
        wait_for(lambda: len(server.results) == connections)
        digest = hashlib.sha256(b"A" * (calls * length)).hexdigest()
        assert all(r == {"length_bytes": calls * length, "sha256": digest} for r in server.results)
        assert not server.errors
        if tracer.poll() is None:
            tracer.send_signal(signal.SIGINT)
        tracer.wait(5)
        assert detached(fixture.pid)
        diagnostic = (directory / "tracer.stderr").read_text()
        assert "cleanup_incomplete" not in diagnostic
        if name != "busy_attach":
            assert tracer.returncode == 0, diagnostic
        result["tracer_exit"] = tracer.returncode
        result["diagnostic"] = diagnostic
        writes = [e for e in records(events) if e.get("kind") == "transport.before_send"]
        result["hits"] = len(writes)
        if name in ("churn", "group_resume"):
            assert len(writes) == connections * calls, result
        result["server_verified"] = True
        fixture.stdin.write("\n")
        fixture.stdin.flush()
        fixture.wait(5)
        assert fixture.returncode == 0
        result["status"] = "PASS"
    except Exception as error:
        result["status"] = "FAIL"
        result["error"] = f"{type(error).__name__}: {error}"
    finally:
        if fixture and fixture.poll() is None:
            fixture.send_signal(signal.SIGCONT)
        stop(tracer)
        stop(fixture)
        server.close()
        result["cleanup"] = {
            "tracer_reaped": tracer is None or tracer.poll() is not None,
            "fixture_reaped": fixture is None or fixture.poll() is not None,
        }
        (directory / "result.json").write_text(json.dumps(result, indent=2))
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    os.umask(0o077)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        print(json.dumps({"status": "SKIP", "reason": "requires_linux_x86_64"}))
        return 77
    options.output.mkdir(mode=0o700)
    assets = prepare(options.output / "assets")
    results = []
    for threads in (1, 4, 8, 16):
        for name in ("churn", "busy_attach", "group_resume", "group_detach"):
            result = run(options.output, assets, name, threads)
            results.append(result)
            print(json.dumps(result), flush=True)
    (options.output / "results.json").write_text(json.dumps(results, indent=2))
    return int(any(result["status"] != "PASS" for result in results))


if __name__ == "__main__":
    raise SystemExit(main())
