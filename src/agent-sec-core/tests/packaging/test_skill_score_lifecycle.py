"""Bounded generated-environment removal and cross-process lease regressions."""

import fcntl
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "score_lifecycle", ROOT / "packaging/skill-score/optional/lifecycle.py"
)
assert SPEC and SPEC.loader
LIFECYCLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LIFECYCLE)


class LifecycleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name).resolve()
        self.root = self.parent / "prefix/lib/anolisa/sec-core-skill-score"
        self.initialize()

    def initialize(self) -> None:
        with LIFECYCLE.exclusive(self.root, create=True):
            LIFECYCLE.request_directory(self.root)

    def environment(self, *, retained: bool = False, receipt: bool = True) -> Path:
        manifest = {
            "schema": "agent-sec-skill-score/runtime-v1",
            "component": "sec-core-skill-score",
            "version": "test",
            "source_commit": "a" * 40,
            "protocol_version": "agent-sec-skill-score/worker-v1",
        }
        content = json.dumps(manifest).encode()
        identity = hashlib.sha256(content).hexdigest()
        name = identity + (".retained-" + "b" * 32 if retained else "")
        path = self.root / "environments" / name
        path.mkdir(parents=True)
        (path / "runtime-manifest.json").write_bytes(content)
        (path / "ready.json").write_text(
            json.dumps(
                {
                    **manifest,
                    "schema": "agent-sec-skill-score/ready-v1",
                    "manifest_sha256": identity,
                }
            )
        )
        if receipt:
            LIFECYCLE.record_environment(path, identity)
        (path / "python").mkdir()
        (path / "python/bin").write_text("generated interpreter fixture")
        return path

    def test_uninstall_removes_only_owned_current_and_retained_environments(
        self,
    ) -> None:
        current = self.environment()
        retained = self.environment(retained=True)
        (retained / "ready.json").write_text("damaged; retained during repair")
        external = self.parent / "user-environment"
        external.mkdir()
        (external / "keep").write_text("user data")
        (current / "external-link").symlink_to(external, target_is_directory=True)
        (self.root / "runtime-manifest.json").write_text("installer-owned payload")
        (self.root / "recovery").mkdir()
        LIFECYCLE.uninstall(self.root)
        self.assertEqual(
            sorted(p.name for p in self.root.iterdir()), ["runtime-manifest.json"]
        )
        self.assertEqual((external / "keep").read_text(), "user data")
        LIFECYCLE.uninstall(self.root)  # Retry after an unrelated installer failure.
        self.initialize()
        clean = self.environment()
        self.assertTrue(clean.exists())
        self.assertEqual(list((self.root / "requests").iterdir()), [])
        LIFECYCLE.uninstall(self.root)

    def test_pre_receipt_environment_requires_intact_manifest_and_ready_proof(
        self,
    ) -> None:
        path = self.environment(receipt=False)
        LIFECYCLE.uninstall(self.root)
        self.assertFalse(path.exists())

    def test_unknown_or_corrupt_environment_prevents_every_deletion(self) -> None:
        valid = self.environment()
        unknown = self.root / "environments/user-created"
        unknown.mkdir()
        with self.assertRaisesRegex(ValueError, "unknown generated"):
            LIFECYCLE.uninstall(self.root)
        self.assertTrue(valid.exists())
        unknown.rmdir()
        (valid / "runtime-manifest.json").write_text("{}")
        with self.assertRaisesRegex(ValueError, "unproven"):
            LIFECYCLE.uninstall(self.root)
        self.assertTrue(valid.exists())
        self.assertTrue((self.root / LIFECYCLE.LOCK).exists())

    def test_pending_recovery_and_forged_records_are_never_consumed(self) -> None:
        target = self.environment()
        for name in ("requests", "recovery"):
            directory = self.root / name
            directory.mkdir(exist_ok=True)
            record = directory / "untrusted.json"
            record.write_text('{"delete": "/unrelated-user-data"}')
            with self.assertRaisesRegex(RuntimeError, "unconfirmed scoring cleanup"):
                LIFECYCLE.uninstall(self.root)
            self.assertTrue(target.exists())
            record.unlink()

    def test_active_process_lease_blocks_uninstall_and_prepare(self) -> None:
        target = self.environment()
        code = (
            "import fcntl,sys; f=open(sys.argv[1]); "
            "fcntl.flock(f,fcntl.LOCK_SH); print('ready',flush=True); sys.stdin.read()"
        )
        child = subprocess.Popen(
            [sys.executable, "-c", code, str(self.root / LIFECYCLE.LOCK)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertEqual(child.stdout.readline().strip(), "ready")
            with self.assertRaisesRegex(RuntimeError, "active"):
                LIFECYCLE.uninstall(self.root)
            with self.assertRaisesRegex(RuntimeError, "active"):
                with LIFECYCLE.exclusive(self.root, create=True):
                    self.fail("preparation entered while scoring was active")
            self.assertTrue(target.exists())
        finally:
            child.communicate(timeout=5)
        LIFECYCLE.uninstall(self.root)
        self.assertFalse(target.exists())

    def test_owner_crash_releases_lock_but_pending_record_blocks_deletion(self) -> None:
        target = self.environment()
        code = (
            "import fcntl,os,sys; f=open(sys.argv[1]); fcntl.flock(f,fcntl.LOCK_SH); "
            "fd=os.open(sys.argv[2],os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600); "
            "os.write(fd,b'pending owned request'); os.close(fd); os._exit(9)"
        )
        record = self.root / "requests/crashed.json"
        child = subprocess.run(
            [sys.executable, "-c", code, str(self.root / LIFECYCLE.LOCK), str(record)],
            check=False,
        )
        self.assertEqual(child.returncode, 9)
        with self.assertRaisesRegex(RuntimeError, "unconfirmed"):
            LIFECYCLE.uninstall(self.root)
        self.assertTrue(target.exists())
        self.assertEqual(record.read_text(), "pending owned request")

    def test_symlink_environment_lock_and_ancestor_are_rejected(self) -> None:
        external = self.parent / "outside"
        external.mkdir()
        (external / "keep").write_text("untouched")
        path = self.root / "environments"
        path.symlink_to(external, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "real directory"):
            LIFECYCLE.uninstall(self.root)
        path.unlink()
        lock = self.root / LIFECYCLE.LOCK
        lock.unlink()
        lock.symlink_to(external / "keep")
        with self.assertRaises(OSError):
            LIFECYCLE.uninstall(self.root)
        link = self.parent / "link"
        link.symlink_to(self.root.parent, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "real directory"):
            LIFECYCLE.uninstall(link / self.root.name)
        self.assertEqual((external / "keep").read_text(), "untouched")

    def test_lock_inode_replacement_is_rejected(self) -> None:
        original = fcntl.flock

        def replace_after_lock(descriptor: int, operation: int) -> None:
            original(descriptor, operation)
            (self.root / LIFECYCLE.LOCK).unlink()
            (self.root / LIFECYCLE.LOCK).write_text("")

        with patch.object(LIFECYCLE.fcntl, "flock", side_effect=replace_after_lock):
            with self.assertRaisesRegex(ValueError, "lock changed"):
                with LIFECYCLE.exclusive(self.root):
                    self.fail("replaced lock was accepted")

    def test_generated_receipt_cannot_overwrite_a_symlink_or_hardlink(self) -> None:
        target = self.environment(receipt=False)
        external = self.parent / "user-data"
        external.write_text("unchanged")
        for symbolic in (True, False):
            receipt = target / ".generated.json"
            if symbolic:
                receipt.symlink_to(external)
            else:
                os.link(external, receipt)
            with self.assertRaisesRegex(ValueError, "unsafe"):
                LIFECYCLE.record_environment(target, target.name)
            self.assertEqual(external.read_text(), "unchanged")
            receipt.unlink()

    def test_hook_does_not_create_untracked_bytecode(self) -> None:
        self.environment()
        for name in ("lifecycle.py", "uninstall.py"):
            shutil.copyfile(
                ROOT / "packaging/skill-score/optional" / name, self.root / name
            )
        result = subprocess.run(
            [sys.executable, str(self.root / "uninstall.py")],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            sorted(path.name for path in self.root.iterdir()),
            ["lifecycle.py", "uninstall.py"],
        )


if __name__ == "__main__":
    unittest.main()
