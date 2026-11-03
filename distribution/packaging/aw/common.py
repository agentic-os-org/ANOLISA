"""Bound AW package build processes and interrupt owned file operations."""

from __future__ import annotations

import hashlib
import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path


class Cancelled(RuntimeError):
    """The caller interrupted a package operation before publication."""


class Cancellation:
    """Signal handlers request cleanup on the foreground Python thread."""

    def __init__(self) -> None:
        self.cancelled = False

    def request(self, *_: object) -> None:
        self.cancelled = True

    def check(self) -> None:
        if self.cancelled:
            raise Cancelled("AW package build interrupted")

    def register(self) -> None:
        for number in (signal.SIGINT, signal.SIGTERM):
            signal.signal(number, self.request)


def digest(path: Path, cancel: Cancellation) -> str:
    """Hash incrementally so large binaries remain interruptible."""
    result = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            cancel.check()
            result.update(chunk)
    cancel.check()
    return result.hexdigest()


def copy_file(source: Path, destination: Path, cancel: Cancellation, mode: int) -> None:
    """Copy only into a caller-owned staging tree."""
    cancel.check()
    destination.parent.mkdir(parents=True, exist_ok=True)
    with source.open("rb") as incoming, destination.open("xb") as outgoing:
        while chunk := incoming.read(1024 * 1024):
            cancel.check()
            outgoing.write(chunk)
    destination.chmod(mode)
    cancel.check()


def _signal_group(process: subprocess.Popen[bytes], number: int) -> None:
    try:
        os.killpg(process.pid, number)
    except ProcessLookupError:
        pass


def run(command: list[str], cwd: Path, cancel: Cancellation, seconds: int = 1200) -> bytes:
    """Capture bounded output and terminate the command's entire process group."""
    cancel.check()
    print("+ " + " ".join(command), file=sys.stderr, flush=True)
    deadline = time.monotonic() + seconds
    limit = 64 * 1024 * 1024
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=stdout,
            stderr=stderr,
            start_new_session=True,
        )
        try:
            while process.poll() is None:
                cancel.check()
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"command exceeded {seconds}s: {command[0]}")
                if (
                    os.fstat(stdout.fileno()).st_size > limit
                    or os.fstat(stderr.fileno()).st_size > limit
                ):
                    raise RuntimeError(f"command output exceeds 64 MiB: {command[0]}")
                time.sleep(0.05)
            cancel.check()
            if (
                os.fstat(stdout.fileno()).st_size > limit
                or os.fstat(stderr.fileno()).st_size > limit
            ):
                raise RuntimeError(f"command output exceeds 64 MiB: {command[0]}")
            stdout.seek(0)
            stderr.seek(0)
            error_output = stderr.read()
            if error_output:
                sys.stderr.write(error_output.decode("utf-8", errors="replace"))
                sys.stderr.flush()
            if process.returncode:
                raise RuntimeError(f"{command[0]} exited with {process.returncode}")
            return stdout.read()
        finally:
            # The parent may exit while descendants still own inherited resources.
            _signal_group(process, signal.SIGTERM)
            try:
                process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                pass
            _signal_group(process, signal.SIGKILL)
            process.wait()
