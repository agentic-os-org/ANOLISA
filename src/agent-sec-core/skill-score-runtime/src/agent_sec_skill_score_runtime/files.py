"""Bounded no-follow reads at the boundary of container-controlled artifacts."""

import hashlib
import json
import os
import stat
import tempfile
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_score.contracts import ContractError

FILE_LIMIT = 16 * 1024 * 1024
TREE_LIMIT = 128 * 1024 * 1024
ENTRY_LIMIT = 4096
DEPTH_LIMIT = 16


def open_directory(path: Path) -> int:
    """Open every ancestor without following links, retaining the final directory fd."""
    path = path.absolute()
    current = os.open(path.anchor, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            if part in {".", ".."}:
                raise ContractError("artifact path must be canonical")
            following = os.open(
                part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=current
            )
            os.close(current)
            current = following
        return current
    except BaseException:
        os.close(current)
        raise


def read_regular(parent: int, name: str, limit: int = FILE_LIMIT) -> bytes:
    """Reject links, hard links, special files, excess size and in-read replacement."""
    descriptor = os.open(
        name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent
    )
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise ContractError("artifact must be a regular file with one link")
        if before.st_size > limit:
            raise ContractError("artifact exceeds its byte limit")
        with os.fdopen(os.dup(descriptor), "rb") as stream:
            content = stream.read(limit + 1)
        after = os.fstat(descriptor)
        if len(content) > limit or (
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        ) != (
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ):
            raise ContractError("artifact changed while being collected")
        return content
    finally:
        os.close(descriptor)


def read_file(path: Path, limit: int = FILE_LIMIT) -> bytes:
    parent = open_directory(path.parent)
    try:
        return read_regular(parent, path.name, limit)
    finally:
        os.close(parent)


def atomic_json(path: Path, value: Any) -> None:
    """Publish a private response without opening an attacker-chosen temporary link."""
    parent = open_directory(path.parent)
    name = ""
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", dir=path.parent, delete=False
        ) as stream:
            name = Path(stream.name).name
            json.dump(value, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.rename(name, path.name, src_dir_fd=parent, dst_dir_fd=parent)
        name = ""
    finally:
        if name:
            os.unlink(name, dir_fd=parent)
        os.close(parent)


def snapshot_tree(
    source: Path, destination: Path, *, exclude_root: tuple[str, ...] = ()
) -> list[dict[str, Any]]:
    """Copy a bounded snapshot; hashes describe collection integrity, not authorship."""
    manifest: list[dict[str, Any]] = []
    consumed, entries = 0, 0

    def visit(descriptor: int, target: Path, relative: Path, depth: int) -> None:
        nonlocal consumed, entries
        if depth > DEPTH_LIMIT:
            raise ContractError("artifact tree exceeds its depth limit")
        target.mkdir(mode=0o700)
        # Bound enumeration before sorting: an oversized directory must not allocate an unbounded list.
        names = []
        with os.scandir(descriptor) as items:
            for item in items:
                entries += 1
                if entries > ENTRY_LIMIT:
                    raise ContractError("artifact tree exceeds its entry limit")
                names.append(item.name)
        for name in sorted(names):
            if depth == 0 and name in exclude_root:
                continue
            info = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
            rel = relative / name
            if stat.S_ISDIR(info.st_mode):
                child = os.open(
                    name,
                    os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                    dir_fd=descriptor,
                )
                try:
                    visit(child, target / name, rel, depth + 1)
                finally:
                    os.close(child)
            else:
                content = read_regular(descriptor, name)
                consumed += len(content)
                if consumed > TREE_LIMIT:
                    raise ContractError(
                        "artifact tree exceeds its aggregate byte limit"
                    )
                (target / name).write_bytes(content)
                (target / name).chmod(0o600)
                manifest.append(
                    {
                        "path": rel.as_posix(),
                        "size": len(content),
                        "sha256": hashlib.sha256(content).hexdigest(),
                    }
                )

    root = open_directory(source)
    try:
        visit(root, destination, Path(), 0)
    finally:
        os.close(root)
    return manifest
