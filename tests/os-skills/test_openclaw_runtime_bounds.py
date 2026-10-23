"""Invalid ports and timeouts must stop before installer side effects."""

import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/ai/install-openclaw/scripts/install_openclaw.py"
)
TIMEOUTS = (
    "--gateway-command-timeout",
    "--gateway-ready-timeout",
    "--gateway-status-timeout",
    "--gateway-write-check-timeout",
    "--preflight-timeout",
)


class OpenClawRuntimeBoundsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("openclaw_runtime_bounds", SCRIPT)
        cls.installer = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = cls.installer
        spec.loader.exec_module(cls.installer)

    def assert_invalid(self, option, value):
        errors = io.StringIO()
        with (
            mock.patch.object(sys, "argv", [str(SCRIPT), "--precheck-only", option, value]),
            mock.patch.object(self.installer, "dependency_precheck") as precheck,
            mock.patch.object(self.installer, "build_config") as build,
            contextlib.redirect_stderr(errors),
            self.assertRaises(SystemExit) as error,
        ):
            self.installer.main()
        self.assertEqual(error.exception.code, 2)
        self.assertIn(option, errors.getvalue())
        precheck.assert_not_called()
        build.assert_not_called()

    def test_timeouts_reject_zero_and_negative(self):
        for option in TIMEOUTS:
            for value in ("0", "-1"):
                with self.subTest(option=option, value=value):
                    self.assert_invalid(option, value)

    def test_port_rejects_non_tcp_bounds(self):
        for value in ("0", "-1", "65536", "999999"):
            with self.subTest(value=value):
                self.assert_invalid("--gateway-port", value)

    def test_nonnumeric_values_still_fail_cleanly(self):
        for option in (*TIMEOUTS, "--gateway-port"):
            with self.subTest(option=option):
                self.assert_invalid(option, "1.5")

    def test_positive_timeout_values_are_accepted(self):
        for option in TIMEOUTS:
            with (
                self.subTest(option=option),
                mock.patch.object(sys, "argv", [str(SCRIPT), option, "1"]),
            ):
                args = self.installer.parse_args()
                self.assertEqual(getattr(args, option[2:].replace("-", "_")), 1)

    def test_tcp_port_boundaries_are_accepted(self):
        for value in ("1", "65535"):
            with (
                self.subTest(value=value),
                mock.patch.object(sys, "argv", [str(SCRIPT), "--gateway-port", value]),
            ):
                self.assertEqual(self.installer.parse_args().gateway_port, int(value))

    def test_default_runtime_values_remain_unchanged(self):
        with mock.patch.object(sys, "argv", [str(SCRIPT)]):
            args = self.installer.parse_args()
        self.assertEqual(args.gateway_port, 18789)
        self.assertEqual(
            [getattr(args, option[2:].replace("-", "_")) for option in TIMEOUTS],
            [30, 30, 8, 60, 20],
        )

    def test_valid_precheck_mode_reaches_only_dependency_check(self):
        with (
            mock.patch.object(
                sys,
                "argv",
                [str(SCRIPT), "--precheck-only", "--gateway-port", "1", "--preflight-timeout", "1"],
            ),
            mock.patch.object(self.installer, "dependency_precheck") as precheck,
            mock.patch.object(self.installer, "print_api_key_guidance"),
            mock.patch.object(self.installer, "build_config") as build,
        ):
            self.installer.main()
        precheck.assert_called_once()
        build.assert_not_called()


if __name__ == "__main__":
    unittest.main()
