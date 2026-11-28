"""No PID-only or ambiguous instance association can become a verified Scope."""

import json
import os
from pathlib import Path
import sys
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from correlation import correlate, process_ref


class CorrelationTests(unittest.TestCase):
    def setUp(self):
        self.process = process_ref(os.getpid())
        self.event = {"schema_version": "aw.transport-observation-poc/v0", "process": self.process,
                      "observed_at_ns": str(time.monotonic_ns()), "aw_instance_id": None, "aw_scope_id": None}
        self.mapping = {"process": self.process, "aw_instance_id": "instance", "aw_scope_id": "scope",
                        "service_identity": {"generation": "fixture-generation"}, "valid_from_ns": "0",
                        "valid_until_ns": str(time.monotonic_ns() + 10**9)}

    def test_unknown_process_is_only_observed(self):
        value = correlate(self.event, [])
        self.assertIsNone(value["aw_scope_id"])
        self.assertEqual(value["association"], {"status": "unassociated", "process_state": "observed"})

    def test_unique_and_ambiguous_same_pid(self):
        self.assertEqual(correlate(self.event, [self.mapping])["aw_scope_id"], "scope")
        second = dict(self.mapping, aw_instance_id="second", aw_scope_id="other")
        value = correlate(self.event, [self.mapping, second])
        self.assertEqual(value["association"]["status"], "ambiguous_process_owner")
        self.assertIsNone(value["aw_instance_id"])

    def test_start_uid_namespace_boot_exe_reuse_rejected(self):
        for key in ("start_ticks", "uid", "pid_namespace", "boot_id", "exe_inode", "exe"):
            changed = dict(self.process, **{key: "changed"})
            value = correlate(self.event, [self.mapping], current=changed)
            self.assertEqual(value["association"]["status"], "stale_process_identity", key)
            self.assertIsNone(value["aw_scope_id"])

    def test_expired_mapping_and_future_event_do_not_correlate(self):
        expired = dict(self.mapping, valid_until_ns="0")
        self.assertIsNone(correlate(self.event, [expired])["aw_instance_id"])
        future = dict(self.event, observed_at_ns=str(time.monotonic_ns() + 10**12))
        self.assertIsNone(correlate(future, [self.mapping])["aw_instance_id"])


if __name__ == "__main__":
    unittest.main()
