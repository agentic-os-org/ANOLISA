"""Serialize preparation/removal against scoring and retain uncertain cleanup."""

from __future__ import annotations

import contextlib
import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
from collections.abc import Iterator
from pathlib import Path

COMPONENT = "sec-core-skill-score"
LOCK = ".lifecycle.lock"


def directory(path: Path, *, create: bool = False) -> None:
    """Reject symlink ancestors instead of resolving them into deletion targets."""
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("component lifecycle requires an absolute, non-escaping path")
    for item in reversed((path, *path.parents)):
        if create:
            try:
                item.mkdir()
            except FileExistsError:
                pass
        if not stat.S_ISDIR(item.lstat().st_mode):
            raise ValueError(f"component directory is not a real directory: {item}")


@contextlib.contextmanager
def exclusive(root: Path, *, create: bool = False) -> Iterator[None]:
    directory(root, create=create)
    flags = os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
    if create:
        flags |= os.O_CREAT
    descriptor = os.open(root / LOCK, flags, 0o644)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError("component lifecycle lock is not a private regular file")
        if create:
            os.fchmod(descriptor, 0o644)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError(
                "scoring or preparation is active; retry after it finishes"
            ) from None
        current = (root / LOCK).lstat()
        if (metadata.st_dev, metadata.st_ino) != (current.st_dev, current.st_ino):
            raise ValueError("component lifecycle lock changed; retry explicitly")
        yield
    finally:
        os.close(descriptor)


def pending_cleanup(root: Path) -> None:
    for name in ("requests", "recovery"):
        path = root / name
        if not path.exists() and not path.is_symlink():
            continue
        directory(path)
        if any(path.iterdir()):
            raise RuntimeError(
                f"unconfirmed scoring cleanup retained at {path}; recover before retrying"
            )


def request_directory(root: Path) -> None:
    path = root / "requests"
    directory(path, create=True)
    if path.stat().st_uid != root.stat().st_uid:
        raise ValueError("request recovery directory has an unexpected owner")
    # System installation remains callable by users with Docker access. Records
    # are private files; sticky mode prevents users removing each other's records.
    path.chmod(0o1777)


def record_environment(path: Path, identity: str) -> None:
    receipt = path / ".generated.json"
    expected = {
        "schema": "agent-sec-skill-score/generated-v1",
        "component": COMPONENT,
        "manifest_sha256": identity,
    }
    if receipt.exists() or receipt.is_symlink():
        metadata = receipt.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_size > 1048576
            or json.loads(receipt.read_bytes()) != expected
        ):
            raise ValueError("generated environment ownership receipt is unsafe")
        return
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC
    descriptor = os.open(receipt, flags, 0o644)
    with os.fdopen(descriptor, "w") as stream:
        json.dump(expected, stream)


def owned_environment(path: Path) -> None:
    name = re.fullmatch(r"([a-f0-9]{64})(?:\.retained-[a-f0-9]{32})?", path.name)
    if name is None:
        raise ValueError(f"unknown generated environment retained at {path}")
    directory(path)
    manifest_path = path / "runtime-manifest.json"
    receipt = path / ".generated.json"
    proof_path = (
        receipt if receipt.exists() or receipt.is_symlink() else path / "ready.json"
    )
    for item in (manifest_path, proof_path):
        metadata = item.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_size > 1048576
        ):
            raise ValueError(
                f"unsafe environment ownership evidence retained at {item}"
            )
    content = manifest_path.read_bytes()
    manifest = json.loads(content)
    proof = json.loads(proof_path.read_bytes())
    owned = (
        proof.get("schema") == "agent-sec-skill-score/generated-v1"
        and proof.get("component") == COMPONENT
        if proof_path == receipt
        else proof.get("schema") == "agent-sec-skill-score/ready-v1"
        and all(
            proof.get(key) == manifest.get(key)
            for key in ("source_commit", "version", "protocol_version")
        )
    )
    if (
        hashlib.sha256(content).hexdigest() != name[1]
        or manifest.get("schema") != "agent-sec-skill-score/runtime-v1"
        or manifest.get("component") != COMPONENT
        or not re.fullmatch(r"[a-f0-9]{40}", manifest.get("source_commit", ""))
        or not isinstance(manifest.get("version"), str)
        or manifest.get("protocol_version") != "agent-sec-skill-score/worker-v1"
        or not owned
        or proof.get("manifest_sha256") != name[1]
    ):
        raise ValueError(f"unproven environment ownership retained at {path}")


def uninstall(root: Path) -> None:
    """Remove only proven generated environments, never caches or Docker resources."""
    directory(root)
    # A successful pre-hook can be retried after a later installer failure.
    if not (root / LOCK).exists() and not (root / LOCK).is_symlink():
        pending_cleanup(root)
        if (
            not (root / "environments").exists()
            and not (root / "environments").is_symlink()
        ):
            for name in ("requests", "recovery"):
                path = root / name
                if path.exists():
                    path.rmdir()
            return
    with exclusive(root):
        pending_cleanup(root)
        environments = root / "environments"
        targets = []
        if environments.exists() or environments.is_symlink():
            directory(environments)
            targets = list(environments.iterdir())
            # Preflight every target before the first deletion, including retained
            # repair environments. Damaged/unknown ownership remains actionable.
            for target in targets:
                owned_environment(target)
        if not shutil.rmtree.avoids_symlink_attacks:
            raise RuntimeError("safe descriptor-based directory removal is unavailable")
        for target in targets:
            shutil.rmtree(target)
        for name in ("environments", "requests", "recovery"):
            path = root / name
            if path.exists():
                path.rmdir()
        # No future Runtime may acquire a valid lease. A waiter holding the old
        # inode must reject it; installer-owned files can now be removed safely.
        (root / LOCK).unlink()
