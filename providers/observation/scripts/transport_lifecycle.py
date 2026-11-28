"""Repeat target-exit races and reject a live fixture's changed executable."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

from correlation import process_ref
from run_case import ROOT, Server, line, records, stop, wait_for
from transport_cases import build, one


def executable_change(output: Path, assets: tuple) -> dict:
    directory = output / "exec"
    directory.mkdir(mode=0o700)
    client, certificate, key = assets
    server = Server(certificate, key)
    target = collector = None
    result = {"case": "exec_identity_change"}
    try:
        target = subprocess.Popen(
            [str(client), str(server.port), "1", "10", "128", "1", "0", "2", "0"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        assert line(target)["pid"] == target.pid
        before = process_ref(target.pid)
        permit = directory / "permit.json"
        permit.write_text(
            json.dumps(
                {
                    "schema_version": "aw.ptrace-permit-poc/v0",
                    "authorized_experiment": True,
                    "process": before,
                    "exe_sha256": hashlib.sha256(
                        Path(f"/proc/{target.pid}/exe").read_bytes()
                    ).hexdigest(),
                }
            )
        )
        events = directory / "events.jsonl"
        with (directory / "collector.stderr").open("w") as error:
            collector = subprocess.Popen(
                [
                    sys.executable,
                    str(ROOT / "scripts/uprobe_transport.py"),
                    "--permit",
                    str(permit),
                    "--library",
                    str(output / "loader.so"),
                    "--object",
                    str(output / "transport.bpf.o"),
                    "--output",
                    str(events),
                    "--privileged-fixture-setup",
                ],
                stderr=error,
            )
        wait_for(lambda: records(events) or collector.poll() is not None)
        assert collector.poll() is None and records(events)[0]["state"] == "observing"
        target.stdin.write("\n")
        target.stdin.flush()
        collector.wait(5)
        assert collector.returncode == 0
        assert target.poll() is None
        after = process_ref(target.pid)
        assert (
            before["start_ticks"] == after["start_ticks"]
            and before["exe_inode"] != after["exe_inode"]
        )
        summary = records(events)[-1]
        assert summary["reason"] == "identity_changed" and summary["hits"] == 0
        assert summary["accounting_complete"]
        assert "TracerPid:\t0\n" in Path(f"/proc/{target.pid}/status").read_text()
        result.update(status="PASS", target_remained_alive=True, summary=summary)
    except Exception as error:
        result.update(status="FAIL", error=f"{type(error).__name__}: {error}")
    finally:
        stop(collector)
        stop(target)
        server.close()
        (directory / "result.json").write_text(json.dumps(result, indent=2))
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        parser.error("output must be new")
    os.umask(0o077)
    assets = build(args.output)
    results = [executable_change(args.output, assets)]
    for index in range(20):
        results.append(
            one(
                args.output,
                assets,
                f"exit-{index}",
                threads=4 if index % 2 else 16,
                mode="target_exit",
            )
        )
    (args.output / "results.json").write_text(json.dumps(results, indent=2))
    print(
        json.dumps(
            {
                "passed": sum(row["status"] == "PASS" for row in results),
                "failed": sum(row["status"] != "PASS" for row in results),
            }
        )
    )
    raise SystemExit(any(row["status"] != "PASS" for row in results))


if __name__ == "__main__":
    main()
