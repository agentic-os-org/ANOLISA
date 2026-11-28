"""Two independent BPF Sources on one fixture, with an unobserved TLS control."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess

from correlation import process_ref
from run_case import ROOT, Server, line, prepare, records, stop, wait_for


def case(output: Path, assets: tuple, threads: int) -> dict:
    directory = output / f"threads-{threads}"
    directory.mkdir(mode=0o700)
    client, certificate, key = assets
    server = Server(certificate, key)
    processes = []
    result = {"threads": threads}
    try:
        for _ in range(2):
            processes.append(
                subprocess.Popen(
                    [
                        str(client),
                        str(server.port),
                        str(threads),
                        "1000",
                        "128",
                        "1",
                        "500",
                        "0",
                        "0",
                    ],
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    text=True,
                )
            )
        target, control = processes
        for process in processes:
            assert line(process)["pid"] == process.pid
        permit = directory / "permit.json"
        permit.write_text(
            json.dumps(
                {
                    "schema_version": "aw.ptrace-permit-poc/v0",
                    "authorized_experiment": True,
                    "process": process_ref(target.pid),
                    "exe_sha256": hashlib.sha256(
                        Path(f"/proc/{target.pid}/exe").read_bytes()
                    ).hexdigest(),
                }
            )
        )
        files = [directory / "first.jsonl", directory / "second.jsonl"]
        for path in files:
            with path.open("w") as stream, path.with_suffix(".stderr").open("w") as error:
                process = subprocess.Popen(
                    [
                        "/usr/bin/python3",
                        str(ROOT / "scripts/uprobe_counter.py"),
                        "--permit",
                        str(permit),
                        "--library",
                        str(output / "uprobe-bpf.so"),
                        "--privileged-fixture-setup",
                    ],
                    stdout=stream,
                    stderr=error,
                )
            processes.append(process)
            wait_for(lambda: records(path) or process.poll() is not None)
            assert records(path)[0]["state"] == "observing", records(path)
            assert records(path)[0]["collector_pid"] == process.pid
        first, second = processes[2:]
        # A same-UID process using the same libssl inode must never enter these counters.
        control.stdin.write("\n")
        control.stdin.flush()
        control_result = line(control, 15)
        assert control_result["failures"] == 0
        assert all(
            any(row.get("hits") == 0 for row in records(path))
            and all(row.get("hits", 0) == 0 for row in records(path))
            for path in files
        )
        result["unrelated_fixture_hits"] = [0, 0]
        target.stdin.write("\n")
        target.stdin.flush()
        wait_for(
            lambda: all(any(row.get("hits", 0) >= 10 for row in records(path)) for path in files)
        )
        first.kill()
        first.wait(5)
        result["first_collector_exit"] = first.returncode
        target_result = line(target, 15)
        assert target_result["failures"] == 0
        second.send_signal(signal.SIGINT)
        second.wait(5)
        assert second.returncode == 0
        assert records(files[1])[-1]["hits"] == threads * 1000
        result["surviving_collector_hits"] = records(files[1])[-1]["hits"]
        wait_for(lambda: len(server.results) == threads * 2)
        digest = hashlib.sha256(b"A" * 128000).hexdigest()
        assert not server.errors
        assert all(item == {"length_bytes": 128000, "sha256": digest} for item in server.results)
        result["server_bytes"] = sum(item["length_bytes"] for item in server.results)
        result["server_digest_verified"] = True
        for process in (target, control):
            assert "TracerPid:\t0\n" in Path(f"/proc/{process.pid}/status").read_text()
            process.stdin.write("\n")
            process.stdin.flush()
            process.wait(5)
            assert process.returncode == 0
        result["status"] = "PASS"
    except Exception as error:
        result.update(status="FAIL", error=f"{type(error).__name__}: {error}")
    finally:
        for process in reversed(processes):
            stop(process)
        server.close()
        result["all_children_reaped"] = all(p.poll() is not None for p in processes)
        (directory / "result.json").write_text(json.dumps(result, indent=2))
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    if options.output.exists():
        parser.error("output must be new")
    os.umask(0o077)
    assets = prepare(options.output)
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
            str(options.output / "uprobe-bpf.so"),
            str(ROOT / "fixtures/uprobe_bpf.c"),
        ],
        check=True,
        timeout=30,
    )
    results = [case(options.output, assets, threads) for threads in (1, 4, 8, 16)]
    (options.output / "results.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2))
    raise SystemExit(any(row["status"] != "PASS" for row in results))


if __name__ == "__main__":
    main()
