"""Native acceptance for bounded BPF snapshots, loss and real AW association."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

from aw_fixture import AwFixture
from correlation import correlate, import_registration, process_ref
from run_case import ROOT, Server, line, prepare, records, stop, wait_for
from uprobe_cases import process_stat


def build(output: Path) -> tuple:
    assets = prepare(output)
    subprocess.run(
        [
            "clang",
            "-target",
            "bpf",
            "-O2",
            "-g",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-c",
            str(ROOT / "src/bpf/transport.bpf.c"),
            "-o",
            str(output / "transport.bpf.o"),
        ],
        check=True,
        timeout=30,
    )
    subprocess.run(
        [
            "gcc",
            "-shared",
            "-fPIC",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            str(ROOT / "fixtures/transport_loader.c"),
            "-lbpf",
            "-o",
            str(output / "loader.so"),
        ],
        check=True,
        timeout=30,
    )
    return assets


def one(
    output: Path,
    assets: tuple,
    name: str,
    threads: int = 1,
    length: int = 128,
    late: int = 0,
    function: int = 0,
    mode: str = "normal",
    aw: AwFixture | None = None,
    instances: int = 1,
    calls: int = 50,
) -> dict:
    directory = output / name
    directory.mkdir(mode=0o700)
    client, certificate, key = assets
    server = Server(certificate, key)
    target = collector = None
    result = {
        "case": name,
        "threads": threads,
        "length": length,
        "late": late,
        "function": "SSL_write_ex" if function else "SSL_write",
        "mode": mode,
        "calls": calls,
    }
    event_path = directory / "events.jsonl"
    delay = 0 if mode in ("backpressure", "load", "baseline") else 5000
    try:
        argv = [
            str(client),
            str(server.port),
            str(threads),
            str(calls),
            str(length),
            str(late),
            str(delay),
            str(function),
            "0",
        ]
        if aw:
            argv = [
                sys.executable,
                str(ROOT / "fixtures/aw_owner.py"),
                aw.aw,
                aw.socket,
                str(directory / "registration.json"),
                str(instances),
                *argv,
            ]
        with (directory / "target.stderr").open("w") as error:
            target = subprocess.Popen(
                argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=error, text=True
            )
        assert line(target)["pid"] == target.pid
        identity = process_ref(target.pid)
        mapping = import_registration(directory / "registration.json", identity) if aw else []
        permit = {
            "schema_version": "aw.ptrace-permit-poc/v0",
            "authorized_experiment": True,
            "process": identity,
            "exe_sha256": hashlib.sha256(Path(f"/proc/{target.pid}/exe").read_bytes()).hexdigest(),
        }
        path = directory / "permit.json"
        path.write_text(json.dumps(permit))
        if mode != "baseline":
            with (directory / "collector.stderr").open("w") as error:
                collector = subprocess.Popen(
                    [
                        sys.executable,
                        str(ROOT / "scripts/uprobe_transport.py"),
                        "--permit",
                        str(path),
                        "--library",
                        str(output / "loader.so"),
                        "--object",
                        str(output / "transport.bpf.o"),
                        "--function",
                        result["function"],
                        "--output",
                        str(event_path),
                        "--privileged-fixture-setup",
                        "--max-events",
                        "10" if mode == "cap" else "65536",
                        "--rate",
                        "1" if mode == "rate" else "100000",
                    ],
                    stderr=error,
                )
            wait_for(lambda: records(event_path) or collector.poll() is not None)
            assert collector.poll() is None, (directory / "collector.stderr").read_text()
            status = records(event_path)[0]
            assert status["state"] == "observing" and status["collector_pid"] == collector.pid
            result["collector_before"] = process_stat(collector.pid)
            assert result["collector_before"]["uid"] == os.getuid()
            assert int(result["collector_before"]["cap_eff"], 16) == 0
            if mode in ("backpressure", "kill_ready"):
                collector.send_signal(signal.SIGSTOP if mode == "backpressure" else signal.SIGKILL)
                if mode == "kill_ready":
                    collector.wait(5)
        target.stdin.write("\n")
        target.stdin.flush()
        if mode in ("kill", "interrupt", "terminate", "target_exit"):
            wait_for(lambda: len(records(event_path)) >= 10)
            if mode == "target_exit":
                target.kill()
                target.wait(5)
                collector.wait(5)
                assert collector.returncode == 0
                assert records(event_path)[-1]["reason"] == "target_exited"
                result.update(
                    status="PASS",
                    summary=records(event_path)[-1],
                    collector_exit=collector.returncode,
                )
                return result
            collector.send_signal(
                {"kill": signal.SIGKILL, "interrupt": signal.SIGINT, "terminate": signal.SIGTERM}[
                    mode
                ]
            )
            collector.wait(5)
        result["fixture"] = line(target, 30)
        result["target_resources"] = process_stat(target.pid)
        assert result["fixture"]["failures"] == 0
        connections = threads * (4 if late == 2 else 1)
        wait_for(lambda: len(server.results) == connections)
        wire = b"".join((b"B" if function == 4 and i % 2 else b"A") * length for i in range(calls))
        expected = hashlib.sha256(wire).hexdigest()
        assert not server.errors, server.errors
        assert all(
            item == {"length_bytes": calls * length, "sha256": expected} for item in server.results
        )
        result["server"] = {"bytes": calls * length * connections, "digest_verified": True}
        if collector and collector.poll() is None:
            result["collector_after"] = process_stat(collector.pid)
            if mode == "backpressure":
                result["completed_while_collector_stopped"] = True
                collector.send_signal(signal.SIGCONT)
            collector.send_signal(signal.SIGINT)
            collector.wait(5)
        if collector:
            result["collector_exit"] = collector.returncode
            stream = records(event_path)
            events = [event for event in stream if event["kind"] == "transport.before_send"]
            summaries = [event for event in stream if event["kind"] == "source_summary"]
            result["summary"] = summaries[-1] if summaries else None
            result["events"] = len(events)
            if mode in ("kill", "kill_ready"):
                assert collector.returncode == -signal.SIGKILL
                assert result["summary"] is None
            assert "AAAA" not in event_path.read_text() and "BBBB" not in event_path.read_text()
            assert all(len(row) <= 8192 for row in event_path.read_bytes().splitlines())
            valid_hashes = {
                hashlib.sha256(bytes([value]) * length).hexdigest()
                for value in ([65, 66] if function == 4 else [65])
            }
            for event in events:
                assert event["transport"]["length_bytes"] == length
                assert event["coverage"]["status"] == "unknown"
                assert event["transport"]["adoption"] == "unknown"
                assert (
                    event["transport"]["snapshot_consistency"] == "non_atomic_user_memory_snapshot"
                )
                if length <= 4096:
                    assert event["transport"]["payload_sha256"] in valid_hashes
                else:
                    assert event["transport"]["payload_sha256"] is None
                    assert event["coverage"]["reason"] == "payload_over_limit"
            if function == 4 and mode == "normal":
                for tid in {event["thread_id"] for event in events}:
                    timeline = sorted(
                        (event for event in events if event["thread_id"] == tid),
                        key=lambda event: int(event["observed_at_ns"]),
                    )
                    assert len(timeline) == calls
                    for index, event in enumerate(timeline):
                        expected_digest = hashlib.sha256(
                            (b"B" if index % 2 else b"A") * length
                        ).hexdigest()
                        assert event["transport"]["payload_sha256"] == expected_digest
            assert len({event["sequence"] for event in events}) == len(events)
            if mode not in ("kill", "kill_ready"):
                assert collector.returncode == 0, (directory / "collector.stderr").read_text()
                summary = result["summary"]
                assert summary["accounting_complete"]
                assert summary["hits"] == summary["ring_full"] + summary["received"]
                assert (
                    summary["received"]
                    == summary["emitted"] + summary["output_dropped"] + summary["identity_dropped"]
                )
                if mode not in ("interrupt", "terminate"):
                    assert summary["hits"] == result["fixture"]["attempts"]
                if mode == "normal":
                    assert not summary["ring_full"] and len(events) == result["fixture"]["attempts"]
                if mode == "backpressure":
                    assert summary["ring_full"] > 0
                if mode in ("cap", "rate"):
                    assert summary["output_dropped"] > 0
                if mode == "cap":
                    assert len(events) == 10
            if events:
                correlated = [correlate(event, mapping) for event in events]
                status = (
                    "unassociated"
                    if not aw
                    else "verified_fixture_owner" if instances == 1 else "ambiguous_process_owner"
                )
                assert all(event["association"]["status"] == status for event in correlated)
                result["association"] = status
                (directory / "correlated.jsonl").write_text(
                    "".join(json.dumps(e) + "\n" for e in correlated)
                )
        assert "TracerPid:\t0\n" in Path(f"/proc/{target.pid}/status").read_text()
        target.stdin.write("\n")
        target.stdin.flush()
        target.wait(5)
        assert target.returncode == 0
        result["status"] = "PASS"
    except Exception as error:
        result.update(status="FAIL", error=f"{type(error).__name__}: {error}")
    finally:
        if collector and collector.poll() is None:
            collector.send_signal(signal.SIGCONT)
        stop(collector)
        stop(target)
        server.close()
        result["children_reaped"] = all(
            p is None or p.poll() is not None for p in (collector, target)
        )
        (directory / "result.json").write_text(json.dumps(result, indent=2))
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--aw", type=Path)
    parser.add_argument("--smoke", action="store_true")
    options = parser.parse_args()
    if options.output.exists():
        parser.error("output must be new")
    os.umask(0o077)
    assets = build(options.output)
    results = []
    specs = [("single", {})]
    if not options.smoke:
        for threads in (1, 4, 8, 16):
            for mode in (
                "normal",
                "kill",
                "kill_ready",
                "interrupt",
                "terminate",
                "backpressure",
                "cap",
                "rate",
            ):
                specs.append(
                    (
                        f"{mode}-{threads}",
                        {
                            "threads": threads,
                            "mode": mode,
                            "calls": 512 if mode == "backpressure" else 50,
                        },
                    )
                )
            for late in (1, 2):
                specs.append((f"clone-{late}-{threads}", {"threads": threads, "late": late}))
            for function in (1, 4, 5):
                specs.append(
                    (f"function-{function}-{threads}", {"threads": threads, "function": function})
                )
            for mode in ("baseline", "load"):
                specs.append(
                    (f"load-{mode}-{threads}", {"threads": threads, "mode": mode, "calls": 1024})
                )
        for length in (0, 1, 4096, 4097, 8192):
            specs.append((f"length-{length}", {"length": length}))
        specs.append(("target-exit", {"threads": 4, "mode": "target_exit"}))
    for name, kwargs in specs:
        result = one(options.output, assets, name, **kwargs)
        results.append(result)
        print(
            json.dumps({"case": name, "status": result["status"], "error": result.get("error")}),
            flush=True,
        )
        (options.output / "results.json").write_text(json.dumps(results, indent=2))
    if options.aw:
        daemon = AwFixture(options.aw.resolve(), options.output / "aw")
        try:
            for count in (1, 2):
                result = one(
                    options.output, assets, f"aw-{count}", threads=4, aw=daemon, instances=count
                )
                results.append(result)
                wait_for(lambda: daemon.status()["bindings"] == 0)
            assert daemon.status()["bindings"] == 0
        finally:
            daemon.close()
    (options.output / "results.json").write_text(json.dumps(results, indent=2))
    raise SystemExit(any(row["status"] != "PASS" for row in results))


if __name__ == "__main__":
    main()
