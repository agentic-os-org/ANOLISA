"""Enforce configuration subprocess limits while output is still streaming."""

import os
from pathlib import Path
import sys
import tempfile
import unittest

from aw_sdk import AwError
from aw_sdk.configuration import load_configuration


class ConfigurationTests(unittest.TestCase):
    def test_stdout_and_stderr_overflow_stop_and_reap_the_writer(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aw-config-limit-", dir="/tmp") as directory:
            root = Path(directory)
            for fd in (1, 2):
                with self.subTest(fd=fd):
                    executable = root / "writer"
                    executable.write_text(
                        f"#!{sys.executable}\nimport os\nfrom pathlib import Path\n"
                        f"Path({str(root / 'pid')!r}).write_text(str(os.getpid()))\n"
                        f"while True: os.write({fd}, b'x' * 65536)\n"
                    )
                    executable.chmod(0o700)
                    with self.assertRaisesRegex(AwError, "sdk_configuration_too_large"):
                        load_configuration(
                            str(root / "aw.yaml"),
                            {"adapter": "fixture", "version": "v1", "entrypoint": "worker"},
                            None,
                            str(executable),
                        )
                    pid = int((root / "pid").read_text())
                    self.assertFalse(Path(f"/proc/{pid}").exists())
