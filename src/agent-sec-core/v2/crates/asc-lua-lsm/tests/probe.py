"""Controlled syscall subject embedded in the Rust acceptance-test binary."""

import array
import ctypes
import json
import mmap
import os
import socket
import subprocess
import sys
import tempfile
import threading

libc = ctypes.CDLL(None, use_errno=True)
libc.mmap.restype = ctypes.c_void_p
libc.mmap.argtypes = [
    ctypes.c_void_p,
    ctypes.c_size_t,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_long,
]
libc.mprotect.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
libc.munmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
held: int | None = None
mapping: int | None = None
existing_child: subprocess.Popen[str] | None = None


def checked(result: int) -> None:
    if result == -1:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code))


def mapping_identities(path: str) -> list[tuple[int, int, int]]:
    stat = os.stat(path)
    with (
        open(path, "rb") as backing,
        mmap.mmap(backing.fileno(), 0, access=mmap.ACCESS_COPY) as region,
        open("/proc/self/maps", encoding="utf-8", errors="replace") as maps,
    ):
        # Use the mapping address to select the entry even if the path or IDs differ.
        address = ctypes.addressof(ctypes.c_char.from_buffer(region))
        for line in maps:
            bounds, _, _, device, inode, *_ = line.split(maxsplit=5)
            start, end = bounds.split("-")
            if int(start, 16) <= address < int(end, 16):
                major, minor = device.split(":")
                return [
                    (os.major(stat.st_dev), os.minor(stat.st_dev), stat.st_ino),
                    (int(major, 16), int(minor, 16), int(inode)),
                ]
    raise RuntimeError(f"No /proc/self/maps entry for the probe file {path!r}")


def execute(operation: str, path: str) -> None:
    global held, mapping, existing_child
    if operation in ("hold", "hold-rw"):
        held = os.open(path, os.O_RDONLY if operation == "hold" else os.O_RDWR)
    elif operation == "read":
        os.close(os.open(path, os.O_RDONLY))
    elif operation == "readfd":
        os.pread(held, 1, 0)
    elif operation == "readv-fd":
        os.lseek(held, 0, os.SEEK_SET)
        os.readv(held, [bytearray(1)])
    elif operation == "preadv-fd":
        os.preadv(held, [bytearray(1)], 0)
    elif operation == "dup-read-fd":
        fd = os.dup(held)
        try:
            os.pread(fd, 1, 0)
        finally:
            os.close(fd)
    elif operation == "scm-read-fd":
        left, right = socket.socketpair()
        try:
            left.sendmsg(
                [b"x"],
                [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [held]))],
            )
            _, ancillary, _, _ = right.recvmsg(
                1, socket.CMSG_SPACE(array.array("i").itemsize)
            )
            values = array.array("i")
            values.frombytes(ancillary[0][2])
            fd = values[0]
            try:
                os.pread(fd, 1, 0)
            finally:
                os.close(fd)
        finally:
            left.close()
            right.close()
    elif operation in ("sendfile-read-fd", "copy-read-fd", "splice-read-fd"):
        with tempfile.TemporaryFile() as sink:
            if operation == "sendfile-read-fd":
                os.sendfile(sink.fileno(), held, 0, 1)
            elif operation == "copy-read-fd":
                os.copy_file_range(held, sink.fileno(), 1, offset_src=0)
            else:
                read_end, write_end = os.pipe()
                try:
                    os.splice(held, write_end, 1, offset_src=0)
                finally:
                    os.close(read_end)
                    os.close(write_end)
    elif operation in ("write", "create", "otrunc"):
        flags = os.O_WRONLY
        if operation == "create":
            flags |= os.O_CREAT
        elif operation == "otrunc":
            flags |= os.O_TRUNC
        fd = os.open(path, flags, 0o600)
        try:
            os.write(fd, b"x")
        finally:
            os.close(fd)
    elif operation == "write-fd":
        os.write(held, b"x")
    elif operation == "pwrite-fd":
        os.pwrite(held, b"x", 0)
    elif operation == "writev-fd":
        os.writev(held, [b"x"])
    elif operation == "pwritev-fd":
        os.pwritev(held, [b"x"], 0)
    elif operation == "truncate":
        os.truncate(path, 0)
    elif operation == "ftruncate-fd":
        os.ftruncate(held, 0)
    elif operation == "punch-fd":
        checked(libc.fallocate(held, 3, ctypes.c_long(0), ctypes.c_long(1)))
    elif operation == "copy-write-fd":
        with tempfile.TemporaryFile() as source:
            source.write(b"x")
            source.flush()
            os.copy_file_range(source.fileno(), held, 1, offset_src=0, offset_dst=0)
    elif operation.startswith(("mmap-", "holdmap-")):
        private = "private" in operation
        if "none" in operation:
            prot = 0
        elif "exec" in operation:
            prot = 4
        else:
            prot = 3 if "write" in operation else 1
        fd = (
            held
            if held is not None
            else os.open(path, os.O_RDONLY if prot < 2 else os.O_RDWR)
        )
        try:
            address = libc.mmap(None, 4096, prot, 2 if private else 1, fd, 0)
            if address == ctypes.c_void_p(-1).value:
                checked(-1)
            if operation.startswith("holdmap-"):
                mapping = address
            else:
                try:
                    if prot & 2:
                        ctypes.memset(address, ord("x"), 1)
                finally:
                    checked(libc.munmap(address, 4096))
        finally:
            if held is None:
                os.close(fd)
    elif operation in ("mprotect-read", "mprotect-write"):
        checked(libc.mprotect(mapping, 4096, 1 if operation == "mprotect-read" else 3))
    elif operation == "unmap":
        checked(libc.munmap(mapping, 4096))
        mapping = None
    elif operation == "delete":
        os.unlink(path)
    elif operation == "rmdir":
        os.rmdir(path)
    elif operation == "mkdir":
        os.mkdir(path)
    elif operation.startswith("link:"):
        os.link(path, operation.split(":", 1)[1])
    elif operation.startswith("rename:"):
        os.rename(path, operation.split(":", 1)[1])
    elif operation.startswith("exchange:"):
        destination = operation.split(":", 1)[1]
        checked(
            libc.renameat2(-100, os.fsencode(path), -100, os.fsencode(destination), 2)
        )
    elif operation.startswith("bind-mount:"):
        destination = operation.split(":", 1)[1]
        checked(
            libc.mount(os.fsencode(path), os.fsencode(destination), None, 4096, None)
        )
    elif operation == "exec":
        subprocess.run(
            [path], check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL
        )
    elif operation == "chmod":
        os.chmod(path, 0o600)
    elif operation == "drop-uid":
        os.setgroups([])
        os.setgid(65534)
        os.setuid(65534)
    elif operation == "unregister":
        fd = os.open("/sys/kernel/security/lua/unregister", os.O_WRONLY)
        try:
            os.write(fd, path.encode())
        finally:
            os.close(fd)
    elif operation in ("spawn-child", "spawn-child-map"):
        code = """
import mmap, os, sys
if sys.argv[1]:
    fd = os.open(sys.argv[1], os.O_RDONLY)
    retained_mapping = mmap.mmap(fd, 4096, flags=mmap.MAP_PRIVATE, prot=mmap.PROT_READ)
    os.close(fd)
for line in sys.stdin:
    try:
        os.close(os.open(line.rstrip('\\n'), os.O_RDONLY))
        print('ok', flush=True)
    except OSError as error:
        print('errno=' + str(error.errno), flush=True)
"""
        existing_child = subprocess.Popen(
            [
                sys.executable,
                "-u",
                "-c",
                code,
                path if operation == "spawn-child-map" else "",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        existing_child.stdin.write(path + "\n")
        existing_child.stdin.flush()
        assert existing_child.stdout.readline().strip() == "ok"
    elif operation == "existing-child-read":
        existing_child.stdin.write(path + "\n")
        existing_child.stdin.flush()
        response = existing_child.stdout.readline().strip()
        if response != "ok":
            raise OSError(int(response.split("=", 1)[1]), response)
    elif operation in ("exec-child-read", "setsid-child-read"):
        code = """
import os, sys
if sys.argv[2] == 'setsid-child-read': os.setsid()
try: os.close(os.open(sys.argv[1], os.O_RDONLY))
except OSError as error: sys.exit(error.errno)
"""
        result = subprocess.run(
            [sys.executable, "-c", code, path, operation], check=False
        )
        if result.returncode:
            raise OSError(result.returncode, "child read denied")
    elif operation == "thread-read":
        errors: list[OSError] = []

        def read_in_thread() -> None:
            try:
                os.close(os.open(path, os.O_RDONLY))
            except OSError as error:
                errors.append(error)

        thread = threading.Thread(target=read_in_thread)
        thread.start()
        thread.join()
        if errors:
            raise errors[0]
    else:
        raise ValueError(operation)


for line in sys.stdin:
    op, target = line.rstrip("\n").split(" ", 1)
    if op == "orphan":
        pid = os.fork()
        if pid:
            print("child=" + str(pid), flush=True)
            os._exit(0)
        continue
    try:
        if op == "mapping-identities":
            print(json.dumps(mapping_identities(target)), flush=True)
        else:
            execute(op, target)
            print("ok", flush=True)
    except OSError as error:
        print("errno=" + str(error.errno), flush=True)
