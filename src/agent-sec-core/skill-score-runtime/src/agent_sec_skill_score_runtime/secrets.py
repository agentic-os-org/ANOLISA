"""Scrub only regular private artifacts after execution resources are stopped."""

import os
import stat
from pathlib import Path

from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime.files import (
    ENTRY_LIMIT,
    TREE_LIMIT,
    open_directory,
    read_regular,
)


def scrub_artifacts(root: Path) -> None:
    keys = {
        os.environ[name].encode()
        for name in ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN")
        if os.environ.get(name)
    }
    if not keys:
        return
    entries, size = 0, 0

    def visit(directory: int, depth: int) -> None:
        nonlocal entries, size
        if depth > 16:
            raise ContractError("cannot safely scrub an excessively deep artifact tree")
        with os.scandir(directory) as items:
            for item in items:
                entries += 1
                if entries > ENTRY_LIMIT:
                    raise ContractError(
                        "cannot safely scrub an excessive artifact count"
                    )
                if any(key in os.fsencode(item.name) for key in keys):
                    raise ContractError(
                        "artifact filename contains a runtime credential"
                    )
                info = os.stat(item.name, dir_fd=directory, follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode):
                    child = os.open(
                        item.name,
                        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                        dir_fd=directory,
                    )
                    try:
                        visit(child, depth + 1)
                    finally:
                        os.close(child)
                    continue
                content = read_regular(directory, item.name)
                size += len(content)
                if size > TREE_LIMIT:
                    raise ContractError("cannot safely scrub excessive artifact bytes")
                redacted = content
                for key in keys:
                    redacted = redacted.replace(key, b"[REDACTED]")
                if redacted != content:
                    fd = os.open(
                        item.name,
                        os.O_WRONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                        dir_fd=directory,
                    )
                    try:
                        current = os.fstat(fd)
                        if (
                            not stat.S_ISREG(current.st_mode)
                            or current.st_nlink != 1
                            or (current.st_dev, current.st_ino)
                            != (info.st_dev, info.st_ino)
                        ):
                            raise ContractError(
                                "artifact changed before secret scrubbing"
                            )
                        with os.fdopen(os.dup(fd), "wb") as output:
                            output.write(redacted)
                            output.truncate()
                    finally:
                        os.close(fd)

    directory = open_directory(root)
    try:
        visit(directory, 0)
    finally:
        os.close(directory)
