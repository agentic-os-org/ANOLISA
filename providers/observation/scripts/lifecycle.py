"""Deterministic tracer-death and recovery experiments on disposable TLS children."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import signal
import subprocess

from run_case import ROOT, Server, line, prepare, records, stop, wait_for


def detached(pid: int) -> bool:
    tasks = Path(f"/proc/{pid}/task")
    return tasks.exists() and all(
        "TracerPid:\t0\n" in (entry / "status").read_text() for entry in tasks.iterdir()
    )


def experiment(output: Path, assets: tuple, phase: str, action: str, threads: int) -> dict:
    directory = output / f"{phase}-{action}-{threads}"
    directory.mkdir(mode=0o700)
    server = Server(assets[1], assets[2])
    fixture = tracer = None
    result = {"phase": phase, "action": action, "threads": threads}
    calls, length = 50, 128
    try:
        fixture = subprocess.Popen(
            [
                str(assets[0]),
                str(server.port),
                str(threads),
                str(calls),
                str(length),
                str(int(phase == "clone_stop")),
                "1000",
                "0",
                "0",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        line(fixture)
        binary = ROOT / "target/release/aw-observation-ptrace-poc"
        permit = directory / "permit.json"
        subprocess.run(
            [str(binary), "authorize", "--pid", str(fixture.pid), "--output", str(permit)],
            check=True,
            timeout=5,
        )
        result["process"] = json.loads(permit.read_text())["process"]
        if action == "deny":
            binary = ROOT / "target/safe/release/aw-observation-ptrace-poc"
        with (directory / "tracer.stderr").open("w") as log:
            tracer = subprocess.Popen(
                [
                    str(binary),
                    "observe",
                    "--permit",
                    str(permit),
                    "--output",
                    str(directory / "events.jsonl"),
                    "--duration",
                    "10",
                ],
                env={
                    **os.environ,
                    "AW_PTRACE_TEST_PHASE": phase,
                    "AW_PTRACE_TEST_TRACE_CLEANUP": "1",
                },
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=log,
                text=True,
            )
        result["tracer_pid"] = tracer.pid
        released = phase in (
            "trap_stop",
            "after_rf",
            "before_continue",
            "after_continue",
            "clone_stop",
        )
        if released:
            wait_for(lambda: records(directory / "events.jsonl") or tracer.poll() is not None)
            fixture.stdin.write("\n")
            fixture.stdin.flush()
        if action == "deny":
            tracer.wait(5)
            assert tracer.returncode == 2
            assert "observation_disabled" in (directory / "tracer.stderr").read_text()
            assert not (directory / "events.jsonl").exists()
        else:
            result["checkpoint"] = line(tracer)
            assert result["checkpoint"]["phase"] == phase
            if action == "kill":
                tracer.kill()
            else:
                tracer.stdin.write("f")
                tracer.stdin.flush()
            tracer.wait(5)
            assert tracer.returncode == (-signal.SIGKILL if action == "kill" else 2)
            assert "cleanup_incomplete" not in (directory / "tracer.stderr").read_text()
        result["tracer_exit"] = tracer.returncode
        if not released:
            # This snapshot separates residual debug state from an already pending write trap.
            result["detached_before_first_write"] = detached(fixture.pid)
            fixture.stdin.write("\n")
            fixture.stdin.flush()
        result["fixture"] = line(fixture, 10)
        assert result["fixture"]["failures"] == 0
        wait_for(lambda: len(server.results) == threads)
        digest = hashlib.sha256(b"A" * (calls * length)).hexdigest()
        assert all(r == {"length_bytes": calls * length, "sha256": digest} for r in server.results)
        assert not server.errors
        assert detached(fixture.pid)
        result["server_verified"] = True
        fixture.stdin.write("\n")
        fixture.stdin.flush()
        fixture.wait(5)
        assert fixture.returncode == 0
        result["status"] = "PASS"
    except Exception as error:
        result["status"] = "FAIL"
        result["error"] = f"{type(error).__name__}: {error}"
        if fixture and fixture.poll() is not None:
            result["fixture_exit"] = fixture.returncode
    finally:
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
    parser.add_argument("--repeat", type=int, default=3, choices=range(1, 101))
    phases = (
        "seized",
        "partial_arm",
        "enabled_arm",
        "armed",
        "trap_stop",
        "after_rf",
        "before_continue",
        "after_continue",
        "clone_stop",
    )
    parser.add_argument("--phase", choices=phases)
    parser.add_argument("--action", choices=("fail", "kill"))
    parser.add_argument("--threads", type=int, choices=(1, 4, 16))
    options = parser.parse_args()
    os.umask(0o077)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        print(json.dumps({"status": "SKIP", "reason": "requires_linux_x86_64"}))
        return 77
    options.output.mkdir(mode=0o700)
    assets = prepare(options.output / "assets")
    results = []
    for iteration in range(options.repeat):
        output = options.output / str(iteration)
        output.mkdir(mode=0o700)
        if not options.phase:
            results.append(experiment(output, assets, "default", "deny", 4))
        for threads in (options.threads,) if options.threads else (1, 4, 16):
            for phase in (options.phase,) if options.phase else phases:
                for action in (options.action,) if options.action else ("fail", "kill"):
                    result = experiment(output, assets, phase, action, threads)
                    results.append(result)
                    print(
                        json.dumps(
                            {k: result[k] for k in ("phase", "action", "threads", "status")}
                        ),
                        flush=True,
                    )
    summary = {
        "results": results,
        "counts": {state: sum(r["status"] == state for r in results) for state in ("PASS", "FAIL")},
        "p0_crash_safety": (
            "FAIL"
            if any(r["action"] == "kill" and r["status"] == "FAIL" for r in results)
            else "NOT_ESTABLISHED"
        ),
    }
    (options.output / "results.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary["counts"]))
    return int(summary["counts"]["FAIL"] > 0)


if __name__ == "__main__":
    raise SystemExit(main())
