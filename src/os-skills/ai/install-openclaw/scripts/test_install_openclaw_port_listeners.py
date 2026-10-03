#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for install_openclaw.py gateway_port_listeners robustness.

Regression tests for the fuser branch catching only FileNotFoundError:
a hanging fuser crashed the installer with an uncaught
subprocess.TimeoutExpired, while the lsof branch already degraded
gracefully. Stub PATH binaries per the audit technique.
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "install_openclaw.py")


def load_module():
    spec = importlib.util.spec_from_file_location("install_openclaw_port_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def make_stub(root, name, body):
    bin_dir = os.path.join(root, "stubbin")
    os.makedirs(bin_dir, exist_ok=True)
    path = os.path.join(bin_dir, name)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(body)
    os.chmod(path, 0o755)
    return bin_dir


class Args:
    gateway_port = 18799
    gateway_status_timeout = 1


class StubPath:
    """Puts the stub bin dir first on PATH for the duration of a test."""

    def __init__(self, bin_dir):
        self.bin_dir = bin_dir
        self.old_path = os.environ.get("PATH", "")

    def __enter__(self):
        os.environ["PATH"] = self.bin_dir + os.pathsep + self.old_path
        return self

    def __exit__(self, *a):
        os.environ["PATH"] = self.old_path
        return False


class TestGatewayPortListeners(unittest.TestCase):
    def test_hanging_fuser_returns_empty(self):
        """A wedged fuser must degrade to lsof instead of raising."""
        module = load_module()
        with tempfile.TemporaryDirectory() as root:
            make_stub(root, "fuser", "#!/bin/sh\nsleep 30\n")
            make_stub(root, "lsof", "#!/bin/sh\nexit 1\n")
            with StubPath(os.path.join(root, "stubbin")):
                self.assertEqual(module.gateway_port_listeners(Args()), [])

    def test_hanging_fuser_and_lsof_returns_empty(self):
        """Both tools wedged: no traceback, empty pid list."""
        module = load_module()
        with tempfile.TemporaryDirectory() as root:
            make_stub(root, "fuser", "#!/bin/sh\nsleep 30\n")
            make_stub(root, "lsof", "#!/bin/sh\nsleep 30\n")
            with StubPath(os.path.join(root, "stubbin")):
                self.assertEqual(module.gateway_port_listeners(Args()), [])

    def test_fuser_reporting_pid_wins(self):
        """Control: a healthy fuser still yields its pid."""
        module = load_module()
        with tempfile.TemporaryDirectory() as root:
            make_stub(root, "fuser", "#!/bin/sh\necho '18799/tcp:'; echo ' 4321'\n")
            with StubPath(os.path.join(root, "stubbin")):
                self.assertEqual(module.gateway_port_listeners(Args()), ["4321"])

    def test_lsof_fallback_after_fuser_timeout(self):
        """A timed-out fuser falls through to a working lsof."""
        module = load_module()
        with tempfile.TemporaryDirectory() as root:
            make_stub(root, "fuser", "#!/bin/sh\nsleep 30\n")
            make_stub(root, "lsof", "#!/bin/sh\necho 8765\n")
            with StubPath(os.path.join(root, "stubbin")):
                self.assertEqual(module.gateway_port_listeners(Args()), ["8765"])


if __name__ == "__main__":
    unittest.main()
