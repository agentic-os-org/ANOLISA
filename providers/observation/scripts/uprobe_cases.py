"""Exercise fd-owned uprobe counters against owned, gated loopback TLS fixtures."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import time

from correlation import process_ref
from run_case import ROOT, Server, line, prepare, records, stop, wait_for


def process_stat(pid: int) -> dict:
    base = Path(f"/proc/{pid}")
    fields = (base / "stat").read_text().rsplit(") ", 1)[1].split()
    status = (base / "status").read_text()
    return {
        "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
        "rss_kib": int(
            next(row for row in status.splitlines() if row.startswith("VmHWM:")).split()[1]
        ),
        "uid": int(next(row for row in status.splitlines() if row.startswith("Uid:")).split()[1]),
        "cap_eff": next(row for row in status.splitlines() if row.startswith("CapEff:")).split()[1],
    }


def one(
    output: Path,
    assets: tuple,
    name: str,
    threads: int = 1,
    mode: str = "normal",
    late: int = 0,
    use_ex: bool = False,
    privileged: bool = True,
    calls_override: int | None = None,
    idle_seconds: float = 0.15,
) -> dict:
    directory = output / name
    directory.mkdir(mode=0o700)
    client, certificate, key = assets
    server = Server(certificate, key)
    fixture = collector = None
    collector_pid = None
    result = {
        "case": name,
        "threads": threads,
        "mode": mode,
        "late": late,
        "function": "SSL_write_ex" if use_ex else "SSL_write",
    }
    calls = 1000 if mode in ("kill_active", "stop_active", "detach_active", "target_exit") else 200
    if calls_override is not None:
        calls = calls_override
    delay = 500 if "active" in mode or mode == "target_exit" else 0
    event_file = directory / "events.jsonl"
    try:
        with (directory / "fixture.stderr").open("w") as error:
            fixture = subprocess.Popen(
                [
                    str(client),
                    str(server.port),
                    str(threads),
                    str(calls),
                    "128",
                    str(late),
                    str(delay),
                    str(int(use_ex)),
                    "0",
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=error,
                text=True,
            )
        assert line(fixture)["pid"] == fixture.pid
        identity = process_ref(fixture.pid)
        permit = {
            "schema_version": "aw.ptrace-permit-poc/v0",
            "authorized_experiment": True,
            "process": identity,
            "exe_sha256": hashlib.sha256(Path(f"/proc/{fixture.pid}/exe").read_bytes()).hexdigest(),
        }
        if mode == "stale":
            permit["process"]["start_ticks"] += "0"
        permit_path = directory / "permit.json"
        permit_path.write_text(json.dumps(permit))
        if mode != "baseline":
            command = [
                "/usr/bin/python3",
                str(ROOT / "scripts/uprobe_counter.py"),
                "--permit",
                str(permit_path),
                "--function",
                result["function"],
                "--duration",
                "8",
            ]
            command += ["--library", str(output / "uprobe-bpf.so")]
            if privileged:
                command += ["--privileged-fixture-setup"]
            with event_file.open("w") as log, (directory / "collector.stderr").open("w") as error:
                collector = subprocess.Popen(
                    command, stdout=log, stderr=error, start_new_session=True
                )
            wait_for(lambda: records(event_file) or collector.poll() is not None)
            entries = records(event_file)
            assert entries, "collector exited without status"
            if mode in ("permission", "stale"):
                collector.wait(5)
                assert collector.returncode == 1 and entries[0]["kind"] == "error", entries
                if mode == "permission":
                    assert (
                        "[Errno 13]" in entries[0]["error"] or "[Errno 1]" in entries[0]["error"]
                    ), entries
                else:
                    assert entries[0]["error"] == "stale_process_identity", entries
                result["rejection"] = entries[0]["error"]
            else:
                assert entries[0]["state"] == "observing", entries
                collector_pid = entries[0]["collector_pid"]
                result["collector_identity"] = process_stat(collector_pid)
                assert result["collector_identity"]["uid"] == os.getuid()
                assert int(result["collector_identity"]["cap_eff"], 16) == 0
                initial = process_stat(collector_pid)
                time.sleep(idle_seconds)
                result["idle_cpu_seconds"] = (
                    process_stat(collector_pid)["cpu_seconds"] - initial["cpu_seconds"]
                )
                result["idle_window_seconds"] = idle_seconds
                active_cpu_start = process_stat(collector_pid)["cpu_seconds"]
                if mode == "kill_ready":
                    os.kill(collector_pid, signal.SIGKILL)
                    collector.wait(5)
        fixture.stdin.write("\n")
        fixture.stdin.flush()
        if mode in ("kill_active", "stop_active", "detach_active", "target_exit"):
            wait_for(lambda: any(row.get("hits", 0) >= 10 for row in records(event_file)))
            if mode == "target_exit":
                fixture.kill()
                fixture.wait(5)
                collector.wait(5)
                assert any(row.get("state") == "target_exited" for row in records(event_file))
                result["target_exit_detected"] = True
            else:
                result["hits_before_fault"] = records(event_file)[-1].get("hits")
                os.kill(
                    collector_pid,
                    {
                        "kill_active": signal.SIGKILL,
                        "stop_active": signal.SIGSTOP,
                        "detach_active": signal.SIGINT,
                    }[mode],
                )
                if mode != "stop_active":
                    collector.wait(5)
        if mode != "target_exit":
            result["fixture"] = line(fixture, 30)
            result["fixture_resources"] = process_stat(fixture.pid)
            assert result["fixture"]["failures"] == 0, result
            connections = threads * (4 if late == 2 else 1)
            wait_for(lambda: len(server.results) == connections)
            expected = hashlib.sha256(b"A" * (128 * calls)).hexdigest()
            assert all(
                item == {"length_bytes": 128 * calls, "sha256": expected} for item in server.results
            ), server.results
            assert not server.errors, server.errors
            result["server"] = {
                "connections": connections,
                "bytes": sum(r["length_bytes"] for r in server.results),
                "digest_verified": True,
            }
            if collector_pid and collector.poll() is None:
                result["collector_resources"] = process_stat(collector_pid)
                result["collector_active_cpu_seconds"] = (
                    result["collector_resources"]["cpu_seconds"] - active_cpu_start
                )
                if mode == "stop_active":
                    # Completion is verified while the collector remains stopped.
                    result["completed_while_collector_stopped"] = True
                    os.kill(collector_pid, signal.SIGCONT)
                os.kill(collector_pid, signal.SIGINT)
                collector.wait(5)
            for tid in Path(f"/proc/{fixture.pid}/task").iterdir():
                assert "TracerPid:\t0\n" in (tid / "status").read_text()
            result["no_ptrace_owner"] = True
            fixture.stdin.write("\n")
            fixture.stdin.flush()
            fixture.wait(5)
            assert fixture.returncode == 0, fixture.returncode
        if collector is not None:
            result["collector_exit"] = collector.returncode
            entries = records(event_file)
            counts = [row["hits"] for row in entries if "hits" in row]
            result["hits"] = counts[-1] if counts else None
            assert len(entries) <= 512
            if mode in ("normal", "stop_active"):
                assert result["hits"] == connections * calls, result
            if collector_pid:
                assert not Path(f"/proc/{collector_pid}").exists(), "collector not reaped"
        result["status"] = "PASS"
    except Exception as error:
        result.update(status="FAIL", error=f"{type(error).__name__}: {error}")
    finally:
        if collector_pid and Path(f"/proc/{collector_pid}").exists():
            os.kill(collector_pid, signal.SIGCONT)
            os.kill(collector_pid, signal.SIGTERM)
        stop(collector)
        stop(fixture)
        server.close()
        result["cleanup"] = {
            "collector_reaped": collector is None or collector.poll() is not None,
            "fixture_reaped": fixture is None or fixture.poll() is not None,
        }
        (directory / "result.json").write_text(json.dumps(result, indent=2))
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--performance", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.repeat <= 20:
        parser.error("repeat must be in [1, 20]")
    if args.output.exists():
        parser.error("output must be new")
    os.umask(0o077)
    assets = prepare(args.output)
    subprocess.run(
        [
            "gcc",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-shared",
            "-fPIC",
            "-o",
            str(args.output / "uprobe-bpf.so"),
            str(ROOT / "fixtures/uprobe_bpf.c"),
        ],
        check=True,
        timeout=30,
    )
    results = []
    specs = [
        ("permission", 1, "permission", 0, False, False),
        ("stale", 1, "stale", 0, False, True),
        ("smoke", 1, "normal", 0, False, True),
    ]
    if not args.smoke:
        for repeat in range(args.repeat):
            for threads in (1, 4, 8, 16):
                for mode in (
                    "baseline",
                    "normal",
                    "kill_ready",
                    "kill_active",
                    "stop_active",
                    "detach_active",
                ):
                    specs.append((f"{mode}-{threads}-{repeat}", threads, mode, 0, False, True))
                for late in (1, 2):
                    specs.append(
                        (f"clone-{late}-{threads}-{repeat}", threads, "normal", late, False, True)
                    )
                specs.append((f"ex-{threads}-{repeat}", threads, "normal", 0, True, True))
        specs.append(("target-exit", 4, "target_exit", 0, False, True))
    if args.performance:
        specs = [
            (f"{mode}-{threads}-{repeat}", threads, mode, 0, False, True)
            for repeat in range(args.repeat)
            for threads in (1, 4, 8, 16)
            for mode in ("baseline", "normal")
        ]
    for spec in specs:
        result = one(
            args.output,
            assets,
            *spec,
            calls_override=4096 if args.performance else None,
            idle_seconds=2 if args.performance else 0.15,
        )
        results.append(result)
        print(
            json.dumps(
                {"case": result["case"], "status": result["status"], "error": result.get("error")}
            ),
            flush=True,
        )
        summary = {
            "environment": {
                "kernel": platform.release(),
                "machine": platform.machine(),
                "perf_event_paranoid": Path("/proc/sys/kernel/perf_event_paranoid")
                .read_text()
                .strip(),
                "unprivileged_bpf_disabled": Path("/proc/sys/kernel/unprivileged_bpf_disabled")
                .read_text()
                .strip(),
            },
            "results": results,
            "passed": sum(row["status"] == "PASS" for row in results),
            "failed": sum(row["status"] == "FAIL" for row in results),
        }
        (args.output / "results.json").write_text(json.dumps(summary, indent=2))
    raise SystemExit(bool(summary["failed"]))


if __name__ == "__main__":
    main()
