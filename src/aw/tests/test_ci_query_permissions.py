"""The gate must run package readers, transactions and leases under child umasks."""

import importlib.util
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

AW = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("query_gate", AW / "scripts/check.py")
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class QueryPermissionTests(unittest.TestCase):
    def fixture(self) -> tuple[Path, Path]:
        (AW / "target").mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="query-umasks-", dir=AW / "target")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name).resolve()
        executable = root / "cargo"
        executable.write_text(
            f"#!{sys.executable}\n"
            "import json, os, pathlib, stat, sys\n"
            "root = pathlib.Path.cwd()\n"
            "capture = root / 'calls.jsonl'\n"
            "index = len(capture.read_text().splitlines()) if capture.exists() else 0\n"
            "probe = root / f'probe-{index}'\n"
            "probe.write_text('child mode')\n"
            "mode = stat.S_IMODE(probe.stat().st_mode)\n"
            "with capture.open('a') as stream:\n"
            "    stream.write(json.dumps([sys.argv[1:], mode]) + '\\n')\n"
            "if os.environ.get('FAIL_GROUP_WRITABLE') and mode == 0o664:\n"
            "    sys.exit(7)\n",
            encoding="utf-8",
        )
        executable.chmod(0o700)
        return root, root / "calls.jsonl"

    def test_package_targets_run_under_both_masks_without_changing_the_parent(self) -> None:
        root, capture = self.fixture()
        before = root / "parent-before"
        before.write_text("parent mode")
        with patch.object(gate, "AW", root), patch.dict(os.environ, {"PATH": str(root)}):
            gate.package_permissions()
        after = root / "parent-after"
        after.write_text("parent mode")
        self.assertEqual(stat.S_IMODE(before.stat().st_mode), stat.S_IMODE(after.stat().st_mode))
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertEqual(calls, [
            [["test", "--locked", "-p", *target], mode]
            for mode in (0o644, 0o664)
            for target in (
                ("aw-package", "--test", "database"), ("aw-cli", "--test", "inventory"),
                ("aw-package", "--lib"), ("aw-package", "--test", "management"),
                ("aw-host", "--test", "host", "package_leases"),
                ("aw-service", "--lib", "inspect::package_leases"),
                ("aw-exec", "--test", "packages"), ("aw-cli", "--test", "packages"),
            )
        ])

    def test_a_failure_in_the_group_writable_mask_fails_the_gate(self) -> None:
        root, capture = self.fixture()
        with patch.object(gate, "AW", root), patch.dict(
            os.environ, {"PATH": str(root), "FAIL_GROUP_WRITABLE": "1"}
        ), self.assertRaises(subprocess.CalledProcessError) as failure:
            gate.package_permissions()
        self.assertEqual(failure.exception.returncode, 7)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertEqual(len(calls), 9)
        self.assertEqual(calls[-1][1], 0o664)


if __name__ == "__main__":
    unittest.main()
