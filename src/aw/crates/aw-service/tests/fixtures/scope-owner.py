"""Own and reap a launcher/worker family while testing either owner's death."""

import ctypes
import json
import os
from pathlib import Path
import signal
import sys
import time

directory = Path(sys.argv[1])
assert ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0  # PR_SET_CHILD_SUBREAPER
reader, writer = os.pipe()
launcher = os.fork()
if launcher == 0:
    os.close(reader)
    worker = os.fork()
    if worker == 0:
        os.close(writer)
        time.sleep(30)
        os._exit(0)
    os.write(writer, str(worker).encode())
    os.close(writer)
    time.sleep(30)
    os._exit(0)
os.close(writer)
worker = int(os.read(reader, 32))
os.close(reader)
reaped = set()


def stop(pid: int) -> None:
    if pid not in reaped:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
        reaped.add(pid)


try:
    ready = directory / "owners.tmp"
    ready.write_text(json.dumps({"launcher": launcher, "worker": worker}))
    ready.replace(directory / "owners.json")
    deadline = time.monotonic() + 30
    while not (directory / "owners-close").exists():
        if time.monotonic() >= deadline:
            raise TimeoutError("owner fixture deadline")
        request = directory / "owner-stop"
        if request.exists() and not (directory / "owner-stopped").exists():
            target = launcher if request.read_text() == "launcher" else worker
            os.kill(target, signal.SIGKILL)
            # A worker killed first remains its launcher's zombie until teardown.
            if target == launcher:
                os.waitpid(launcher, 0)
                reaped.add(launcher)
            (directory / "owner-stopped").write_text("stopped")
        time.sleep(0.005)
finally:
    stop(launcher)
    stop(worker)
