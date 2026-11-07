"""Explicit failure-path acceptance against owned, idle experimental processes."""

import json
import os
from pathlib import Path
import signal
import subprocess
import time

from run_case import ROOT, line, records, stop, wait_for


def run_negative_cases(output, assets):
    binary = ROOT / "target/release/aw-observation-ptrace-poc"
    results = []
    for name in ("stale_start", "stale_namespace", "stale_boot", "stale_executable", "stale_fingerprint",
                 "permission_denied", "second_debugger", "unsupported_tls", "target_gone", "offsets", "enforce", "exec", "fork"):
        directory = output / name
        directory.mkdir(mode=0o700)
        first = None
        argv = [str(assets[0]), "1", "1", "1", "128", "0", "0", "0", "0"]
        if name in ("exec", "fork"):
            argv[7] = "2" if name == "exec" else "3"
        if name == "unsupported_tls":
            argv = ["/bin/sleep", "30"]
        fixture = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            if name != "unsupported_tls":
                line(fixture)
            permit = directory / "permit.json"
            subprocess.run([str(binary), "authorize", "--pid", str(fixture.pid), "--output", str(permit)], check=True, timeout=5)
            data = json.loads(permit.read_text())
            fields = {"stale_start": "start_ticks", "stale_namespace": "pid_namespace", "stale_boot": "boot_id", "stale_executable": "exe"}
            if name in fields:
                data["process"][fields[name]] += "changed"
                permit.write_text(json.dumps(data))
            if name == "stale_fingerprint":
                data["exe_sha256"] = "0" * 64
                permit.write_text(json.dumps(data))
            if name == "permission_denied":
                fixture.send_signal(signal.SIGUSR2)
                time.sleep(0.05)
            if name == "target_gone":
                fixture.terminate()
                fixture.wait(3)
            if name in ("second_debugger", "exec", "fork"):
                with (directory / "first.stderr").open("w") as log:
                    first = subprocess.Popen([str(binary), "observe", "--permit", str(permit), "--output", str(directory / "first.jsonl")], stderr=log)
                wait_for(lambda: records(directory / "first.jsonl") or first.poll() is not None)
                assert first.poll() is None
            if name in ("exec", "fork"):
                fixture.stdin.write("\n")
                fixture.stdin.flush()
                first.wait(5)
                assert first.returncode == 2, name
                diagnostic = (directory / "first.stderr").read_text()
                expected = ("exec_identity_changed",) if name == "exec" else ("unsupported_fork", "unsupported_non_thread_clone")
                assert any(code in diagnostic for code in expected), diagnostic
                assert "cleanup_incomplete" not in diagnostic, diagnostic
                if name == "exec":
                    assert fixture.poll() is None
                    assert "TracerPid:\t0\n" in Path(f"/proc/{fixture.pid}/status").read_text()
                else:
                    assert line(fixture)["kind"] == "fixture_forked"
                    assert "TracerPid:\t0\n" in Path(f"/proc/{fixture.pid}/status").read_text()
                    fixture.stdin.write("\n")
                    fixture.stdin.flush()
                    fixture.wait(3)
                    assert fixture.returncode == 0
                result = {"case": name, "status": "PASS", "diagnostic": diagnostic}
                results.append(result)
                (directory / "result.json").write_text(json.dumps(result, indent=2))
                continue
            extra = {"offsets": ["--offsets", "unsupported.json"], "enforce": ["--mode", "enforce"]}.get(name, [])
            completed = subprocess.run([str(binary), "observe", "--permit", str(permit), "--output", str(directory / "events.jsonl"), *extra],
                                       capture_output=True, text=True, timeout=8)
            assert completed.returncode == 2, (name, completed)
            if name == "offsets":
                assert "unsupported_offsets" in completed.stderr
            if name == "enforce":
                assert "unsupported_mode" in completed.stderr
            if first:
                assert first.poll() is None
                first.send_signal(signal.SIGINT)
                first.wait(5)
                assert first.returncode == 0
            if name != "target_gone":
                assert fixture.poll() is None, (name, fixture.returncode)
                assert "TracerPid:\t0\n" in Path(f"/proc/{fixture.pid}/status").read_text()
            (directory / "stderr.log").write_text(completed.stderr)
            result = {"case": name, "status": "PASS", "exit": completed.returncode, "diagnostic": completed.stderr.strip()}
            (directory / "result.json").write_text(json.dumps(result, indent=2))
            results.append(result)
        finally:
            stop(first)
            stop(fixture)
    return results
