"""Drive one bounded removal confirmation through actual terminal descriptors."""

import errno
import json
import os
import pty
import select
import signal
import subprocess
import sys
import time


def capture() -> dict:
    mode = sys.argv[1]
    reader, writer = pty.openpty()
    terminal_input = mode != "stderr-tty"
    terminal_error = mode != "stdin-tty"
    process = subprocess.Popen(
        sys.argv[2:],
        stdin=writer if terminal_input else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=writer if terminal_error else subprocess.PIPE,
    )
    os.close(writer)
    transcript = bytearray()
    acted = False
    # Accepted removal verifies the real CLI payload before durable deletion.
    deadline = time.monotonic() + 20
    try:
        while True:
            if time.monotonic() >= deadline:
                raise TimeoutError("removal confirmation did not finish")
            readable, _, _ = select.select([reader], [], [], 0.02)
            if readable:
                try:
                    chunk = os.read(reader, 4096)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    chunk = b""
                transcript.extend(chunk)
                if len(transcript) > 65536:
                    raise ValueError("confirmation transcript exceeds limit")
            if not acted and b"[y/N]" in transcript:
                acted = True
                if mode == "sigint":
                    os.kill(process.pid, signal.SIGINT)
                elif mode == "sigterm":
                    os.kill(process.pid, signal.SIGTERM)
                else:
                    os.write(reader, {
                        "accept": b"yes\n",
                        "decline": b"no\n",
                        "default": b"\n",
                        "eof": b"\x04",
                    }.get(mode, b"no\n"))
            if process.poll() is not None:
                # Drain the terminal after wait; small output cannot fill its buffer.
                while select.select([reader], [], [], 0)[0]:
                    try:
                        chunk = os.read(reader, 4096)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        break
                    if not chunk:
                        break
                    transcript.extend(chunk)
                stdout, stderr = process.communicate(timeout=1)
                return {
                    "code": process.returncode,
                    "stdout": list(stdout),
                    "stderr": list(stderr or b""),
                    "terminal": list(transcript),
                }
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        os.close(reader)


if __name__ == "__main__":
    print(json.dumps(capture()))
