#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for install_openclaw.py gateway readiness failure.

Regression tests for the installer exiting 0 after the gateway port never
listened: wait_gateway_ready used to print diagnostics and return, and the
only hard failure came from ensure_gateway_write_scope, which returns
immediately under --skip-gateway-write-check. wait_gateway_ready must now
fail unconditionally; the skip flag only skips the write smoke test.
"""

import importlib.util
import os
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "install_openclaw.py")

# Minimal stubs for everything the installer runs externally during the
# gateway phase (audit technique: stub PATH binaries).
STUBS = {
    "openclaw": (
        "#!/bin/sh\n"
        "case \"$1 $2\" in\n"
        "  'gateway status') echo 'stub: gateway status (no-op, does not start anything)'; exit 0;;\n"
        "  *) echo \"stub: $* (no-op, does not start anything)\"; exit 0;;\n"
        "esac\n"
    ),
    "node": "#!/bin/sh\nif [ \"$1\" = \"-p\" ]; then echo 22; else echo \"v22.0.0\"; fi\n",
    "npm": "#!/bin/sh\necho \"10.0.0\"\n",
    "ps": "#!/bin/sh\necho \"PID TTY TIME CMD\"; exit 0\n",
    "journalctl": "#!/bin/sh\nexit 1\n",
}


def load_module():
    spec = importlib.util.spec_from_file_location("install_openclaw_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def make_stub_bin(root):
    bin_dir = os.path.join(root, "stubbin")
    os.makedirs(bin_dir, exist_ok=True)
    for name, body in STUBS.items():
        path = os.path.join(bin_dir, name)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(body)
        os.chmod(path, 0o755)
    return bin_dir


class Args:
    dry_run = False
    skip_gateway_write_check = False
    gateway_port = 18799
    gateway_status_timeout = 1
    gateway_ready_timeout = 0
    gateway_log = "/nonexistent/openclaw-setup.log"


class TestWaitGatewayReadyFailsHard(unittest.TestCase):
    def setUp(self):
        self.module = load_module()
        self.calls = []
        self.module.run_command = lambda cmd, **kw: self.calls.append(cmd) or subprocess.CompletedProcess(cmd, 0, "", "")
        self.module.print_gateway_logs = lambda args: self.calls.append(["print_gateway_logs"])

    def test_not_listening_raises_system_exit(self):
        """A port that never listens must fail wait_gateway_ready itself."""
        args = Args()
        with self.assertRaises(SystemExit) as ctx:
            self.module.wait_gateway_ready(args)
        self.assertNotEqual(ctx.exception.code, 0)
        self.assertTrue(any("gateway status" in " ".join(c) for c in self.calls))

    def test_listening_port_returns_normally(self):
        """Control: an actually listening gateway still passes readiness."""
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            sock.listen(1)
            args = Args()
            args.gateway_port = sock.getsockname()[1]
            args.gateway_ready_timeout = 5
            self.assertIsNone(self.module.wait_gateway_ready(args))

    def test_dry_run_skips_readiness(self):
        args = Args()
        args.dry_run = True
        self.assertIsNone(self.module.wait_gateway_ready(args))


class TestInstallerEndToEnd(unittest.TestCase):
    """The full installer must exit non-zero when the gateway never listens."""

    TIMEOUT = 120

    def run_installer(self, root, extra):
        bin_dir = make_stub_bin(root)
        home = os.path.join(root, "home")
        os.makedirs(home, exist_ok=True)
        env = dict(os.environ)
        env["PATH"] = bin_dir + os.pathsep + env.get("PATH", "")
        env["HOME"] = home
        cmd = [
            sys.executable, SCRIPT,
            "--skip-install-openclaw", "--skip-preflight", "--skip-tokenless",
            "--dashscope-api-key", "sk-test",
            "--gateway-port", "18799",
            "--gateway-ready-timeout", "2", "--gateway-status-timeout", "2",
        ] + extra
        return subprocess.run(cmd, capture_output=True, text=True,
                              timeout=self.TIMEOUT, env=env)

    def test_skip_write_check_still_fails_when_not_listening(self):
        with tempfile.TemporaryDirectory() as root:
            result = self.run_installer(root, ["--skip-gateway-write-check"])
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("Gateway port did not listen", result.stdout + result.stderr)
            self.assertNotIn("Skipping gateway write-scope check", result.stdout)

    def test_without_skip_flag_fails_with_honest_message(self):
        with tempfile.TemporaryDirectory() as root:
            result = self.run_installer(root, [])
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("did not listen", result.stdout + result.stderr)
            self.assertNotIn(
                "Gateway port is listening", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
