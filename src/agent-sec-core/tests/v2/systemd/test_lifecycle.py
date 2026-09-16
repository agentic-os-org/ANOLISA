"""DPROC-013 system-manager gate; run explicitly as root on a systemd host.

Uses an isolated unit/runtime namespace with the shipped root identity. Never
starts, stops, or overwrites the installed agent-sec-core service.
"""

import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]


def command(*args, check=True, timeout=95):
    return subprocess.run(
        args, capture_output=True, text=True, check=check, timeout=timeout
    )


def wait_for(predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise AssertionError("systemd lifecycle condition timed out")


def test_dproc_013_systemd_lifecycle(request):
    if os.geteuid() != 0 or Path("/proc/1/comm").read_text().strip() != "systemd":
        reason = (
            "requires root and PID 1 systemd; system-manager acceptance not executed"
        )
        if request.config.getoption("--require-systemd"):
            pytest.fail(reason)
        pytest.skip(reason)
    source_binary = shutil.which("agent-sec-daemon")
    assert (
        source_binary is not None
    ), "agent-sec-daemon must be installed or built and on PATH"
    with tempfile.TemporaryDirectory(prefix="asc-systemd-", dir="/run") as staging:
        staging = Path(staging)
        staging.chmod(0o755)
        # /run may be noexec; execute the selected artifact at its installed/build path.
        binary = Path(source_binary).resolve(strict=True)
        name = staging.name
        unit = name + ".service"
        runtime = Path("/run") / (name + "-runtime")
        endpoint = runtime / "daemon.sock"
        state = Path("/var/lib") / name
        audit = Path("/var/log") / name
        config = staging / "skillguard.json"
        config.write_text(json.dumps({"stateDir": str(state)}))
        config.chmod(0o600)
        unit_path = Path("/run/systemd/system") / unit
        template = (ROOT / "packaging/systemd/agent-sec-core-v2.service.in").read_text()
        rendered = template.replace("{bindir}/agent-sec-daemon", str(binary))
        rendered = rendered.replace(
            "RuntimeDirectory=agent-sec-core", f"RuntimeDirectory={runtime.name}"
        )
        rendered = rendered.replace(
            "StateDirectory=agent-sec/skillguard", f"StateDirectory={name}"
        )
        rendered = rendered.replace("LogsDirectory=agent-sec", f"LogsDirectory={name}")
        rendered = rendered.replace(
            " serve\n",
            f" serve --socket {endpoint} --skillguard-config {config}\n"
            f"Environment=AGENT_SEC_DATA_DIR={audit}\n",
        )

        def ctl(*args, check=True):
            return command("systemctl", *args, unit, check=check)

        def value(prop):
            return ctl("show", "--value", f"--property={prop}").stdout.strip()

        def rpc():
            try:
                with socket.socket(socket.AF_UNIX) as stream:
                    stream.settimeout(0.5)
                    stream.connect(str(endpoint))
                    stream.sendall(
                        b'{"method":"policy.templates.list","params":{"limit":1,"offset":0}}\n'
                    )
                    data = b""
                    while not data.endswith(b"\n") and len(data) < 65536:
                        chunk = stream.recv(4096)
                        if not chunk:
                            return False
                        data += chunk
                    response = json.loads(data)
                    return (
                        isinstance(response, dict)
                        and set(response) == {"requestId", "result"}
                        and isinstance(response["requestId"], str)
                        and bool(response["requestId"])
                    )
            except (OSError, ValueError):
                return False

        try:
            unit_path.write_text(rendered)
            command("systemd-analyze", "verify", str(unit_path))
            command("systemctl", "daemon-reload")
            ctl("start")
            wait_for(rpc)
            pid = int(value("MainPID"))
            assert Path(f"/proc/{pid}/exe").resolve() == binary
            assert runtime.stat().st_uid == 0
            assert runtime.stat().st_gid == 0
            assert value("User") == "root"
            assert value("Group") == "root"
            assert runtime.stat().st_mode & 0o777 == 0o755
            assert endpoint.stat().st_mode & 0o777 == 0o666
            lock_inode = (runtime / "daemon.lock").stat().st_ino
            journal = command("journalctl", "-u", unit, "--no-pager").stdout
            assert "PAP state is process-local" in journal
            print(
                "PASS foreground PID, runtime identity/modes, RPC and journal",
                flush=True,
            )

            ctl("kill", "--signal=SIGKILL", "--kill-whom=main")
            wait_for(lambda: int(value("MainPID") or "0") not in (0, pid) and rpc())
            assert (runtime / "daemon.lock").stat().st_ino == lock_inode
            print("PASS Restart=on-failure and stale socket recovery", flush=True)

            with socket.socket(socket.AF_UNIX) as stream:
                stream.connect(str(endpoint))
                stream.sendall(b'{"method":')
                before = time.monotonic()
                ctl("stop")
                assert time.monotonic() - before < 10
            assert value("MainPID") == "0" and not endpoint.exists()
            assert value("Result") == "success"
            ctl("start")
            wait_for(rpc)
            ctl("restart")
            wait_for(rpc)
            print("PASS bounded drain, stop and explicit restart", flush=True)

            pid = int(value("MainPID"))
            # systemd sends SIGCONT after KillSignal, so SIGSTOP cannot simulate a hung stop.
            # Inject a non-terminating signal while retaining the shipped timeout and KillMode.
            unit_path.write_text(
                rendered.replace("KillSignal=SIGTERM", "KillSignal=SIGCONT")
            )
            command("systemctl", "daemon-reload")
            before = time.monotonic()
            ctl("stop", check=False)
            elapsed = time.monotonic() - before
            assert 70 <= elapsed < 90, elapsed
            assert value("MainPID") == "0" and not Path(f"/proc/{pid}").exists()
            assert value("Result") == "timeout"
            print("PASS 75s stop timeout and forced process cleanup", flush=True)

            # Real startup failure exercises the shipped burst/interval/restart settings.
            unit_path.write_text(
                rendered.replace(f"--socket {endpoint}", "--socket relative.sock")
            )
            command("systemctl", "daemon-reload")
            ctl("reset-failed")
            ctl("start", check=False)
            # Some systemd builds retain the last process Result (exit-code) at the limit.
            # Verify admission itself: the burst is exhausted and another start is refused.
            wait_for(
                lambda: value("ActiveState") == "failed"
                and int(value("NRestarts")) == 5,
                timeout=25,
            )
            assert ctl("start", check=False).returncode != 0
            assert value("MainPID") == "0" and int(value("NRestarts")) == 5
            journal = command("journalctl", "-u", unit, "--no-pager").stdout
            assert "Start request repeated too quickly" in journal
            print("PASS repeated startup failure reaches start-limit-hit", flush=True)
        finally:
            ctl("stop", check=False)
            ctl("reset-failed", check=False)
            unit_path.unlink(missing_ok=True)
            command("systemctl", "daemon-reload")
            for directory in (runtime, state, audit):
                if directory.exists():
                    shutil.rmtree(directory)
