"""Isolate a Linux subreaper to verify ENOSPC cleanup without orphaning fixtures."""

import ctypes
import errno
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from native_acceptance import Commands


def probe(root: Path, stage: str) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "cannot adopt owned fixture descendants")
    ready = root / "descendant.pid"
    descendant = (
        "import os,signal,time; from pathlib import Path; "
        "signal.signal(signal.SIGINT,signal.SIG_IGN); "
        "signal.signal(signal.SIGTERM,signal.SIG_IGN); "
        f"Path({str(ready)!r}).write_text(str(os.getpid())); time.sleep(30)"
    )
    parent = (
        "import signal,subprocess,sys,time; "
        "signal.signal(signal.SIGINT,signal.SIG_IGN); "
        f"subprocess.Popen([sys.executable,'-c',{descendant!r}]); time.sleep(3)"
    )
    owned = []
    failed_at = []
    original_open, original_spawn = Path.open, subprocess.Popen

    def spawn(*args: object, **kwargs: object) -> subprocess.Popen:
        child = original_spawn(*args, **kwargs)
        owned.append(child)
        (root / "fixture-process.json").write_text(
            json.dumps(
                {
                    "command": args[0],
                    "cwd": str(root),
                    "pid": child.pid,
                    "pgid": child.pid,
                    "ports": [],
                    "timeout_seconds": 15,
                    "stop": f"kill -KILL -- -{child.pid}",
                }
            )
        )
        return child

    def fail() -> None:
        deadline = time.monotonic() + 5
        while not ready.exists():
            if time.monotonic() >= deadline:
                raise AssertionError("descendant did not start before ledger failure")
            time.sleep(0.01)
        failed_at.append(time.monotonic())
        raise OSError(errno.ENOSPC, "injected full ledger device")

    class Ledger(io.StringIO):
        def write(self, text: str) -> int:
            if stage == "write":
                fail()
            return super().write(text)

        def __exit__(self, *args: object) -> None:
            super().__exit__(*args)
            if stage == "flush":
                fail()

    def open_file(path: Path, *args: object, **kwargs: object) -> object:
        if path == root / "processes.jsonl":
            if stage == "open":
                fail()
            return Ledger()
        return original_open(path, *args, **kwargs)

    try:
        with patch.object(Path, "open", open_file), patch.object(subprocess, "Popen", spawn):
            try:
                Commands(root, dict(os.environ)).run([sys.executable, "-c", parent], root, 0.1)
            except OSError as error:
                assert error.errno == errno.ENOSPC, error
            else:
                raise AssertionError("ledger failure was swallowed")
        elapsed = time.monotonic() - failed_at[0]
        assert elapsed < 1.5, f"ledger failure entered an unbounded wait: {elapsed:.3f}s"
        child_pid = int(ready.read_text())
        deadline = time.monotonic() + 2
        while os.waitpid(child_pid, os.WNOHANG)[0] == 0:
            assert time.monotonic() < deadline, "owned descendant survived ledger failure"
            time.sleep(0.01)
        assert not Path(f"/proc/{child_pid}").exists()
        assert owned[0].returncode is not None
        print(json.dumps({"stage": stage, "elapsed": elapsed, "descendant_reaped": True}))
    finally:
        # Also reap on a regression, so testing the old behavior leaves no descendants.
        for child in owned:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=5)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    pid, _ = os.waitpid(-child.pid, os.WNOHANG)
                except ChildProcessError:
                    break
                if pid == 0:
                    time.sleep(0.01)
            else:
                raise AssertionError("owned descendants were not reaped during teardown")


if __name__ == "__main__":
    probe(Path(sys.argv[1]), sys.argv[2])
