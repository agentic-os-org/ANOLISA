"""Deterministic Linux child behaviors for the executor's runtime tests."""

import os
from pathlib import Path
import signal
import sys
import time


def record_process(directory: Path, name: str) -> None:
    fields = Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()
    temporary = directory / f"{name}.tmp"
    temporary.write_text(f"{os.getpid()} {fields[19]}")
    temporary.replace(directory / f"{name}.pid")


def write_all(descriptor: int, data: bytes) -> None:
    remaining = memoryview(data)
    while remaining:
        remaining = remaining[os.write(descriptor, remaining) :]


def wait_for_release(directory: Path) -> None:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if (directory / "release").exists():
            write_all(1, b"released")
            return
        time.sleep(0.005)
    raise TimeoutError("fixture did not receive release within 10 seconds")


def main() -> None:
    signal.alarm(10)
    directory = Path(sys.argv[1])
    scenario = sys.argv[2]
    arguments = sys.argv[3:]
    record_process(directory, "leader")

    if scenario == "context":
        records = [os.getcwdb(), *(os.fsencode(arg) for arg in arguments), b""]
        records.extend(key + b"=" + value for key, value in sorted(os.environb.items()))
        write_all(1, b"\0".join(records))
    elif scenario == "binary":
        data = sys.stdin.buffer.read()
        write_all(1, data + b"\0\xfftail")
        write_all(2, data[::-1] + b"\xfe")
    elif scenario == "duplex":
        write_all(1, b"O" * 262144)
        write_all(2, b"E" * 262144)
        write_all(1, sys.stdin.buffer.read())
    elif scenario == "emit":
        write_all(1, b"O" * int(arguments[0]))
        write_all(2, b"E" * int(arguments[1]))
    elif scenario == "close-stdin":
        os.close(0)
        write_all(1, b"stdin closed")
    elif scenario == "exit":
        sys.exit(int(arguments[0]))
    elif scenario == "signal":
        os.kill(os.getpid(), signal.SIGTERM)
    elif scenario == "wait":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        wait_for_release(directory)
    elif scenario == "flood":
        descriptor = int(arguments[0])
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            write_all(descriptor, b"F" * 4096)
            # Keep memory use stable even when the test runner is oversubscribed.
            time.sleep(0.0005)
        raise TimeoutError("flood fixture exceeded its 10-second lifetime")
    elif scenario == "descendant":
        reader, writer = os.pipe()
        if os.fork() == 0:
            os.close(reader)
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            record_process(directory, "descendant")
            if arguments[0] == "closed":
                for descriptor in (0, 1, 2):
                    os.close(descriptor)
            os.write(writer, b"R")
            os.close(writer)
            time.sleep(10)
            os._exit(0)
        os.close(writer)
        assert os.read(reader, 1) == b"R", "descendant failed to initialize"
        os.close(reader)
        if arguments[1] == "wait":
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            time.sleep(10)
    else:
        raise ValueError(f"unknown fixture scenario: {scenario}")


if __name__ == "__main__":
    main()
