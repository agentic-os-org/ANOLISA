"""Private setup state and a lock shared by setup and installation."""

import fcntl
import json
import os
import stat
import tempfile
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from i18n import tr


def private_directory(path: Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid():
        raise ValueError(f"Unsafe application directory: {path}")
    path.chmod(0o700)


def private_read(path: Path) -> bytes | None:
    try:
        with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
                raise ValueError(f"Expected a private user-owned file: {path}")
            return stream.read()
    except FileNotFoundError:
        return None


def atomic_write(path: Path, data: bytes, mode: int = 0o600) -> None:
    descriptor, filename = tempfile.mkstemp(prefix=".write-", dir=path.parent)
    temporary = Path(filename)
    try:
        with os.fdopen(descriptor, "wb") as output:
            os.fchmod(output.fileno(), mode)
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def json_bytes(data: dict) -> bytes:
    return (json.dumps(data, ensure_ascii=False, indent=2) + "\n").encode()


def read_json(path: Path) -> dict[str, Any] | None:
    data = private_read(path)
    if data is None:
        return None
    try:
        result = json.loads(data)
    except (ValueError, UnicodeError) as error:
        raise ValueError(f"Invalid application data: {path.name}") from error
    if not isinstance(result, dict):
        raise ValueError(f"Invalid application data: {path.name}")
    return result


@contextmanager
def lock(path: Path) -> Iterator[None]:
    private_directory(path)
    flags = os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK
    with os.fdopen(os.open(path / "configuration.lock", flags, 0o600), "r+b") as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError(
                tr("Setup or installation is already running. Return to the original terminal.")
            ) from None
        yield


def running(path: Path) -> bool:
    try:
        descriptor = os.open(
            path / "configuration.lock", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
        )
    except FileNotFoundError:
        return False
    with os.fdopen(descriptor, "rb") as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_SH | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
    return False


def read_state(config: Path) -> dict[str, Any]:
    state = read_json(config / "setup.json")
    if state is None:
        return {
            "schema": 1,
            "deferred": False,
            "cosh": {"choice": "pending"},
            "qoder": {"choice": "pending"},
        }
    if state.get("schema") != 1 or type(state.get("deferred")) is not bool:
        raise ValueError("Invalid setup state")
    for component in ("cosh", "qoder"):
        value = state.get(component)
        if not isinstance(value, dict) or value.get("choice") not in (
            "pending",
            "configure",
            "skipped",
        ):
            raise ValueError("Invalid setup choice")
    if state["qoder"].get("auth_mode", "native") not in ("native", "pat"):
        raise ValueError("Invalid Qoder authentication mode")
    return state


def save_state(config: Path, state: dict) -> None:
    private_directory(config)
    atomic_write(config / "setup.json", json_bytes(state))
