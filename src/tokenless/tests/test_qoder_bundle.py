#!/usr/bin/env python3
"""Exercise standalone package validation and detached-cache dispatch."""

import importlib.util
import json
import os
import platform
import re
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


class QoderBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="qoder bundle ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.prebuilt = self.root / "prebuilt"
        system = "darwin" if platform.system() == "Darwin" else "linux"
        arch = "arm64" if platform.machine() in ("arm64", "aarch64") else "x64"
        self.target = f"{system}-{arch}"
        self.source = self.prebuilt / self.target
        self.source.mkdir(parents=True)
        self.version = re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)[
            1
        ]
        (self.source / "version.txt").write_text(self.version)
        header = bytearray(64)
        if system == "darwin":
            header[:4] = b"\xcf\xfa\xed\xfe"
            struct.pack_into("<I", header, 4, 0x0100000C if arch == "arm64" else 0x01000007)
        else:
            header[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<H", header, 18, 183 if arch == "arm64" else 62)
        for name in ("tokenless", "rtk"):
            (self.source / name).write_bytes(header)
        self.output = self.root / "detached cache"

    def package(self):
        return subprocess.run(
            [
                sys.executable,
                str(ROOT / "packaging/qoder/package.py"),
                "--prebuilt-root",
                str(self.prebuilt),
                "--target",
                self.target,
                "--output",
                str(self.output),
            ],
            capture_output=True,
            text=True,
        )

    def test_detached_bundle_uses_own_binaries_and_disables_recovery(self):
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stderr)
        native = self.output / "native" / self.target
        (native / "tokenless").write_text("#!/bin/sh\necho bundled\n")
        (native / "rtk").write_text("#!/bin/sh\necho rtk\n")
        hook = self.output / "common/hooks/compress_response_hook.py"
        hook.write_text(
            "import json, os, subprocess\n"
            "print(json.dumps({'binary': subprocess.check_output(['tokenless']).decode().strip(),"
            "'recovery': os.environ.get('TOKENLESS_DISABLE_SHELL_RECOVERY')}))\n"
        )
        env = dict(os.environ, HOME=str(self.root / "empty-home"))
        result = subprocess.run(
            ["bash", str(self.output / "hooks/run-hook.sh"), "compress_response_hook.py"],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"binary": "bundled", "recovery": "1"})
        (native / "tokenless").unlink()
        result = subprocess.run(
            ["bash", str(self.output / "hooks/run-hook.sh"), "compress_response_hook.py"],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.stdout.strip(), "{}")
        self.assertIn("Missing bundled executable", result.stderr)

    def test_rejects_wrong_version_before_creating_output(self):
        (self.source / "version.txt").write_text("wrong")
        self.assertNotEqual(self.package().returncode, 0)
        self.assertFalse(self.output.exists())

    def test_rejects_wrong_architecture_and_missing_binary(self):
        (self.source / "rtk").write_bytes(b"not a native binary")
        self.assertNotEqual(self.package().returncode, 0)
        self.assertFalse(self.output.exists())
        (self.source / "rtk").unlink()
        self.assertNotEqual(self.package().returncode, 0)
        self.assertFalse(self.output.exists())

    def test_refuses_to_overwrite_existing_output(self):
        self.output.mkdir()
        marker = self.output / "keep"
        marker.write_text("keep")
        self.assertNotEqual(self.package().returncode, 0)
        self.assertEqual(marker.read_text(), "keep")

    def test_bundle_does_not_advertise_shell_recovery(self):
        spec = importlib.util.spec_from_file_location(
            "bundle_hook_utils", ROOT / "adapters/tokenless/common/hooks/hook_utils.py"
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with patch.object(module.shutil, "which", return_value="/bin/tokenless"):
            with patch.dict(os.environ, {"TOKENLESS_DISABLE_SHELL_RECOVERY": "1"}):
                self.assertFalse(module.tokenless_retrieve_command_available())
            with patch.dict(os.environ, {"TOKENLESS_DISABLE_SHELL_RECOVERY": "0"}):
                self.assertTrue(module.tokenless_retrieve_command_available())


if __name__ == "__main__":
    unittest.main()
