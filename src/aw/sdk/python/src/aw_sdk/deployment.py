"""Read trusted deployment hints; the digest binds bytes, not publisher authority."""

import errno
import hashlib
import json
import os
from pathlib import Path
import re
import stat

from aw_sdk.transport import AwError

LIMIT = 4 * 1024 * 1024
# Unicode White_Space excludes the U+001C–U+001F controls removed by str.strip().
WHITE_SPACE = (
    "\t\n\v\f\r \u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004"
    "\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000"
)
FIELDS = {
    "version",
    "configuration_sha256",
    "target",
    "profile",
    "endpoint",
    "wait_for_input_submit",
    "availability",
}


NATIVE_ACLS = (
    "system.nfs4_acl",
    "system.nfs4_dacl",
    "system.richacl",
    "system.cifs_acl",
    "system.cifs_ntsd",
    "system.cifs_ntsd_full",
    "system.smb3_acl",
    "system.smb3_ntsd",
    "system.smb3_ntsd_full",
)


def check_filesystems(*descriptors: int) -> None:
    # Kernel mount IDs avoid architecture-dependent libc statfs structure layouts.
    pending = set()
    for descriptor in descriptors:
        with open(f"/proc/self/fdinfo/{descriptor}") as info:
            ids = [line.partition(":")[2].strip() for line in info if line.startswith("mnt_id:")]
        if len(ids) != 1 or not ids[0].isdigit():
            raise ValueError()
        pending.add(ids[0])
    with open("/proc/self/mountinfo", encoding="utf-8", errors="surrogateescape") as mounts:
        for line in mounts:
            mount_id, _, details = line.partition(" ")
            if mount_id not in pending:
                continue
            filesystem = details.partition(" - ")[2].partition(" ")[0]
            if filesystem not in {"ext2", "ext3", "ext4", "xfs", "btrfs", "tmpfs", "overlay"}:
                raise ValueError()
            pending.remove(mount_id)
            if not pending:
                return
    raise ValueError()


def reject_acls(descriptor: int, *, directory: bool = False) -> None:
    attributes = NATIVE_ACLS if directory else ("system.posix_acl_access", *NATIVE_ACLS)
    for attribute in attributes:
        try:
            os.getxattr(f"/proc/self/fd/{descriptor}", attribute)
        except OSError as error:
            if error.errno not in (errno.ENODATA, errno.EOPNOTSUPP):
                raise
        else:
            raise ValueError()


def identity(info: os.stat_result) -> tuple:
    return (
        info.st_dev,
        info.st_ino,
        info.st_mode,
        info.st_uid,
        info.st_gid,
        info.st_nlink,
        info.st_size,
        info.st_mtime_ns,
    )


def read_file(path: str) -> bytes:
    if not isinstance(path, str) or not Path(path).is_absolute() or ".." in path.split("/"):
        raise AwError("sdk_descriptor_file_invalid")
    parent = None
    try:
        parts = Path(path).parts[1:]
        if not parts:
            raise ValueError()
        expected_parent = os.stat(Path(path).parent)
        flags = os.O_PATH | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        parent = os.open("/", flags)
        for part in parts[:-1]:
            child = os.open(part, flags, dir_fd=parent)
            os.close(parent)
            parent = child
        parent_info = os.fstat(parent)
        if (
            (parent_info.st_dev, parent_info.st_ino)
            != (expected_parent.st_dev, expected_parent.st_ino)
            or parent_info.st_uid != os.geteuid()
            or parent_info.st_mode & 0o022
        ):
            raise ValueError()
        descriptor = os.open(
            parts[-1],
            os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=parent,
        )
        with os.fdopen(descriptor, "rb") as stream:
            info = os.fstat(stream.fileno())
            if (
                not stat.S_ISREG(info.st_mode)
                or info.st_size > LIMIT
                or info.st_uid != os.geteuid()
                or info.st_mode & 0o7022
                or info.st_nlink != 1
            ):
                raise ValueError()
            check_filesystems(parent, stream.fileno())
            reject_acls(parent, directory=True)
            data = stream.read(LIMIT + 1)
            reject_acls(stream.fileno())
            reject_acls(parent, directory=True)
            current_parent = os.fstat(parent)
            if (
                len(data) > LIMIT
                or identity(info) != identity(os.fstat(stream.fileno()))
                or identity(info)
                != identity(os.stat(parts[-1], dir_fd=parent, follow_symlinks=False))
                or current_parent.st_uid != os.geteuid()
                or current_parent.st_mode & 0o022
            ):
                raise ValueError()
            return data
    except (OSError, ValueError):
        raise AwError("sdk_descriptor_file_invalid") from None
    finally:
        if parent is not None:
            os.close(parent)


def object_fields(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key")
        result[key] = value
    return result


def load_descriptor(
    config_file: str,
    descriptor_file: str,
    profile: dict,
    target: str | None,
    *,
    requires_service: bool = False,
) -> dict:
    """Use deployment-owned files only; runtime admission remains Service-owned."""
    encoded = read_file(descriptor_file)
    try:
        value = json.loads(encoded.decode("utf-8"), object_pairs_hook=object_fields)
        if not isinstance(value, dict) or set(value) != FIELDS:
            raise ValueError()
        actual_profile = value["profile"]
        if not isinstance(actual_profile, dict) or set(actual_profile) != {
            "adapter",
            "version",
            "entrypoint",
        }:
            raise ValueError()
        strings = [
            value[key]
            for key in ("version", "configuration_sha256", "target", "endpoint", "availability")
        ]
        strings.extend(actual_profile.values())
        if any(
            not isinstance(text, str) or re.search(r"[\ud800-\udfff]", text) for text in strings
        ):
            raise ValueError()
        if (
            value["version"] != "aw-deployment/v1alpha1"
            or not re.fullmatch(r"[a-f0-9]{64}", value["configuration_sha256"])
            or not re.fullmatch(r"[a-zA-Z0-9._-]{1,128}", value["target"])
            or not value["endpoint"].startswith("/")
            or "\0" in value["endpoint"]
            or type(value["wait_for_input_submit"]) is not bool
            or value["availability"] not in {"required", "native_fallback"}
            or any(not text.strip(WHITE_SPACE) for text in actual_profile.values())
        ):
            raise ValueError()
    except (ValueError, TypeError, RecursionError):
        raise AwError("sdk_descriptor_invalid") from None
    if actual_profile != profile or (
        requires_service and value["availability"] != "required"
    ):
        raise AwError("sdk_descriptor_profile_mismatch")
    if target is not None and target != value["target"]:
        raise AwError("sdk_descriptor_target_mismatch")
    if hashlib.sha256(read_file(config_file)).hexdigest() != value["configuration_sha256"]:
        raise AwError("sdk_descriptor_stale")
    return {
        "target": value["target"],
        "endpoint": value["endpoint"],
        "input_enabled": value["wait_for_input_submit"],
        "availability": value["availability"],
    }
