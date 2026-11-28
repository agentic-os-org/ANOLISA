"""Digest, coverage, output bounds and malformed-record regression checks."""

import ctypes
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from transport_events import Entry, Sink, project


class TransportTests(unittest.TestCase):
    def entry(self, payload: bytes) -> Entry:
        entry = Entry(
            pid=10, tid=11, timestamp_ns=100, sequence=1, length=len(payload), captured=len(payload)
        )
        entry.payload[: len(payload)] = payload
        return entry

    def test_kernel_layout_and_digest_only_projection(self) -> None:
        self.assertEqual(ctypes.sizeof(Entry), 4144)
        payload = b"synthetic-private-body"
        event = project(self.entry(payload), {"pid": 10})
        self.assertNotIn(payload.decode(), json.dumps(event))
        self.assertEqual(event["transport"]["payload_sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(event["transport"]["adoption"], "unknown")
        self.assertEqual(event["coverage"]["status"], "unknown")
        self.assertEqual(event["association"]["process_state"], "observed")
        self.assertIsNone(event["aw_scope_id"])

    def test_empty_maximum_oversize_and_read_failure(self) -> None:
        for payload in (b"", b"X" * 4096):
            event = project(self.entry(payload), {"pid": 10})
            self.assertEqual(event["transport"]["hashed_bytes"], len(payload))
        for status, reason in (
            (1, "payload_over_limit"),
            (2, "payload_read_failed"),
            (3, "invalid_length"),
        ):
            entry = self.entry(b"")
            entry.status, entry.length = status, 4097
            event = project(entry, {"pid": 10})
            self.assertIsNone(event["transport"]["payload_sha256"])
            self.assertEqual(event["coverage"]["reason"], reason)

    def test_malformed_or_wrong_process_is_rejected(self) -> None:
        for field, value in (
            ("pid", 12),
            ("function", 3),
            ("status", 7),
            ("captured", 4097),
            ("length", 8),
        ):
            entry = self.entry(b"abc")
            setattr(entry, field, value)
            with self.assertRaises(ValueError):
                project(entry, {"pid": 10})

    def test_quantity_rate_and_private_exclusive_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            sink = Sink(path, 1, 100)
            try:
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                event = project(self.entry(b"abc"), {"pid": 10})
                sink.event(event)
                sink.event(event)
                self.assertEqual((sink.emitted, sink.output_dropped), (1, 1))
                with self.assertRaises(FileExistsError):
                    Sink(path, 1, 1)
            finally:
                sink.close()
            sink = Sink(Path(directory) / "rate.jsonl", 100, 1)
            try:
                sink.event(event)
                sink.event(event)
                self.assertEqual(sink.output_dropped, 1)
            finally:
                sink.close()

    def test_file_budget_drops_before_exceeding_limit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sink = Sink(Path(directory) / "bounded.jsonl", 100, 100)
            try:
                sink.data_bytes = 64 * 1024 * 1024
                sink.event(project(self.entry(b"a"), {"pid": 10}))
                self.assertEqual(sink.emitted, 0)
                self.assertEqual(sink.output_dropped, 1)
            finally:
                sink.close()


if __name__ == "__main__":
    unittest.main()
