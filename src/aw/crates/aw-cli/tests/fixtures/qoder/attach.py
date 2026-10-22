"""Require native Qoder TUI attach, first-tool denial and detach without restarting."""

import argparse
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid

AW = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(AW / "scripts"))
from native_qoder import install


def execute(args):
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    report = {"passed": False, "native_version": "1.1.73", "cases": []}
    if not os.environ.get("QODER_PERSONAL_ACCESS_TOKEN"):
        raise RuntimeError("QODER_PERSONAL_ACCESS_TOKEN is required")
    controls = {key: value for key, value in os.environ.items() if key != "QODER_PERSONAL_ACCESS_TOKEN"}
    name = "aw-native-attach-" + uuid.uuid4().hex[:12]
    child = None
    master = None
    other_child = None
    other_master = None
    with tempfile.TemporaryDirectory(prefix="aw-qoder-attach-") as directory:
        root = Path(directory)
        home = root / "home"
        (home / ".qoder").mkdir(parents=True, mode=0o700)
        (root / "work").mkdir(mode=0o700)
        (root / "other-work").mkdir(mode=0o700)
        (root / "other-home/.qoder").mkdir(parents=True, mode=0o700)
        unrelated = root / "other-home/.qoder/settings.json"
        unrelated.write_text('{"theme":"other"}')
        unrelated.chmod(0o600)
        settings = home / ".qoder/settings.json"
        settings.write_text('{"general":{"enableAutoUpdate":false}}')
        settings.chmod(0o600)
        binary = args.qoder
        if binary is None:
            pin = json.loads((AW / "tests/native-matrix.json").read_text())["agents"]["qoder"]["qoder_reload_cli"]
            token = os.environ.pop("QODER_PERSONAL_ACCESS_TOKEN")
            try:
                version = install(pin, root / "qoder")
            finally:
                os.environ["QODER_PERSONAL_ACCESS_TOKEN"] = token
            (output / "version.json").write_text(json.dumps(version))
            binary = root / "qoder/qodercli"
        command = ["docker", "run", "--name", name, "--init", "--cpus", "2", "--memory", "2g",
                   "--user", f"{os.getuid()}:{os.getgid()}", "-it", "--env", "QODER_PERSONAL_ACCESS_TOKEN",
                   "--env", "HOME=/probe/home", "--env", "TERM=xterm-256color",
                   "--mount", f"type=bind,source={root},target=/probe",
                   "--mount", f"type=bind,source={binary.resolve()},target=/qodercli,readonly",
                   "--mount", f"type=bind,source={args.bin.resolve() / 'aw'},target=/aw,readonly",
                   "--workdir", "/probe/work", args.image, "/qodercli", "--dangerously-skip-permissions",
                   "-m", "Performance", "--max-output-tokens", "256", "--max-model-request-retries", "0",
                   "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}']
        record = {"command": command, "cwd": str(root), "ports": [], "log": None,
                  "output_policy": "discard native terminal bytes", "deadline_seconds": 300,
                  "container_stop": f"docker rm -f {name}"}
        (output / "process.json").write_text(json.dumps(record, indent=2))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        screen = bytearray()
        def drain(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], min(.1, max(0, deadline-time.monotonic())))[0]:
                    try:
                        screen.extend(os.read(master, 65536))
                        del screen[:-65536]
                    except OSError:
                        break
                if child.poll() is not None:
                    break
        def docker(*argv):
            result = subprocess.run(["docker", *map(str, argv)], env=controls, capture_output=True,
                                    text=True, timeout=40)
            if result.returncode:
                raise RuntimeError(f"Docker control failed ({result.returncode}): {result.stderr.strip()}")
            return result.stdout
        def control(*argv):
            return json.loads(docker("exec", "--user", f"{os.getuid()}:{os.getgid()}", name, "/aw", *argv))
        def status():
            return control("status", "--config", "/probe/aw.json")["result"]
        try:
            child = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
            os.close(slave)
            record.update(pid=child.pid, pgid=child.pid, stop=f"kill -TERM -- -{child.pid}")
            (output / "process.json").write_text(json.dumps(record, indent=2))
            drain(8)
            for _ in range(4):
                rendered = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(screen)).decode(errors="replace")
                if re.search(r"(Do you trust|Trust (this|the) (folder|directory)|trust the files)", rendered, re.I):
                    os.write(master, b"\r")
                    screen.clear()
                    drain(3)
                else:
                    break
            assert child.poll() is None, "Qoder TUI exited"
            assert "Please sign in" not in rendered, "Qoder TUI authentication failed"
            host_pid = next(int(line.split()[0]) for line in docker("top", name, "-eo", "pid,comm").splitlines()[1:] if "qoder" in line)
            native_pid = int(next(line for line in Path(f"/proc/{host_pid}/status").read_text().splitlines() if line.startswith("NSpid:")).split()[-1])
            identity = Path(f"/proc/{host_pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
            # A second native TUI shares the container but owns a different HOME/project.
            other_master, other_slave = pty.openpty()
            fcntl.ioctl(other_slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
            other_command = ["docker", "exec", "-it", "--env", "HOME=/probe/other-home",
                             "--workdir", "/probe/other-work", name, "/qodercli", "--dangerously-skip-permissions",
                             "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}']
            other_child = subprocess.Popen(other_command, env=controls, stdin=other_slave,
                                           stdout=other_slave, stderr=other_slave, start_new_session=True)
            os.close(other_slave)
            (output / "other-process.json").write_text(json.dumps({"command": other_command,
                "cwd": str(root), "pid": other_child.pid, "pgid": other_child.pid,
                "ports": [], "log": None, "deadline_seconds": 300,
                "stop": f"docker rm -f {name}", "output_policy": "discard native terminal bytes"}, indent=2))
            other_screen = bytearray()
            def drain_other(seconds):
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    if select.select([other_master], [], [], .1)[0]:
                        other_screen.extend(os.read(other_master, 65536))
                        del other_screen[:-65536]
            drain_other(8)
            for _ in range(4):
                rendered = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(other_screen)).decode(errors="replace")
                if re.search(r"(Do you trust|Trust (this|the) (folder|directory)|trust the files)", rendered, re.I):
                    os.write(other_master, b"\r")
                    other_screen.clear()
                    drain_other(3)
                else:
                    break
            assert other_child.poll() is None
            other_pid = next(int(line.split()[0]) for line in docker("top", name, "-eo", "pid,comm").splitlines()[1:]
                             if "qoder" in line and int(line.split()[0]) != host_pid)
            other_identity = Path(f"/proc/{other_pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
            other_settings = unrelated.read_bytes()
            document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "native-attach"}, "spec": {
                "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": "/probe/state"},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 1000},
                "audit": {"enabled": True, "payload": "metadata_only"},
                "agents": {"native": {"adapter": "qoder", "argv": ["/qodercli"], "qoder": {"sequential": True}}},
                "providers": {"deny": {"protocol": "native-hook/v1alpha1", "transport": {"type": "stdio", "location": "agent",
                    "argv": ["/bin/sh", "-c", "printf hook >> /probe/hook-seen; exit 2"]}, "timeout_ms": 500, "max_output_bytes": 4096, "config": {}}},
                "events": {"tool.before": {"enabled": True, "steps": [{"id": "deny", "provider": "deny", "native": {}, "on_error": "block"}]}},
            }}
            (root / "aw.json").write_text(json.dumps(document))
            attached = control("install", "--config", "/probe/aw.json", "--agent", "native", "--pid", native_pid)
            assert attached["attached"] and status()["instances"][0]["ready"]
            assert not (root / "hook-seen").exists()
            report["cases"].append({"case": "confirmed-before-input", "passed": True})
            prompt = b"Use Bash exactly once to run: printf attached > /probe/work/tool-marker. If blocked, stop immediately. Do not retry, delegate, or use other tools.\r"
            os.write(master, prompt)
            drain(2)
            rendered = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(screen)).decode(errors="replace")
            if "Do you trust the files in this folder?" in rendered:
                os.write(master, b"\r")
                drain(2)
                os.write(master, prompt)
            deadline = time.monotonic() + 80
            while time.monotonic() < deadline:
                drain(.2)
                if (root / "hook-seen").exists() and status()["instances"][0]["requests"] == 0:
                    break
            else:
                raise TimeoutError("first native tool turn did not reach the attached Hook and finish")
            assert not (root / "work/tool-marker").exists()
            report["cases"].append({"case": "first-native-tool-denied", "passed": True})
            other_native_pid = int(next(line for line in Path(f"/proc/{other_pid}/status").read_text().splitlines()
                                        if line.startswith("NSpid:")).split()[-1])
            control("install", "--config", "/probe/aw.json", "--agent", "native", "--pid", other_native_pid)
            before_update = status()["instances"]
            assert len(before_update) == 2
            other_instance = next(row for row in before_update if row["process"]["pid"] == other_native_pid)
            other_registered = unrelated.read_bytes()
            candidate = copy.deepcopy(document)
            candidate["spec"]["events"] = {}
            (root / "next.json").write_text(json.dumps(candidate))
            activated = control("update", "--config", "/probe/aw.json", "--pid", native_pid,
                                "--policy", "/probe/next.json")
            current = status()["instances"]
            assert other_instance in current
            selected = next(row for row in current if row["process"]["pid"] == native_pid)
            assert selected["instance_id"] == activated["instance_id"]
            assert selected["policy_revision"] == activated["policy_revision"] != other_instance["policy_revision"]
            assert selected["process"]["start_ticks"] == identity
            os.write(master, b"The policy now permits this operation. Use Bash exactly once to run: printf attached > /probe/work/tool-marker.\r")
            deadline = time.monotonic() + 80
            while time.monotonic() < deadline:
                drain(.2)
                if (root / "work/tool-marker").exists() and next(
                    row for row in status()["instances"] if row["process"]["pid"] == native_pid)["requests"] == 0:
                    break
            else:
                raise TimeoutError("updated native tool turn did not execute and finish")
            assert other_instance in status()["instances"] and unrelated.read_bytes() == other_registered
            control("install", "--config", "/probe/aw.json", "--agent", "native", "--pid", other_native_pid, "--remove")
            report["cases"].append({"case": "pid-update-same-target-isolation", "passed": True})
            original = json.loads(settings.read_text())
            original["userAttachTest"] = "preserve"
            temporary = settings.with_suffix(".next")
            temporary.write_text(json.dumps(original))
            temporary.chmod(0o600)
            temporary.replace(settings)
            detached = control("install", "--config", "/probe/aw.json", "--agent", "native", "--pid", native_pid, "--remove")
            assert detached["detached"] and status()["bindings"] == 0
            assert not (home / ".qoder/.aw-attachment.json").exists()
            assert not list((root / "state").glob("attach-*"))
            final = json.loads(settings.read_text())
            assert final["userAttachTest"] == "preserve" and not final.get("hooks")
            other_final = json.loads(unrelated.read_text())
            assert not other_final.pop("hooks", {})
            assert other_final == json.loads(other_settings)
            assert other_child.poll() is None
            assert Path(f"/proc/{other_pid}/stat").read_text().rsplit(")", 1)[1].split()[19] == other_identity
            report["other_profile_process_unchanged"] = True
            assert Path(f"/proc/{host_pid}/stat").read_text().rsplit(")", 1)[1].split()[19] == identity
            assert child.poll() is None
            report["cases"].append({"case": "detach-preserves-process-and-edits", "passed": True})
            control("install", "--config", "/probe/aw.json", "--agent", "native", "--pid", native_pid)
            control("install", "--config", "/probe/aw.json", "--agent", "native", "--pid", native_pid, "--remove")
            report["cases"].append({"case": "reattach", "passed": True})
            report.update(passed=True, same_process=True, instance_released=True,
                          binary_sha256=hashlib.sha256((args.bin / "aw").read_bytes()).hexdigest())
            control("stop", "--config", "/probe/aw.json")
        finally:
            docker("rm", "-f", name)
            if child is not None:
                child.wait(timeout=10)
            if other_child is not None:
                other_child.wait(timeout=10)
            if other_master is not None:
                os.close(other_master)
            if master is not None:
                os.close(master)
            assert not docker("ps", "-aq", "--filter", f"name=^{name}$").strip()
            report["runtime_removed"] = True
            (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    assert not root.exists()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--qoder", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--image", default="ubuntu:24.04@sha256:68ee22004a9109b44973313b02ff7b1de250f1de0f5ecb628d73f579aaff33ac")
    execute(parser.parse_args())
