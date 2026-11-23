#!/usr/bin/env python3
"""Run from a verified root-owned copy and install only the pinned sec-core daemon."""

import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

MARKER = "# ANOLISA Omarchy managed system service"
PARENT = Path("/opt/anolisa-omarchy/sec-core")
SERVICE = Path("/etc/systemd/system/anolisa-omarchy-security.service")
SKILLSEC = Path("/etc/anolisa-omarchy/skillsec.json")
JOURNAL = PARENT / ".install-transaction.json"


def secure_directory(path: Path, mode: int = 0o755) -> None:
    if path.is_symlink():
        raise ValueError(f"Refusing a linked system directory: {path}")
    path.mkdir(parents=True, exist_ok=True, mode=mode)
    metadata = path.stat()
    if metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) & 0o022:
        raise ValueError(f"System directory must be owned by root and protected: {path}")


def install_binary(binary: Path, expected: str) -> Path:
    if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise ValueError("Invalid security daemon digest")
    parent = PARENT
    destination = parent / expected
    if not destination.exists():
        descriptor, name = tempfile.mkstemp(prefix=".daemon-", dir=parent)
        temporary = Path(name)
        try:
            with os.fdopen(descriptor, "wb") as output:
                source_fd = os.open(binary, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                with os.fdopen(source_fd, "rb") as source:
                    if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                        raise ValueError("Security daemon input must be a regular file")
                    shutil.copyfileobj(source, output)
                output.flush()
                os.fsync(output.fileno())
            if hashlib.sha256(temporary.read_bytes()).hexdigest() != expected:
                raise ValueError("Security daemon checksum mismatch")
            temporary.chmod(0o755)
            temporary.replace(destination)
        finally:
            temporary.unlink(missing_ok=True)
    metadata = destination.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0:
        raise ValueError("Unsafe installed security daemon")
    if stat.S_IMODE(metadata.st_mode) & 0o022:
        raise ValueError("Installed security daemon is writable by another user")
    if hashlib.sha256(destination.read_bytes()).hexdigest() != expected:
        raise ValueError("Installed security daemon was modified")
    return destination


def unit_text(destination: Path) -> str:
    return (
        MARKER + "\n[Unit]\nDescription=ANOLISA Omarchy code-safety daemon\n"
        "[Service]\nType=simple\nUMask=0077\nRuntimeDirectory=anolisa-omarchy\n"
        "RuntimeDirectoryMode=0755\nEnvironment=AGENT_SEC_DATA_DIR=/var/lib/anolisa-omarchy/security\n"
        f"ExecStart={destination} serve --socket /run/anolisa-omarchy/sec-core.sock "
        f"--skillsec-config {SKILLSEC}\n"
        "Restart=on-failure\nRestartSec=2\nTimeoutStopSec=75\n"
        "[Install]\nWantedBy=multi-user.target\n"
    )


def atomic_text(path: Path, text: str, mode: int = 0o600) -> None:
    fd, name = tempfile.mkstemp(prefix=".anolisa-", dir=path.parent)
    temporary = Path(name)
    try:
        with os.fdopen(fd, "w") as stream:
            os.fchmod(stream.fileno(), mode)
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def systemctl(*arguments: str) -> None:
    subprocess.run(["/usr/bin/systemctl", *arguments], check=True, timeout=95)


def service_state(action: str) -> bool:
    return (
        subprocess.run(
            ["/usr/bin/systemctl", action, "--quiet", SERVICE.name], check=False, timeout=5
        ).returncode
        == 0
    )


def previous_unit() -> str | None:
    if SERVICE.is_symlink():
        raise ValueError("Refusing to replace a linked system service")
    if not SERVICE.exists():
        return None
    old = SERVICE.read_text()
    match = re.search(r"(?m)^ExecStart=(/opt/anolisa-omarchy/sec-core/[a-f0-9]{64}) ", old)
    if not match or old != unit_text(Path(match[1])):
        raise ValueError("Existing system service differs from its managed template")
    return old


def rollback(transaction: str) -> None:
    snapshot = json.loads(JOURNAL.read_text())
    if snapshot["transaction"] != transaction:
        raise ValueError("Security-service transaction does not match")
    if snapshot["changed"]:
        if service_state("is-active"):
            systemctl("stop", SERVICE.name)
        if service_state("is-enabled") and not snapshot["enabled"]:
            systemctl("disable", SERVICE.name)
        if snapshot["unit"] is None:
            SERVICE.unlink(missing_ok=True)
        else:
            atomic_text(SERVICE, snapshot["unit"], 0o644)
        systemctl("daemon-reload")
        if snapshot["enabled"]:
            systemctl("enable", SERVICE.name)
        if snapshot["active"]:
            systemctl("start", SERVICE.name)
    if snapshot["created_skillsec"]:
        SKILLSEC.unlink(missing_ok=True)
    JOURNAL.unlink()


def prepare(binary: Path, expected: str, transaction: str) -> None:
    if JOURNAL.exists():
        raise ValueError(
            f"A previous service transaction needs recovery: {JOURNAL}. "
            "Stop other installers, then rerun the prebuilt installer with: bash ./install --recover"
        )
    unit = previous_unit()
    destination = install_binary(binary, expected)
    desired = unit_text(destination)
    secure_directory(SKILLSEC.parent)
    secure_directory(Path("/var/lib/anolisa-omarchy"), 0o700)
    if SKILLSEC.is_symlink() or (
        SKILLSEC.exists()
        and (
            not SKILLSEC.is_file() or SKILLSEC.stat().st_uid != 0 or SKILLSEC.stat().st_mode & 0o022
        )
    ):
        raise ValueError("Unsafe existing sec-core settings")
    active = service_state("is-active")
    enabled = service_state("is-enabled")
    changed = unit != desired or not active or not enabled
    created = not SKILLSEC.exists()
    atomic_text(
        JOURNAL,
        json.dumps(
            {
                "transaction": transaction,
                "unit": unit,
                "enabled": enabled,
                "active": active,
                "changed": changed,
                "created_skillsec": created,
            }
        ),
    )
    try:
        if created:
            atomic_text(
                SKILLSEC,
                json.dumps(
                    {
                        "stateDir": "/var/lib/anolisa-omarchy/skillsec",
                        "enableDefaultSkillDirs": False,
                    }
                ),
            )
        if changed:
            atomic_text(SERVICE, desired, 0o644)
            systemctl("daemon-reload")
            if not enabled:
                systemctl("enable", SERVICE.name)
            systemctl("restart", SERVICE.name)
    except BaseException:
        rollback(transaction)
        raise


def main() -> None:
    if os.geteuid() != 0:
        raise ValueError("The security service installer requires sudo")
    action, *args = sys.argv[1:]
    if action not in ("prepare", "rollback", "commit", "recover") or len(args) != (
        3 if action == "prepare" else 1
    ):
        raise ValueError("Invalid security-service transaction arguments")
    transaction = args[-1]
    if not re.fullmatch(r"[a-f0-9]{32}", transaction):
        raise ValueError("Invalid transaction ID")
    secure_directory(PARENT.parent)
    secure_directory(PARENT)
    flags = os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW
    with os.fdopen(os.open(PARENT / ".install.lock", flags, 0o600), "r+b") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        if action == "prepare":
            prepare(Path(args[0]), args[1], transaction)
        elif action == "rollback":
            rollback(transaction)
        elif action == "recover":
            # Recovery is explicit so retrying cannot silently abort another install.
            if JOURNAL.exists():
                previous = json.loads(JOURNAL.read_text())["transaction"]
                if not re.fullmatch(r"[a-f0-9]{32}", previous):
                    raise ValueError("Invalid interrupted transaction ID")
                rollback(previous)
        else:
            if json.loads(JOURNAL.read_text())["transaction"] != transaction:
                raise ValueError("Security-service transaction does not match")
            JOURNAL.unlink()


if __name__ == "__main__":
    main()
