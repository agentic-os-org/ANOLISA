"""Serialize preparation/removal against scoring and retain uncertain cleanup."""

from __future__ import annotations

import contextlib
import errno
import fcntl
import hashlib
import json
import os
import pwd
import re
import shutil
import stat
import subprocess
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
    descriptor = os.open(root / LOCK, flags, 0o600)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError("component lifecycle lock is not a private regular file")
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


def pending_request_owner(path: Path) -> int | None:
    # A worker syncs this complete record before launching anything. Ignore partial
    # writes and junk in a directory that older installations made world-writable.
    if not re.fullmatch(r"[a-f0-9]{32}\.json", path.name):
        return None
    try:
        if not stat.S_ISREG(path.lstat().st_mode):
            return None
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
        with os.fdopen(os.open(path, flags), "rb") as stream:
            meta = os.fstat(stream.fileno())
            if (
                not stat.S_ISREG(meta.st_mode)
                or meta.st_nlink != 1
                or meta.st_mode & 0o077
            ):
                return None
            data = stream.read(1025)
    except (FileNotFoundError, IsADirectoryError):
        return None
    except OSError as exc:
        if exc.errno == errno.ELOOP:
            return None
        raise
    if len(data) > 1024:
        return None
    try:
        valid = json.loads(data) == {
            "schema": "agent-sec-skill-score/pending-request-v1",
            "request_id": path.stem,
            "compose_project": f"asc-score-{path.stem}__env",
        }
        return meta.st_uid if valid else None
    except (ValueError, RecursionError):
        return None


def docker_writer(uid: int) -> bool:
    try:
        endpoint = Path("/var/run/docker.sock").stat()
        if not stat.S_ISSOCK(endpoint.st_mode):
            return False
        if uid == endpoint.st_uid:
            return bool(endpoint.st_mode & stat.S_IWUSR)
        user = pwd.getpwuid(uid)
        return bool(
            endpoint.st_mode & stat.S_IWGRP
        ) and endpoint.st_gid in os.getgrouplist(user.pw_name, user.pw_gid)
    except (FileNotFoundError, KeyError):
        return False


def retained_projects() -> set[str]:
    # A revoked or deleted account may still own resources from before the access
    # change. Query the daemon once per resource kind, including stopped containers.
    projects: set[str] = set()
    label = "com.docker.compose.project"
    for kind in ("container", "network", "volume"):
        args = [
            "docker",
            kind,
            "ls",
            "--filter",
            f"label={label}",
            "--format",
            '{{.Label "' + label + '"}}',
        ]
        if kind == "container":
            args.append("--all")
        result = subprocess.run(
            args,
            check=True,
            timeout=5,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            env={
                "PATH": "/usr/local/bin:/usr/bin:/bin",
                "DOCKER_HOST": "unix:///var/run/docker.sock",
            },
        )
        # Even a conflicting ownership label leaves cleanup unconfirmed for the project.
        projects.update(
            line
            for line in result.stdout.splitlines()
            if re.fullmatch(r"asc-score-[a-f0-9]{32}__env", line)
        )
    return projects


def pending_cleanup(root: Path) -> None:
    writers = {root.stat().st_uid: True}
    projects = None
    for name in ("recovery", "requests"):
        path = root / name
        if not path.exists() and not path.is_symlink():
            continue
        directory(path)
        with os.scandir(path) as entries:
            for item in entries:
                if name == "requests":
                    record = Path(item.path)
                    uid = pending_request_owner(record)
                    if uid is None:
                        continue
                    if uid not in writers:
                        writers[uid] = docker_writer(uid)
                    if not writers[uid]:
                        if projects is None:
                            try:
                                projects = retained_projects()
                            except (OSError, subprocess.SubprocessError):
                                raise RuntimeError(
                                    f"cannot verify legacy scoring cleanup at {path}; recover before retrying"
                                ) from None
                        if f"asc-score-{record.stem}__env" not in projects:
                            continue
                raise RuntimeError(
                    f"unconfirmed scoring cleanup retained at {path}; recover before retrying"
                )


def request_directory(
    root: Path, docker_gid: int | None, *, verify_only: bool = False
) -> None:
    path = root / "requests"
    directory(path, create=not verify_only)
    # Root and users in the local Docker socket group can actually start worker
    # resources. Other users must not be able to hold a lease or forge records.
    for item, mode in (
        (root / LOCK, 0o600 if docker_gid is None else 0o640),
        (path, 0o1700 if docker_gid is None else 0o1770),
    ):
        meta = item.lstat()
        if meta.st_uid != root.stat().st_uid:
            raise ValueError("request registry has an unexpected owner")
        if verify_only:
            if stat.S_IMODE(meta.st_mode) != mode or (
                docker_gid is not None and meta.st_gid != docker_gid
            ):
                raise ValueError(
                    "request registry permissions changed; explicitly repair sec-core-skill-score"
                )
        else:
            if docker_gid is not None:
                os.chown(item, -1, docker_gid)
            item.chmod(mode)


def remove_empty_directory(path: Path) -> None:
    try:
        path.rmdir()
    except FileNotFoundError:
        pass
    except OSError as exc:
        # Older world-writable registries may contain unrelated user files.
        if path.name != "requests" or exc.errno != errno.ENOTEMPTY:
            raise


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
        or manifest.get("schema")
        not in {"agent-sec-skill-score/runtime-v1", "agent-sec-skill-score/runtime-v2"}
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
                remove_empty_directory(root / name)
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
            remove_empty_directory(root / name)
        # No future Runtime may acquire a valid lease. A waiter holding the old
        # inode must reject it; installer-owned files can now be removed safely.
        (root / LOCK).unlink()
