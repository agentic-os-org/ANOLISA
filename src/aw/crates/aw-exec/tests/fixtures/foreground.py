"""Bounded children for inherited-stream and foreground-terminal runtime tests."""

import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import tty


def record(directory: Path, name: str) -> None:
    start = Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19]
    temporary = directory / f"{name}.tmp"
    temporary.write_text(f"{os.getpid()} {start}")
    temporary.replace(directory / f"{name}.pid")


def main() -> None:
    signal.alarm(8)
    directory, scenario = Path(sys.argv[1]), sys.argv[2]
    assert Path.cwd() == directory
    assert os.environ["FOREGROUND_CONTEXT"] == "explicit"
    assert "AW_FOREGROUND_FIXTURE" not in os.environ
    if scenario == "worker":
        child = subprocess.Popen(
            [sys.executable, "-c", "import signal, sys; signal.alarm(7); "
             "print('ready', flush=True); assert sys.stdin.readline() == 'close\\n'; "
             "print('closed', flush=True)"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
        )
        assert child.stdout.readline() == "ready\n"
        start = Path(f"/proc/{child.pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
        (directory / "sdk.pid").write_text(f"{child.pid} {start}")

        def drain(*_: object) -> None:
            # A worker needs the SDK transport alive until it completes shutdown.
            reply, _ = child.communicate("close\n", timeout=1)
            if child.returncode != 0 or reply != "closed\n":
                sys.exit(42)
            (directory / "cleanup-complete").touch()
            sys.exit(37)

        signal.signal(signal.SIGTERM, drain)
        signal.signal(signal.SIGINT, drain)
    elif scenario == "stubborn":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    elif scenario == "graceful":

        def finish(*_: object) -> None:
            time.sleep(3)
            (directory / "cleanup-complete").touch()
            sys.exit(37)

        signal.signal(signal.SIGTERM, finish)
    elif scenario == "grace-stubborn":
        received = 0

        def ignore(*_: object) -> None:
            nonlocal received
            received += 1
            temporary = directory / "term-received.tmp"
            temporary.write_text(str(received))
            temporary.replace(directory / "term-received")

        signal.signal(signal.SIGTERM, ignore)
    record(directory, "leader")
    if scenario in ("descendant", "stubborn", "grace-stubborn"):
        reader, writer = os.pipe()
        if os.fork() == 0:
            os.close(reader)
            signal.alarm(8)
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            record(directory, "descendant")
            os.write(writer, b"R")
            os.close(writer)
            time.sleep(7)
            os._exit(0)
        os.close(writer)
        assert os.read(reader, 1) == b"R"
        os.close(reader)
        if scenario == "descendant":
            sys.exit(23)
    (directory / "ready").touch()
    if scenario == "streams":
        sys.stdout.buffer.write(sys.stdin.buffer.read())
        sys.stderr.buffer.write(b"inherited-stderr\xff")
        sys.exit(7)
    if scenario in ("signal", "stubborn", "graceful", "grace-stubborn", "worker"):
        time.sleep(7)
    elif scenario in ("tty", "tty-stubborn"):
        assert os.read(0, 1) == b"x"
        assert os.tcgetpgrp(0) == os.getpgrp()
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        tty.setraw(0)
        (directory / "child-terminal").write_text("foreground")
        if scenario == "tty-stubborn":
            time.sleep(7)
    elif scenario == "background":
        assert os.tcgetpgrp(0) != os.getpgrp()
        (directory / "child-terminal").write_text("background")
    else:
        raise ValueError(scenario)


if __name__ == "__main__":
    main()
