"""Exercise health-advisory load accounting with an isolated fake CLI."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from typing import Any

COMPONENT = Path(__file__).resolve().parents[1]
METRICS_HELPER = COMPONENT / "tests/health-advisory-metrics.py"
LOAD_SCRIPT = COMPONENT / "tests/health-advisory-load.sh"


class HealthAdvisoryLoadTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="ws-ckpt-health-advisory-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspaces = self.root / "workspaces"
        self.out_dir = self.root / "out"
        self.workspaces.mkdir()
        self.out_dir.mkdir()
        self.failures_file = self.root / "failures.json"
        self.failures_file.write_text("[]\n", encoding="utf-8")
        self.interruptions_file = self.root / "interruptions.json"
        self.interruptions_file.write_text("[]\n", encoding="utf-8")
        self.block_file = self.root / "blocks.json"
        self.block_file.write_text("[]\n", encoding="utf-8")
        self.block_started = self.root / "block-started"
        self.extras_file = self.root / "extras.json"
        self.extras_file.write_text("[]\n", encoding="utf-8")
        self.kill_daemon_file = self.root / "kill-daemon.json"
        self.kill_daemon_file.write_text("[]\n", encoding="utf-8")
        self.calls_file = self.root / "calls.jsonl"
        self.cli = self.root / "fake-ws-ckpt"
        self.cli.write_text(FAKE_CLI, encoding="utf-8")
        self.cli.chmod(0o755)
        for client in (1, 2):
            workspace = self.workspaces / f"ws-u{client}"
            workspace.mkdir()
            state = [self.snapshot(f"baseline-u{client}-{index}") for index in range(3)]
            (workspace / "snapshots.json").write_text(json.dumps(state), encoding="utf-8")

    def env(self) -> dict[str, str]:
        return {
            **os.environ,
            "WS_ROOT": str(self.workspaces),
            "WS_CKPT_BIN": str(self.cli),
            "OUT_DIR": str(self.out_dir),
            "ITERATIONS": "3",
            "FAILURES_FILE": str(self.failures_file),
            "INTERRUPTIONS_FILE": str(self.interruptions_file),
            "BLOCK_FILE": str(self.block_file),
            "BLOCK_STARTED": str(self.block_started),
            "EXTRAS_FILE": str(self.extras_file),
            "KILL_DAEMON_FILE": str(self.kill_daemon_file),
            "CALLS_FILE": str(self.calls_file),
        }

    @staticmethod
    def snapshot(snapshot_id: str) -> dict[str, Any]:
        return {
            "id": snapshot_id,
            "workspace": "/fixture",
            "meta": {
                "message": "baseline",
                "metadata": None,
                "pinned": False,
                "created_at": "2026-10-24T00:00:00Z",
                "missing": False,
                "parent_id": None,
                "child_ids": [],
            },
        }

    def helper(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["python3", str(METRICS_HELPER), *args],
            env=self.env(),
            capture_output=True,
            text=True,
            check=check,
        )

    def snapshot_counts(self) -> list[int]:
        counts = []
        for client in (1, 2):
            state = json.loads(
                (self.workspaces / f"ws-u{client}" / "snapshots.json").read_text(encoding="utf-8")
            )
            counts.append(len(state))
        return counts

    @staticmethod
    def read_tsv(path: Path) -> list[dict[str, str]]:
        lines = path.read_text(encoding="utf-8").splitlines()
        header = lines[0].split("\t")
        return [dict(zip(header, line.split("\t"), strict=True)) for line in lines[1:]]

    def summary(
        self, interrupted: int = 0, worker_failures: int = 0
    ) -> subprocess.CompletedProcess[str]:
        timestamps: list[tuple[int, int]] = []
        for operation_file in sorted((self.out_dir / "operations").glob("client-*.tsv")):
            for row in self.read_tsv(operation_file):
                start_ns = int(row["start_ns"])
                timestamps.append((start_ns, start_ns + int(row["duration_ns"])))
        started_ns = min((start for start, _ in timestamps), default=1)
        ended_ns = max((end for _, end in timestamps), default=2)
        ended_ns = max(ended_ns, started_ns + 1)
        return self.helper(
            "summarize",
            "--out-dir",
            str(self.out_dir),
            "--clients",
            "2",
            "--iterations",
            "3",
            "--started-ns",
            str(started_ns),
            "--ended-ns",
            str(ended_ns),
            "--worker-failures",
            str(worker_failures),
            "--interrupted",
            str(interrupted),
            check=False,
        )

    def test_success_records_business_latency_and_restores_snapshots(self) -> None:
        before = self.snapshot_counts()
        for client in (1, 2):
            result = self.helper("worker", str(client), "testrun")
            self.assertEqual(result.returncode, 0, result.stderr)

        self.assertEqual(self.snapshot_counts(), before)
        self.assertEqual(list((self.out_dir / "active").glob("*.snapshot")), [])
        summary_result = self.summary()
        self.assertEqual(summary_result.returncode, 0, summary_result.stderr)
        summary = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(summary["status"], "valid")
        self.assertEqual(summary["attempts"], "18")
        self.assertEqual(summary["successes"], "18")
        self.assertEqual(summary["failures"], "0")
        self.assertEqual(summary["skipped"], "0")
        self.assertEqual(summary["completed_cycles"], "6")
        self.assertGreater(float(summary["ops_per_second"]), 0)
        latency = self.read_tsv(self.out_dir / "business-latency.tsv")
        self.assertEqual(len(latency), 9)
        self.assertEqual(
            {row["operation"]: row["count"] for row in latency if row["outcome"] == "success"},
            {"checkpoint": "6", "rollback": "6", "delete": "6"},
        )
        for client in (1, 2):
            operations = self.read_tsv(self.out_dir / f"operations/client-{client}.tsv")
            self.assertEqual(len(operations), 9)
            self.assertTrue(all(row["status"] == "success" for row in operations))

    def test_load_script_samples_a_live_daemon_and_preserves_snapshots(self) -> None:
        daemon = subprocess.Popen(["sleep", "60"])
        self.addCleanup(self.stop_process, daemon)
        before = self.snapshot_counts()
        result = subprocess.run(
            ["bash", str(LOAD_SCRIPT)],
            env={
                **self.env(),
                "DAEMON_PID": str(daemon.pid),
                "CLIENTS": "2",
                "ITERATIONS": "3",
                "EXPECTED_SNAPSHOTS": "3",
                "SAMPLE_INTERVAL_SECS": "1",
                "DRAIN_SECONDS": "1",
                "RUN_LABEL": "unittest",
            },
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.snapshot_counts(), before)
        summary = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(summary["status"], "valid")
        self.assertEqual(summary["attempts"], "18")
        self.assertEqual(summary["unresolved_mutations"], "0")

    def test_term_keeps_partial_summary_invalid(self) -> None:
        daemon = subprocess.Popen(["sleep", "60"])
        self.addCleanup(self.stop_process, daemon)
        self.block_file.write_text(
            json.dumps([{"workspace": "ws-u1", "operation": "checkpoint", "occurrence": 1}]),
            encoding="utf-8",
        )
        process = subprocess.Popen(
            ["bash", str(LOAD_SCRIPT)],
            env={
                **self.env(),
                "DAEMON_PID": str(daemon.pid),
                "CLIENTS": "2",
                "ITERATIONS": "3",
                "EXPECTED_SNAPSHOTS": "3",
                "SAMPLE_INTERVAL_SECS": "1",
                "DRAIN_SECONDS": "1",
                "RUN_LABEL": "term-test",
            },
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.stop_process, process)
        deadline = time.monotonic() + 15
        while (
            not self.block_started.exists()
            and process.poll() is None
            and time.monotonic() < deadline
        ):
            time.sleep(0.05)
        self.assertTrue(self.block_started.exists(), "worker did not reach its controlled CLI call")
        process.send_signal(signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=15)
        self.assertNotEqual(process.returncode, 0, stdout + stderr)
        summary = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(summary["status"], "invalid")
        self.assertEqual(summary["interrupted"], "1")
        self.assertGreater(int(summary["attempts"]), 0)
        self.assertGreater(int(summary["unresolved_mutations"]), 0)

    def test_term_during_settling_does_not_start_workers(self) -> None:
        daemon = subprocess.Popen(["sleep", "60"])
        self.addCleanup(self.stop_process, daemon)
        process = subprocess.Popen(
            ["bash", str(LOAD_SCRIPT)],
            env={
                **self.env(),
                "DAEMON_PID": str(daemon.pid),
                "CLIENTS": "2",
                "ITERATIONS": "3",
                "EXPECTED_SNAPSHOTS": "3",
                "SAMPLE_INTERVAL_SECS": "1",
                "DRAIN_SECONDS": "1",
                "RUN_LABEL": "settle-term-test",
            },
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.stop_process, process)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and process.poll() is None:
            if self.calls_file.exists():
                calls = [
                    json.loads(line)
                    for line in self.calls_file.read_text(encoding="utf-8").splitlines()
                ]
                if len([call for call in calls if call["operation"] == "list"]) == 2:
                    break
            time.sleep(0.01)
        self.assertTrue(self.calls_file.exists())
        process.send_signal(signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 143, stdout + stderr)
        calls = [
            json.loads(line) for line in self.calls_file.read_text(encoding="utf-8").splitlines()
        ]
        self.assertFalse(
            any(call["operation"] in {"checkpoint", "rollback", "delete"} for call in calls)
        )
        self.assertFalse(list((self.out_dir / "operations").glob("client-*.tsv")))
        summary = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(summary["status"], "invalid")
        self.assertEqual(summary["interrupted"], "1")

    def test_term_before_setsid_exec_reaps_wrapper_without_starting_work(self) -> None:
        daemon = subprocess.Popen(["sleep", "60"])
        self.addCleanup(self.stop_process, daemon)
        wrapper_dir = self.root / "wrapper-bin"
        wrapper_dir.mkdir()
        ready = self.root / "setsid-wrapper-ready"
        wrapper = wrapper_dir / "setsid"
        wrapper.write_text(
            "#!/usr/bin/python3\n"
            "import os, sys, time\n"
            "from pathlib import Path\n"
            "Path(os.environ['SETSID_WRAPPER_READY']).write_text('ready\\n', encoding='utf-8')\n"
            "time.sleep(30)\n"
            "os.execv('/usr/bin/setsid', ['/usr/bin/setsid', *sys.argv[1:]])\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)
        process = subprocess.Popen(
            ["bash", str(LOAD_SCRIPT)],
            env={
                **self.env(),
                "PATH": f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}",
                "SETSID_WRAPPER_READY": str(ready),
                "DAEMON_PID": str(daemon.pid),
                "CLIENTS": "2",
                "ITERATIONS": "3",
                "EXPECTED_SNAPSHOTS": "3",
                "SAMPLE_INTERVAL_SECS": "1",
                "DRAIN_SECONDS": "1",
                "RUN_LABEL": "setsid-race-test",
            },
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.stop_process, process)
        deadline = time.monotonic() + 15
        while not ready.exists() and process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertTrue(ready.exists(), "setsid wrapper did not reach its pre-exec delay")
        process.send_signal(signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=8)
        self.assertEqual(process.returncode, 143, stdout + stderr)
        calls = [
            json.loads(line) for line in self.calls_file.read_text(encoding="utf-8").splitlines()
        ]
        self.assertFalse(
            any(call["operation"] in {"checkpoint", "rollback", "delete"} for call in calls)
        )
        summary = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(summary["status"], "invalid")
        self.assertEqual(summary["interrupted"], "1")
        self.assertEqual(summary["attempts"], "0")
        self.assertFalse(list((self.out_dir / "operations").glob("client-*.tsv")))

    def test_term_escalates_when_cli_ignores_signal_and_reaps_child(self) -> None:
        ready = self.root / "ignoring-cli-ready"
        child_pid_file = self.root / "ignoring-cli.pid"
        stubborn_cli = self.root / "ignoring-ws-ckpt"
        stubborn_cli.write_text(
            "#!/usr/bin/python3\n"
            "import os, signal, time\n"
            "from pathlib import Path\n"
            "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
            "Path(os.environ['IGNORING_CLI_PID']).write_text(str(os.getpid()), encoding='utf-8')\n"
            "Path(os.environ['IGNORING_CLI_READY']).write_text('ready\\n', encoding='utf-8')\n"
            "while True:\n"
            "    signal.pause()\n",
            encoding="utf-8",
        )
        stubborn_cli.chmod(0o755)
        worker_env = {
            **self.env(),
            "WS_CKPT_BIN": str(stubborn_cli),
            "IGNORING_CLI_READY": str(ready),
            "IGNORING_CLI_PID": str(child_pid_file),
        }
        worker: subprocess.Popen[str] | None = None
        child_pid: int | None = None
        try:
            worker = subprocess.Popen(
                ["python3", str(METRICS_HELPER), "worker", "1", "ignoreterm"],
                env=worker_env,
                start_new_session=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            deadline = time.monotonic() + 5
            while not ready.exists() and worker.poll() is None and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(ready.exists(), "fake CLI did not start")
            child_pid = int(child_pid_file.read_text(encoding="utf-8"))
            started_at = time.monotonic()
            worker.send_signal(signal.SIGTERM)
            _, stderr = worker.communicate(timeout=7)
            elapsed = time.monotonic() - started_at
            self.assertNotEqual(worker.returncode, 0, stderr)
            self.assertLessEqual(elapsed, 7)
            operations = self.read_tsv(self.out_dir / "operations/client-1.tsv")
            self.assertEqual(len(operations), 1)
            self.assertEqual(operations[0]["status"], "unknown")
            marker = self.out_dir / "active/client-1.snapshot"
            self.assertEqual(
                marker.read_text(encoding="utf-8").splitlines(),
                ["haignoreterm-u1-n1", "unknown"],
            )
            self.assertFalse(Path(f"/proc/{child_pid}").exists())
        finally:
            if child_pid is None and child_pid_file.exists():
                child_pid = int(child_pid_file.read_text(encoding="utf-8"))
            if child_pid is not None and Path(f"/proc/{child_pid}").exists():
                try:
                    os.kill(child_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if worker is not None and worker.poll() is None:
                worker.send_signal(signal.SIGTERM)
                try:
                    worker.communicate(timeout=7)
                except subprocess.TimeoutExpired:
                    worker.kill()
                    worker.communicate(timeout=5)

    def test_empty_snapshot_array_counts_as_zero_and_is_rejected(self) -> None:
        for client in (1, 2):
            (self.workspaces / f"ws-u{client}/snapshots.json").write_text("[]\n", encoding="utf-8")
        daemon = subprocess.Popen(["sleep", "60"])
        self.addCleanup(self.stop_process, daemon)
        result = subprocess.run(
            ["bash", str(LOAD_SCRIPT)],
            env={
                **self.env(),
                "DAEMON_PID": str(daemon.pid),
                "CLIENTS": "2",
                "ITERATIONS": "1",
                "EXPECTED_SNAPSHOTS": "1",
                "SAMPLE_INTERVAL_SECS": "1",
                "DRAIN_SECONDS": "1",
            },
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("has 0 snapshots", result.stderr)
        self.assertEqual(
            list((self.out_dir / "snapshots-before").glob("*.ids")),
            [self.out_dir / "snapshots-before/client-1.ids"],
        )
        self.assertEqual(
            (self.out_dir / "snapshots-before/client-1.ids").read_text(encoding="utf-8"),
            "",
        )
        summary = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(summary["status"], "invalid")

    def test_missing_cli_is_not_counted_as_a_started_operation(self) -> None:
        self.cli.unlink()
        worker = self.helper("worker", "1", "noclibin", check=False)
        self.assertNotEqual(worker.returncode, 0)
        operation_rows = self.read_tsv(self.out_dir / "operations/client-1.tsv")
        self.assertEqual(operation_rows, [])
        self.assertFalse((self.out_dir / "active/client-1.snapshot").exists())
        summary = self.summary(worker_failures=1)
        self.assertNotEqual(summary.returncode, 0)
        metrics = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(metrics["status"], "invalid")
        self.assertEqual(metrics["attempts"], "0")
        self.assertEqual(metrics["skipped"], "18")
        self.assertEqual(metrics["launch_failures"], "1")

    def test_extra_snapshot_is_reported_in_extra_ids_column(self) -> None:
        self.extras_file.write_text(
            json.dumps([{"workspace": "ws-u1", "occurrence": 2, "id": "external-extra"}]),
            encoding="utf-8",
        )
        daemon = subprocess.Popen(["sleep", "60"])
        self.addCleanup(self.stop_process, daemon)
        result = subprocess.run(
            ["bash", str(LOAD_SCRIPT)],
            env={
                **self.env(),
                "DAEMON_PID": str(daemon.pid),
                "CLIENTS": "2",
                "ITERATIONS": "1",
                "EXPECTED_SNAPSHOTS": "3",
                "SAMPLE_INTERVAL_SECS": "1",
                "DRAIN_SECONDS": "1",
            },
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        diff = self.read_tsv(self.out_dir / "snapshot-diff.tsv")
        client_one = next(row for row in diff if row["client"] == "1")
        self.assertEqual(client_one["status"], "mismatch")
        self.assertEqual(client_one["missing_ids"], "")
        self.assertEqual(client_one["extra_ids"], "external-extra")
        summary = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(summary["status"], "valid")
        self.assertEqual(
            {row["metric"]: row["value"] for row in self.read_tsv(self.out_dir / "run-status.tsv")}[
                "run_status"
            ],
            "invalid",
        )

    def test_dead_daemon_marks_sampler_invalid(self) -> None:
        self.kill_daemon_file.write_text(
            json.dumps([{"workspace": "ws-u1", "operation": "checkpoint", "occurrence": 1}]),
            encoding="utf-8",
        )
        daemon = subprocess.Popen(["sleep", "60"])
        self.addCleanup(self.stop_process, daemon)
        result = subprocess.run(
            ["bash", str(LOAD_SCRIPT)],
            env={
                **self.env(),
                "DAEMON_PID": str(daemon.pid),
                "CLIENTS": "2",
                "ITERATIONS": "1",
                "EXPECTED_SNAPSHOTS": "3",
                "SAMPLE_INTERVAL_SECS": "1",
                "DRAIN_SECONDS": "1",
            },
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        status = {
            row["metric"]: row["value"] for row in self.read_tsv(self.out_dir / "run-status.tsv")
        }
        self.assertEqual(status["sampler_status"], "invalid")

    @staticmethod
    def stop_process(process: subprocess.Popen[Any]) -> None:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)

    def test_worker_failures_keep_partial_metrics_and_unknown_receipts(self) -> None:
        for operation in ("checkpoint", "rollback", "delete"):
            with self.subTest(operation=operation):
                self.setUp()
                self.addCleanup(self.temporary.cleanup)
                self.failures_file.write_text(
                    json.dumps([{"workspace": "ws-u1", "operation": operation, "occurrence": 2}]),
                    encoding="utf-8",
                )
                worker = self.helper("worker", "1", "failrun", check=False)
                self.assertNotEqual(worker.returncode, 0)
                cleanup = self.helper("cleanup", "1", check=False)
                self.assertNotEqual(cleanup.returncode, 0)
                marker = self.out_dir / "active/client-1.snapshot"
                self.assertTrue(marker.exists())
                self.assertEqual(
                    marker.read_text(encoding="utf-8").splitlines(),
                    ["hafailrun-u1-n2", "unknown"],
                )
                summary = self.summary(worker_failures=1)
                self.assertNotEqual(summary.returncode, 0)
                metrics = {
                    row["metric"]: row["value"]
                    for row in self.read_tsv(self.out_dir / "business-summary.tsv")
                }
                self.assertEqual(metrics["status"], "invalid")
                self.assertEqual(metrics["worker_failures"], "1")
                self.assertGreater(int(metrics["failures"]), 0)
                self.assertGreater(int(metrics["skipped"]), 0)
                self.assertEqual(metrics["unresolved_mutations"], "1")
                operation_rows = self.read_tsv(self.out_dir / "operations/client-1.tsv")
                failed = [
                    row
                    for row in operation_rows
                    if row["operation"] == operation and row["status"] == "failure"
                ]
                self.assertEqual(len(failed), 1)
                self.assertNotEqual(failed[0]["exit_code"], "0")

    def test_cleanup_keeps_pending_receipt_without_inspecting_snapshot(self) -> None:
        for snapshot_present in (False, True):
            with self.subTest(snapshot_present=snapshot_present):
                self.setUp()
                self.addCleanup(self.temporary.cleanup)
                active = self.out_dir / "active"
                active.mkdir()
                marker = active / "client-1.snapshot"
                snapshot_id = "hapendingrun-u1-n1"
                marker.write_text(f"{snapshot_id}\npending\n", encoding="utf-8")
                state_path = self.workspaces / "ws-u1/snapshots.json"
                state = json.loads(state_path.read_text(encoding="utf-8"))
                if snapshot_present:
                    state.append(self.snapshot(snapshot_id))
                    state_path.write_text(json.dumps(state), encoding="utf-8")
                result = self.helper("cleanup", "1", check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(marker.exists())
                self.assertIn("pending", (result.stdout + result.stderr).lower())
                self.assertFalse(self.calls_file.exists())
                current = json.loads(state_path.read_text(encoding="utf-8"))
                self.assertEqual(snapshot_id in {item["id"] for item in current}, snapshot_present)

    def test_legacy_single_line_marker_is_not_assumed_owned(self) -> None:
        active = self.out_dir / "active"
        active.mkdir()
        marker = active / "client-1.snapshot"
        marker.write_text("halegacy-u1-n1\n", encoding="utf-8")
        result = self.helper("cleanup", "1", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(marker.exists())
        self.assertFalse(self.calls_file.exists())

    def test_cleanup_reconciles_successful_delete_before_marker_removal(self) -> None:
        active = self.out_dir / "active"
        active.mkdir()
        snapshot_id = "hatest-u1-n1"
        marker = active / "client-1.snapshot"
        marker.write_text(f"{snapshot_id}\nowned\n", encoding="utf-8")
        result = self.helper("cleanup", "1", check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(marker.exists())
        self.assertFalse((self.out_dir / "recovery.tsv").exists())
        calls = [
            json.loads(line) for line in self.calls_file.read_text(encoding="utf-8").splitlines()
        ]
        self.assertEqual([call["operation"] for call in calls], ["list"])

    def test_in_flight_signal_preserves_unknown_receipt_for_reconciliation(self) -> None:
        self.interruptions_file.write_text(
            json.dumps([{"workspace": "ws-u1", "operation": "checkpoint", "occurrence": 1}]),
            encoding="utf-8",
        )
        worker = self.helper("worker", "1", "signalrun", check=False)
        self.assertNotEqual(worker.returncode, 0)
        operations = self.read_tsv(self.out_dir / "operations/client-1.tsv")
        self.assertEqual(len(operations), 1)
        self.assertEqual(operations[0]["status"], "unknown")
        marker = self.out_dir / "active/client-1.snapshot"
        self.assertEqual(
            marker.read_text(encoding="utf-8").splitlines(),
            ["hasignalrun-u1-n1", "unknown"],
        )
        cleanup = self.helper("cleanup", "1", check=False)
        self.assertNotEqual(cleanup.returncode, 0)
        self.assertTrue(marker.exists())
        self.assertIn("unknown", (cleanup.stdout + cleanup.stderr).lower())
        workspace_state = json.loads(
            (self.workspaces / "ws-u1/snapshots.json").read_text(encoding="utf-8")
        )
        self.assertIn("hasignalrun-u1-n1", {item["id"] for item in workspace_state})
        summary = self.summary(worker_failures=1)
        self.assertNotEqual(summary.returncode, 0)
        values = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(values["status"], "invalid")
        self.assertEqual(values["unknown_attempts"], "1")
        self.assertEqual(values["failures"], "0")

    def test_incomplete_and_interrupted_summaries_are_invalid(self) -> None:
        operations_dir = self.out_dir / "operations"
        operations_dir.mkdir()
        operation_file = operations_dir / "client-1.tsv"
        operation_file.write_text(
            "iteration\toperation\tstart_ns\tduration_ns\texit_code\tstatus\n"
            "1\tcheckpoint\t100\t10\t0\tsuccess\n",
            encoding="utf-8",
        )
        incomplete = self.summary()
        self.assertNotEqual(incomplete.returncode, 0)
        values = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(values["status"], "invalid")
        self.assertEqual(values["attempts"], "1")
        self.assertEqual(values["skipped"], "17")
        interrupted = self.summary(interrupted=1)
        self.assertNotEqual(interrupted.returncode, 0)
        values = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(values["status"], "invalid")
        self.assertEqual(values["interrupted"], "1")

    def test_operation_outside_measurement_window_invalidates_summary(self) -> None:
        operations_dir = self.out_dir / "operations"
        operations_dir.mkdir()
        (operations_dir / "client-1.tsv").write_text(
            "iteration\toperation\tstart_ns\tduration_ns\texit_code\tstatus\n"
            "1\tcheckpoint\t1000\t100\t0\tsuccess\n",
            encoding="utf-8",
        )
        result = self.helper(
            "summarize",
            "--out-dir",
            str(self.out_dir),
            "--clients",
            "2",
            "--iterations",
            "3",
            "--started-ns",
            "900",
            "--ended-ns",
            "1050",
            "--worker-failures",
            "0",
            "--interrupted",
            "0",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        values = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertEqual(values["status"], "invalid")
        self.assertIn("window", values["invalid_reasons"])

    def test_cli_exit_status_must_match_operation_outcome(self) -> None:
        operations_dir = self.out_dir / "operations"
        operations_dir.mkdir()
        (operations_dir / "client-1.tsv").write_text(
            "iteration\toperation\tstart_ns\tduration_ns\texit_code\tstatus\n"
            "1\tcheckpoint\t1000\t100\t17\tsuccess\n",
            encoding="utf-8",
        )
        result = self.helper(
            "summarize",
            "--out-dir",
            str(self.out_dir),
            "--clients",
            "2",
            "--iterations",
            "3",
            "--started-ns",
            "900",
            "--ended-ns",
            "1200",
            "--worker-failures",
            "0",
            "--interrupted",
            "0",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        values = {
            row["metric"]: row["value"]
            for row in self.read_tsv(self.out_dir / "business-summary.tsv")
        }
        self.assertIn("inconsistent", values["invalid_reasons"])


FAKE_CLI = r"""#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

args = sys.argv[1:]
command = args[0]
workspace = Path(args[args.index("--workspace") + 1])
state_path = workspace / "snapshots.json"
state = json.loads(state_path.read_text(encoding="utf-8"))
operation = command
failure_rules = json.loads(Path(os.environ["FAILURES_FILE"]).read_text(encoding="utf-8"))
interruption_rules = json.loads(Path(os.environ["INTERRUPTIONS_FILE"]).read_text(encoding="utf-8"))
block_rules = json.loads(Path(os.environ["BLOCK_FILE"]).read_text(encoding="utf-8"))
extra_rules = json.loads(Path(os.environ["EXTRAS_FILE"]).read_text(encoding="utf-8"))
kill_daemon_rules = json.loads(Path(os.environ["KILL_DAEMON_FILE"]).read_text(encoding="utf-8"))
calls_path = Path(os.environ["CALLS_FILE"])
workspace_key = workspace.name
prior = [
    json.loads(line)
    for line in calls_path.read_text(encoding="utf-8").splitlines()
] if calls_path.exists() else []
occurrence = sum(1 for item in prior if item["workspace"] == workspace_key and item["operation"] == operation) + 1
fail = any(
    rule["workspace"] == workspace_key
    and rule["operation"] == operation
    and rule["occurrence"] == occurrence
    for rule in failure_rules
)
snapshot_id = None
if "--snapshot" in args:
    snapshot_id = args[args.index("--snapshot") + 1]
record = {"workspace": workspace_key, "operation": operation, "occurrence": occurrence, "snapshot": snapshot_id}
with calls_path.open("a", encoding="utf-8") as calls:
    calls.write(json.dumps(record) + "\n")
if fail:
    print(f"injected {operation} failure", file=sys.stderr)
    raise SystemExit(17)
if any(
    rule["workspace"] == workspace_key
    and rule["operation"] == operation
    and rule["occurrence"] == occurrence
    for rule in block_rules
):
    Path(os.environ["BLOCK_STARTED"]).write_text("started\n", encoding="utf-8")
    while True:
        import time
        time.sleep(1)
if any(
    rule["workspace"] == workspace_key
    and rule["operation"] == operation
    and rule["occurrence"] == occurrence
    for rule in kill_daemon_rules
):
    os.kill(int(os.environ["DAEMON_PID"]), 15)

if command == "list":
    for rule in extra_rules:
        if rule["workspace"] == workspace_key and occurrence == rule["occurrence"]:
            state.append({
                "id": rule["id"],
                "workspace": str(workspace),
                "meta": {"message": "external", "metadata": None, "pinned": False,
                         "created_at": "2026-10-24T00:00:00Z", "missing": False,
                         "parent_id": None, "child_ids": []},
            })
            state_path.write_text(json.dumps(state), encoding="utf-8")
    print(json.dumps(state))
elif command == "checkpoint":
    state.append({
        "id": snapshot_id,
        "workspace": str(workspace),
        "meta": {"message": "load", "metadata": None, "pinned": False,
                 "created_at": "2026-10-24T00:00:00Z", "missing": False,
                 "parent_id": None, "child_ids": []},
    })
    state_path.write_text(json.dumps(state), encoding="utf-8")
    interrupt = any(
        rule["workspace"] == workspace_key
        and rule["operation"] == operation
        and rule["occurrence"] == occurrence
        for rule in interruption_rules
    )
    if interrupt:
        os.kill(os.getpid(), 15)
elif command == "rollback":
    if "--num-ancestors" not in args or args[args.index("--num-ancestors") + 1] != "1":
        raise SystemExit("unexpected rollback arguments")
elif command == "delete":
    state = [item for item in state if item["id"] != snapshot_id]
    state_path.write_text(json.dumps(state), encoding="utf-8")
else:
    raise SystemExit(f"unsupported command: {command}")
"""


if __name__ == "__main__":
    unittest.main()
