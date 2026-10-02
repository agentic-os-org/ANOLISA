"""Unit tests for security_middleware.backends.hardening."""

import subprocess
import unittest
from unittest.mock import patch

from agent_sec_cli.security_middleware.backends.hardening import (
    _MISSING_LOONGSHIELD_ERROR,
    HardeningBackend,
    _strip_ansi,
)
from agent_sec_cli.security_middleware.context import RequestContext

LOONGSHIELD_ALL_PASS = """\
\x1b[32m[INFO  10:07:54]\x1b[0m engine.lua:150: [1.1.1] PASS: Ensure mounting of cramfs is disabled
\x1b[32m[INFO  10:07:54]\x1b[0m engine.lua:150: [1.1.2] PASS: Ensure mounting of squashfs is disabled
\x1b[32m[INFO  10:08:01]\x1b[0m engine.lua:292: SEHarden Finished. 23 passed, 0 fixed, 0 failed, 0 manual, 0 dry-run-pending / 23 total.
"""

LOONGSHIELD_WITH_FAILURES = """\
\x1b[32m[INFO  14:30:00]\x1b[0m engine.lua:150: [1.1.1] PASS: Ensure cramfs disabled
\x1b[33m[WARN  14:30:01]\x1b[0m engine.lua:186: [fs.udf_disabled] FAIL: Ensure mounting of udf is disabled
\x1b[33m[WARN  14:30:02]\x1b[0m engine.lua:186: [time.sync_enabled] FAIL: Ensure time sync is enabled
\x1b[32m[INFO  14:30:03]\x1b[0m engine.lua:292: [audit.5.1.1] MANUAL: No reinforce steps for audit rules
\x1b[32m[INFO  14:30:04]\x1b[0m engine.lua:292: SEHarden Finished. 20 passed, 0 fixed, 2 failed, 1 manual, 0 dry-run-pending / 23 total.
"""

LOONGSHIELD_VERBOSE_SCAN_WITH_FAILURES = """\
\x1b[1;36mSEHarden scan\x1b[0m: profile='loongshield_e2e', level='strict', 2 rule(s)
  \x1b[1;32mPASS\x1b[0m [e2e.max_auth_tries] Ensure fixture MaxAuthTries is hardened
  \x1b[1;31mFAIL\x1b[0m [e2e.strict_banner] Ensure fixture SSH banner is configured
    \x1b[33mreason:\x1b[0m actual: nil
\x1b[1;36mSummary:\x1b[0m \x1b[32m1 passed\x1b[0m, \x1b[32m0 fixed\x1b[0m, \x1b[31m1 failed\x1b[0m, \x1b[2m0 manual\x1b[0m, \x1b[2m0 dry-run-pending\x1b[0m / 2 total
"""

LOONGSHIELD_REINFORCE = """\
\x1b[33m[WARN  14:30:01]\x1b[0m engine.lua:186: [fs.udf_disabled] FAIL: Ensure mounting of udf is disabled
\x1b[31m[ERROR 14:30:04]\x1b[0m engine.lua:307: [fs.shadow_perms] FAILED-TO-FIX: Cannot set file permissions on /etc/shadow
\x1b[31m[ERROR 14:30:04]\x1b[0m engine.lua:295: [kern.sysctl_apply] ENFORCE-ERROR: Failed to apply sysctl setting
\x1b[32m[INFO  14:30:05]\x1b[0m engine.lua:292: SEHarden Finished. 18 passed, 1 fixed, 1 failed, 0 manual, 0 dry-run-pending / 20 total.
"""

LOONGSHIELD_REINFORCE_FIXED = """\
  \x1b[1;31mFAIL\x1b[0m [e2e.max_auth_tries] Ensure fixture MaxAuthTries is hardened
    \x1b[33mreason:\x1b[0m actual: 6
\x1b[32m[INFO  14:30:05]\x1b[0m engine.lua:292: [e2e.max_auth_tries] FIXED: Ensure fixture MaxAuthTries is hardened
\x1b[32m[INFO  14:30:06]\x1b[0m engine.lua:292: SEHarden Finished. 0 passed, 1 fixed, 0 failed, 0 manual, 0 dry-run-pending / 1 total.
"""

LOONGSHIELD_REINFORCE_MIXED_FAIL_AND_FIXED = """\
  \x1b[1;31mFAIL\x1b[0m [e2e.strict_banner] Ensure fixture SSH banner is configured
    \x1b[33mreason:\x1b[0m actual: nil
\x1b[32m[INFO  14:30:05]\x1b[0m engine.lua:292: [e2e.max_auth_tries] FIXED: Ensure fixture MaxAuthTries is hardened
\x1b[33m[WARN  14:30:06]\x1b[0m engine.lua:186: [fs.udf_disabled] FAIL: Ensure mounting of udf is disabled
\x1b[32m[INFO  14:30:07]\x1b[0m engine.lua:292: SEHarden Finished. 0 passed, 2 fixed, 1 failed, 0 manual, 0 dry-run-pending / 3 total.
"""

LOONGSHIELD_DRYRUN = """\
\x1b[32m[INFO  14:30:01]\x1b[0m engine.lua:298: [fs.cramfs_blacklist] DRY-RUN: would apply cramfs blacklist
\x1b[32m[INFO  14:30:02]\x1b[0m engine.lua:298: [svc.chronyd_enable] DRY-RUN: would enable chronyd
\x1b[32m[INFO  14:30:03]\x1b[0m engine.lua:292: SEHarden Finished. 20 passed, 0 fixed, 0 failed, 0 manual, 2 dry-run-pending / 22 total.
"""

LOONGSHIELD_ENGINE_ERROR = """\
\x1b[31m[ERROR 14:30:04]\x1b[0m engine.lua:350: Engine Error: config file not found: /etc/missing.conf
\x1b[32m[INFO  14:30:05]\x1b[0m engine.lua:292: SEHarden Finished. 0 passed, 0 fixed, 0 failed, 0 manual, 0 dry-run-pending / 0 total.
"""


def _mock_proc(stdout: str, returncode: int = 0) -> subprocess.CompletedProcess:
    return subprocess.CompletedProcess(
        args=["/usr/bin/loongshield", "seharden"],
        returncode=returncode,
        stdout=stdout,
    )


class TestBuildCommand(unittest.TestCase):
    def test_build_command_with_resolved_binary(self):
        cmd = HardeningBackend._build_command(
            ["--scan", "--config", "agentos_baseline"],
            loongshield_path="/usr/bin/loongshield",
        )
        self.assertEqual(
            cmd,
            [
                "/usr/bin/loongshield",
                "seharden",
                "--scan",
                "--config",
                "agentos_baseline",
            ],
        )

    def test_build_command_without_resolved_binary(self):
        cmd = HardeningBackend._build_command(["--reinforce", "--dry-run"])
        self.assertEqual(cmd, ["loongshield", "seharden", "--reinforce", "--dry-run"])


class TestStripAnsi(unittest.TestCase):
    def test_strips_colour_codes(self):
        self.assertEqual(_strip_ansi("\x1b[32mGREEN\x1b[0m normal"), "GREEN normal")


class TestHardeningExecute(unittest.TestCase):
    def setUp(self):
        self.backend = HardeningBackend()
        self.ctx = RequestContext(action="harden")

    @patch("agent_sec_cli.security_middleware.backends.hardening.os.access")
    @patch("agent_sec_cli.security_middleware.backends.hardening.os.path.isfile")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_missing_loongshield_returns_clear_error(
        self, mock_which, mock_isfile, mock_access
    ):
        mock_which.return_value = None
        mock_isfile.return_value = False
        mock_access.return_value = False

        with self.assertLogs(
            "agent_sec_cli.security_middleware.backends.hardening", level="WARNING"
        ) as logs:
            result = self.backend.execute(self.ctx, args=["--scan"])

        self.assertFalse(result.success)
        self.assertEqual(result.exit_code, 127)
        self.assertEqual(result.error, _MISSING_LOONGSHIELD_ERROR)
        self.assertEqual(result.data["argv"], ["loongshield", "seharden", "--scan"])
        self.assertEqual(result.data["failures"], [])
        self.assertEqual(result.data["fixed_items"], [])
        self.assertEqual(len(logs.records), 1)
        record = logs.records[0]
        self.assertEqual(record.levelname, "WARNING")
        self.assertEqual(record.trace_id, self.ctx.trace_id)
        # Domain fields live under `data` in the new schema.
        self.assertEqual(record.data["action"], "harden")
        self.assertEqual(record.data["exit_code"], 127)
        self.assertEqual(record.data["error_type"], "FileNotFoundError")

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.os.access")
    @patch("agent_sec_cli.security_middleware.backends.hardening.os.path.isfile")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_falls_back_to_packaged_sbindir_binary(
        self, mock_which, mock_isfile, mock_access, mock_run
    ):
        mock_which.return_value = None
        mock_isfile.return_value = True
        mock_access.return_value = True
        mock_run.return_value = _mock_proc(LOONGSHIELD_ALL_PASS, 0)

        result = self.backend.execute(self.ctx, args=["--scan"])

        mock_run.assert_called_once_with(
            [
                "/usr/sbin/loongshield",
                "seharden",
                "--scan",
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self.assertTrue(result.success)
        self.assertEqual(result.data["tool_path"], "/usr/sbin/loongshield")

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_no_args_preserve_legacy_default_scan_and_config(
        self, mock_which, mock_run
    ):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_ALL_PASS, 0)

        result = self.backend.execute(self.ctx)

        mock_run.assert_called_once_with(
            [
                "/usr/bin/loongshield",
                "seharden",
                "--scan",
                "--config",
                "agentos_baseline",
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self.assertTrue(result.success)
        self.assertEqual(result.data["mode"], "scan")
        self.assertEqual(result.data["config"], "agentos_baseline")

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_passthrough_args_are_executed_verbatim_and_parsed(
        self, mock_which, mock_run
    ):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_WITH_FAILURES, 1)

        result = self.backend.execute(
            self.ctx, args=["--scan", "--config", "agentos_baseline"]
        )

        mock_run.assert_called_once_with(
            [
                "/usr/bin/loongshield",
                "seharden",
                "--scan",
                "--config",
                "agentos_baseline",
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self.assertFalse(result.success)
        self.assertEqual(result.exit_code, 1)
        self.assertEqual(result.data["passed"], 20)
        self.assertEqual(result.data["failed"], 2)
        self.assertEqual(result.data["manual"], 1)
        self.assertEqual(len(result.data["failures"]), 3)
        self.assertEqual(result.data["fixed_items"], [])
        self.assertEqual(
            result.data["raw_args"], ["--scan", "--config", "agentos_baseline"]
        )

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_nonzero_exit_code_is_preserved(self, mock_which, mock_run):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_WITH_FAILURES, 3)

        result = self.backend.execute(self.ctx, args=["--scan"])

        self.assertFalse(result.success)
        self.assertEqual(result.exit_code, 3)
        self.assertEqual(result.data["returncode"], 3)

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_verbose_summary_and_rule_status_are_parsed(self, mock_which, mock_run):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_VERBOSE_SCAN_WITH_FAILURES, 1)

        result = self.backend.execute(self.ctx, args=["--scan", "--verbose"])

        self.assertFalse(result.success)
        self.assertEqual(result.data["passed"], 1)
        self.assertEqual(result.data["failed"], 1)
        self.assertEqual(result.data["total"], 2)
        self.assertEqual(
            result.data["failures"],
            [
                {
                    "rule_id": "e2e.strict_banner",
                    "status": "FAIL",
                    "message": "Ensure fixture SSH banner is configured",
                }
            ],
        )

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_legacy_mode_and_config_are_translated(self, mock_which, mock_run):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_REINFORCE, 1)

        result = self.backend.execute(
            self.ctx,
            mode="reinforce",
            config="agentos_baseline",
        )

        mock_run.assert_called_once_with(
            [
                "/usr/bin/loongshield",
                "seharden",
                "--reinforce",
                "--config",
                "agentos_baseline",
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        self.assertEqual(result.data["mode"], "reinforce")
        self.assertEqual(result.data["config"], "agentos_baseline")

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_reinforce_results_keep_failures_and_fixed_items(
        self, mock_which, mock_run
    ):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_REINFORCE, 1)

        result = self.backend.execute(
            self.ctx, args=["--reinforce", "--config", "agentos_baseline"]
        )

        self.assertEqual(result.data["fixed"], 1)
        self.assertEqual(len(result.data["fixed_items"]), 1)
        self.assertEqual(result.data["fixed_items"][0]["status"], "FAIL")
        statuses = [item["status"] for item in result.data["failures"]]
        self.assertIn("FAILED-TO-FIX", statuses)
        self.assertIn("ENFORCE-ERROR", statuses)

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_reinforce_fixed_status_is_preferred_over_verbose_fail(
        self, mock_which, mock_run
    ):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_REINFORCE_FIXED, 0)

        result = self.backend.execute(
            self.ctx, args=["--reinforce", "--verbose", "--config", "agentos_baseline"]
        )

        self.assertTrue(result.success)
        self.assertEqual(result.data["fixed"], 1)
        self.assertEqual(result.data["failures"], [])
        self.assertEqual(
            result.data["fixed_items"],
            [
                {
                    "rule_id": "e2e.max_auth_tries",
                    "status": "FIXED",
                    "message": "Ensure fixture MaxAuthTries is hardened",
                }
            ],
        )

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_reinforce_distinguishes_verbose_fail_from_legacy_fail(
        self, mock_which, mock_run
    ):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(
            LOONGSHIELD_REINFORCE_MIXED_FAIL_AND_FIXED, 1
        )

        result = self.backend.execute(
            self.ctx, args=["--reinforce", "--verbose", "--config", "agentos_baseline"]
        )

        self.assertFalse(result.success)
        self.assertEqual(result.data["fixed"], 2)
        self.assertEqual(result.data["failed"], 1)
        self.assertEqual(
            result.data["failures"],
            [
                {
                    "rule_id": "e2e.strict_banner",
                    "status": "FAIL",
                    "message": "Ensure fixture SSH banner is configured",
                }
            ],
        )
        self.assertEqual(
            result.data["fixed_items"],
            [
                {
                    "rule_id": "e2e.max_auth_tries",
                    "status": "FIXED",
                    "message": "Ensure fixture MaxAuthTries is hardened",
                },
                {
                    "rule_id": "fs.udf_disabled",
                    "status": "FAIL",
                    "message": "Ensure mounting of udf is disabled",
                },
            ],
        )

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_dry_run_entries_are_reported(self, mock_which, mock_run):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_DRYRUN, 0)

        result = self.backend.execute(
            self.ctx, args=["--reinforce", "--dry-run", "--config", "agentos_baseline"]
        )

        self.assertTrue(result.success)
        self.assertEqual(result.data["mode"], "dry-run")
        self.assertEqual(result.data["dry_run_pending"], 2)
        statuses = [item["status"] for item in result.data["failures"]]
        self.assertIn("DRY-RUN", statuses)

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_dry_run_mode_detection_is_order_independent(self, mock_which, mock_run):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_DRYRUN, 0)

        result = self.backend.execute(
            self.ctx, args=["--dry-run", "--reinforce", "--config", "agentos_baseline"]
        )

        self.assertEqual(result.data["mode"], "dry-run")

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_engine_error_is_kept_in_failures(self, mock_which, mock_run):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(LOONGSHIELD_ENGINE_ERROR, 1)

        result = self.backend.execute(self.ctx, args=["--scan"])

        engine_errors = [
            item for item in result.data["failures"] if item["status"] == "Engine Error"
        ]
        self.assertEqual(len(engine_errors), 1)
        self.assertIn("config file not found", engine_errors[0]["message"])

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_no_summary_line_keeps_metadata_only(self, mock_which, mock_run):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc("some random output\n", 0)

        result = self.backend.execute(self.ctx, args=["--help"])

        self.assertTrue(result.success)
        self.assertNotIn("passed", result.data)
        self.assertNotIn("mode", result.data)
        self.assertEqual(result.stdout, "some random output\n")

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_fallback_when_summary_reports_unparsed_failures(
        self, mock_which, mock_run
    ):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.return_value = _mock_proc(
            "[WARN  14:30:01] engine.lua:186: [~~weird~~] ???: something odd\n"
            "[INFO  14:30:02] engine.lua:292: SEHarden Finished. "
            "22 passed, 0 fixed, 1 failed, 0 manual, 0 dry-run-pending / 23 total.\n",
            1,
        )

        result = self.backend.execute(self.ctx, args=["--scan"])

        self.assertEqual(len(result.data["failures"]), 1)
        self.assertEqual(result.data["failures"][0]["status"], "UNKNOWN")
        self.assertIn("could not be parsed", result.data["failures"][0]["message"])

    @patch("agent_sec_cli.security_middleware.backends.hardening.subprocess.run")
    @patch("agent_sec_cli.security_middleware.backends.hardening.shutil.which")
    def test_oserror_is_reported_clearly(self, mock_which, mock_run):
        mock_which.return_value = "/usr/bin/loongshield"
        mock_run.side_effect = OSError("Permission denied")

        with self.assertLogs(
            "agent_sec_cli.security_middleware.backends.hardening", level="ERROR"
        ) as logs:
            result = self.backend.execute(self.ctx, args=["--scan"])

        self.assertFalse(result.success)
        self.assertIn("Failed to execute `loongshield seharden`", result.error)
        self.assertIn("Permission denied", result.error)
        self.assertEqual(len(logs.records), 1)
        record = logs.records[0]
        self.assertEqual(record.levelname, "ERROR")
        self.assertEqual(record.trace_id, self.ctx.trace_id)
        # action/exit_code go via `data`; error_type is derived from exc_info
        # by the JSONL handler, so it does not need to be in `extra=`.
        self.assertEqual(record.data["action"], "harden")
        self.assertEqual(record.data["exit_code"], 1)
        self.assertIsNotNone(record.exc_info)
        self.assertIs(record.exc_info[0], OSError)

    def test_unknown_legacy_kwargs_are_rejected(self):
        with self.assertRaises(TypeError):
            self.backend.execute(self.ctx, profile="agentos_baseline")

    def test_mixing_args_and_legacy_kwargs_is_rejected(self):
        with self.assertRaises(TypeError):
            self.backend.execute(
                self.ctx,
                args=["--scan"],
                mode="reinforce",
            )


if __name__ == "__main__":
    unittest.main()
