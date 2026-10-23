"""Reject bad build requests before dependency installs or system checks."""

import os
import subprocess
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/devops/kernel-dev/scripts/build-kernel.sh"
)


@unittest.skipUnless(os.name == "posix", "uses isolated shell function stubs")
class KernelArgumentTests(unittest.TestCase):
    def run_main(self, *args):
        source = SCRIPT.read_text(encoding="utf-8")
        source = source.rsplit('main "$@"', 1)[0]
        stubs = r"""
check_root() { echo ROOT; }
detect_system() { echo DETECT; if [ "${BAD_ARCH:-}" = 1 ]; then exit 99; fi; }
install_deps() { echo DEPENDENCIES; }
build_upstream() { printf 'UPSTREAM jobs=%s config=%s\n' "$PARALLEL_JOBS" "$CONFIG_TYPE"; }
build_srpm() { printf 'SRPM jobs=%s\n' "$PARALLEL_JOBS"; }
install_kernel() { printf 'INSTALL %s\n' "$1"; }
show_status() { echo STATUS; }
main "$@"
"""
        return subprocess.run(
            ["bash", "-c", source + stubs, str(SCRIPT), *args],
            env={
                **os.environ,
                "BAD_ARCH": "1" if args and args[0] in ("help", "--help", "-h", "status") else "0",
            },
            capture_output=True,
            text=True,
            timeout=10,
        )

    def assert_no_environment(self, result):
        for marker in ("ROOT", "DETECT", "DEPENDENCIES", "UPSTREAM jobs", "SRPM jobs", "INSTALL "):
            self.assertNotIn(marker, result.stdout)

    def test_help_does_not_require_supported_build_environment(self):
        for option in ("help", "--help", "-h"):
            with self.subTest(option=option):
                result = self.run_main(option)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("Usage:", result.stdout)
                self.assert_no_environment(result)

    def test_status_does_not_detect_build_architecture(self):
        result = self.run_main("status")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("STATUS", result.stdout)
        self.assert_no_environment(result)

    def test_invalid_jobs_stop_before_dependencies(self):
        for method in ("upstream", "srpm"):
            for jobs in ("0", "-1", "abc", "1.5"):
                with self.subTest(method=method, jobs=jobs):
                    result = self.run_main(method, "6.12.9", jobs)
                    self.assertEqual(result.returncode, 1)
                    self.assertIn("jobs", result.stdout.lower() + result.stderr.lower())
                    self.assert_no_environment(result)

    def test_unknown_config_stops_before_dependencies(self):
        result = self.run_main("upstream", "6.12.9", "4", "typo-config")
        self.assertEqual(result.returncode, 1)
        self.assertIn("config", result.stdout.lower())
        self.assert_no_environment(result)

    def test_unknown_method_stops_before_environment(self):
        result = self.run_main("typo-method")
        self.assertEqual(result.returncode, 1)
        self.assertIn("Unknown method", result.stdout)
        self.assert_no_environment(result)

    def test_invalid_install_method_is_rejected(self):
        result = self.run_main("install", "typo-method")
        self.assertEqual(result.returncode, 1)
        self.assert_no_environment(result)

    def test_valid_build_dispatch_preserves_jobs_and_config(self):
        for config in ("defconfig", "tinyconfig", "menuconfig", "current"):
            with self.subTest(config=config):
                result = self.run_main("upstream", "6.12.9", "4", config)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(f"UPSTREAM jobs=4 config={config}", result.stdout)
        result = self.run_main("srpm", "6.6.102", "2")
        self.assertEqual(result.returncode, 0)
        self.assertIn("SRPM jobs=2", result.stdout)

    def test_valid_install_dispatch_is_preserved(self):
        for method in ("upstream", "srpm"):
            with self.subTest(method=method):
                result = self.run_main("install", method)
                self.assertEqual(result.returncode, 0)
                self.assertIn("INSTALL " + method, result.stdout)


if __name__ == "__main__":
    unittest.main()
