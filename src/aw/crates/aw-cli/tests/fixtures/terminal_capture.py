"""Bound a real CLI child with a PTY or a closed output stream."""

import errno
import json
import os
import pty
import subprocess
import sys


def capture() -> dict:
    mode = sys.argv[1]
    if mode == "pty":
        reader, writer = pty.openpty()
    elif mode in ("closed", "closed-stderr"):
        reader, writer = os.pipe()
        os.close(reader)
        reader = None
    else:
        raise ValueError("unknown capture mode")
    try:
        process = subprocess.Popen(
            sys.argv[2:],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE if mode == "closed-stderr" else writer,
            stderr=writer if mode == "closed-stderr" else subprocess.PIPE,
        )
    finally:
        os.close(writer)
    try:
        # Installation verifies the real CLI payload and fsyncs its transaction.
        pipe_stdout, pipe_stderr = process.communicate(timeout=20)
        stdout = bytearray(pipe_stdout or b"")
        stderr = pipe_stderr or b""
        if reader is not None:
            while True:
                try:
                    chunk = os.read(reader, 4096)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                stdout.extend(chunk)
                if len(stdout) > 65536:
                    raise ValueError("fixture output exceeds limit")
        if len(stderr) > 65536:
            raise ValueError("fixture diagnostics exceed limit")
        return {"code": process.returncode, "stdout": list(stdout), "stderr": list(stderr)}
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        if reader is not None:
            os.close(reader)


if __name__ == "__main__":
    print(json.dumps(capture()))
