"""One bounded native experiment; also supplies reusable acceptance helpers."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "fixtures"))
from server import Server


def line(process, timeout=15):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout):
            raise TimeoutError("fixture output deadline")
    value = process.stdout.readline()
    if not value:
        raise RuntimeError(f"fixture exited: {process.poll()}")
    return json.loads(value)


def wait_for(predicate, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise TimeoutError("condition deadline")


def records(path):
    if not path.exists():
        return []
    data = path.read_text().splitlines()
    result = []
    for item in data:
        try:
            result.append(json.loads(item))
        except json.JSONDecodeError:
            break
    return result


def stop(process):
    if process is None:
        return
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(4)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(3)
    for stream in (process.stdin, process.stdout):
        if stream:
            stream.close()


def prepare(output):
    output.mkdir(parents=True, exist_ok=True)
    os.chmod(output, 0o700)
    client = output / "tls-client"
    subprocess.run(["gcc", "-O2", "-Wall", "-Wextra", "-Werror", "-o", str(client),
                    str(ROOT / "fixtures/tls_client.c"), "-lssl", "-lcrypto", "-lpthread"], check=True, timeout=30)
    certificate, key = output / "cert.pem", output / "key.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                    "-subj", "/CN=localhost", "-keyout", str(key), "-out", str(certificate)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
    os.chmod(key, 0o600)
    return client, certificate, key


def run_case(output, assets, name, threads=1, calls=100, length=128, late=False,
             trace=True, mode="detach", use_ex=False, max_events=65536, rate=100000,
             aw=None, socket=None, instances=1, idle_seconds=0.15):
    directory = output / name
    directory.mkdir(mode=0o700)
    client, certificate, key = assets
    server = Server(certificate, key)
    fixture = tracer = None
    binary = ROOT / "target/release/aw-observation-ptrace-poc"
    event_file = directory / "events.jsonl"
    result = {"case": name, "threads": threads, "calls_per_thread": calls, "length": length, "trace": trace}
    try:
        argv = [str(client), str(server.port), str(threads), str(calls), str(length),
                str(int(late)), "1000" if mode in ("sigkill", "sigint", "sigterm") else "0", str(int(use_ex)), "0"]
        if aw:
            argv = [sys.executable, str(ROOT / "fixtures/aw_owner.py"), str(aw), socket,
                    str(directory / "registration.json"), str(instances), *argv]
        with (directory / "fixture.stderr").open("w") as error:
            fixture = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=error, text=True)
        ready = line(fixture)
        assert ready["pid"] == fixture.pid
        permit = directory / "permit.json"
        subprocess.run([str(binary), "authorize", "--pid", str(fixture.pid), "--output", str(permit)], check=True, timeout=10)
        identity = json.loads(permit.read_text())["process"]
        result["process"] = identity
        if aw:
            from correlation import import_registration
            mapping = import_registration(directory / "registration.json", identity)
            (directory / "mapping.json").write_text(json.dumps(mapping, indent=2))
        if trace:
            with (directory / "tracer.stderr").open("w") as error:
                tracer = subprocess.Popen([str(binary), "observe", "--permit", str(permit), "--output", str(event_file),
                                           "--duration", "15", "--max-events", str(max_events), "--rate", str(rate)], stderr=error)
            wait_for(lambda: records(event_file) or tracer.poll() is not None)
            assert tracer.poll() is None, (directory / "tracer.stderr").read_text()
            assert records(event_file)[0]["state"] == "observing"
            stat = Path(f"/proc/{tracer.pid}/stat").read_text().rsplit(") ", 1)[1].split()
            idle_before = int(stat[11]) + int(stat[12])
            idle_started = time.monotonic()
            time.sleep(idle_seconds)
            stat = Path(f"/proc/{tracer.pid}/stat").read_text().rsplit(") ", 1)[1].split()
            result["idle_window_seconds"] = time.monotonic() - idle_started
            result["idle_cpu_seconds"] = (int(stat[11]) + int(stat[12]) - idle_before) / os.sysconf("SC_CLK_TCK")
        fixture.stdin.write("\n")
        fixture.stdin.flush()
        if trace and mode == "usr1":
            fixture.send_signal(signal.SIGUSR1)
        if trace and mode in ("sigkill", "sigint", "sigterm"):
            wait_for(lambda: len(records(event_file)) > 10)
            tracer.send_signal({"sigkill": signal.SIGKILL, "sigint": signal.SIGINT, "sigterm": signal.SIGTERM}[mode])
        if mode == "target_exit":
            fixture.kill()
            fixture.wait(3)
            if tracer:
                tracer.wait(5)
            result["target_exit"] = True
        else:
            result["fixture"] = line(fixture, 30)
            assert result["fixture"]["failures"] == 0, result
            if mode == "usr1":
                assert result["fixture"]["signals"] == 1, result
            wait_for(lambda: len(server.results) == threads)
            expected = hashlib.sha256(b"A" * (length * calls)).hexdigest()
            assert all(r == {"length_bytes": length * calls, "sha256": expected} for r in server.results), server.results
            assert not server.errors, server.errors
            result["server"] = {"connections": len(server.results), "bytes": sum(r["length_bytes"] for r in server.results), "digest_verified": True}
            if trace and tracer.poll() is None:
                status = Path(f"/proc/{tracer.pid}/status").read_text()
                result["tracer_rss_kib"] = int(next(l for l in status.splitlines() if l.startswith("VmHWM:")).split()[1])
                stat = Path(f"/proc/{tracer.pid}/stat").read_text().rsplit(") ", 1)[1].split()
                result["tracer_cpu_seconds"] = (int(stat[11]) + int(stat[12])) / os.sysconf("SC_CLK_TCK")
                tracer.send_signal(signal.SIGINT)
            if tracer:
                tracer.wait(5)
                result["tracer_exit"] = tracer.returncode
            status = Path(f"/proc/{fixture.pid}/status").read_text()
            assert "TracerPid:\t0\n" in status
            result["detached"] = True
            if aw and trace:
                from correlation import correlate
                events = [r for r in records(event_file) if r.get("kind") == "transport.before_send"]
                correlated = [correlate(e, mapping) for e in events]
                (directory / "correlated.jsonl").write_text("".join(json.dumps(e) + "\n" for e in correlated))
                expected_status = "verified_fixture_owner" if instances == 1 else "ambiguous_process_owner"
                assert correlated and all(e["association"]["status"] == expected_status for e in correlated)
                result["association"] = expected_status
            fixture.stdin.write("\n")
            fixture.stdin.flush()
            fixture.wait(5)
            assert fixture.returncode == 0, fixture.returncode
        if trace:
            events = records(event_file)
            writes = [e for e in events if e.get("kind") == "transport.before_send"]
            summaries = [e for e in events if e.get("kind") == "source_summary"]
            result["events"] = len(writes)
            result["summary"] = summaries[-1] if summaries else None
            if mode == "detach":
                assert tracer.returncode == 0, (directory / "tracer.stderr").read_text()
                assert result["summary"]["hits"] == threads * calls, result
                if rate == 100000:
                    assert len(writes) == min(threads * calls, max_events), result
                else:
                    assert result["summary"]["dropped"] > 0, result
            assert all(e["transport"]["payload_sha256"] is None for e in writes if length > 4096)
            assert "AAAA" not in event_file.read_text()
        result["status"] = "PASS"
        return result
    except Exception as error:
        result["status"] = "FAIL"
        result["error"] = f"{type(error).__name__}: {error}"
        if fixture is not None:
            result["fixture_exit"] = fixture.poll()
        if tracer is not None:
            result["tracer_exit"] = tracer.poll()
        raise
    finally:
        stop(tracer)
        stop(fixture)
        server.close()
        result["cleanup"] = {"tracer_reaped": tracer is None or tracer.poll() is not None,
                             "fixture_reaped": fixture is None or fixture.poll() is not None}
        (directory / "result.json").write_text(json.dumps(result, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    os.umask(0o077)
    assets = prepare(options.output)
    print(json.dumps(run_case(options.output, assets, "single"), indent=2))
