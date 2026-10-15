"""Bounded children for inherited-stream and foreground-terminal runtime tests."""

import os
from pathlib import Path
import signal
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
    if scenario == "stubborn":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    record(directory, "leader")
    if scenario in ("descendant", "stubborn"):
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
    if scenario in ("signal", "stubborn"):
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
