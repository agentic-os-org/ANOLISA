"""Verify forced timeout and ENOSPC cleanup across independent native sessions."""

import ctypes
import errno
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from native_acceptance import Commands
from native_cleanup import reap_children


def probe(root: Path, case: Path, stage: str, binaries: str = "", image: str = "") -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "cannot adopt owned fixture descendants")
    ready = root / "descendant.pid"
    (root / "runtime.json").write_text(json.dumps({"directory": str(case)}))
    grandchild = root / "grandchild.pid"
    descendant = (
        "import os,signal,subprocess,sys,time; from pathlib import Path; "
        "signal.signal(signal.SIGINT,signal.SIG_IGN); "
        "signal.signal(signal.SIGTERM,signal.SIG_IGN); "
        "child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'],start_new_session=True); "
        f"Path({str(grandchild)!r}).write_text(str(child.pid)); "
        f"Path({str(ready)!r}).write_text(str(os.getpid())); time.sleep(30)"
    )
    backend = (
        (
            "from runtime import Backend; "
            f"backend=Backend(case,root,processes,Path({binaries!r}),{image!r}); "
            "backend.start(); backend.inject('timeout-block'); "
        )
        if binaries
        else ""
    )
    parent = (
        "import os,signal,subprocess,sys,time; from pathlib import Path; "
        "signal.signal(signal.SIGINT,signal.SIG_IGN); "
        f"sys.path.insert(0,{str(Path(__file__).resolve().parents[2] / 'crates/aw-cli/tests/fixtures/sec_core')!r}); "
        "from runtime import Processes; "
        f"root=Path({str(root)!r}); case=Path({str(case)!r}); (case/'work').mkdir(); "
        "processes=Processes(case,root,dict(os.environ)); "
        + backend
        + f"processes.start('escaped-child',[sys.executable,'-c',{descendant!r}]); time.sleep(30)"
    )
    owned = []
    failed_at = []
    inject_failure = stage != "no-children"
    original_open, original_spawn = Path.open, subprocess.Popen

    def spawn(*args: object, **kwargs: object) -> subprocess.Popen:
        child = original_spawn(*args, **kwargs)
        if not kwargs.get("start_new_session"):
            return child
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

    def ready_child() -> None:
        deadline = time.monotonic() + 20
        while not ready.exists():
            if time.monotonic() >= deadline:
                raise AssertionError("descendant did not start before ledger failure")
            time.sleep(0.01)
        failed_at.append(time.monotonic())

    def fail() -> None:
        ready_child()
        raise OSError(errno.ENOSPC, "injected full ledger device")

    class Ledger(io.StringIO):
        def write(self, text: str) -> int:
            if stage in ("timeout", "no-children"):
                ready_child()
            if stage == "interrupt":
                ready_child()
                raise KeyboardInterrupt("injected cancellation")
            if stage == "write":
                fail()
            return super().write(text)

        def __exit__(self, *args: object) -> None:
            super().__exit__(*args)
            if stage == "flush":
                fail()

    def open_file(path: Path, *args: object, **kwargs: object) -> object:
        if stage == "no-children" and path.name == "children" and str(path).startswith("/proc/"):
            raise FileNotFoundError(errno.ENOENT, "optional proc children file disabled")
        if path == root / "processes.jsonl" and inject_failure:
            if stage == "open":
                fail()
            return Ledger()
        return original_open(path, *args, **kwargs)

    try:
        with patch.object(Path, "open", open_file), patch.object(subprocess, "Popen", spawn):
            commands = Commands(root, dict(os.environ))
            if stage == "no-children":
                commands.run([sys.executable, "-c", "pass"], root, 1)
                inject_failure = True
            try:
                commands.run([sys.executable, "-c", parent], root, 0.1, containers=bool(binaries))
            except (OSError, subprocess.TimeoutExpired, KeyboardInterrupt) as error:
                if stage in ("timeout", "no-children"):
                    assert isinstance(error, subprocess.TimeoutExpired), error
                elif stage == "interrupt":
                    assert isinstance(error, KeyboardInterrupt), error
                else:
                    assert error.errno == errno.ENOSPC, error
            else:
                raise AssertionError("ledger failure was swallowed")
        elapsed = time.monotonic() - failed_at[0]
        assert elapsed < (90 if binaries else 1.5), f"cleanup exceeded its bound: {elapsed:.3f}s"
        child_pid = int(ready.read_text())
        assert not Path(f"/proc/{child_pid}").exists()
        assert not Path(f"/proc/{int(grandchild.read_text())}").exists()
        assert all(child.returncode is not None for child in owned)
        print(json.dumps({"stage": stage, "elapsed": elapsed, "descendant_reaped": True}))
    finally:
        # Also reap on a regression, so testing the old behavior leaves no descendants.
        for child in owned:
            if child.returncode is None:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait(timeout=5)
            reap_children()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="awcleanup-") as temporary:
        probe(Path(sys.argv[1]), Path(temporary), *sys.argv[2:])
