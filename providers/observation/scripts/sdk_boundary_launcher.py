"""Launch owned fixtures with inherited syscall policy; no SDK callbacks are used."""

import argparse
import array
import ctypes as c
import errno
import json
import os
from pathlib import Path
import select
import signal
import socket
import sys
import time


class Compare(c.Structure):
    _fields_ = [("arg", c.c_uint), ("op", c.c_int), ("mask", c.c_uint64), ("value", c.c_uint64)]


class Data(c.Structure):
    _fields_ = [("nr", c.c_int), ("arch", c.c_uint32), ("ip", c.c_uint64), ("args", c.c_uint64 * 6)]


class Request(c.Structure):
    _fields_ = [("id", c.c_uint64), ("pid", c.c_uint32), ("flags", c.c_uint32), ("data", Data)]


class Response(c.Structure):
    _fields_ = [
        ("id", c.c_uint64),
        ("value", c.c_int64),
        ("error", c.c_int32),
        ("flags", c.c_uint32),
    ]


def library() -> c.CDLL:
    lib = c.CDLL("libseccomp.so.2", use_errno=True)
    lib.seccomp_init.argtypes = [c.c_uint32]
    lib.seccomp_init.restype = c.c_void_p
    lib.seccomp_rule_add_array.argtypes = [
        c.c_void_p,
        c.c_uint32,
        c.c_int,
        c.c_uint,
        c.POINTER(Compare),
    ]
    lib.seccomp_load.argtypes = [c.c_void_p]
    lib.seccomp_notify_fd.argtypes = [c.c_void_p]
    lib.seccomp_release.argtypes = [c.c_void_p]
    lib.seccomp_syscall_resolve_name.argtypes = [c.c_char_p]
    return lib


def checked(result: int) -> int:
    if result < 0:
        raise OSError(-result, "seccomp operation failed")
    return result


def install(lib: c.CDLL) -> int:
    context = lib.seccomp_init(0x7FFF0000)
    if not context:
        raise OSError("seccomp allocation failed")
    try:
        for syscall in (b"execve", b"execveat"):
            number = checked(lib.seccomp_syscall_resolve_name(syscall))
            checked(lib.seccomp_rule_add_array(context, 0x7FC00000, number, 0, None))
        for syscall, arg in ((b"open", 1), (b"openat", 2)):
            number = checked(lib.seccomp_syscall_resolve_name(syscall))
            for flag in (os.O_WRONLY, os.O_RDWR, os.O_CREAT, os.O_TRUNC):
                compare = Compare(arg, 7, flag, flag)
                checked(
                    lib.seccomp_rule_add_array(
                        context, 0x50000 | errno.EPERM, number, 1, c.byref(compare)
                    )
                )
        for syscall in (b"creat", b"openat2"):
            number = checked(lib.seccomp_syscall_resolve_name(syscall))
            checked(lib.seccomp_rule_add_array(context, 0x50000 | errno.EPERM, number, 0, None))
        checked(lib.seccomp_load(context))
        return os.dup(checked(lib.seccomp_notify_fd(context)))
    finally:
        lib.seccomp_release(context)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--deny", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or not Path(command[0]).is_absolute():
        parser.error("an absolute executable is required")
    if not args.deny:
        os.execv(command[0], command)
    lib = library()
    allocated_request = c.POINTER(Request)()
    allocated_response = c.POINTER(Response)()
    checked(lib.seccomp_notify_alloc(c.byref(allocated_request), c.byref(allocated_response)))
    parent, child = socket.socketpair()
    supervisor = os.getpid()
    pid = os.fork()
    if pid == 0:
        parent.close()
        os.setsid()
        # Kill only this owned child if its supervisor disappears.
        libc = c.CDLL(None, use_errno=True)
        if libc.prctl(1, signal.SIGKILL, 0, 0, 0) != 0 or os.getppid() != supervisor:
            os._exit(125)
        try:
            listener = install(lib)
            child.sendmsg(
                [b"ready"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [listener]))]
            )
            os.close(listener)
            child.close()
            os.execv(command[0], command)
        except BaseException:
            os._exit(125)
    child.close()
    listener = -1
    denied = 0
    admitted = False
    status = None
    try:
        parent.settimeout(5)
        _, controls, _, _ = parent.recvmsg(16, socket.CMSG_SPACE(4))
        descriptors = array.array("i")
        for level, kind, data in controls:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                descriptors.frombytes(data[: len(data) - len(data) % descriptors.itemsize])
        if len(descriptors) != 1:
            raise RuntimeError("fixture did not transfer one seccomp listener")
        listener = descriptors[0]
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            exited, candidate = os.waitpid(pid, os.WNOHANG)
            if exited:
                status = candidate
                break
            if not select.select([listener], [], [], 0.05)[0]:
                continue
            c.memset(allocated_request, 0, c.sizeof(Request))
            result = lib.seccomp_notify_receive(listener, allocated_request)
            if result in (-errno.ENOENT, -errno.EINTR):
                continue
            checked(result)
            request = allocated_request.contents
            initial = not admitted and request.pid == pid
            response = Response(request.id, 0, 0 if initial else -errno.EPERM, 1 if initial else 0)
            checked(lib.seccomp_notify_respond(listener, c.byref(response)))
            if initial:
                admitted = True
            else:
                denied += 1
        if status is None:
            raise TimeoutError("fixture exceeded its 20 second execution budget")
        print(
            json.dumps(
                {"os_policy": "seccomp", "initial_exec_allowed": admitted, "exec_denied": denied}
            ),
            file=sys.stderr,
        )
        return os.waitstatus_to_exitcode(status)
    finally:
        lib.seccomp_notify_free(allocated_request, allocated_response)
        parent.close()
        if listener >= 0:
            os.close(listener)
        if status is None:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)


if __name__ == "__main__":
    sys.exit(main())
