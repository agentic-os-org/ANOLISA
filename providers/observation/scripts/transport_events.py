"""Bounded transport metadata projection; no payload bytes are persisted."""

import ctypes
import hashlib
import json
import os
from pathlib import Path
import time

SCHEMA = "aw.transport-observation-poc/v0"
STAT_NAMES = ("hits", "submitted", "ring_full", "over_limit", "read_failed", "invalid_length")


class Entry(ctypes.Structure):
    _fields_ = (
        [(name, ctypes.c_uint64) for name in ("timestamp_ns", "sequence", "length")]
        + [
            (name, ctypes.c_uint32)
            for name in ("pid", "tid", "function", "captured", "status", "reserved")
        ]
        + [("payload", ctypes.c_ubyte * 4096)]
    )


def project(entry: Entry, process: dict) -> dict:
    if entry.pid != process["pid"] or entry.function not in (0, 1) or entry.status not in range(4):
        raise ValueError("invalid_kernel_record")
    if entry.captured > 4096 or (entry.status == 0 and entry.captured != entry.length):
        raise ValueError("invalid_capture_length")
    if entry.status and entry.captured:
        raise ValueError("failed_capture_has_payload")
    reason = (
        "transport_write_only",
        "payload_over_limit",
        "payload_read_failed",
        "invalid_length",
    )[entry.status]
    digest = (
        hashlib.sha256(bytes(entry.payload[: entry.captured])).hexdigest()
        if not entry.status
        else None
    )
    return {
        "schema_version": SCHEMA,
        "kind": "transport.before_send",
        "source": "bpf.uprobe.ssl_write",
        "process": process,
        "thread_id": entry.tid,
        "sequence": str(entry.sequence),
        "aw_instance_id": None,
        "aw_scope_id": None,
        "association": {"status": "unassociated", "process_state": "observed"},
        "observed_at_ns": str(entry.timestamp_ns),
        "clock_domain": "linux.CLOCK_MONOTONIC",
        "transport": {
            "function": "SSL_write_ex" if entry.function else "SSL_write",
            "length_bytes": entry.length if entry.status != 3 else None,
            "payload_sha256": digest,
            "hashed_bytes": entry.captured,
            "adoption": "unknown",
            "snapshot_consistency": "non_atomic_user_memory_snapshot",
        },
        "coverage": {
            "status": "unknown",
            "reason": reason,
            "loss_status": "see_source_summary",
            "application_boundary": "unknown",
        },
    }


class Sink:
    def __init__(self, path: Path, max_events: int, rate: int) -> None:
        self.file = os.fdopen(
            os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600),
            "wb",
            buffering=0,
        )
        self.max_events, self.rate = max_events, rate
        self.emitted = self.output_dropped = self.data_bytes = self.in_window = 0
        self.window = time.monotonic()

    def write(self, data: bytes) -> None:
        remaining = memoryview(data)
        while remaining:
            count = self.file.write(remaining)
            if count is None or count <= 0:
                raise OSError("observation_output_write_failed")
            remaining = remaining[count:]

    def control(self, record: dict) -> None:
        data = json.dumps(record, separators=(",", ":")).encode() + b"\n"
        if len(data) > 8192:
            raise ValueError("record_limit")
        self.write(data)

    def event(self, record: dict) -> None:
        now = time.monotonic()
        if now - self.window >= 1:
            self.window, self.in_window = now, 0
        data = json.dumps(record, separators=(",", ":")).encode() + b"\n"
        if len(data) > 8192:
            raise ValueError("record_limit")
        if (
            self.emitted >= self.max_events
            or self.in_window >= self.rate
            or self.data_bytes + len(data) > 64 * 1024 * 1024
        ):
            self.output_dropped += 1
            return
        self.write(data)
        self.emitted += 1
        self.in_window += 1
        self.data_bytes += len(data)

    def close(self) -> None:
        self.file.close()
