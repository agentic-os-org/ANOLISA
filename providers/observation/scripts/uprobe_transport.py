"""Fixture-only BPF entry snapshots and bounded metadata, separate from AW RPC."""

import argparse
import ctypes
import os
from pathlib import Path
import platform
import resource
import select
import signal
import time

from correlation import process_ref
from transport_events import Entry, SCHEMA, STAT_NAMES, Sink, project
from uprobe_counter import broker, drop_privileges, fingerprint, load_permit, receive_link, resolve

STOPPING = False
CALLBACK = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t)


def stop_signal(_number: int, _frame: object) -> None:
    global STOPPING
    STOPPING = True


def open_transport(options: argparse.Namespace, uid: int) -> tuple[list[int], dict]:
    process = load_permit(options.permit, uid)
    pidfd = os.pidfd_open(process["pid"])
    descriptors = (ctypes.c_int * 3)(-1, -1, -1)
    try:
        path, offset, digest = resolve(process["pid"], options.function)
        if select.select([pidfd], [], [], 0)[0] or process_ref(process["pid"]) != process:
            raise ValueError("stale_process_identity")
        loader = ctypes.CDLL(str(options.library), use_errno=True)
        loader.transport_open.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint64,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.POINTER(ctypes.c_int),
        ]
        if loader.transport_open(
            process["pid"],
            os.fsencode(path),
            offset,
            os.fsencode(options.object),
            int(options.function == "SSL_write_ex"),
            descriptors,
        ):
            raise OSError(ctypes.get_errno(), "transport_bpf_setup_failed")
        if process_ref(process["pid"]) != process or fingerprint(path) != digest:
            raise ValueError("identity_changed_during_attach")
        return list(descriptors), {
            "library_sha256": digest,
            "symbol_file_offset": offset,
            "bpf_object_sha256": fingerprint(options.object),
        }
    except BaseException:
        for fd in descriptors:
            if fd >= 0:
                os.close(fd)
        raise
    finally:
        os.close(pidfd)


def libbpf() -> ctypes.CDLL:
    library = ctypes.CDLL("libbpf.so.1", use_errno=True)
    library.ring_buffer__new.argtypes = [ctypes.c_int, CALLBACK, ctypes.c_void_p, ctypes.c_void_p]
    library.ring_buffer__new.restype = ctypes.c_void_p
    library.ring_buffer__poll.argtypes = [ctypes.c_void_p, ctypes.c_int]
    library.ring_buffer__free.argtypes = [ctypes.c_void_p]
    library.bpf_map_lookup_elem.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p]
    return library


def collect(options: argparse.Namespace) -> None:
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("unsupported_platform")
    if os.geteuid() == 0:
        raise ValueError("root_collection_forbidden")
    process = load_permit(options.permit, os.getuid())
    pidfd = os.pidfd_open(process["pid"])
    descriptors = []
    ring = None
    sink = None
    backend = libbpf()
    errors = []
    received = identity_dropped = 0
    identity_lost = False
    reason = "duration_elapsed"
    try:
        sink = Sink(options.output, options.max_events, options.rate)
        descriptors, metadata = receive_link(
            options,
            opener=open_transport,
            program=Path(__file__).resolve(),
            extras=("--object", str(options.object)),
            fd_count=3,
        )
        drop_privileges(os.getuid(), os.getgid())
        if process_ref(process["pid"]) != process:
            raise ValueError("stale_process_identity")

        @CALLBACK
        def consume(_context: int, data: int, size: int) -> int:
            nonlocal received, identity_dropped, identity_lost
            try:
                if size != ctypes.sizeof(Entry):
                    raise ValueError("invalid_kernel_record_size")
                received += 1
                try:
                    valid = not identity_lost and process_ref(process["pid"]) == process
                except (OSError, ValueError):
                    valid = False
                if not valid:
                    identity_lost = True
                    identity_dropped += 1
                    return 0
                event = project(Entry.from_address(data), process)
                sink.event(event)
                return 0
            except (OSError, ValueError) as error:
                if not errors:
                    errors.append(str(error)[:256])
                return -1

        ring = backend.ring_buffer__new(descriptors[1], consume, None, None)
        if not ring:
            raise OSError(ctypes.get_errno(), "ring_buffer_setup_failed")
        sink.control(
            {
                "schema_version": SCHEMA,
                "kind": "source_status",
                "state": "observing",
                "source": "bpf.uprobe.ssl_write",
                "collector_pid": os.getpid(),
                "collector_uid": os.geteuid(),
                "capabilities": "none",
                "process": process,
                "coverage": {"status": "unknown", "reason": "transport_write_only"},
                "ring_bytes": 1048576,
                "max_capture_bytes": 4096,
                **metadata,
            }
        )
        deadline = time.monotonic() + options.duration
        while not STOPPING and time.monotonic() < deadline:
            if select.select([pidfd], [], [], 0)[0]:
                reason = "target_exited"
                break
            try:
                valid = process_ref(process["pid"]) == process
            except (OSError, ValueError):
                valid = False
            if not valid or identity_lost:
                identity_lost = True
                reason = (
                    "target_exited" if select.select([pidfd], [], [], 0)[0] else "identity_changed"
                )
                break
            result = backend.ring_buffer__poll(ring, 20)
            if errors or result < 0 and result != -4:
                raise ValueError(errors[0] if errors else "ring_buffer_poll_failed")
        if STOPPING:
            reason = "signal"
        # Unregister this link before draining so final accounting has a closed interval.
        os.close(descriptors.pop())
        for _ in range(100):
            result = backend.ring_buffer__poll(ring, 0)
            if errors or result < 0:
                raise ValueError(errors[0] if errors else "ring_buffer_drain_failed")
            if result == 0:
                break
        else:
            raise ValueError("ring_buffer_drain_deadline")
        values = (ctypes.c_uint64 * len(STAT_NAMES))()
        key = ctypes.c_uint32(0)
        if backend.bpf_map_lookup_elem(descriptors[0], ctypes.byref(key), ctypes.byref(values)):
            raise OSError(ctypes.get_errno(), "stats_lookup_failed")
        stats = dict(zip(STAT_NAMES, values))
        balanced = (
            stats["hits"] == stats["submitted"] + stats["ring_full"]
            and stats["submitted"] == received
            and received == sink.emitted + sink.output_dropped + identity_dropped
        )
        # /proc/exe can disappear before the last thread makes pidfd readable.
        # Classify that bounded exit transition after unregistering the link.
        if reason == "identity_changed" and select.select([pidfd], [], [], 0.1)[0]:
            reason = "target_exited"
        sink.control(
            {
                "schema_version": SCHEMA,
                "kind": "source_summary",
                "reason": reason,
                **stats,
                "received": received,
                "identity_dropped": identity_dropped,
                "emitted": sink.emitted,
                "output_dropped": sink.output_dropped,
                "accounting_complete": balanced,
                "coverage": "unknown_transport_write_only",
            }
        )
        if not balanced:
            raise ValueError("incomplete_capture_accounting")
    finally:
        if descriptors and len(descriptors) == 3:
            os.close(descriptors.pop())
        if ring:
            backend.ring_buffer__free(ring)
        for fd in descriptors:
            os.close(fd)
        os.close(pidfd)
        if sink:
            sink.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--permit", type=Path, required=True)
    parser.add_argument("--library", type=Path, required=True)
    parser.add_argument("--object", type=Path, required=True)
    parser.add_argument("--function", choices=("SSL_write", "SSL_write_ex"), default="SSL_write")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--duration", type=float, default=10)
    parser.add_argument("--max-events", type=int, default=4096)
    parser.add_argument("--rate", type=int, default=10000)
    parser.add_argument("--privileged-fixture-setup", action="store_true")
    parser.add_argument("--broker", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if (
        not 0 < args.duration <= 30
        or not 1 <= args.max_events <= 65536
        or not 1 <= args.rate <= 100000
    ):
        parser.error("duration/event/rate outside experiment bounds")
    if not args.broker and args.output is None:
        parser.error("--output is required for collection")
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    signal.signal(signal.SIGINT, stop_signal)
    signal.signal(signal.SIGTERM, stop_signal)
    try:
        if args.broker:
            broker(args, opener=open_transport)
        else:
            collect(args)
    except (OSError, ValueError, KeyError) as failure:
        parser.exit(1, f"observation_unavailable: {str(failure)[:512]}\n")
