"""Owned native processes and a disposable, real sec-core daemon container."""

import json
import os
from pathlib import Path
import signal
import subprocess
import time
import uuid


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


class Processes:
    def __init__(self, case, evidence, env):
        self.case, self.evidence, self.env, self.items = case, evidence, env, []

    def record(self):
        save(self.evidence / "processes.json", [item[2] for item in self.items])

    def start(self, name, command, ports=None, extra_env=None):
        log = self.evidence / (name + ".log")
        output = log.open("wb")
        record = {
            "name": name,
            "command": list(map(str, command)),
            "cwd": str(self.case / "work"),
            "ports": ports or [],
            "log": str(log),
            "deadline_seconds": 180,
            "pid": None,
        }
        self.items.append([None, output, record])
        self.record()
        child = subprocess.Popen(
            command,
            cwd=self.case / "work",
            env={**self.env, **(extra_env or {})},
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self.items[-1][0] = child
        record.update(pid=child.pid, pgid=child.pid, stop=f"kill -TERM -- -{child.pid}")
        self.record()
        print(
            json.dumps({"started": name, "pid": child.pid, "ports": record["ports"]}),
            flush=True,
        )
        return child

    @staticmethod
    def exited(child):
        return os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)

    def wait(self, child, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            result = self.exited(child)
            if result is not None:
                return result.si_status if result.si_code == os.CLD_EXITED else -result.si_status
            time.sleep(0.1)
        raise TimeoutError("native process exceeded deadline")

    def stop(self, child):
        for proc, output, record in self.items:
            if proc is not child or record.get("stopped"):
                continue
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            deadline = time.monotonic() + 5
            while self.exited(proc) is None and time.monotonic() < deadline:
                time.sleep(0.1)
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.wait(timeout=5)
            output.close()
            record.update(
                stopped=True,
                exit_code=proc.returncode,
                pid_absent=not Path(f"/proc/{proc.pid}").exists(),
            )
            self.record()

    def close(self):
        errors = []
        for proc, output, _ in reversed(self.items):
            try:
                if proc:
                    self.stop(proc)
                else:
                    output.close()
            except Exception as error:
                errors.append(error)
        if errors:
            raise errors[0]


def await_condition(predicate, seconds, label):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise TimeoutError(label)


def without_qoder_token(environment):
    return {
        key: value for key, value in environment.items() if key != "QODER_PERSONAL_ACCESS_TOKEN"
    }


def checked(command, **kwargs):
    kwargs["env"] = without_qoder_token(kwargs.pop("env", os.environ))
    return subprocess.run(
        list(map(str, command)),
        capture_output=True,
        text=True,
        timeout=kwargs.pop("timeout", 15),
        check=True,
        **kwargs,
    )


class Backend:
    def __init__(self, case, evidence, processes, binaries, image):
        self.evidence, self.processes, self.binaries, self.image = (
            evidence,
            processes,
            binaries,
            image,
        )
        self.runtime = case / "sec-runtime"
        self.runtime.mkdir(parents=True)
        # Preserve host readiness probes throughout the container's root chown.
        # The enclosing case directory remains private to the native runner.
        self.runtime.chmod(0o755)
        self.socket = self.runtime / "daemon.sock"
        self.name = "aw-sec-" + uuid.uuid4().hex[:16]
        self.created = False
        self.paused = False
        self.record = {
            "container": self.name,
            "image": image,
            "socket": str(self.socket),
            "ports": [],
            "stop": f"docker stop --timeout 10 {self.name}",
            "remove": f"docker rm -f {self.name}",
            "deadline_seconds": 180,
        }

    def start(self):
        # Only the socket directory is writable on the host; all backend data
        # and root-owned configuration remain inside this disposable container.
        script = """set -eu
umask 077
chown 0:0 /run/agent-sec-core
mkdir -p /etc/agent-sec /var/log/agent-sec
printf '%s\\n' '{"stateDir":"/var/lib/agent-sec/skillsec","enableDefaultSkillDirs":false}' > /etc/agent-sec/skillsec.json
cleanup() { chown -R UID:GID /run/agent-sec-core; }
stop() { kill -TERM "$backend_pid" 2>/dev/null || true; wait "$backend_pid" || true; cleanup; exit 0; }
trap stop TERM INT
/app/agent-sec-daemon --socket /run/agent-sec-core/daemon.sock --skillsec-config /etc/agent-sec/skillsec.json &
backend_pid=$!
set +e
wait "$backend_pid"
result=$?
cleanup
exit "$result"
""".replace("UID:GID", f"{os.getuid()}:{os.getgid()}")
        command = [
            "docker",
            "create",
            "--name",
            self.name,
            "--label",
            "aw.native-owner=" + (os.environ.get("AW_NATIVE_OWNER") or self.name),
            "--network",
            "none",
            "--security-opt",
            "no-new-privileges",
            "--mount",
            f'type=bind,source={self.binaries / "agent-sec-daemon"},target=/app/agent-sec-daemon,readonly',
            "--mount",
            f"type=bind,source={self.runtime},target=/run/agent-sec-core",
            self.image,
            "/bin/sh",
            "-c",
            script,
        ]
        self.record["command"] = command
        save(self.evidence / "backend.json", self.record)
        self.created = True
        self.record["id"] = checked(command).stdout.strip()
        save(self.evidence / "backend.json", self.record)
        child = self.processes.start("sec-core", ["docker", "start", "--attach", self.name])
        await_condition(
            lambda: self.socket.exists() or self.processes.exited(child),
            15,
            "sec-core startup",
        )
        if self.processes.exited(child):
            raise RuntimeError("real sec-core daemon exited before readiness")
        self.record["resolved_image"] = checked(
            ["docker", "inspect", "-f", "{{.Image}}", self.name]
        ).stdout.strip()
        self.record["host_pid"] = int(
            checked(["docker", "inspect", "-f", "{{.State.Pid}}", self.name]).stdout
        )
        save(self.evidence / "backend.json", self.record)

    def scan(self, code, timeout_ms=2000):
        command = [
            self.binaries / "agent-sec-cli",
            "--socket",
            self.socket,
            "--timeout-ms",
            str(timeout_ms),
            "scan-code",
            "--code",
            code,
            "--language",
            "bash",
            "--mode",
            "regex",
        ]
        result = subprocess.run(
            list(map(str, command)),
            capture_output=True,
            text=True,
            timeout=5,
            env=without_qoder_token(os.environ),
        )
        return {
            "exit_code": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "command": list(map(str, command)),
        }

    def inject(self, mode):
        if mode == "backend-error":
            checked(["docker", "stop", "--timeout", "10", self.name], timeout=15)
            state = json.loads(
                checked(["docker", "inspect", "-f", "{{json .State}}", self.name]).stdout
            )
            assert state["Running"] is False
            self.record["injected_fault"] = {"action": "stop", "state": state}
        elif mode == "timeout-block":
            checked(["docker", "pause", self.name])
            self.paused = True
            state = json.loads(
                checked(["docker", "inspect", "-f", "{{json .State}}", self.name]).stdout
            )
            assert state["Paused"] is True
            self.record["injected_fault"] = {"action": "pause", "state": state}
        save(self.evidence / "backend.json", self.record)

        if mode in ("backend-error", "timeout-block"):
            probe = self.scan("printf 'fault probe'", timeout_ms=1200)
            save(self.evidence / "fault-probe.json", probe)
            assert probe["exit_code"] != 0, probe

    def recover(self):
        if self.paused:
            checked(["docker", "unpause", self.name])
            self.paused = False

    def events(self):
        result = checked(
            [
                self.binaries / "agent-sec-cli",
                "--socket",
                self.socket,
                "--timeout-ms",
                "2000",
                "events",
                "--event-type",
                "code_scan",
                "--include-details",
                "-o",
                "json",
            ]
        )
        (self.evidence / "sec-events.json").write_text(result.stdout)

    def close(self):
        if not self.created:
            return
        try:
            self.recover()
            checked(["docker", "stop", "--timeout", "10", self.name], timeout=15)
            checked(
                [
                    "docker",
                    "cp",
                    f"{self.name}:/var/log/agent-sec",
                    self.evidence / "sec-data",
                ]
            )
            self.record["final_state"] = json.loads(
                checked(["docker", "inspect", "-f", "{{json .State}}", self.name]).stdout
            )
        finally:
            # Evidence collection failure must not leave a backend running.
            checked(["docker", "rm", "-f", self.name])
            remaining = checked(
                ["docker", "ps", "-aq", "--filter", f"name=^/{self.name}$"]
            ).stdout.strip()
            self.record["removed"] = not remaining
            self.record["socket_removed"] = not self.socket.exists()
            self.record["directory_owner_restored"] = self.runtime.stat().st_uid == os.getuid()
            save(self.evidence / "backend.json", self.record)
