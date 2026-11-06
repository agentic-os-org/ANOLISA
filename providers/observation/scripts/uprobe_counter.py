"""Fixture-only, fd-owned Linux x86-64 uprobe counting isolation experiment.

A PID-filtered BPF link increments one counter without reading payloads.
An optional short-lived privileged helper passes fds to the ordinary collector.
"""

import argparse
import array
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import select
import signal
import socket
import subprocess
import sys
import stat
import struct
import time

from correlation import process_ref

LIBC = ctypes.CDLL(None, use_errno=True)
STOPPING = False


def emit(value: dict) -> None:
    record = json.dumps(
        {"schema_version": "aw.uprobe-counter-poc/v0", **value}, separators=(",", ":")
    )
    if len(record) > 8192:
        raise ValueError("record_limit")
    print(record, flush=True)


def stop_signal(_number: int, _frame: object) -> None:
    global STOPPING
    STOPPING = True


def fingerprint(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def load_permit(path: Path, uid: int) -> dict:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != uid
            or info.st_mode & 0o077
            or info.st_size > 8192
        ):
            raise ValueError("invalid_permit_owner_mode_or_size")
        permit = json.loads(source.read(8193))
    process = permit["process"]
    if (
        permit["schema_version"] != "aw.ptrace-permit-poc/v0"
        or permit["authorized_experiment"] is not True
        or process["uid"] != uid
    ):
        raise ValueError("experiment_not_authorized")
    if process_ref(process["pid"]) != process:
        raise ValueError("stale_process_identity")
    if fingerprint(Path(f"/proc/{process['pid']}/exe")) != permit["exe_sha256"]:
        raise ValueError("executable_fingerprint_mismatch")
    return process


def symbol_offset(data: bytes, name: str) -> int:
    """Resolve an ELF dynamic symbol through its executable PT_LOAD file mapping."""
    if data[:6] != b"\x7fELF\x02\x01" or len(data) < 64:
        raise ValueError("unsupported_elf")
    header = struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
    kind, machine, _, _, phoff, shoff, _, _, phsize, phnum, shsize, shnum, _ = header
    if kind != 3 or machine != 62 or phsize != 56 or shsize != 64:
        raise ValueError("unsupported_abi")
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, shoff + i * shsize) for i in range(shnum)]
    addresses = set()
    for section in sections:
        if section[1] != 11:
            continue
        _, _, _, _, offset, size, link, _, _, entry_size = section
        if entry_size != 24 or size % 24:
            raise ValueError("unsupported_dynsym")
        strings = sections[link]
        names = data[strings[4] : strings[4] + strings[5]]
        for entry in range(offset, offset + size, entry_size):
            string, info, _, index, value, _ = struct.unpack_from("<IBBHQQ", data, entry)
            if index and info & 15 == 2 and names[string:].split(b"\0", 1)[0] == name.encode():
                addresses.add(value)
    if len(addresses) != 1:
        raise ValueError("unsupported_or_ambiguous_symbol")
    address = addresses.pop()
    offsets = []
    for index in range(phnum):
        segment, flags, offset, virtual, _, size, _, _ = struct.unpack_from(
            "<IIQQQQQQ", data, phoff + index * phsize
        )
        if segment == 1 and flags & 1 and virtual <= address < virtual + size:
            offsets.append(offset + address - virtual)
    if len(offsets) != 1:
        raise ValueError("unsupported_symbol_mapping")
    return offsets[0]


def resolve(pid: int, function: str) -> tuple[Path, int, str]:
    candidates = set()
    for line in Path(f"/proc/{pid}/maps").read_text().splitlines():
        fields = line.split(maxsplit=5)
        if len(fields) != 6 or "x" not in fields[1]:
            continue
        path = Path(fields[5])
        if not path.name.startswith("libssl.so") or not path.is_absolute():
            continue
        actual = Path(f"/proc/{pid}/root") / path.relative_to("/")
        info = actual.stat()
        device = ":".join(f"{n:02x}" for n in (os.major(info.st_dev), os.minor(info.st_dev)))
        if info.st_ino != int(fields[4]) or device != fields[3] or info.st_size > 32 << 20:
            raise ValueError("library_mapping_mismatch")
        candidates.add(actual)
    if len(candidates) != 1:
        raise ValueError("unsupported_tls_library")
    path = candidates.pop()
    data = path.read_bytes()
    return path, symbol_offset(data, function), hashlib.sha256(data).hexdigest()


def library(path: Path) -> ctypes.CDLL:
    result = ctypes.CDLL(str(path), use_errno=True)
    result.observation_open.argtypes = [
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint64,
        ctypes.POINTER(ctypes.c_int),
    ]
    result.observation_count.argtypes = [ctypes.c_int, ctypes.POINTER(ctypes.c_uint64)]
    return result


def open_link(options: argparse.Namespace, uid: int) -> tuple[list[int], dict]:
    process = load_permit(options.permit, uid)
    pidfd = os.pidfd_open(process["pid"])
    descriptors = (ctypes.c_int * 2)(-1, -1)
    try:
        path, offset, digest = resolve(process["pid"], options.function)
        if select.select([pidfd], [], [], 0)[0] or process_ref(process["pid"]) != process:
            raise ValueError("stale_process_identity")
        if library(options.library).observation_open(
            process["pid"], os.fsencode(path), offset, descriptors
        ):
            error = ctypes.get_errno()
            raise OSError(error, "bpf_link_setup: " + os.strerror(error))
        if process_ref(process["pid"]) != process or fingerprint(path) != digest:
            raise ValueError("identity_changed_during_attach")
        return list(descriptors), {"library_sha256": digest, "symbol_file_offset": offset}
    except BaseException:
        for fd in descriptors:
            if fd >= 0:
                os.close(fd)
        raise
    finally:
        os.close(pidfd)


def broker(options: argparse.Namespace) -> None:
    """A single-request helper; no socket path, persistent service or retained fd."""
    signal.alarm(5)
    with socket.socket(fileno=0) as connection:
        descriptors = []
        try:
            uid = int(os.environ["SUDO_UID"])
            if os.geteuid() != 0 or uid == 0:
                raise ValueError("nonroot_fixture_owner_required")
            descriptors, metadata = open_link(options, uid)
            connection.sendmsg(
                [json.dumps(metadata).encode()],
                [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", descriptors))],
            )
        except (OSError, ValueError, KeyError, struct.error, IndexError) as error:
            connection.send(json.dumps({"error": str(error)[:512]}).encode())
        finally:
            for fd in descriptors:
                os.close(fd)


def receive_link(options: argparse.Namespace) -> tuple[list[int], dict]:
    if not options.privileged_fixture_setup:
        return open_link(options, os.getuid())
    parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    descriptors = []
    try:
        parent.settimeout(10)
        command = [
            "sudo",
            "-n",
            "--",
            "/usr/bin/python3",
            str(Path(__file__).resolve()),
            "--broker",
            "--permit",
            str(options.permit),
            "--library",
            str(options.library),
            "--function",
            options.function,
        ]
        with subprocess.Popen(command, stdin=child, stdout=subprocess.DEVNULL) as helper:
            child.close()
            data, controls, flags, _ = parent.recvmsg(
                8192, socket.CMSG_SPACE(8), socket.MSG_CMSG_CLOEXEC
            )
            for level, kind, value in controls:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    received = array.array("i")
                    received.frombytes(value)
                    descriptors.extend(received)
            helper.wait(10)
            metadata = json.loads(data)
            if "error" in metadata:
                raise ValueError(metadata["error"])
            if (
                helper.returncode
                or flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC)
                or len(descriptors) != 2
            ):
                raise ValueError("invalid_broker_reply")
        return descriptors, metadata
    except BaseException:
        for fd in descriptors:
            os.close(fd)
        raise
    finally:
        parent.close()
        child.close()


def drop_privileges(uid: int, gid: int) -> None:
    if os.geteuid() == 0:
        os.setgroups([])
        os.setresgid(gid, gid, gid)
        os.setresuid(uid, uid, uid)
    if LIBC.prctl(38, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_NO_NEW_PRIVS")
    status = Path("/proc/self/status").read_text().splitlines()
    caps = {line.split(":", 1)[0]: line.split()[1] for line in status if line.startswith("Cap")}
    if os.getresuid() != (uid, uid, uid) or any(
        int(caps[key], 16) for key in ("CapEff", "CapPrm", "CapInh", "CapAmb")
    ):
        raise ValueError("privilege_drop_failed")


def run(options: argparse.Namespace) -> None:
    if platform.machine() != "x86_64" or platform.system() != "Linux":
        raise ValueError("unsupported_platform")
    uid, gid = os.getuid(), os.getgid()
    if os.geteuid() == 0:
        raise ValueError("root_collection_forbidden")
    process = load_permit(options.permit, uid)
    pid = process["pid"]
    descriptors = []
    pidfd = os.pidfd_open(pid)
    try:
        descriptors, metadata = receive_link(options)
        drop_privileges(uid, gid)
        if process_ref(pid) != process:
            raise ValueError("identity_changed_during_attach")
        backend = library(options.library)
        emit(
            {
                "kind": "source_status",
                "state": "observing",
                "source": "bpf.uprobe.counter",
                "collector_pid": os.getpid(),
                "collector_uid": os.geteuid(),
                "capabilities": "none",
                "process": process,
                "aw_instance_id": None,
                "aw_scope_id": None,
                "process_state": "observed",
                **metadata,
                "function": options.function,
                "thread_scope": "kernel_pid_filter",
                "coverage": "entry_count_only_no_payload_or_return_value",
                "clock": "CLOCK_MONOTONIC",
                "max_records": 512,
            }
        )
        deadline = time.monotonic() + options.duration
        previous = 0
        for _ in range(510):
            if select.select([pidfd], [], [], 0.02)[0]:
                emit({"kind": "source_status", "state": "target_exited"})
                break
            if process_ref(pid) != process:
                raise ValueError("stale_process_identity")
            value = ctypes.c_uint64()
            if backend.observation_count(descriptors[0], ctypes.byref(value)):
                raise OSError(ctypes.get_errno(), "bpf_map_lookup")
            count = value.value
            emit(
                {
                    "kind": "count",
                    "hits": count,
                    "delta": count - previous,
                    "observed_at_ns": str(time.monotonic_ns()),
                }
            )
            previous = count
            if STOPPING or time.monotonic() >= deadline:
                break
        emit({"kind": "source_summary", "hits": previous})
    finally:
        for fd in descriptors:
            os.close(fd)
        os.close(pidfd)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--broker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--library", type=Path, required=True)
    parser.add_argument("--permit", type=Path, required=True)
    parser.add_argument("--function", choices=("SSL_write", "SSL_write_ex"), default="SSL_write")
    parser.add_argument("--duration", type=float, default=8)
    parser.add_argument("--privileged-fixture-setup", action="store_true")
    args = parser.parse_args()
    if not 0 < args.duration <= 10:
        parser.error("duration must be in (0, 10]")
    signal.signal(signal.SIGINT, stop_signal)
    signal.signal(signal.SIGTERM, stop_signal)
    try:
        if args.broker:
            broker(args)
        else:
            run(args)
    except (OSError, ValueError, KeyError, struct.error, IndexError) as failure:
        emit({"kind": "error", "error": str(failure)[:512]})
        raise SystemExit(1)
