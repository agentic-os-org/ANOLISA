"""`getcap` echoes the binary path, so its output is not ASCII either.

``has_ebpf_privilege`` decodes ``getcap`` output with ``text=True`` and no
``encoding``, i.e. with ``locale.getpreferredencoding``. ``getcap`` prints the
file it inspected next to the capability set and repeats the path in its
diagnostic for a file it cannot read, so a binary outside an ASCII path made
the privilege preflight raise ``UnicodeDecodeError`` (a ``ValueError``, not the
``OSError`` a caller can handle) under the C locale instead of reporting an
absent capability.
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

HELPER = (
    "import base64,os,sys\n"
    f"sys.path.insert(0, r'{BENCHMARK_DIR / 'campaign'}')\n"
    f"sys.path.insert(0, r'{BENCHMARK_DIR / 'single_run'}')\n"
    "import run_campaign\n"
    "from pathlib import Path\n"
    "binary = Path(os.fsdecode(base64.urlsafe_b64decode(sys.argv[1])))\n"
    "print('privileged' if run_campaign.has_ebpf_privilege(binary) else 'no')\n"
)


# The helper imports `run_campaign`, which needs `typing.Self` (Python 3.11+),
# so an older interpreter fails inside the helper instead of exercising the
# decoder. Probe the import here so the suite can skip rather than fail.
try:
    import run_campaign  # noqa: F401
except ImportError:  # Python < 3.11
    run_campaign = None


@unittest.skipIf(run_campaign is None, "run_campaign requires Python 3.11+ (typing.Self)")
@unittest.skipUnless(os.name == "posix", "the forced C locale fixture requires POSIX")
class GetcapOutputEncodingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def run_check(self, binary: Path, bin_dir: Path):
        return run_helper(
            self.root, HELPER, encode(str(binary)), path_prefix=bin_dir
        )

    def test_non_ascii_path_in_getcap_output_is_decoded_as_utf8(self) -> None:
        binary = non_ascii_directory(self.root) / "agentsight"
        binary.write_bytes(b"\x7fELF")
        echoed = os.fsencode(binary) + b" cap_bpf,cap_perfmon=ep\n"
        result = self.run_check(binary, fake_utility(self.root, "getcap", echoed, b"", 0))
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "privileged")

    def test_non_ascii_path_in_a_getcap_error_is_decoded_as_utf8(self) -> None:
        binary = non_ascii_directory(self.root) / "agentsight"
        diagnostic = os.fsencode(binary) + b": No such file or directory\n"
        bin_dir = fake_utility(self.root, "getcap", b"", diagnostic, status=1)
        result = self.run_check(binary, bin_dir)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "no")

    def test_ascii_capability_output_is_unaffected(self) -> None:
        binary = self.root / "agentsight"
        binary.write_bytes(b"\x7fELF")
        echoed = os.fsencode(binary) + b" cap_bpf,cap_perfmon=ep\n"
        result = self.run_check(binary, fake_utility(self.root, "getcap", echoed, b"", 0))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "privileged")


if __name__ == "__main__":
    unittest.main()
