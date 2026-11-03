"""`mount` and `umount` quote the storage path, so decode them as UTF-8.

``mounted_storage`` decodes both utilities with ``text=True`` and no
``encoding``, i.e. with ``locale.getpreferredencoding``. Each diagnostic quotes
the path involved (the bind source for ``mount``, the mount target for
``umount``), so a path outside ASCII made the storage-isolation context manager
raise ``UnicodeDecodeError`` -- a ``ValueError``, not the ``RuntimeError`` the
function reports -- while it was already masking the real failure.
"""

import os
import tempfile
import unittest
from pathlib import Path

from subprocess_encoding_support import (
    BENCHMARK_DIR,
    encode,
    fake_utility,
    non_ascii_directory,
    run_helper,
)

# `run_campaign` imports `typing.Self`, which Python 3.10 does not have. The
# script itself requires 3.11+, so the module import raises there; hold the
# failure so the class can skip instead of reporting an import error.
try:
    import run_campaign
except ImportError:  # Python < 3.11
    run_campaign = None

HELPER = (
    "import base64,sys\n"
    f"sys.path.insert(0, r'{BENCHMARK_DIR / 'campaign'}')\n"
    f"sys.path.insert(0, r'{BENCHMARK_DIR / 'single_run'}')\n"
    "import os,run_campaign\n"
    "from pathlib import Path\n"
    "decode=lambda value: Path(os.fsdecode(base64.urlsafe_b64decode(value)))\n"
    "run_campaign.STATE_MOUNT_TARGET = decode(sys.argv[2])\n"
    "storage = decode(sys.argv[1])\n"
    "def report(message):\n"
    "    sys.stdout.buffer.write((message + '\\n').encode('utf-8', 'backslashreplace'))\n"
    "    sys.stdout.buffer.flush()\n"
    "try:\n"
    "    with run_campaign.mounted_storage(storage):\n"
    "        report('inside')\n"
    "except RuntimeError as error:\n"
    "    report(f'error: {error}')\n"
)


@unittest.skipIf(run_campaign is None, "run_campaign requires Python 3.11+ (typing.Self)")
@unittest.skipUnless(os.name == "posix", "the forced C locale fixture requires POSIX")
class MountOutputEncodingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def run_storage(
        self, storage: Path, bin_dir: Path, target: Path
    ):
        return run_helper(
            self.root,
            HELPER,
            encode(str(storage)),
            encode(str(target)),
            path_prefix=bin_dir,
        )

    def test_non_ascii_storage_path_in_a_mount_error_is_decoded_as_utf8(self) -> None:
        storage = non_ascii_directory(self.root)
        diagnostic = b"mount: " + os.fsencode(storage) + b": must be superuser to use mount.\n"
        bin_dir = fake_utility(self.root, "mount", b"", diagnostic, status=32)
        result = self.run_storage(storage, bin_dir, run_campaign.STATE_MOUNT_TARGET)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)
        # The decode failure used to replace the actionable mount diagnostic.
        self.assertIn("must be superuser", result.stdout)
        self.assertNotIn("inside", result.stdout)

    def test_non_ascii_mount_target_in_an_umount_error_is_decoded_as_utf8(self) -> None:
        storage = self.root / "storage"
        storage.mkdir()
        target = Path(os.fsdecode(os.fsencode(self.root) + b"/" + b"\xe4\xb8\xad\xe6\x96\x87")) / ".agentsight"
        bin_dir = fake_utility(self.root, "mount", b"", b"", status=0)
        diagnostic = b"umount: " + os.fsencode(target) + b": not mounted.\n"
        fake_utility(self.root, "umount", b"", diagnostic, status=32)
        result = self.run_storage(storage, bin_dir, target)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("inside", result.stdout)
        self.assertIn("not mounted", result.stdout)

    def test_ascii_storage_path_is_unaffected(self) -> None:
        storage = self.root / "storage"
        storage.mkdir()
        bin_dir = fake_utility(self.root, "mount", b"", b"", status=0)
        fake_utility(self.root, "umount", b"", b"", status=0)
        result = self.run_storage(storage, bin_dir, Path("/var/log/sysak/.agentsight"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("inside", result.stdout)


if __name__ == "__main__":
    unittest.main()
