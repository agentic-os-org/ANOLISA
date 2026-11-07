"""Run bounded x86-64 acceptance and report failures/SKIPs without granting P1."""

import argparse
import json
import os
from pathlib import Path
import platform
import resource
import sys

from aw_fixture import AwFixture
from negative_cases import run_negative_cases
from run_case import prepare, run_case, wait_for


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--aw", type=Path)
    options = parser.parse_args()
    os.umask(0o077)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    output = options.output.resolve()
    if output.exists():
        parser.error("output must be a new directory")
    output.mkdir(parents=True, mode=0o700)
    policy_path = Path("/proc/sys/kernel/yama/ptrace_scope")
    policy = policy_path.read_text().strip() if policy_path.exists() else "unavailable"
    report = {"environment": {"machine": platform.machine(), "kernel": platform.release(),
                               "uid": os.getuid(), "ptrace_scope": policy}, "cases": []}
    if platform.machine() != "x86_64" or platform.system() != "Linux":
        report["cases"].append({"case": "native_ptrace", "status": "SKIP", "reason": "requires Linux x86-64"})
        (output / "results.json").write_text(json.dumps(report, indent=2))
        return 77
    assets = prepare(output)
    cases = []
    for repeat in range(3):
        for threads in (1, 4, 8, 16):
            cases.append((f"baseline-{threads}-{repeat}", {"threads": threads, "calls": 300, "trace": False}))
            cases.append((f"observe-{threads}-{repeat}", {"threads": threads, "calls": 300}))
    for threads in (4, 8, 16):
        cases.append((f"clone-{threads}", {"threads": threads, "calls": 100, "late": True}))
    cases.extend([
        ("idle", {"idle_seconds": 2.0}),
        ("sustained-cap", {"threads": 16, "calls": 4096, "max_events": 10}),
        ("write-ex", {"use_ex": True}), ("zero", {"length": 0}),
        ("boundary-4096", {"length": 4096}), ("oversize-4097", {"length": 4097}),
        ("fragmented", {"length": 7, "calls": 500}),
        ("event-cap", {"max_events": 10}), ("rate-cap", {"rate": 1}),
        ("sigint", {"mode": "sigint", "threads": 4, "calls": 200}),
        ("sigterm", {"mode": "sigterm", "threads": 4, "calls": 200}),
        ("foreign-signal", {"mode": "usr1"}),
        ("target-kill", {"mode": "target_exit", "threads": 4}),
    ])
    for repeat in range(5):
        cases.append((f"tracer-sigkill-{repeat}", {"mode": "sigkill", "threads": 4, "calls": 300}))

    def execute(name, arguments):
        try:
            result = run_case(output, assets, name, **arguments)
        except Exception as error:
            stored = output / name / "result.json"
            result = json.loads(stored.read_text()) if stored.exists() else {"case": name, "status": "FAIL", "error": str(error)}
        report["cases"].append(result)
        print(name, result.get("status"), flush=True)
        (output / "results.json").write_text(json.dumps(report, indent=2))

    for name, arguments in cases:
        execute(name, arguments)
    try:
        report["cases"].extend(run_negative_cases(output, assets))
    except Exception as error:
        report["cases"].append({"case": "negative_paths", "status": "FAIL", "error": str(error)})
    if options.aw:
        aw = AwFixture(options.aw.resolve(), output / "aw-daemon")
        try:
            for count in (1, 2):
                execute(f"aw-instances-{count}", {"aw": aw.aw, "socket": aw.socket, "instances": count, "threads": 4})
                wait_for(lambda: aw.status()["bindings"] == 0)
            report["aw_after_owner_exit"] = aw.status()
        finally:
            aw.close()
        report["aw_daemon_reaped"] = aw.process.poll() is not None
    else:
        report["cases"].append({"case": "aw_instance", "status": "SKIP", "reason": "--aw binary not supplied"})
    report["cases"].extend([
        {"case": "uprobe_coexistence", "status": "SKIP", "reason": "requires a separately authorized existing uProbe environment"},
        {"case": "actual_pid_wraparound", "status": "SKIP", "reason": "identity mutation is tested; kernel PID wraparound is not forced"},
        {"case": "other_tls_abis", "status": "SKIP", "reason": "dynamic x86-64 OpenSSL only"},
        {"case": "p1_rewrite", "status": "SKIP", "reason": "P0 SIGKILL crash-safety gate not satisfied; no mutation path implemented"},
    ])
    report["ptrace_scope_unchanged"] = not policy_path.exists() or policy_path.read_text().strip() == policy
    report["p0_accepted"] = all(c["status"] == "PASS" for c in report["cases"] if c["case"] not in ("p1_rewrite", "actual_pid_wraparound", "uprobe_coexistence", "other_tls_abis"))
    (output / "results.json").write_text(json.dumps(report, indent=2))
    return 0 if report["p0_accepted"] else 1


if __name__ == "__main__":
    sys.exit(main())
