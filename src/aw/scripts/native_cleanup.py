"""Reap owned native descendants across sessions and remove labelled containers."""

import ctypes
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def own_children() -> None:
    """Require an exclusive runner so adopted children cannot belong to another task."""
    try:
        os.waitid(os.P_ALL, 0, os.WEXITED | os.WNOHANG | os.WNOWAIT)
    except ChildProcessError:
        pass
    else:
        raise RuntimeError("native command runner already has an owned child")
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "cannot adopt native descendants")


def reap_children() -> list[int]:
    """Kill and reap descendants adopted after their parent exits, including setsid()."""
    reaped = []
    deadline = time.monotonic() + 5
    parent = os.getpid()
    while time.monotonic() < deadline:
        # Only this synchronous runner reaps its children, reserving each PID
        # until after signalling. Grandchildren are adopted on the next pass.
        for task in Path("/proc").iterdir():
            if time.monotonic() >= deadline:
                raise TimeoutError("native process scan exceeded cleanup deadline")
            if not task.name.isdecimal():
                continue
            try:
                fields = (task / "stat").read_bytes().rsplit(b")", 1)[1].split()
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
            # PPid is always available; task/<tid>/children needs an optional
            # kernel configuration. comm may contain spaces or parentheses.
            if int(fields[1]) == parent:
                os.kill(int(task.name), signal.SIGKILL)
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return reaped
        if pid:
            reaped.append(pid)
        else:
            time.sleep(0.01)
    raise TimeoutError("native descendants did not exit within cleanup deadline")


def wait_child(child: subprocess.Popen, seconds: float) -> int:
    """Observe exit without reaping the leader before its final group signal."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        if result is not None:
            return result.si_status if result.si_code == os.CLD_EXITED else -result.si_status
        time.sleep(0.01)
    raise subprocess.TimeoutExpired(child.args, seconds)


def remove_containers(owner: str, environment: dict[str, str]) -> list[str]:
    """Remove only containers labelled by this command, even if its ledger was lost."""
    environment = dict(environment)
    environment.pop("QODER_PERSONAL_ACCESS_TOKEN", None)

    def docker(*args: str) -> str:
        return subprocess.check_output(
            ["docker", *args], env=environment, text=True, stderr=subprocess.PIPE, timeout=15
        ).strip()

    containers = docker("ps", "-aq", "--filter", f"label=aw.native-owner={owner}").splitlines()
    errors = []
    for container in containers:
        try:
            state = json.loads(docker("inspect", "-f", "{{json .State}}", container))
            if state["Paused"]:
                docker("unpause", container)
            if state["Running"]:
                # Forced removal skips the daemon's exit trap. Restore the only
                # writable host mount before removal so private profiles can go.
                docker(
                    "exec",
                    container,
                    "chown",
                    "-R",
                    f"{os.getuid()}:{os.getgid()}",
                    "/run/agent-sec-core",
                )
        except Exception as error:
            errors.append(error)
        finally:
            try:
                docker("rm", "-f", container)
            except Exception as error:
                errors.append(error)
    if errors:
        raise RuntimeError(f"native container cleanup failed: {errors}")
    if docker("ps", "-aq", "--filter", f"label=aw.native-owner={owner}"):
        raise RuntimeError("owned native containers remain after removal")
    return containers
